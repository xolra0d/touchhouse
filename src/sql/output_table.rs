use crate::error::{Error, Result};
use crate::sql::{Projection, ProjectionValue};
use crate::storage::{ColumnDef, Constraints, PhysicalColumn, Value, ValueType};
use serde::Serialize;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OutputColumn {
    pub proj: Projection,
    pub data: Vec<Value>,
}

impl OutputColumn {
    pub fn into_column(self) -> Result<PhysicalColumn> {
        let ProjectionValue::ColumnDef(column_def) = self.proj.source else {
            return Err(Error::InvalidSource(format!(
                "expected to be column definition, got ({:?}) instead during output to physical column conversion.",
                self.proj.source
            )));
        };
        Ok(PhysicalColumn {
            column_def,
            data: self.data,
        })
    }
}

impl From<Projection> for OutputColumn {
    fn from(proj: Projection) -> Self {
        Self {
            proj,
            data: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct OutputTable {
    pub columns: Vec<OutputColumn>,
    pub execution_time: Duration,
}

impl OutputTable {
    /// Creates new `OutputTable` with provided columns.
    pub fn new(columns: Vec<OutputColumn>) -> Self {
        Self {
            columns,
            execution_time: Duration::from_millis(0),
        }
    }

    /// Builds a simple OK response table.
    pub fn build_ok() -> Self {
        let ok_col_def = ColumnDef {
            name: "OK".to_string(),
            field_type: ValueType::String,
            constraints: Constraints::default(),
        };

        Self {
            columns: vec![OutputColumn {
                proj: Projection {
                    alias: None,
                    source: ProjectionValue::ColumnDef(ok_col_def),
                },
                data: vec![Value::String("OK".to_string())],
            }],
            execution_time: Duration::from_millis(0),
        }
    }
}
