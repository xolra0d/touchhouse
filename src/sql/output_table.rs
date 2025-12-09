use crate::storage::{Column, ColumnDef, Constraints, Value, ValueType};
use serde::Serialize;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OutputColumn {
    pub alias: Option<String>,
    pub column_def: ColumnDef,
    pub data: Vec<Value>,
    pub is_virtual: bool,
}

impl OutputColumn {
    pub fn to_column(self) -> Column {
        Column {
            column_def: self.column_def,
            data: self.data,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct OutputTable {
    pub columns: Vec<OutputColumn>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_time: Option<Duration>,
}

impl OutputTable {
    /// Creates new `OutputTable` with provided columns.
    pub fn new(columns: Vec<OutputColumn>) -> Self {
        Self {
            columns,
            execution_time: None,
        }
    }

    /// Sets the execution time for this output table.
    pub fn with_execution_time(mut self, duration: Duration) -> Self {
        self.execution_time = Some(duration);
        self
    }

    /// Builds a simple OK response table.
    pub fn build_ok() -> Self {
        Self {
            columns: vec![OutputColumn {
                alias: None,
                column_def: ColumnDef {
                    name: "OK".to_string(),
                    field_type: ValueType::String,
                    constraints: Constraints::default(),
                },
                data: vec![Value::String("OK".to_string())],
                is_virtual: true,
            }],
            execution_time: None,
        }
    }
}
