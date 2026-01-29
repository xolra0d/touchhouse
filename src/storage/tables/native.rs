use dashmap::mapref::one::{Ref, RefMut};
use log::error;
use memmap2::Mmap;
use rkyv::vec::ArchivedVec;
use std::marker::PhantomData;

use crate::error::{Error, Result};
use crate::runtime_config::{TABLE_DATA, TableConfig};
use crate::sql::Projection;
use crate::storage::table_metadata::STANDARD_GRANULARITY;
use crate::storage::{
    ArchivedValue, PhysicalColumn, TableDef, TablePart, TablePartInfo, TableSchema, ToValue,
    tables::{Immutable, Mutable, StorageRead, StorageWrite, sealed},
};

#[derive(Debug)]
enum LockFormat<'a> {
    Ref(Ref<'a, TableDef, TableConfig>),
    RefMut(RefMut<'a, TableDef, TableConfig>),
}

impl<'a> LockFormat<'a> {
    fn table_def(&self) -> &TableDef {
        match self {
            Self::Ref(data) => data.key(),
            Self::RefMut(data) => data.key(),
        }
    }
}

struct LoadedChunk {
    part_info: TablePartInfo, // for table scan we are forced to copy each part info, because of lifetime issues
    granule_idx: usize,
    mmaps: Vec<Mmap>,
    chunk_bytes: Vec<Vec<u8>>,
}

pub struct NativeStorage<'a, Mode: sealed::SealedMode> {
    data_lock: LockFormat<'a>,
    loaded_chunk: Option<LoadedChunk>,
    _marker: PhantomData<Mode>,
}

impl<'a> TryFrom<&TableDef> for NativeStorage<'a, Immutable> {
    type Error = Error;
    fn try_from(table_def: &TableDef) -> Result<Self> {
        let Some(data_lock) = TABLE_DATA.get(table_def) else {
            return Err(Error::TableNotFound);
        };
        Ok(Self {
            data_lock: LockFormat::Ref(data_lock),
            loaded_chunk: None,
            _marker: PhantomData,
        })
    }
}

impl<'a> NativeStorage<'a, Mutable> {
    pub fn try_from_mut(table_def: &TableDef) -> Result<Self> {
        let Some(data_lock) = TABLE_DATA.get_mut(table_def) else {
            return Err(Error::TableNotFound);
        };
        Ok(Self {
            data_lock: LockFormat::RefMut(data_lock),
            loaded_chunk: None,
            _marker: PhantomData,
        })
    }
}

impl<'a, Mode: sealed::SealedMode> StorageRead for NativeStorage<'a, Mode> {
    fn get_total_rows(&self) -> usize {
        self.get_ref_data()
            .1
            .infos
            .iter()
            .map(|x| x.row_count as usize)
            .sum()
    }

    fn get_schema(&self) -> &TableSchema {
        &self.get_ref_data().1.metadata.schema
    }

    fn load_next_chunk(&mut self) -> Result<Option<()>> {
        if self.advance_chunk_metadata()?.is_none() {
            return Ok(None);
        }

        if let Some(loaded_chunk) = &mut self.loaded_chunk {
            let granule_mark_infos = &loaded_chunk.part_info.marks[loaded_chunk.granule_idx].info;
            let mut chunk_bytes = Vec::with_capacity(loaded_chunk.part_info.column_defs.len());

            for col_idx in 0..loaded_chunk.part_info.column_defs.len() {
                let bytes = TablePartInfo::get_granule_bytes_decompressed(
                    &loaded_chunk.mmaps[col_idx],
                    &granule_mark_infos[col_idx],
                    &loaded_chunk.part_info.column_defs[col_idx]
                        .constraints
                        .compression_type,
                )?;
                chunk_bytes.push(bytes);
            }

            loaded_chunk.chunk_bytes = chunk_bytes;

            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    fn access_chunk_column(&self, proj: &Projection) -> Result<Vec<impl ToValue>> {
        let Some(loaded_chunk) = &self.loaded_chunk else {
            let msg = format!(
                "Tried to acces chunk data in `NativeStorage::access_chunk_column` while no data was loaded for table: {}",
                self.data_lock.table_def()
            );
            error!("{msg}");
            return Err(Error::Internal(msg));
        };

        let proj_source_string = proj.source.to_string();

        let Some(col_idx) = loaded_chunk
            .part_info
            .column_defs
            .iter()
            .position(|x| x.name == proj_source_string)
        else {
            return Ok(vec![&ArchivedValue::Null; STANDARD_GRANULARITY]);
        };

        let values: &ArchivedVec<ArchivedValue> =
            unsafe { rkyv::access_unchecked(&loaded_chunk.chunk_bytes[col_idx]) };

        let mut result = Vec::with_capacity(values.len());
        for value in values.into_iter() {
            result.push(value);
        }
        Ok(result)
    }
}

impl StorageWrite for NativeStorage<'_, Mutable> {
    fn insert(&mut self, columns: Vec<PhysicalColumn>) -> Result<()> {
        let (table_def, table_config) = self.get_mut_data();

        let mut table_part = TablePart::try_new(&table_config.metadata, columns, None)?;

        table_part.save_raw(table_def, table_config.metadata.settings.index_granularity)?;
        table_part.move_to_normal(table_def, &mut table_config.infos)?;

        Ok(())
    }

    fn drop(self, if_exists: bool) -> Result<()> {
        let table_def = self.data_lock.table_def().clone();
        drop(self.data_lock); // release mut lock

        let _ = TABLE_DATA.remove(&table_def);

        let table_path = table_def.get_path();

        let remove_result = std::fs::remove_dir_all(&table_path);
        match (remove_result, if_exists) {
            (Ok(()), _) => Ok(()),
            (Err(error), true) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            (Err(error), false) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(Error::TableNotFound)
            }
            (Err(error), _) => {
                let error_msg = format!(
                    "Could not remove table entry from disk: {}. Stop database, remove {:?} folder, and restart the database.",
                    error,
                    std::path::absolute(&table_path).unwrap_or(table_path),
                );

                error!("{error_msg}");
                Err(Error::Internal(error_msg))
            }
        }
    }
}

impl<'a> NativeStorage<'a, Mutable> {
    fn get_mut_data(&mut self) -> (&TableDef, &mut TableConfig) {
        match &mut self.data_lock {
            LockFormat::RefMut(r) => r.pair_mut(),
            LockFormat::Ref(_) => unreachable!("Cannot have immutable lock in mutable open"),
        }
    }
}

impl<'a, Mode: sealed::SealedMode> NativeStorage<'a, Mode> {
    fn advance_chunk_metadata(&mut self) -> Result<Option<()>> {
        if let Some(loaded_chunk) = &self.loaded_chunk {
            if loaded_chunk.granule_idx + 1 == loaded_chunk.part_info.marks.len() {
                // currently loaded granule is the last one in part, so we need to load another part
                let table_config = self.get_ref_data().1;

                let Some(next_part_info_idx) = table_config
                    .infos
                    .iter()
                    .position(|info| *info == loaded_chunk.part_info)
                    .map(|x| x + 1)
                else {
                    return Ok(None);
                };

                let Some(next_part_info) = table_config.infos.get(next_part_info_idx) else {
                    return Ok(None);
                };

                self.loaded_chunk = Some(self.gen_next_chunk(next_part_info.clone())?);
            } else {
                let Some(loaded_chunk) = &mut self.loaded_chunk.as_mut() else {
                    let msg = format!(
                        "Could not reborrow data in `NativeStorage::advance_chunk_metadata` for table: {}",
                        self.data_lock.table_def()
                    );
                    error!("{msg}");
                    return Err(Error::Internal(msg));
                };
                loaded_chunk.granule_idx += 1;
            }
        } else {
            let table_config = self.get_ref_data().1;

            let Some(next_part_info) = table_config.infos.first() else {
                return Ok(None);
            };

            self.loaded_chunk = Some(self.gen_next_chunk(next_part_info.clone())?);
        }
        Ok(Some(()))
    }

    fn gen_next_chunk(&self, new_part: TablePartInfo) -> Result<LoadedChunk> {
        let table_def = self.data_lock.table_def();

        let mut mmaps = Vec::with_capacity(new_part.column_defs.len());

        for col_def in &new_part.column_defs {
            let mmap = PhysicalColumn::open_as_mmap(&new_part.get_column_path(table_def, col_def))?;
            PhysicalColumn::validate_mmap(&mmap, &col_def.name)?;
            mmaps.push(mmap);
        }

        Ok(LoadedChunk {
            part_info: new_part,
            granule_idx: 0,
            mmaps,
            chunk_bytes: Vec::new(),
        })
    }

    fn get_ref_data(&self) -> (&TableDef, &TableConfig) {
        match &self.data_lock {
            LockFormat::RefMut(r) => r.pair(),
            LockFormat::Ref(r) => r.pair(),
        }
    }
}
