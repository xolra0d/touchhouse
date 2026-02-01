use crate::error::Result;
use crate::sql::Projection;
use crate::storage::{NativeStorage, StorageRead, TablePart, ToValue, Value};

use std::{cmp::Ordering, time::Duration};

use log::{error, info, warn};
use uuid::Uuid;

use crate::{
    config::CONFIG,
    runtime_config::{DATABASE_LOAD, TABLE_DATA},
    storage::{PhysicalColumn, TableDef, TableMetadata, TablePartInfo},
};

const SLEEP_IF_NOT_FOUND: Duration = Duration::from_secs(5);

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
                // Too busy with selects to allocate resources and lock for background merges
                std::thread::sleep(SLEEP_IF_NOT_FOUND);
                continue;
            }

            let Some(parts_to_merge) = Self::find_parts_to_merge() else {
                std::thread::sleep(SLEEP_IF_NOT_FOUND);
                continue;
            };

            let part_1_cols = match Self::load_part_cols(
                &parts_to_merge.table_def,
                parts_to_merge.part_1_info.clone(),
            ) {
                Ok(cols) => cols,
                Err(error) => {
                    error!(
                        "Error loading part ({}): {error:?}",
                        &parts_to_merge.part_1_info.name
                    );

                    std::thread::sleep(SLEEP_IF_NOT_FOUND);
                    continue;
                }
            };
            let part_2_cols = match Self::load_part_cols(
                &parts_to_merge.table_def,
                parts_to_merge.part_2_info.clone(),
            ) {
                Ok(cols) => cols,
                Err(error) => {
                    error!(
                        "Error loading part ({}): {error:?}",
                        &parts_to_merge.part_2_info.name
                    );

                    std::thread::sleep(SLEEP_IF_NOT_FOUND);
                    continue;
                }
            };

            let combined_cols = Self::combine_cols(part_1_cols, part_2_cols);
            let mut new_part = match TablePart::try_new(
                &parts_to_merge.table_metadata,
                combined_cols,
                Some(parts_to_merge.part_2_info.name.clone()), // use latest name of two for proper future merging
            ) {
                Ok(new_part) => new_part,
                Err(error) => {
                    error!("Failed to create new TablePart during merge: {error}");
                    continue;
                }
            };
            if let Err(error) = new_part.save_raw(
                &parts_to_merge.table_def,
                parts_to_merge.table_metadata.settings.index_granularity,
            ) {
                error!("Failed to save merged TablePart: {error}");
                continue;
            }

            if !Self::atomic_part_move(parts_to_merge, new_part) {
                error!("Failed to move merged TablePart");
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
    }

    fn find_parts_to_merge() -> Option<PartsToMerge> {
        let table = TABLE_DATA.iter().find(|x| x.infos.len() > 1)?;

        let mut parts_names: Vec<_> = table.infos.iter().map(|x| &x.name).collect();
        parts_names.sort_unstable_by(|a, b| cmp_uuid_strs_asc(a, b));

        let part_1 = table.infos.iter().find(|x| x.name == *parts_names[0])?;
        let part_2 = table.infos.iter().find(|x| x.name == *parts_names[1])?;

        Some(PartsToMerge {
            table_def: table.key().clone(),
            table_metadata: table.value().metadata.clone(),
            part_1_info: part_1.clone(),
            part_2_info: part_2.clone(),
        })
    }

    fn load_part_cols(
        table_def: &TableDef,
        part_info: TablePartInfo,
    ) -> Result<Vec<PhysicalColumn>> {
        let mut columns: Vec<_> = part_info
            .column_defs
            .iter()
            .map(|column_def| PhysicalColumn {
                column_def: column_def.clone(),
                data: Vec::new(),
            })
            .collect();

        let col_defs_count = part_info.column_defs.len();
        let marks_count = part_info.marks.len();
        let projections: Vec<_> = part_info
            .column_defs
            .iter()
            .map(|x| Projection::from(x.clone()))
            .collect();

        let mut storage = NativeStorage::try_from_table_def_and_part(table_def, part_info)?;

        for _ in 0..marks_count {
            storage.load_next_chunk()?;

            for column_idx in 0..col_defs_count {
                let data = storage.access_chunk_column(&projections[column_idx])?;
                let data = data
                    .into_iter()
                    .map(ToValue::to_value)
                    .collect::<Result<Vec<Value>>>()?;
                columns[column_idx].data.extend(data);
            }
        }

        Ok(columns)
    }

    fn combine_cols(
        mut cols1: Vec<PhysicalColumn>,
        cols2: Vec<PhysicalColumn>,
    ) -> Vec<PhysicalColumn> {
        for column_1 in cols2 {
            if let Some(position) = cols1
                .iter()
                .position(|col| col.column_def == column_1.column_def)
            {
                cols1[position].data.extend(column_1.data); // parts are guaranteed to be non-empty.
            } else {
                let default_value = column_1
                    .column_def
                    .constraints
                    .default
                    .clone()
                    .unwrap_or_default();
                let mut data = vec![default_value; cols1[0].data.len()];
                data.extend(column_1.data.into_iter());
                cols1.push(PhysicalColumn {
                    column_def: column_1.column_def.clone(),
                    data,
                });
            }
        }

        cols1
    }

    /// Atomically replaces old parts with the merged part.
    ///
    /// Renames old parts to `.old` suffix, updates in-memory index, moves new part,
    /// and cleans up old directories. Rolls back on failure.
    ///
    /// Returns: `true` on success, `false` on failure (with rollback attempted).
    fn atomic_part_move(parts_to_merge: PartsToMerge, new_part: TablePart) -> bool {
        // prevent from new selects
        let Some(mut config) = TABLE_DATA.get_mut(&parts_to_merge.table_def) else {
            warn!("could not get mutable table config");
            return false;
        };
        let part_0_old = parts_to_merge
            .table_def
            .get_path()
            .join(&parts_to_merge.part_1_info.name);
        let part_0_new = parts_to_merge
            .table_def
            .get_path()
            .join(format!("{}.old", &parts_to_merge.part_1_info.name));
        let part_1_old = parts_to_merge
            .table_def
            .get_path()
            .join(&parts_to_merge.part_2_info.name);
        let part_1_new = parts_to_merge
            .table_def
            .get_path()
            .join(format!("{}.old", &parts_to_merge.part_2_info.name));

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
        config.infos.retain(|x| {
            x.name != parts_to_merge.part_1_info.name && x.name != parts_to_merge.part_2_info.name
        });

        if new_part
            .move_to_normal(&parts_to_merge.table_def, &mut config.value_mut().infos)
            .is_err()
        {
            let Some(mut config) = TABLE_DATA.get_mut(&parts_to_merge.table_def) else {
                return false;
            };
            if let Err(error) = std::fs::rename(&part_0_new, &part_0_old) {
                error!(
                    "Couldn't move part ({}). Remove `.old` extension and solve the issue: {}",
                    part_0_new.display(),
                    error
                );
            } else {
                config.infos.push(parts_to_merge.part_1_info);
            }
            if let Err(error) = std::fs::rename(&part_1_new, &part_1_old) {
                error!(
                    "Couldn't move part ({}). Remove `.old` extension and solve the issue: {}",
                    part_1_new.display(),
                    error
                );
            } else {
                config.infos.push(parts_to_merge.part_2_info);
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

/// Try to parse both UUIDs and compare their timestamps.
fn cmp_uuid_strs_asc(t1: &str, t2: &str) -> Ordering {
    if t1 == t2 {
        return Ordering::Equal;
    }

    // (seconds, subsec_nanos)
    let Some(t1_unix) = Uuid::parse_str(t1)
        .ok()
        .and_then(|uuid| uuid.get_timestamp().map(|ts| ts.to_unix()))
    else {
        return Ordering::Equal;
    };
    let Some(t2_unix) = Uuid::parse_str(t2)
        .ok()
        .and_then(|uuid| uuid.get_timestamp().map(|ts| ts.to_unix()))
    else {
        return Ordering::Equal;
    };

    if (t1_unix.0 > t2_unix.0) || (t1_unix.0 == t2_unix.0 && t1_unix.1 > t2_unix.1) {
        Ordering::Greater
    } else {
        Ordering::Less
    }
}

struct PartsToMerge {
    table_def: TableDef,
    table_metadata: TableMetadata,
    part_1_info: TablePartInfo,
    part_2_info: TablePartInfo,
}
