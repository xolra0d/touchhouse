use crate::error::Result;
use crate::sql::CommandRunner;
use crate::storage::{NativeStorage, OutputColumn, PhysicalColumn, StorageWrite as _, TableDef};

impl CommandRunner {
    /// Executes INSERT operation by creating new table part.
    ///
    /// Creates a new part, saves it to raw directory, then atomically moves to normal directory.
    /// Which results in atomic inserts.
    ///
    /// Returns:
    ///   * Ok: `OutputTable` with success status
    ///   * Error: `TableNotFound` or `CouldNotInsertData` on failure
    pub fn insert(table_def: &TableDef, columns: Vec<PhysicalColumn>) -> Result<Vec<OutputColumn>> {
        let mut storage = NativeStorage::try_from_mut(table_def)?;
        storage.insert(columns)?;

        Ok(OutputColumn::build_ok_vec())
    }
}
