use serde::Serialize;
use std::time::Duration;

use crate::storage::OutputColumn;

#[derive(Debug, Serialize)]
pub struct OutputTable {
    columns: Vec<OutputColumn>,
    execution_time: Duration,
}

impl OutputTable {
    /// Creates new `OutputTable` with provided columns.
    pub fn new(columns: Vec<OutputColumn>, execution_time: Duration) -> Self {
        Self {
            columns,
            execution_time,
        }
    }
}
