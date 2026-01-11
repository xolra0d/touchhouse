use crate::error::Result;
use crate::sql::CommandRunner;
use crate::sql::output_table::OutputTable;
use crate::storage::{PhysicalColumn, TableDef, TablePart};

impl CommandRunner {
    /// Executes INSERT operation by creating new table part.
    ///
    /// Creates a new part, saves it to raw directory, then atomically moves to normal directory.
    /// Which results in atomic inserts.
    ///
    /// Returns:
    ///   * Ok: `OutputTable` with success status
    ///   * Error: `TableNotFound` or `CouldNotInsertData` on failure
    pub fn insert(table_def: &TableDef, columns: Vec<PhysicalColumn>) -> Result<OutputTable> {
        let mut table_part = TablePart::try_new(table_def, columns, None)?;

        table_part.save_raw(table_def)?;

        table_part.move_to_normal(table_def)?;

        Ok(OutputTable::build_ok())
    }
}
