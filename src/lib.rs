#[allow(clippy::cast_possible_truncation)]
mod background_merge;
mod config;
mod engines;
mod error;
mod runtime_config;
mod sql;
mod storage;
mod tcp_io_parser;

use futures::{SinkExt as _, StreamExt as _};
use log::{error, info, warn};
use std::fs::DirEntry;
use std::path::Path;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio_util::codec::Decoder as _;

use crate::background_merge::BackgroundMerge;
use crate::config::CONFIG;
use crate::error::Error;
use crate::runtime_config::{TABLE_DATA, TableConfig};
use crate::sql::CommandRunner;
use crate::storage::{TableDef, TableMetadata, TablePartInfo};
use crate::tcp_io_parser::Parser;

pub use crate::error::Result;

pub fn build_logger() {
    env_logger::Builder::from_default_env()
        .filter_level(CONFIG.get_log_level())
        .init();
}

pub fn spawn_background_merges() {
    std::thread::spawn(|| {
        BackgroundMerge::start();
    });
}

pub fn init_conn_semaphore() -> Arc<Semaphore> {
    Arc::new(Semaphore::new(CONFIG.get_max_connections()))
}

/// Tries to initialize TCP listener.
///
/// # Errors
/// * If failes to bind to `CONFIG.get_tcp_socket_addr()`
pub async fn init_listener() -> Result<TcpListener> {
    TcpListener::bind(&CONFIG.get_tcp_socket_addr())
        .await
        .map_err(|error| {
            Error::Internal(format!(
                "Failed to bind to {}: {error}.",
                CONFIG.get_tcp_socket_addr()
            ))
        })
}

pub fn log_startup_info() {
    info!("TCP server listening on {}", CONFIG.get_tcp_socket_addr());
    info!("Database directory: {}", CONFIG.get_db_dir().display());
    info!("Log level: {:?}", CONFIG.get_log_level());
}

/// Accepts each connection in blocking format, with max `max_conn` concurrent connections.
pub async fn accept_conn(max_conn: Arc<Semaphore>, listener: &TcpListener) {
    let Ok(connection_permit) = max_conn.acquire_owned().await else {
        unreachable!()
    };
    match listener.accept().await {
        Ok((mut socket, addr)) => {
            tokio::spawn(async move {
                if handle_connection(&mut socket).await.is_err() {
                    error!("Could not send to {addr}. Closing connection.");
                }
                drop(socket);
                drop(connection_permit);
            });
        }
        Err(error) => {
            error!("Failed to accept connection: {error}");
            drop(connection_permit);
        }
    }
}

async fn handle_connection(socket: &mut TcpStream) -> Result<()> {
    // using tokio_util `Decoder, Encoder` traits to receive and send bytes
    // link: https://docs.rs/tokio-util/latest/tokio_util/codec/index.html
    let mut transport = Parser.framed(socket);

    while let Some(sql_command) = transport.next().await {
        let Ok(value) = sql_command else {
            let error = sql_command.unwrap_err();
            if let Err(send_error) = transport.send(Err(error)).await {
                error!("Failed to send response: {send_error}");
                return Err(Error::SendResponse);
            }
            continue;
        };

        if value == "exit" {
            break;
        }

        let output = tokio::task::spawn_blocking(move || CommandRunner::execute_command(&value))
            .await
            .unwrap_or_else(|error| {
                error!("Thread panicked: {error}");
                Err(Error::Internal(format!("Thread panicked: {error}")))
            });

        if let Err(send_error) = transport.send(output).await {
            error!("Failed to send response: {send_error}");
            return Err(Error::SendResponse);
        }
    }
    info!("Connection closed.");
    Ok(())
}

/// Loads all table parts from filesystem into memory on startup.
///
/// Scans all databases and tables, loads part indexes, and populates `TABLE_DATA`.
/// Cleans up any leftover raw directories from previous runs.
///
/// # Errors
/// * Could not load database dir, read database dir, read table dir
///
/// # Panics
/// * Table definition is removed from `TABLE_DATA` while loading parts.
pub fn load_existing_parts() -> Result<()> {
    let db_dir = CONFIG.get_db_dir();
    info!(
        "Loading parts from database directory: {}",
        db_dir.display()
    );

    if !db_dir.exists() {
        warn!("Database directory does not exist: {}", db_dir.display());
        return Err(Error::Internal(format!(
            "Database directory does not exist: {}",
            db_dir.display()
        )));
    }

    let databases = std::fs::read_dir(db_dir).map_err(|error| {
        Error::Internal(format!(
            "Failed to read database directory ({}): {error}",
            db_dir.display()
        ))
    })?;

    for database_entry in databases {
        let database_entry = database_entry
            .map_err(|error| Error::Internal(format!("Failed to read database entry: {error}")))?;
        process_database_entry(&database_entry)?;
    }

    info!("Finished loading parts");
    Ok(())
}

fn process_database_entry(database_entry: &DirEntry) -> Result<()> {
    let database_path = database_entry.path();
    if !database_path.is_dir() {
        return Ok(());
    }

    let database_name = database_entry.file_name().to_string_lossy().to_string();
    let tables = std::fs::read_dir(&database_path).map_err(|error| {
        Error::Internal(format!(
            "Failed to read tables in database {database_name:?}: {error}"
        ))
    })?;

    for table_entry in tables {
        let table_entry = table_entry
            .map_err(|error| Error::Internal(format!("Failed to read table entry: {error}")))?;
        process_table_entry(&table_entry, &database_name)?;
    }

    Ok(())
}

fn process_table_entry(table_entry: &DirEntry, database_name: &str) -> Result<()> {
    let table_path = table_entry.path();
    if !table_path.is_dir() {
        return Ok(());
    }

    let table = table_entry.file_name().to_string_lossy().to_string();
    let table_def = TableDef {
        database: database_name.to_string(),
        table,
    };

    let table_metadata = TableMetadata::read_from(&table_def).map_err(|error| {
        Error::Internal(format!(
            "Could not read table metadata from table definition ({table_def}): {error}"
        ))
    })?;

    TABLE_DATA.insert(
        table_def.clone(),
        TableConfig {
            metadata: table_metadata,
            infos: Vec::new(),
        },
    );

    let parts = std::fs::read_dir(table_path).map_err(|error| {
        Error::Internal(format!(
            "Failed to read parts from table definition ({table_def}): {error}"
        ))
    })?;

    for part_entry in parts {
        let part_entry = part_entry.map_err(|error| {
            Error::Internal(format!(
                "Failed to read part entry for table definition ({table_def}): {error}"
            ))
        })?;
        process_part_entry(&part_entry, &table_def)?;
    }

    Ok(())
}

fn process_part_entry(part_entry: &DirEntry, table_def: &TableDef) -> Result<()> {
    let part_path = part_entry.path();
    let part_name = part_entry.file_name().to_string_lossy().to_string();

    if !part_path.is_dir()
        || part_name.starts_with('.')
        || should_remove_raw_directory(&part_name, &part_path, table_def)
        || should_skip_old_part(&part_path, &part_name)
    {
        return Ok(());
    }

    match TablePartInfo::read_from(table_def, &part_name) {
        Ok(info) => {
            let Some(mut result) = TABLE_DATA.get_mut(table_def) else {
                return Err(Error::Internal(
                    "Table definition is removed from TABLE_DATA while loading all parts.."
                        .to_string(),
                ));
            };
            result.infos.push(info);
            info!("Loaded part ({part_name}) for table ({table_def})");
            Ok(())
        }
        Err(error) => {
            warn!("Failed to load part {part_name} for table {table_def}: {error:?}");
            Ok(())
        }
    }
}

fn should_remove_raw_directory(part_name: &str, part_path: &Path, table_def: &TableDef) -> bool {
    if part_name == "raw" {
        match std::fs::remove_dir_all(part_path) {
            Ok(()) => {
                info!("Removed raw directory for table {table_def}");
            }
            Err(e) => {
                warn!("Failed to remove raw directory for table {table_def}: {e}");
            }
        }
        return true;
    }
    false
}

fn should_skip_old_part(part_path: &Path, part_name: &str) -> bool {
    if part_path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("old"))
    {
        warn!("Found old part: {part_name}. Consult the logs to make the decision about removal.");
        return true;
    }
    false
}
