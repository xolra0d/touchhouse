use memmap2::{Advice, Mmap};
use rkyv::{Archive as RkyvArchive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::Serialize;
use std::fmt;
use std::fs::File;
use std::path::Path;

use crate::error::{Error, Result};
use crate::sql::{Projection, ProjectionValue};
use crate::storage::{CompressionType, Value, ValueType, table_part::MAGIC_BYTES_COLUMN};

#[derive(Debug, Clone, PartialEq, Serialize, RkyvSerialize, RkyvArchive, RkyvDeserialize)]
pub struct Constraints {
    pub nullable: bool,
    pub default: Option<Value>,
    pub compression_type: CompressionType,
}

impl Default for Constraints {
    fn default() -> Self {
        Self {
            nullable: true,
            default: None,
            compression_type: CompressionType::default(),
        }
    }
}

#[derive(Debug, PartialEq, Clone, RkyvSerialize, RkyvArchive, RkyvDeserialize, Serialize)]
pub struct ColumnDef {
    pub name: String,
    pub field_type: ValueType,
    pub constraints: Constraints,
}

impl fmt::Display for ColumnDef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PhysicalColumn {
    pub column_def: ColumnDef,
    pub data: Vec<Value>,
}

impl From<ColumnDef> for PhysicalColumn {
    fn from(column_def: ColumnDef) -> Self {
        Self {
            column_def,
            data: Vec::new(),
        }
    }
}

impl PhysicalColumn {
    pub fn into_output_column(self) -> OutputColumn {
        OutputColumn {
            proj: Projection {
                alias: None,
                source: ProjectionValue::ColumnDef(self.column_def),
            },
            data: self.data,
        }
    }
}

impl PhysicalColumn {
    pub fn open_as_mmap(file_path: &Path) -> Result<Mmap> {
        let file = File::open(file_path).map_err(|error| {
            Error::CouldNotReadData(format!(
                "Could not open column file ({}): {error}",
                file_path.display()
            ))
        })?;

        let mmap = unsafe {
            Mmap::map(&file).map_err(|error| {
                Error::CouldNotReadData(format!(
                    "Could not open mmap for column file ({}): {error}",
                    file_path.display()
                ))
            })?
        };

        mmap.advise(Advice::Sequential).map_err(|error| {
            Error::CouldNotReadData(format!(
                "Could not advice mmap for column file ({}): {error}",
                file_path.display()
            ))
        })?;

        Ok(mmap)
    }

    pub fn validate_mmap(mmap: &Mmap, col_name: &str) -> Result<()> {
        if mmap.len() <= MAGIC_BYTES_COLUMN.len() + 4 {
            return Err(Error::CouldNotReadData(format!(
                "Column file ({col_name}) too small"
            )));
        }

        let file_magic_bytes = &mmap[0..MAGIC_BYTES_COLUMN.len()];
        if file_magic_bytes != MAGIC_BYTES_COLUMN {
            return Err(Error::CouldNotReadData(format!(
                "Invalid magic bytes in column file ({col_name})"
            )));
        }

        let mut result = Crc32Writer(crc32fast::Hasher::new());
        std::io::copy(
            &mut std::io::Cursor::new(&mmap[MAGIC_BYTES_COLUMN.len()..(mmap.len() - 4)]),
            &mut result,
        )
        .map_err(|error| {
            Error::CouldNotReadData(format!(
                "Could not read mmap of column ({col_name}): {error}"
            ))
        })?;
        let actual_crc = result.finalize();
        let expected_crc = u32::from_le_bytes([
            mmap[mmap.len() - 4],
            mmap[mmap.len() - 3],
            mmap[mmap.len() - 2],
            mmap[mmap.len() - 1],
        ]);

        if expected_crc != actual_crc {
            return Err(Error::CouldNotReadData(format!(
                "CRC mismatch in column file ({col_name})"
            )));
        }

        Ok(())
    }
}

/// Tiny wrapper for implementing `std::io::Write` for `crc32fast::Hasher`.
///
/// Gives 20% speedup.
struct Crc32Writer(crc32fast::Hasher);

impl Crc32Writer {
    fn finalize(self) -> u32 {
        self.0.finalize()
    }
}

impl std::io::Write for Crc32Writer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OutputColumn {
    pub proj: Projection,
    pub data: Vec<Value>,
}

impl OutputColumn {
    /// Builds a simple OK response column in vec.
    pub fn build_ok_vec() -> Vec<Self> {
        vec![Self {
            proj: Projection {
                alias: None,
                source: ProjectionValue::Value(Value::String("OK".to_string())),
            },
            data: vec![Value::String("OK".to_string())],
        }]
    }
}

impl From<PhysicalColumn> for OutputColumn {
    fn from(value: PhysicalColumn) -> Self {
        let PhysicalColumn { column_def, data } = value;

        OutputColumn {
            proj: Projection {
                alias: None,
                source: ProjectionValue::ColumnDef(column_def),
            },
            data,
        }
    }
}

impl TryFrom<OutputColumn> for PhysicalColumn {
    type Error = Error;

    fn try_from(value: OutputColumn) -> Result<Self> {
        let ProjectionValue::ColumnDef(column_def) = value.proj.source else {
            return Err(Error::InvalidSource(format!(
                "expected to be column definition, got ({:?}) instead during output to physical column conversion.",
                value.proj.source
            )));
        };
        Ok(PhysicalColumn {
            column_def,
            data: value.data,
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
