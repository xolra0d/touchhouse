use crate::engines::EngineName;
use crate::error::{Error, Result};
use crate::storage::{ColumnDef, TableDef};
use std::time::SystemTime;

use rkyv::{Archive as RkyvArchive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};

pub const TABLE_METADATA_MAGIC_BYTES: &[u8] = b"THMETA".as_slice();
pub const TABLE_METADATA_FILENAME: &str = ".metadata";
pub const STANDARD_GRANULARITY: u32 = 8192;

const VERSION: u16 = 1;

pub mod flags {
    pub const NONE: u32 = 0x0000_0000;
}

#[derive(Debug, PartialEq, Clone, RkyvSerialize, RkyvArchive, RkyvDeserialize)]
pub struct TableSchema {
    pub columns: Vec<ColumnDef>,
    pub order_by: Vec<ColumnDef>,
    pub primary_key: Vec<ColumnDef>,
}

/// Table settings parsed from options received in CREATE command.
#[derive(Debug, PartialEq, Clone, RkyvSerialize, RkyvArchive, RkyvDeserialize)]
pub struct TableSettings {
    pub index_granularity: u32,
    pub engine: EngineName,
}

impl Default for TableSettings {
    fn default() -> Self {
        TableSettings {
            index_granularity: STANDARD_GRANULARITY,
            engine: EngineName::MergeTree,
        }
    }
}

/// Single immutable table metadata, stored as file (`TABLE_METADATA_FILENAME`)
/// Used to get global table configuration
#[derive(Debug, PartialEq, Clone, RkyvSerialize, RkyvArchive, RkyvDeserialize)]
pub struct TableMetadata {
    pub version: u16,
    pub flags: u32,
    pub created_at: u64,
    pub settings: TableSettings,
    pub schema: TableSchema,
}

impl TableMetadata {
    /// Creates new table metadata with current timestamp and default flags.
    ///
    /// Returns: `TableMetadata` or error from `get_unix_time()`
    pub fn try_new(schema: TableSchema, settings: TableSettings) -> Result<Self> {
        Ok(Self {
            version: VERSION,
            flags: flags::NONE,
            created_at: get_unix_time()?,
            settings,
            schema,
        })
    }

    /// Writes table metadata to disk with magic bytes and CRC32 checksum.
    ///
    /// Returns:
    ///   * Ok: on successful write.
    ///   * Error: `CouldNotInsertData` on serialization or I/O failure.
    pub fn write_to(&self, table_def: &TableDef) -> Result<()> {
        let mut bytes = Vec::from(TABLE_METADATA_MAGIC_BYTES);

        let data_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(self).map_err(|error| {
            Error::CouldNotInsertData(format!("Failed to serialize table metadata: {error}"))
        })?;
        let crc = crc32fast::hash(&data_bytes);

        bytes.extend(&data_bytes[..]);
        bytes.extend(crc.to_le_bytes());

        let metadata_path = table_def.get_path().join(TABLE_METADATA_FILENAME);
        let temp_path = metadata_path.with_extension("tmp");

        std::fs::write(&temp_path, &bytes).map_err(|error| {
            Error::CouldNotInsertData(format!("Failed to write table metadata file: {error}"))
        })?;

        std::fs::rename(&temp_path, &metadata_path).map_err(|error| {
            Error::CouldNotInsertData(format!("Failed to rename temp metadata file: {error}"))
        })
    }

    /// Reads table metadata from disk, verifying magic bytes and CRC32 checksum.
    ///
    /// Returns:
    ///   * Ok: `TableMetadata` on successful read and validation.
    ///   * Error: `CouldNotReadData` on I/O failure, invalid magic bytes, or CRC mismatch.
    pub fn read_from(table_def: &TableDef) -> Result<Self> {
        let file_bytes = std::fs::read(table_def.get_path().join(TABLE_METADATA_FILENAME))
            .map_err(|error| {
                Error::CouldNotReadData(format!("Failed to read table metadata: {error}"))
            })?;

        if file_bytes.len() <= TABLE_METADATA_MAGIC_BYTES.len() + 4 {
            return Err(Error::CouldNotReadData(
                "Table metadata file too small".to_string(),
            ));
        }

        let file_magic_bytes = &file_bytes[0..TABLE_METADATA_MAGIC_BYTES.len()];
        if file_magic_bytes != TABLE_METADATA_MAGIC_BYTES {
            return Err(Error::CouldNotReadData(
                "Invalid magic bytes in table metadata".to_string(),
            ));
        }

        let data_bytes = &file_bytes[TABLE_METADATA_MAGIC_BYTES.len()..(file_bytes.len() - 4)];

        let expected_crc = u32::from_le_bytes([
            file_bytes[file_bytes.len() - 4],
            file_bytes[file_bytes.len() - 3],
            file_bytes[file_bytes.len() - 2],
            file_bytes[file_bytes.len() - 1],
        ]);

        let actual_crc = crc32fast::hash(data_bytes);
        if expected_crc != actual_crc {
            return Err(Error::CouldNotReadData(
                "CRC mismatch in table metadata".to_string(),
            ));
        }
        // data is not aligned correctly, because of magic bytes
        let mut aligned_data = rkyv::util::AlignedVec::<16>::with_capacity(data_bytes.len());
        aligned_data.extend_from_slice(data_bytes);
        rkyv::from_bytes::<TableMetadata, rkyv::rancor::Error>(&aligned_data).map_err(|error| {
            Error::CouldNotReadData(format!("Failed to deserialize table metadata: {error}"))
        })
    }
}

/// Returns current Unix timestamp in milliseconds.
///
/// Returns: u64 timestamp or `SystemTimeWentBackword` error
fn get_unix_time() -> Result<u64> {
    let now: SystemTime = SystemTime::now();
    u64::try_from(
        now.duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_| Error::SystemTimeWentBackword)?
            .as_millis(),
    )
    .map_err(|_| Error::SystemTimeWentBackword)
}
