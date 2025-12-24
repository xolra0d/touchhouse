use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::error::{Error, Result};
use crate::sql::execution::select::Strategy;
use crate::sql::{OutputColumn, Projection, ProjectionValue};
use crate::storage::value::ArchivedValue;
use crate::storage::{Column, ColumnDef, MarkInfo, TableDef, TablePartInfo, Value};
use rkyv::vec::ArchivedVec;

pub struct ScanLogic;

impl ScanLogic {
    pub fn get_archived_values<F>(
        table_def: &TableDef,
        row_parts_masks: Vec<(&TablePartInfo, Vec<(usize, Vec<bool>)>)>,
        projections: Vec<Projection>,
        mut accumulator: Vec<Vec<Value>>,
        mut accumulator_fn: F,
        index_granularity: usize,
        strategy: &Strategy,
    ) -> Result<Vec<OutputColumn>>
    where
        F: FnMut(Vec<Vec<Value>>, &Vec<Option<(Vec<u8>, &[bool])>>, usize) -> Vec<Vec<Value>>,
    {
        let accepted_row_count = Arc::new(AtomicUsize::new(0));
        let mut output_columns = Self::convert_proj_to_out_cols(projections);

        let might_disk_col_defs: Vec<_> = output_columns
            .iter()
            .filter(|col| !col.is_virtual)
            .map(|col| &col.column_def)
            .collect();

        // let results: Result<Vec<_>> = row_parts_masks
        //     .par_iter()
        //     .map(|(part_info, granules_mask)| {})
        //     .collect();

        for (part_info, granules_mask) in row_parts_masks {
            let mapping =
                Self::create_out_cols_to_disk_cols_mapping(&output_columns, &part_info.column_defs);

            let mut mmaps = Vec::with_capacity(might_disk_col_defs.len());

            // todo: open only required..
            for col_def in &part_info.column_defs {
                let mmap = Column::open_as_mmap(&part_info.get_column_path(&table_def, col_def))?;
                Column::validate_mmap(&mmap, &col_def.name)?;
                mmaps.push(mmap);
            }

            let mut refs: Vec<Option<(Vec<u8>, &[bool])>> = vec![None; output_columns.len()];

            for (granule_idx, granule_mask) in granules_mask {
                if accepted_row_count.load(Ordering::Relaxed) >= strategy.lines_to_read {
                    break;
                }

                let mut row_count = None;
                for &(out_col_idx, part_col_idx) in &mapping {
                    let granule_bytes = TablePartInfo::get_granule_bytes_decompressed(
                        &mmaps[part_col_idx],
                        &part_info.marks[granule_idx].info[part_col_idx],
                        &output_columns[out_col_idx]
                            .column_def
                            .constraints
                            .compression_type,
                    )?;
                    if row_count.is_none() {
                        let archived: &ArchivedVec<ArchivedValue> =
                            unsafe { rkyv::access_unchecked(&granule_bytes) };
                        row_count = Some(archived.len());
                    }
                    refs[out_col_idx] = Some((granule_bytes, &granule_mask));
                }

                let row_count = row_count.unwrap_or({
                    let Some(last_mark) = part_info.marks.last() else {
                        return Err(Error::NoColumnsSpecified);
                    };
                    if part_info.marks[granule_idx] == *last_mark {
                        Self::get_granule_rows_fallback(table_def, part_info, &last_mark.info)?
                    } else {
                        index_granularity
                    }
                });
                accepted_row_count.fetch_add(row_count, Ordering::Relaxed);
                accumulator = accumulator_fn(accumulator, &refs, row_count);
                refs = vec![None; output_columns.len()];
            }
        }

        // apply accumulated values
        for (col_idx, acc_col_data) in accumulator.into_iter().enumerate() {
            output_columns[col_idx].data.extend(acc_col_data);
        }

        Ok(output_columns)
    }

    pub fn create_out_cols_to_disk_cols_mapping(
        out_cols: &[OutputColumn],
        part_col_defs: &[ColumnDef],
    ) -> Vec<(usize, usize)> {
        let mut result = Vec::new();

        for (col_idx, col) in out_cols.iter().enumerate() {
            if col.is_virtual {
                continue;
            }

            if let Some(position) = part_col_defs
                .iter()
                .position(|p_col_def| *p_col_def == col.column_def)
            {
                result.push((col_idx, position));
            }
        }

        result
    }

    pub fn convert_proj_to_out_cols(projections: Vec<Projection>) -> Vec<OutputColumn> {
        let mut output_columns = Vec::with_capacity(projections.len());

        for projection in projections {
            let Projection { alias, source } = projection;
            match source {
                ProjectionValue::Value(value) => {
                    output_columns.push(OutputColumn {
                        alias,
                        column_def: ColumnDef {
                            name: format!("{value:?}"),
                            field_type: value.get_type(),
                            constraints: Default::default(),
                        },
                        data: Vec::new(),
                        is_virtual: true,
                    });
                }
                ProjectionValue::ColumnDef(column_def) => output_columns.push(OutputColumn {
                    alias,
                    column_def,
                    data: Vec::new(),
                    is_virtual: false,
                }),
            }
        }

        output_columns
    }

    pub fn get_granule_rows_fallback(
        table_def: &TableDef,
        part_info: &TablePartInfo,
        mark_info: &Vec<MarkInfo>,
    ) -> Result<usize> {
        let Some(first_mark_info) = mark_info.first() else {
            return Err(Error::NoColumnsSpecified);
        };
        let Some(col_def) = part_info.column_defs.first() else {
            return Err(Error::NoColumnsSpecified);
        };
        let mmap = Column::open_as_mmap(&part_info.get_column_path(&table_def, col_def))?;
        Column::validate_mmap(&mmap, &col_def.name)?;

        let granule_bytes = TablePartInfo::get_granule_bytes_decompressed(
            &mmap,
            &first_mark_info,
            &col_def.constraints.compression_type,
        )?;
        let archived: &ArchivedVec<ArchivedValue> =
            unsafe { rkyv::access_unchecked(&granule_bytes) };

        Ok(archived.len())
    }
}
