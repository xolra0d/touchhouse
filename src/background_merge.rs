use crate::error::{Error, Result};
use crate::runtime_config::{DATABASE_LOAD, TABLE_DATA};
use crate::storage::{Column, TableDef, TablePart, TablePartInfo, Value};

use crate::config::CONFIG;
use log::{error, info, warn};
use uuid::Uuid;

/// Background merge service that combines table parts to optimize storage and queries.
pub struct BackgroundMerge;

impl BackgroundMerge {
    /// Starts the background merge loop.
    ///
    /// Continuously monitors tables for parts that can be merged. When database load
    /// is below threshold and two parts exist, merges them into a single part.
    /// Runs indefinitely until the process is terminated.
    pub fn start() {
        info!("Background merges started");
        loop {
            if DATABASE_LOAD.load(std::sync::atomic::Ordering::Relaxed)
                >= CONFIG.get_background_merge_available_under()
            {
                // too busy to allocate resources for background merges
                std::thread::sleep(std::time::Duration::from_secs(5));
                continue;
            }

            let Some(merge_data) = find_two_parts() else {
                std::thread::sleep(std::time::Duration::from_secs(1));
                continue;
            };

            let Some((part_0_cols, part_1_cols)) = Self::load_both_parts(&merge_data) else {
                std::thread::sleep(std::time::Duration::from_secs(1));
                continue;
            };

            let merged = Self::merge_parts(part_0_cols, part_1_cols);

            let mut new_part = match TablePart::try_new(
                &merge_data.table_def,
                merged.into_iter().map(Column::into_output_column).collect(),
                Some(merge_data.part_1.name.clone()), // use latest name of two for proper future merging
            ) {
                Ok(new_part) => new_part,
                Err(error) => {
                    error!("Failed to create new TablePart during merge: {error}");
                    continue;
                }
            };

            if let Err(error) = new_part.save_raw(&merge_data.table_def) {
                error!("Failed to save merged TablePart: {error}");
                continue;
            }

            if !Self::atomic_part_move(merge_data, new_part) {
                error!("Failed to move merged TablePart");
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
    }

    /// Loads all columns from a table part into memory.
    ///
    /// Returns:
    ///   * Ok: `Vec<Column>` with all part data.
    ///   * Error: `CouldNotReadData` on I/O or deserialization failure.
    fn load_part(table_def: &TableDef, part: &TablePartInfo) -> Result<Vec<Column>> {
        let mut columns = Vec::new();

        // column-stored version
        let mut marks = vec![Vec::new(); part.column_defs.len()];
        for mark in &part.marks {
            for (mark_idx, mark_info) in mark.info.iter().enumerate() {
                marks[mark_idx].push(mark_info.clone());
            }
        }

        for (col_idx, column_def) in part.column_defs.iter().enumerate() {
            let mmap = Column::open_as_mmap(&part.get_column_path(table_def, column_def))?;

            let mut data = Vec::new();
            for mark_info in &marks[col_idx] {
                let granule_data = TablePartInfo::get_granule_bytes_decompressed(
                    &mmap,
                    mark_info,
                    &column_def.constraints.compression_type,
                )?;
                let granule_data = rkyv::from_bytes::<Vec<Value>, rkyv::rancor::Error>(
                    &granule_data,
                )
                .map_err(|error| {
                    Error::CouldNotReadData(format!("Could not read data while merging: {error}"))
                })?;
                data.extend(granule_data);
            }
            let column = Column {
                column_def: column_def.clone(),
                data,
            };
            columns.push(column);
        }
        Ok(columns)
    }

    /// Merges two parts' columns into one.
    ///
    /// Extends `part_0` with data from `part_1`. If a column exists in `part_1` but not `part_0`,
    /// fills missing rows with default values.
    fn merge_parts(mut part_0: Vec<Column>, part_1: Vec<Column>) -> Vec<Column> {
        for column_1 in part_1 {
            if let Some(position) = part_0
                .iter()
                .position(|col| col.column_def == column_1.column_def)
            {
                part_0[position].data.extend(column_1.data.into_iter()); // parts are guaranteed to be non-empty.
            } else {
                let default_value = column_1
                    .column_def
                    .constraints
                    .default
                    .clone()
                    .unwrap_or_default();
                let mut data = vec![default_value; part_0[0].data.len()];
                data.extend(column_1.data.into_iter());
                part_0.push(Column {
                    column_def: column_1.column_def.clone(),
                    data,
                });
            }
        }

        part_0
    }

    /// Loads both parts to be merged into memory.
    ///
    /// Returns: `Some((part_0_cols, part_1_cols))` on success, `None` on failure.
    fn load_both_parts(merge_data: &MergeData) -> Option<(Vec<Column>, Vec<Column>)> {
        let part_0_cols = Self::load_part(&merge_data.table_def, &merge_data.part_0)
            .map_err(|error| {
                error!(
                    "Error loading part ({}): {error:?}",
                    &merge_data.part_0.name
                );
                error
            })
            .ok()?;

        let part_1_cols = Self::load_part(&merge_data.table_def, &merge_data.part_1)
            .map_err(|error| {
                error!(
                    "Error loading part ({}): {error:?}",
                    &merge_data.part_1.name
                );
                error
            })
            .ok()?;

        Some((part_0_cols, part_1_cols))
    }

    /// Atomically replaces old parts with the merged part.
    ///
    /// Renames old parts to `.old` suffix, updates in-memory index, moves new part,
    /// and cleans up old directories. Rolls back on failure.
    ///
    /// Returns: `true` on success, `false` on failure (with rollback attempted).
    fn atomic_part_move(merge_data: MergeData, new_part: TablePart) -> bool {
        // prevent from new selects
        let Some(mut config) = TABLE_DATA.get_mut(&merge_data.table_def) else {
            warn!("could not get mutable table config");
            return false;
        };
        let part_0_old = merge_data
            .table_def
            .get_path()
            .join(&merge_data.part_0.name);
        let part_0_new = merge_data
            .table_def
            .get_path()
            .join(format!("{}.old", &merge_data.part_0.name));
        let part_1_old = merge_data
            .table_def
            .get_path()
            .join(&merge_data.part_1.name);
        let part_1_new = merge_data
            .table_def
            .get_path()
            .join(format!("{}.old", &merge_data.part_1.name));

        if std::fs::rename(&part_0_old, &part_0_new).is_err() {
            warn!(
                "Could not rename normal part to old: {}",
                part_0_old.display()
            );
            return false;
        }

        if std::fs::rename(&part_1_old, &part_1_new).is_err() {
            if let Err(error) = std::fs::rename(&part_0_new, part_0_old) {
                error!(
                    "Couldn't move part ({}). Remove `.old` extension and solve the issue: {}",
                    part_0_new.display(),
                    error
                );
            }
            return false;
        }
        config
            .infos
            .retain(|x| x.name != merge_data.part_0.name && x.name != merge_data.part_1.name);
        drop(config); // drop mut access for `move_to_normal`

        if new_part.move_to_normal(&merge_data.table_def).is_err() {
            let Some(mut config) = TABLE_DATA.get_mut(&merge_data.table_def) else {
                return false;
            };
            if let Err(error) = std::fs::rename(&part_0_new, &part_0_old) {
                error!(
                    "Couldn't move part ({}). Remove `.old` extension and solve the issue: {}",
                    part_0_new.display(),
                    error
                );
            } else {
                config.infos.push(merge_data.part_0);
            }
            if let Err(error) = std::fs::rename(&part_1_new, &part_1_old) {
                error!(
                    "Couldn't move part ({}). Remove `.old` extension and solve the issue: {}",
                    part_1_new.display(),
                    error
                );
            } else {
                config.infos.push(merge_data.part_1);
            }
            return false;
        }

        if let Err(error) = std::fs::remove_dir_all(&part_0_new) {
            warn!(
                "Couldn't remove ({}). Remove directory and solve the issue: {}",
                part_0_new.display(),
                error
            );
        }
        if let Err(error) = std::fs::remove_dir_all(&part_1_new) {
            warn!(
                "Couldn't remove ({}). Remove directory and solve the issue: {}",
                part_1_new.display(),
                error
            );
        }
        true
    }
}

#[derive(Debug)]
struct MergeData {
    table_def: TableDef,
    part_0: TablePartInfo,
    part_1: TablePartInfo,
}

fn find_two_parts() -> Option<MergeData> {
    let data = TABLE_DATA.iter().find(|x| x.infos.len() > 1)?;

    let mut names: Vec<_> = data.infos.iter().map(|x| &x.name).collect();
    names.sort_by(|a, b| uuid_str_cmp(a, b));

    let part_0 = data.infos.iter().find(|x| x.name == *names[0])?;
    let part_1 = data.infos.iter().find(|x| x.name == *names[1])?;

    Some(MergeData {
        table_def: data.pair().0.clone(),
        part_0: part_0.clone(),
        part_1: part_1.clone(),
    })
}

/// Try to parse both UUIDs and compare their timestamps.
/// If either fails, fall back to string comparison.
fn uuid_str_cmp(t1: &str, t2: &str) -> std::cmp::Ordering {
    if t1 == t2 {
        return std::cmp::Ordering::Equal;
    }

    // (seconds, subsec_nanos)
    let t1_unix = Uuid::parse_str(t1)
        .ok()
        .and_then(|uuid| uuid.get_timestamp().map(|ts| ts.to_unix()));
    let t2_unix = Uuid::parse_str(t2)
        .ok()
        .and_then(|uuid| uuid.get_timestamp().map(|ts| ts.to_unix()));

    match (t1_unix, t2_unix) {
        (Some(t1_unix), Some(t2_unix)) => {
            if (t1_unix.0 > t2_unix.0) || (t1_unix.0 == t2_unix.0 && t1_unix.1 > t2_unix.1) {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Less
            }
        }
        _ => t1.cmp(t2),
    }
}
