use log::error;

use crate::config::CONFIG;
use crate::error::{Error, Result};
use crate::runtime_config::TABLE_DATA;
use crate::sql::CommandRunner;
use crate::storage::{NativeStorage, OutputColumn, StorageWrite, TableDef};

impl CommandRunner {
    /// Drops a table.
    ///
    /// Removes table entry in memory, deletes table directory.
    ///
    /// Returns:
    ///   * Ok: `OutputTable` with success status
    ///   * Error: `TableNotFound` or `Internal` on failure
    pub fn drop_table(table_def: &TableDef, if_exists: bool) -> Result<Vec<OutputColumn>> {
        match NativeStorage::try_from_mut(table_def) {
            Ok(storage) => {
                storage.drop(if_exists)?;
                Ok(OutputColumn::build_ok_vec())
            }
            Err(Error::TableNotFound) if if_exists => Ok(OutputColumn::build_ok_vec()),
            Err(e) => Err(e),
        }
    }

    /// Drops a database.
    ///
    /// Removes table entries in memory, deletes database directory.
    ///
    /// Returns:
    ///   * Ok: `OutputTable` with success status
    ///   * Error: `DatabaseNotFound` or `Internal` on failure
    pub fn drop_database(name: &str, if_exists: bool) -> Result<Vec<OutputColumn>> {
        TABLE_DATA.retain(|x, _| x.database != name);

        let remove_result = std::fs::remove_dir_all(CONFIG.get_db_dir().join(name));
        match (remove_result, if_exists) {
            (Ok(()), _) => Ok(OutputColumn::build_ok_vec()),
            (Err(error), true) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(OutputColumn::build_ok_vec())
            }
            (Err(error), false) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(Error::DatabaseNotFound)
            }
            (Err(error), _) => {
                let error_msg = format!(
                    "Could not remove database entry from disk: {}. Stop database, remove {:?} folder, and restart the database.",
                    error,
                    std::path::absolute(CONFIG.get_db_dir().join(name))
                        .unwrap_or(CONFIG.get_db_dir().join(name))
                        .display(),
                );

                error!("{error_msg}");
                Err(Error::Internal(error_msg))
            }
        }
    }
}
