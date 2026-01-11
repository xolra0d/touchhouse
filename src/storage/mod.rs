mod compression;
pub mod table_metadata;
mod table_part;
pub mod value;

use crate::CONFIG;
use crate::error::{Error, Result};
pub use crate::storage::compression::CompressionType;
use crate::storage::table_metadata::TABLE_METADATA_FILENAME;
pub use crate::storage::table_metadata::{TableMetadata, TableSchema, TableSettings};
use crate::storage::table_part::MAGIC_BYTES_COLUMN;
pub use crate::storage::table_part::{Mark, MarkInfo, TablePart, TablePartInfo};
pub use crate::storage::value::{Value, ValueType};

use crate::sql::{OutputColumn, Projection, ProjectionValue};
use memmap2::{Advice, Mmap};
use rkyv::{Archive as RkyvArchive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::Serialize;
use sqlparser::ast::{ObjectName, ObjectNamePart};
use std::fmt;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TableDef {
    pub table: String,
    pub database: String,
}

impl fmt::Display for TableDef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.database, self.table)
    }
}

impl TableDef {
    /// Returns filesystem path for this table.
    pub fn get_path(&self) -> PathBuf {
        CONFIG.get_db_dir().join(&self.database).join(&self.table)
    }

    /// Checks if table exists by verifying database directory and `TABLE_METADATA_FILENAME` file.
    ///
    /// Returns: Ok or DatabaseNotFound/TableNotFound error
    pub fn exists_or_err(&self) -> Result<()> {
        let mut path = CONFIG.get_db_dir().join(&self.database);
        if !path.exists() {
            return Err(Error::DatabaseNotFound);
        }

        path.push(&self.table);
        path.push(TABLE_METADATA_FILENAME);

        if !path.exists() {
            return Err(Error::TableNotFound);
        }

        Ok(())
    }
}

impl TryFrom<&ObjectName> for TableDef {
    type Error = Error;
    fn try_from(object_name: &ObjectName) -> Result<Self> {
        let names = &object_name.0;
        if names.len() != 2 {
            return Err(Error::UnsupportedCommand(
                "You should provide table name in form `database_name.table_name`".to_string(),
            ));
        }

        let ObjectNamePart::Identifier(ref database) = names[0] else {
            return Err(Error::UnsupportedCommand(
                "Currently unimplemented.".to_string(),
            ));
        };
        let database = database.value.clone();

        let ObjectNamePart::Identifier(ref table) = names[1] else {
            return Err(Error::UnsupportedCommand(
                "Currently unimplemented.".to_string(),
            ));
        };
        let table = table.value.clone();

        let table_def = Self { table, database };

        Ok(table_def)
    }
}

/// Returns current Unix timestamp in milliseconds.
///
/// Returns: u64 timestamp or `SystemTimeWentBackword` error
pub fn get_unix_time() -> Result<u64> {
    let now: SystemTime = SystemTime::now();
    u64::try_from(
        now.duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_| Error::SystemTimeWentBackword)?
            .as_millis(),
    )
    .map_err(|_| Error::SystemTimeWentBackword)
}
