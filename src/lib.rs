mod background_merge;
mod config;
mod engines;
mod error;
mod runtime_config;
mod sql;
mod storage;
mod tcp_io_parser;

use futures::{SinkExt as _, StreamExt as _};
use log::{error, info};
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio_util::codec::Decoder as _;

use crate::background_merge::BackgroundMerge;
use crate::config::CONFIG;
use crate::error::Error;
use crate::sql::CommandRunner;
use crate::storage::load_all_parts_on_startup;
use crate::tcp_io_parser::Parser;

pub fn reject_32bit_systems() -> Result<(), String> {
    if size_of::<usize>() == size_of::<u32>() {
        Err(format!(
            "32bit systems are not supported, as they are not optimal for OLAP workload which could require gigabytes of ram and where the number of rows could exceed {}",
            u32::MAX
        ))
    } else {
        Ok(())
    }
}

pub fn build_logger() {
    env_logger::Builder::from_default_env()
        .filter_level(CONFIG.get_log_level())
        .init();
}

pub fn load_parts() -> Result<(), String> {
    load_all_parts_on_startup(CONFIG.get_db_dir())
        .map_err(|error| format!("Failed to load parts on startup: {error:?}"))
}

pub fn spawn_background_merges() {
    std::thread::spawn(|| {
        BackgroundMerge::start();
    });
}

pub fn init_conn_semaphore() -> Arc<Semaphore> {
    Arc::new(Semaphore::new(CONFIG.get_max_connections()))
}

pub async fn init_listener() -> Result<TcpListener, String> {
    TcpListener::bind(&CONFIG.get_tcp_socket_addr())
        .await
        .map_err(|error| {
            format!(
                "Failed to bind to {}: {error}.",
                CONFIG.get_tcp_socket_addr()
            )
        })
}

pub fn log_startup_info() {
    info!("TCP server listening on {}", CONFIG.get_tcp_socket_addr());
    info!("Database directory: {}", CONFIG.get_db_dir().display());
    info!("Log level: {:?}", CONFIG.get_log_level());
}

pub async fn accept_conn(max_conn: Arc<Semaphore>, listener: &TcpListener) -> Result<(), String> {
    let Ok(connection_permit) = max_conn.acquire_owned().await else {
        // currently unimplemented
        return Err("Semaphore closed unexpectedly.".to_string());
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
        Err(error) => error!("Failed to accept connection: {error}"),
    }
    Ok(())
}

async fn handle_connection(socket: &mut TcpStream) -> Result<(), Error> {
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

        let output = tokio::task::spawn_blocking(move || {
            let start = std::time::Instant::now();
            let result = CommandRunner::execute_command(&value);
            let elapsed = start.elapsed();

            result.map(|output_table| output_table.with_execution_time(elapsed))
        })
        .await
        .unwrap_or_else(|error| {
            error!("SQL task panicked: {error}");
            Err(Error::Internal(
                "Internal error during query execution".to_string(),
            ))
        });

        if let Err(send_error) = transport.send(output).await {
            error!("Failed to send response: {send_error}");
            return Err(Error::SendResponse);
        }
    }
    info!("Connection closed.");
    Ok(())
}
