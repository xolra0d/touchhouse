use crate::CONFIG;
use crate::error::{Error, Result};
use crate::runtime_config::TableConfig;
use crate::sql::compiled_filter::CompiledFilter;
use crate::sql::execution::select::{
    Granule, Strategy, create_proj_to_part_cols_mapping, get_granule_rows_fallback,
};
use crate::sql::{Projection, ProjectionValue};
use crate::storage::value::ArchivedValue;
use crate::storage::{ColumnDef, PhysicalColumn, TableDef, TablePartInfo};

use rayon::prelude::*;
use rkyv::vec::ArchivedVec;
use sqlparser::ast::Expr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct FilterLogic;

impl FilterLogic {
    pub fn filter_marks<'a>(
        table_config: &'a TableConfig,
        filter_expr: Option<Expr>,
        table_def: &TableDef,
        strategy: &Strategy,
    ) -> Result<Vec<(&'a TablePartInfo, Vec<Granule>)>> {
        let Some(filter) = filter_expr else {
            return Self::no_filter_fallback(table_def, table_config);
        };

        let compiled_filter =
            CompiledFilter::try_compile(filter, &table_config.metadata.schema.columns)?;

        let col_defs_in_filter =
            compiled_filter.get_col_defs_inside(&table_config.metadata.schema.columns);

        let col_def_mappings = FilterLogic::create_filter_cols_table_cols_mapping(
            &col_defs_in_filter,
            &table_config.metadata.schema.columns,
        )?;

        let use_filter_optimization = FilterLogic::should_use_compiler_optimization(
            &col_defs_in_filter,
            &table_config.metadata.schema.primary_key,
        );

        let projections_to_filter: Vec<_> = col_defs_in_filter
            .iter()
            .map(|col_def| Projection {
                alias: None,
                source: ProjectionValue::ColumnDef((*col_def).clone()),
            })
            .collect();

        let mut part_infos: Vec<_> = table_config.infos.iter().collect();

        // sort DESC, so we will first parse parts with bigger num of rows, allowing more parallelism
        // todo: also cmp by cols in part, so we can sort by rows*K + number_of_cols_in_part_out_of_all_filter_columns*D, where K, D some constants
        // because sorting with less columns in part is easier
        part_infos.sort_unstable_by(|a, b| b.row_count.cmp(&a.row_count));

        let parts_granule_marks: Vec<_> = part_infos
            .par_iter()
            .map(|&part_info| {
                (
                    part_info,
                    compiled_filter.filter_marks(
                        &part_info.marks,
                        use_filter_optimization,
                        &table_config.metadata.schema.primary_key,
                        &table_config.metadata.schema.columns,
                    ),
                )
            })
            .collect();

        let mut total_parts_mask = Vec::with_capacity(part_infos.len());
        let accepted_rows_count = Arc::new(AtomicUsize::new(0));

        for (part_info, mark_idxs_to_scan) in &parts_granule_marks {
            let mapping =
                create_proj_to_part_cols_mapping(&projections_to_filter, &part_info.column_defs);
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

            let part_mask: Result<Vec<Vec<Granule>>> = mark_idxs_to_scan
                .par_iter()
                .chunks(CONFIG.get_tasks_per_thread())
                .map(|mark_indxs_chunk| {
                    let mut data_bytes = vec![None; col_defs_in_filter.len()];
                    let mut granule_masks: Vec<Granule> =
                        Vec::with_capacity(mark_indxs_chunk.len());

                    for &mark_idx in mark_indxs_chunk {
                        if accepted_rows_count.load(Ordering::Relaxed) >= strategy.lines_to_read {
                            break;
                        }

                        let mut row_count = None;

                        for (proj_idx, mmap) in mmaps.iter().enumerate() {
                            if let Some(mmap) = &mmap
                                && let Some(col_def_idx_in_part) = mapping[proj_idx]
                            {
                                let granule_bytes = TablePartInfo::get_granule_bytes_decompressed(
                                    mmap,
                                    &part_info.marks[mark_idx].info[col_def_idx_in_part],
                                    &part_info.column_defs[col_def_idx_in_part]
                                        .constraints
                                        .compression_type,
                                )?;
                                if row_count.is_none() {
                                    let archived: &ArchivedVec<ArchivedValue> =
                                        unsafe { rkyv::access_unchecked(&granule_bytes) };
                                    row_count = Some(archived.len());
                                }
                                data_bytes[proj_idx] = Some(granule_bytes);
                            }
                        }

                        let row_count = row_count.unwrap_or({
                            let Some(last_mark) = part_info.marks.last() else {
                                return Err(Error::NoColumnsSpecified);
                            };
                            if part_info.marks[mark_idx] == *last_mark {
                                get_granule_rows_fallback(table_def, part_info, &last_mark.info)?
                            } else {
                                table_config.metadata.settings.index_granularity as usize
                            }
                        });

                        let granule_mask = compiled_filter.generate_mask(
                            &data_bytes,
                            &col_def_mappings,
                            row_count,
                        );
                        granule_masks.push(Granule {
                            granule_idx: mark_idx,
                            mask: granule_mask,
                        });

                        accepted_rows_count.fetch_add(row_count, Ordering::Relaxed);
                    }

                    Ok(granule_masks)
                })
                .collect();
            let part_mask: Vec<_> = part_mask?.into_iter().flatten().collect();
            total_parts_mask.push((*part_info, part_mask));
        }

        Ok(total_parts_mask)
    }

    fn no_filter_fallback<'a>(
        table_def: &TableDef,
        table_config: &'a TableConfig,
    ) -> Result<Vec<(&'a TablePartInfo, Vec<Granule>)>> {
        let mut total_parts_mask = Vec::with_capacity(table_config.infos.len());

        for (part_idx, part_info) in table_config.infos.iter().enumerate() {
            total_parts_mask.push((part_info, Vec::with_capacity(part_info.marks.len())));
            let last_mark = &part_info.marks[part_info.marks.len() - 1];

            let col_def = part_info
                .column_defs
                .first()
                .ok_or(Error::PartDoesNotHaveColumns(part_info.name.clone()))?;

            for (mark_idx, mark) in part_info.marks.iter().enumerate() {
                if mark == last_mark {
                    let mmap = PhysicalColumn::open_as_mmap(
                        &part_info.get_column_path(table_def, col_def),
                    )?;

                    PhysicalColumn::validate_mmap(&mmap, &col_def.name)?;

                    let granule_bytes = TablePartInfo::get_granule_bytes_decompressed(
                        &mmap,
                        mark.info
                            .first()
                            .ok_or(Error::PartDoesNotHaveColumns(part_info.name.clone()))?,
                        &col_def.constraints.compression_type,
                    )?;
                    let archived: &ArchivedVec<ArchivedValue> =
                        unsafe { rkyv::access_unchecked(&granule_bytes) };

                    total_parts_mask[part_idx].1.push(Granule {
                        granule_idx: mark_idx,
                        mask: vec![true; archived.len()],
                    });

                    break;
                }
                total_parts_mask[part_idx].1.push(Granule {
                    granule_idx: mark_idx,
                    mask: vec![true; table_config.metadata.settings.index_granularity as usize],
                });
            }
        }

        Ok(total_parts_mask)
    }

    pub fn should_use_compiler_optimization(
        columns_to_filter: &[&ColumnDef],
        pk_col_defs: &[ColumnDef],
    ) -> bool {
        columns_to_filter
            .iter()
            .all(|col_def| pk_col_defs.contains(col_def))
    }

    pub fn create_filter_cols_table_cols_mapping(
        filter_col_defs: &[&ColumnDef],
        table_col_defs: &[ColumnDef],
    ) -> Result<Vec<Option<usize>>> {
        let mut result = vec![None; table_col_defs.len()];

        for (idx, filter_col_def) in filter_col_defs.iter().enumerate() {
            let Some(position) = table_col_defs
                .iter()
                .position(|table_col| &table_col == filter_col_def)
            else {
                return Err(Error::ColumnNotFound(format!("{filter_col_def:?}")));
            };
            result[position] = Some(idx);
        }
        Ok(result)
    }
}
