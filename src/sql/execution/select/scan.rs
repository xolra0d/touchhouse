use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::error::{Error, Result};
use crate::sql::execution::select::accumulate_function::AccumulateFn;
use crate::sql::execution::select::{
    Granule, Strategy, create_proj_to_part_cols_mapping, get_granule_rows_fallback,
};
use crate::sql::{OutputColumn, Projection};
use crate::storage::value::ArchivedValue;
use crate::storage::{PhysicalColumn, TableDef, TablePartInfo, Value};
use rayon::prelude::*;
use rkyv::vec::ArchivedVec;

pub struct ScanLogic;

impl ScanLogic {
    pub fn get_out_cols(
        table_def: &TableDef,
        parts_granules: Vec<(&TablePartInfo, Vec<Granule>)>,
        projections: Vec<Projection>,
        mut accumulator: Vec<Vec<Value>>,
        accumulate: &(impl AccumulateFn + Sync),
        index_granularity: usize,
        strategy: &Strategy,
    ) -> Result<Vec<OutputColumn>> {
        let accepted_row_count = Arc::new(AtomicUsize::new(0));
        let mut output_columns: Vec<OutputColumn> = projections
            .iter()
            .cloned()
            .map(OutputColumn::from)
            .collect();

        for (part_info, granules) in parts_granules {
            let mapping = create_proj_to_part_cols_mapping(&projections, &part_info.column_defs);
            let mmaps: Result<Vec<_>> = mapping
                .par_iter()
                .map(|m| {
                    if let Some(col_idx) = m {
                        let mmap = PhysicalColumn::open_as_mmap(
                            &part_info.get_column_path(table_def, &part_info.column_defs[*col_idx]),
                        )?;
                        Ok(Some(mmap))
                    } else {
                        Ok(None)
                    }
                })
                .collect();
            let mmaps = Arc::new(mmaps?);

            accumulator = granules
                .into_par_iter()
                .filter_map(|granule| {
                    if accepted_row_count.load(Ordering::Relaxed) >= strategy.lines_to_read {
                        return None;
                    }
                    let mut refs: Vec<Option<(Vec<u8>, &[bool])>> =
                        vec![None; output_columns.len()];

                    let mut granule_len = None;

                    for (proj_idx, mmap) in mmaps.iter().enumerate() {
                        if let Some(mmap) = &mmap
                            && let Some(col_def_idx_in_part) = mapping[proj_idx]
                        {
                            let granule_bytes = match TablePartInfo::get_granule_bytes_decompressed(
                                mmap,
                                &part_info.marks[granule.granule_idx].info[col_def_idx_in_part],
                                &part_info.column_defs[col_def_idx_in_part]
                                    .constraints
                                    .compression_type,
                            ) {
                                Ok(bytes) => bytes,
                                Err(err) => return Some(Err(err)),
                            };

                            if granule_len.is_none() {
                                let archived: &ArchivedVec<ArchivedValue> =
                                    unsafe { rkyv::access_unchecked(&granule_bytes) };
                                granule_len = Some(archived.len());
                            }
                            refs[proj_idx] = Some((granule_bytes, &granule.mask));
                        }
                    }

                    let row_count = granule_len.unwrap_or({
                        let Some(last_mark) = part_info.marks.last() else {
                            return Some(Err(Error::NoColumnsSpecified));
                        };
                        if part_info.marks[granule.granule_idx] == *last_mark {
                            match get_granule_rows_fallback(table_def, part_info, &last_mark.info) {
                                Ok(rows) => rows,
                                Err(err) => return Some(Err(err)),
                            }
                        } else {
                            index_granularity
                        }
                    });
                    accepted_row_count.fetch_add(row_count, Ordering::Relaxed);

                    Some(accumulate.accumulate_raw(accumulator.clone(), &refs, row_count))
                })
                .try_reduce_with(|a, b| accumulate.accumulate_values(a, b))
                .ok_or(Error::EmptySource)??;
        }
        // apply accumulated values
        for (col_idx, acc_col_data) in accumulator.into_iter().enumerate() {
            output_columns[col_idx].data = acc_col_data;
        }

        Ok(output_columns)
    }
}
