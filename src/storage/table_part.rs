use crate::engines::EngineConfig;
use crate::error::{Error, Result};
use crate::runtime_config::{TABLE_DATA, TableConfig};
use crate::storage::compression::{compress_bytes, decompress_bytes};
use crate::storage::table_metadata::TableMetadata;
use crate::storage::{Column, ColumnDef, CompressionType, TableDef, Value};

use crate::sql::{OutputColumn, Projection, ProjectionValue};
use log::{info, warn};
use rkyv::{Archive as RkyvArchive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub const MAGIC_BYTES_COLUMN: &[u8] = b"THDATA".as_slice();
pub const MAGIC_BYTES_INFO: &[u8] = b"THINDX".as_slice();
pub const PART_INFO_FILENAME: &str = "part.inf";

/// Represents a start byte position and end byte position of the
/// compressed granule.
#[derive(Debug, Clone, RkyvSerialize, RkyvArchive, RkyvDeserialize, PartialEq)]
pub struct MarkInfo {
    pub start: u64,
    pub end: u64,
}

/// Represents a first row of each granule and start-end bytes info for each granule.
#[derive(Debug, Clone, RkyvSerialize, RkyvArchive, RkyvDeserialize, PartialEq)]
pub struct Mark {
    pub index: Vec<Value>,
    pub info: Vec<MarkInfo>,
}

/// Immutable table part information.
#[derive(Debug, Clone, RkyvSerialize, RkyvArchive, RkyvDeserialize)]
pub struct TablePartInfo {
    pub name: String,
    pub row_count: u64, // max rows per tablepart = 18_446_744_073_709_551_615
    pub marks: Vec<Mark>,
    pub column_defs: Vec<ColumnDef>,
}

impl TablePartInfo {
    /// Returns the filesystem path for a column's data file within this part.
    pub fn get_column_path(&self, table_def: &TableDef, column_def: &ColumnDef) -> PathBuf {
        table_def
            .get_path()
            .join(&self.name)
            .join(format!("{}.bin", column_def.name))
    }

    /// Reads and decompresses a granule from disk.
    ///
    /// Args:
    ///   * `file`: Column file.
    ///   * `mark_info`: `MarkInfo` of granule
    ///   * `compression_type`: Compression type for the granule
    ///
    /// Returns: Vec with data from specified granule or `CouldNotReadData` on failure
    pub fn get_granule_bytes_decompressed(
        bytes: &[u8],
        mark_info: &MarkInfo,
        compression_type: &CompressionType,
    ) -> Result<Vec<u8>> {
        if mark_info.end < mark_info.start {
            return Err(Error::CouldNotReadData(format!(
                "Invalid mark bounds: end ({}) < start ({})",
                mark_info.end, mark_info.start
            )));
        }

        if mark_info.end > bytes.len() as u64 {
            return Err(Error::CouldNotReadData(format!(
                "Mark end ({}) exceeds file size ({})",
                mark_info.end,
                bytes.len()
            )));
        }

        let compressed = &bytes[(mark_info.start as usize)..(mark_info.end as usize)];

        decompress_bytes(compressed, compression_type)
    }

    /// Writes part info to disk with magic bytes and CRC32 checksum.
    ///
    /// Args:
    ///   * `table_def`: Table definition for path resolution.
    ///   * `raw`: If true, writes to raw directory; otherwise to normal directory.
    ///
    /// Returns:
    ///   * Ok: on successful write.
    ///   * Error: `CouldNotInsertData` on serialization or I/O failure.
    pub fn write_to(&self, table_def: &TableDef, raw: bool) -> Result<()> {
        let mut bytes = Vec::from(MAGIC_BYTES_INFO);

        let data_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(self).map_err(|error| {
            Error::CouldNotInsertData(format!("Failed to serialize part info: {error}"))
        })?;
        let crc = crc32fast::hash(&data_bytes);

        bytes.extend(&data_bytes[..]);
        bytes.extend(crc.to_le_bytes());

        let mut path = table_def.get_path();
        if raw {
            path.push("raw");
        }
        path.push(&self.name);
        path.push(PART_INFO_FILENAME);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                Error::CouldNotInsertData(format!("Failed to create directory: {error}"))
            })?;
        }

        std::fs::write(path, bytes)
            .map_err(|error| Error::CouldNotInsertData(format!("Failed to write file: {error}")))
    }

    /// Reads part info from disk, verifying magic bytes and CRC32 checksum.
    ///
    /// Returns:
    ///   * Ok: `TablePartInfo` on successful read and validation.
    ///   * Error: `CouldNotReadData` on I/O failure, invalid magic bytes, or CRC mismatch.
    pub fn read_from(table_def: &TableDef, part_name: &str) -> Result<Self> {
        let file_bytes = std::fs::read(
            table_def
                .get_path()
                .join(part_name)
                .join(PART_INFO_FILENAME),
        )
        .map_err(|error| {
            Error::CouldNotReadData(format!("Failed to read part info file: {error}"))
        })?;

        if file_bytes.len() <= MAGIC_BYTES_INFO.len() + 4 {
            return Err(Error::CouldNotReadData(
                "Part info file is too small".to_string(),
            ));
        }

        let file_magic_bytes = &file_bytes[0..MAGIC_BYTES_INFO.len()];
        if file_magic_bytes != MAGIC_BYTES_INFO {
            return Err(Error::CouldNotReadData(
                "Invalid magic bytes in part info file".to_string(),
            ));
        }

        let data_bytes = &file_bytes[MAGIC_BYTES_INFO.len()..(file_bytes.len() - 4)];

        let expected_crc = u32::from_le_bytes([
            file_bytes[file_bytes.len() - 4],
            file_bytes[file_bytes.len() - 3],
            file_bytes[file_bytes.len() - 2],
            file_bytes[file_bytes.len() - 1],
        ]);

        let actual_crc = crc32fast::hash(data_bytes);
        if expected_crc != actual_crc {
            return Err(Error::CouldNotReadData(
                "CRC mismatch in part info file".to_string(),
            ));
        }
        // data is not aligned correctly, because of magic bytes
        let mut aligned_data = rkyv::util::AlignedVec::<16>::with_capacity(data_bytes.len());
        aligned_data.extend_from_slice(data_bytes);
        rkyv::from_bytes::<TablePartInfo, rkyv::rancor::Error>(&aligned_data).map_err(|error| {
            Error::CouldNotReadData(format!("Failed to deserialize part info: {error}"))
        })
    }
}

#[derive(Debug, Clone)]
pub struct TablePart {
    pub info: TablePartInfo,
    pub data: Vec<Column>,
}

impl TablePart {
    /// Creates a new table part with generated UUID name and indexes.
    ///
    /// Orders columns according to engine requirements and generates primary indexes
    /// for ORDER BY columns.
    ///
    /// Returns: Self or engine error
    pub fn try_new(
        table_def: &TableDef,
        columns: Vec<OutputColumn>,
        name: Option<String>,
    ) -> Result<Self> {
        if columns.is_empty() {
            return Err(Error::InvalidSource("No columns provided".to_string()));
        }
        if columns[0].data.is_empty() {
            return Err(Error::InvalidSource("No data provided".to_string()));
        }
        let name = name.unwrap_or(Uuid::now_v7().to_string());

        let Some(table_config) = TABLE_DATA.get(table_def) else {
            return Err(Error::TableNotFound);
        };

        let engine = table_config
            .metadata
            .settings
            .engine
            .get_engine(EngineConfig::default());
        let data = engine.order_columns(
            columns,
            &table_config
                .metadata
                .schema
                .order_by
                .iter()
                .map(|col_def| Projection {
                    alias: None,
                    source: ProjectionValue::ColumnDef(col_def.clone()),
                })
                .collect::<Vec<_>>(),
            &table_config.metadata.schema.primary_key,
        )?;

        let data = data
            .into_iter()
            .map(|out_col| out_col.to_column())
            .collect::<Vec<_>>();

        let marks = generate_indexes(
            &data,
            &table_config.metadata.schema.primary_key,
            table_config.metadata.settings.index_granularity,
        );
        let row_count = data[0].data.len() as u64;

        let info = TablePartInfo {
            name,
            marks,
            row_count,
            column_defs: data.iter().map(|col| col.column_def.clone()).collect(),
        };

        Ok(Self { info, data })
    }

    /// Saves part data and indexes to raw directory.
    ///
    /// Writes each column to separate .bin file and info to `PART_INFO_FILENAME`.
    /// All files include magic bytes and CRC32 checksums.
    ///
    /// Returns: Ok or `CouldNotInsertData` on I/O failure
    pub fn save_raw(&mut self, table_def: &TableDef) -> Result<()> {
        let raw_dir = self.get_raw_dir(table_def);
        std::fs::create_dir_all(&raw_dir)
            .map_err(|_| Error::CouldNotInsertData("Failed to create raw directory".to_string()))?;

        let granularity = {
            let Some(config) = TABLE_DATA.get(table_def) else {
                return Err(Error::TableNotFound);
            };
            Ok(config.metadata.settings.index_granularity)
        }?;

        for col_idx in 0..self.data.len() {
            let column_file = raw_dir.join(format!("{}.bin", self.data[col_idx].column_def.name));
            self.write_column_with_marks(col_idx, &column_file, granularity)?;
        }

        self.info.write_to(table_def, true)?;

        Ok(())
    }

    /// Writes a single column file with granule-by-granule serialization and populates `MarkInfo`.
    fn write_column_with_marks(
        &mut self,
        col_idx: usize,
        path: &PathBuf,
        index_granularity: u32,
    ) -> Result<()> {
        let mut file_bytes = Vec::from(MAGIC_BYTES_COLUMN);
        let granule_size = index_granularity as usize;
        let total_rows = self.data[col_idx].data.len();

        for (granule_idx, chunk_start) in (0..total_rows).step_by(granule_size).enumerate() {
            let chunk_end = (chunk_start + granule_size).min(total_rows);
            let granule_data = self.data[col_idx].data[chunk_start..chunk_end]
                .as_ref()
                .to_vec();

            let start_pos = file_bytes.len() as u64;

            let granule_bytes =
                rkyv::to_bytes(&granule_data).map_err(|error: rkyv::rancor::Error| {
                    Error::CouldNotInsertData(format!("Could not serialize data: {error}"))
                })?;
            let granule_bytes = compress_bytes(
                &granule_bytes,
                &granule_data[0].get_type().get_optimal_compression(),
            )?;
            file_bytes.extend(&granule_bytes);

            let end_pos = file_bytes.len() as u64;

            if granule_idx >= self.info.marks.len() {
                return Err(Error::CouldNotInsertData(
                    "Invalid number of granules. Most probably different column sizes".to_string(),
                ));
            }

            self.info.marks[granule_idx].info.push(MarkInfo {
                start: start_pos,
                end: end_pos,
            });
        }

        let data_bytes = &file_bytes[MAGIC_BYTES_COLUMN.len()..];
        let crc = crc32fast::hash(data_bytes);
        file_bytes.extend(crc.to_le_bytes());

        std::fs::write(path, file_bytes).map_err(|error| {
            Error::CouldNotInsertData(format!("Failed to write column file: {error}"))
        })
    }

    /// Atomically moves part from raw to normal directory and updates in-memory index.
    ///
    /// Updates memory first (under exclusive lock), then renames directory.
    /// Rolls back memory change on filesystem failure.
    ///
    /// Returns: Ok or `CouldNotInsertData` with rollback on failure
    pub fn move_to_normal(self, table_def: &TableDef) -> Result<()> {
        let raw_dir = self.get_raw_dir(table_def);
        let normal_dir = table_def.get_path().join(&self.info.name);

        let Some(mut result) = TABLE_DATA.get_mut(table_def) else {
            return Err(Error::TableNotFound);
        };
        let part_name = self.info.name.clone();
        result.infos.push(self.info);

        if let Err(e) = std::fs::rename(&raw_dir, &normal_dir) {
            result.infos.pop_if(|info| info.name == part_name);
            return Err(Error::CouldNotInsertData(format!(
                "Failed to move part directory: {e}"
            )));
        }

        Ok(())
    }

    fn get_raw_dir(&self, table_def: &TableDef) -> PathBuf {
        table_def.get_path().join("raw").join(&self.info.name)
    }
}

fn generate_indexes(
    columns: &[Column],
    order_by: &[ColumnDef],
    index_granularity: u32,
) -> Vec<Mark> {
    let columns_in_order_by: Vec<&Column> = columns
        .iter()
        .filter(|x| order_by.contains(&x.column_def))
        .collect();

    let total_rows = columns_in_order_by.first().map_or(0, |x| x.data.len());
    let num_granules = total_rows.div_ceil(index_granularity as usize);
    let mut marks = Vec::with_capacity(num_granules);

    for row_idx in (0..total_rows).step_by(index_granularity as usize) {
        let row_values: Vec<Value> = columns_in_order_by
            .iter()
            .map(|x| x.data[row_idx].clone())
            .collect();
        marks.push(Mark {
            index: row_values,
            info: Vec::new(), // Will be filled during `save_raw`
        });
    }
    marks
}

/// Loads all table parts from filesystem into memory on startup.
///
/// Scans all databases and tables, loads part indexes, and populates `TABLE_DATA`.
/// Cleans up any leftover raw directories from crashes.
///
/// Returns: Ok or `CouldNotInsertData` on critical failure
pub fn load_all_parts_on_startup(db_dir: &Path) -> Result<()> {
    info!(
        "Loading parts from database directory: {}",
        db_dir.display()
    );

    if !db_dir.exists() {
        warn!("Database directory does not exist: {}", db_dir.display());
        return Ok(());
    }

    let databases = std::fs::read_dir(db_dir).map_err(|error| {
        Error::CouldNotInsertData(format!("Failed to read database directory: {error}"))
    })?;

    for database_entry in databases {
        let database_entry = database_entry.map_err(|error| {
            Error::CouldNotInsertData(format!("Failed to read database entry: {error}"))
        })?;

        let database_path = database_entry.path();
        if !database_path.is_dir() {
            continue;
        }

        let database_name = database_entry.file_name().to_string_lossy().to_string();

        let tables = std::fs::read_dir(&database_path).map_err(|error| {
            Error::CouldNotInsertData(format!(
                "Failed to read tables in database {database_name}: {error}"
            ))
        })?;

        for table_entry in tables {
            let table_entry = table_entry.map_err(|error| {
                Error::CouldNotInsertData(format!("Failed to read table entry: {error}"))
            })?;

            let table_path = table_entry.path();
            if !table_path.is_dir() {
                continue;
            }

            let table_name = table_entry.file_name().to_string_lossy().to_string();
            let table_def = TableDef {
                database: database_name.clone(),
                table: table_name.clone(),
            };

            let table_metadata = TableMetadata::read_from(&table_def)?;

            TABLE_DATA.insert(
                table_def.clone(),
                TableConfig {
                    metadata: table_metadata,
                    infos: Vec::new(),
                },
            );

            let parts = std::fs::read_dir(&table_path).map_err(|error| {
                Error::CouldNotInsertData(format!(
                    "Failed to read parts in table {table_def}: {error}"
                ))
            })?;

            for part_entry in parts {
                let part_entry = part_entry.map_err(|error| {
                    Error::CouldNotInsertData(format!("Failed to read part entry: {error}"))
                })?;

                let part_path = part_entry.path();
                let part_name = part_entry.file_name().to_string_lossy().to_string();

                if !part_path.is_dir() || part_name.starts_with('.') {
                    continue;
                }

                if part_name == "raw" {
                    match std::fs::remove_dir_all(&part_path) {
                        Ok(()) => {
                            info!("Removed raw directory for table {table_def}");
                        }
                        Err(e) => {
                            warn!("Failed to remove raw directory for table {table_def}: {e}");
                        }
                    }
                    continue;
                }

                if Path::new(&part_path)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("old"))
                {
                    warn!(
                        "Found old part: {part_name}. Consult the logs to make the decision about removal."
                    );
                    continue;
                }

                match TablePartInfo::read_from(&table_def, &part_name) {
                    Ok(info) => {
                        let Some(mut result) = TABLE_DATA.get_mut(&table_def) else {
                            continue;
                        };
                        result.infos.push(info);
                        info!("Loaded part {part_name} for table {table_def}");
                    }
                    Err(e) => {
                        warn!("Failed to load part {part_name} for table {table_def}: {e:?}");
                    }
                }
            }
        }
    }

    info!("Finished loading parts");
    Ok(())
}
