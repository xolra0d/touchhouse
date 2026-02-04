use log::error;
use std::marker::PhantomData;

use crate::error::{Error, Result};
use crate::sql::Projection;
use crate::storage::table_metadata::STANDARD_GRANULARITY;
use crate::storage::tables::{Immutable, StorageRead, sealed};
use crate::storage::{ColumnDef, Constraints, OutputColumn, TableSchema, ToValue};

/// Represents virtual format/storage. Scans from `Vec<OutputColumn>`.
pub struct VirtualStorage<Mode: sealed::SealedMode> {
    columns: Vec<OutputColumn>,
    granule_idx: Option<usize>,
    schema: TableSchema,
    _marker: PhantomData<Mode>,
}

fn proj_to_col_def(proj: &Projection) -> ColumnDef {
    ColumnDef {
        name: proj.to_string(),
        field_type: proj.source.get_field_type(),
        constraints: Constraints::default(),
    }
}

impl From<Vec<OutputColumn>> for VirtualStorage<Immutable> {
    fn from(columns: Vec<OutputColumn>) -> Self {
        let col_defs: Vec<_> = columns.iter().map(|x| proj_to_col_def(&x.proj)).collect();

        Self {
            columns,
            granule_idx: None,
            schema: TableSchema {
                columns: col_defs.clone(),
                primary_key: col_defs.clone(),
                order_by: col_defs.clone(),
            },
            _marker: PhantomData,
        }
    }
}

// impl VirtualStorage<Mutable> {
//     pub fn from_mut(columns: Vec<OutputColumn>) -> Self {
//         let col_defs: Vec<_> = columns.iter().map(|x| proj_to_col_def(&x.proj)).collect();

//         Self {
//             columns,
//             granule_idx: None,
//             schema: TableSchema {
//                 columns: col_defs.clone(),
//                 primary_key: col_defs.clone(),
//                 order_by: col_defs.clone(),
//             },
//             _marker: PhantomData,
//         }
//     }
// }

impl<Mode: sealed::SealedMode> StorageRead for VirtualStorage<Mode> {
    fn get_total_rows(&self) -> usize {
        self.columns.first().map_or(0, |x| x.data.len())
    }

    fn get_schema(&self) -> &TableSchema {
        &self.schema
    }

    fn load_next_chunk(&mut self) -> Result<Option<()>> {
        let granule_count = self
            .get_total_rows()
            .div_ceil(STANDARD_GRANULARITY as usize);

        if let Some(granule_idx) = &mut self.granule_idx
            && *granule_idx + 1 < granule_count
        {
            *granule_idx += 1;
            Ok(Some(()))
        } else if granule_count != 0 {
            self.granule_idx = Some(0);
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    fn access_chunk_column<'v>(&'v self, proj: &Projection) -> Result<Vec<impl ToValue + 'v>> {
        let Some(granule_idx) = self.granule_idx else {
            let msg =
                "Tried to acces chunk data in `VirtualStorage::access_chunk_column` while no data was loaded.".to_string();
            error!("{msg}");
            return Err(Error::Internal(msg));
        };

        let Some(col_idx) = self.columns.iter().position(|x| x.proj == *proj) else {
            let msg = format!(
                "Tried to access column with proj: {proj:?}, but this column is not found."
            );
            error!("{msg}");
            return Err(Error::Internal(msg));
        };

        let col_data = self.columns[col_idx].data[granule_idx * (STANDARD_GRANULARITY as usize)
            ..(granule_idx + 1) * (STANDARD_GRANULARITY as usize)]
            .to_vec(); // TODO: REMOVE THIS. CONSIDER CONSUMING
        Ok(col_data)
    }
}
