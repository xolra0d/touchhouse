use crate::config::CONFIG;
use crate::error::{Error, Result};
use crate::runtime_config::{TABLE_DATA, TableConfig};
use crate::sql::output_table::OutputTable;
use crate::sql::{CommandRunner, validate_name};
use crate::storage::{ColumnDef, TableDef};
use crate::storage::{TableMetadata, TableSchema, TableSettings};
use dashmap::Entry;
use log::error;

impl CommandRunner {
    /// Creates a database directory.
    ///
    /// Returns:
    ///   * Ok: `OutputTable` with success status
    ///   * Error: `InvalidDatabaseName` if directory creation fails
    pub fn create_database(name: String) -> Result<OutputTable> {
        if !validate_name(&name) {
            return Err(Error::InvalidDatabaseName);
        }
        std::fs::create_dir(CONFIG.get_db_dir().join(name)).map_err(|error| {
            match error.kind() {
                std::io::ErrorKind::AlreadyExists => Error::DatabaseAlreadyExists,
                std::io::ErrorKind::PermissionDenied => Error::PermissionDenied,
                _ => Error::InvalidDatabaseName,
            }
        })?;

        Ok(OutputTable::build_ok())
    }

    /// Creates a table.
    ///
    /// Reserves table entry in memory, creates directory, and writes metadata.
    ///
    /// Returns:
    ///   * Ok: `OutputTable` with success status
    ///   * Error: `TableEntryAlreadyExists` or `CouldNotInsertData` on failure
    pub fn create_table(
        table_def: &TableDef,
        columns: Vec<ColumnDef>,
        settings: TableSettings,
        order_by: Vec<ColumnDef>,
        primary_key: Vec<ColumnDef>,
    ) -> Result<OutputTable> {
        let table_schema = TableSchema {
            columns,
            order_by,
            primary_key,
        };
        let table_metadata = TableMetadata::try_new(table_schema, settings)?;

        let table_path = table_def.get_path();
        // will lock for mutual access
        let Entry::Vacant(entry) = TABLE_DATA.entry(table_def.clone()) else {
            return Err(Error::TableAlreadyExists);
        };

        std::fs::create_dir(&table_path).map_err(|error| {
            Error::CouldNotCreateTable(format!("Failed to create table dir: {error}"))
        })?;

        if let Err(error) = table_metadata.write_to(table_def) {
            if let Err(cleanup_err) = std::fs::remove_dir_all(table_path) {
                error!("Failed to cleanup directory after metadata write failure: {cleanup_err}",);
            }
            return Err(error);
        }
        let table_config = TableConfig {
            metadata: table_metadata,
            infos: Vec::new(),
        };

        entry.insert(table_config);

        Ok(OutputTable::build_ok())
    }
}
