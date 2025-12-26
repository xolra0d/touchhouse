use crate::error::{Error, Result};
use crate::runtime_config::TableConfig;
use crate::sql::compiled_filter::{BinOp, CompiledFilter};
use crate::sql::execution::select::{GranuleMask, ScanLogic, Strategy};
use crate::storage::value::ArchivedValue;
use crate::storage::{Column, ColumnDef, Mark, MarkInfo, TableDef, TablePartInfo, Value};

use memmap2::Mmap;
use rayon::prelude::*;
use rkyv::vec::ArchivedVec;
use sqlparser::ast::Expr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::vec;

pub struct FilterLogic;

impl FilterLogic {
    pub fn filter_marks<'a>(
        table_config: &'a TableConfig,
        filter_expr: Option<Expr>,
        table_def: &TableDef,
        strategy: &Strategy,
    ) -> Result<Vec<(&'a TablePartInfo, Vec<GranuleMask>)>> {
        let Some(filter) = filter_expr else {
            return Self::no_fiter_fallback(table_def, table_config);
        };

        let compiled_filter =
            CompiledFilter::try_compile(filter, &table_config.metadata.schema.columns)?;

        let columns_to_filter = FilterLogic::get_filter_columns(
            &compiled_filter,
            &table_config.metadata.schema.columns,
        );
        let col_def_mappings = FilterLogic::create_filter_cols_table_cols_mapping(
            &columns_to_filter,
            &table_config.metadata.schema.columns,
        )?;

        let use_filter_optimization = FilterLogic::should_use_compiler_optimization(
            &columns_to_filter,
            &table_config.metadata.schema.primary_key,
        );

        let mut part_infos: Vec<_> = table_config.infos.iter().collect();

        // sort DESC, so we will first parse parts with bigger num of rows, allowing more parallelism
        // todo: also cmp by cols in part, so we can sort by rows*K + number_of_cols_in_part_out_of_all_filter_columns*D, where K, D some constants
        // because sorting with less columns in part is easier
        part_infos.sort_unstable_by(|a, b| b.row_count.cmp(&a.row_count));

        let parts_granule_marks: Vec<(&TablePartInfo, Vec<&Vec<MarkInfo>>)> = part_infos
            .par_iter()
            .map(|&part_info| {
                (
                    part_info,
                    FilterLogic::filter_granules(
                        &compiled_filter,
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

        for (part_info, marks_to_scan) in &parts_granule_marks {
            let mut mmaps: Vec<Option<Mmap>> = Vec::with_capacity(columns_to_filter.len());
            let mut filter_to_part_col_idx: Vec<Option<usize>> =
                vec![None; columns_to_filter.len()];

            for (col_idx, col_def) in columns_to_filter.iter().enumerate() {
                if let Some(part_col_idx) = part_info.column_defs.iter().position(|c| c == *col_def)
                {
                    let mmap =
                        Column::open_as_mmap(&part_info.get_column_path(table_def, col_def))?;
                    Column::validate_mmap(&mmap, &col_def.name)?;
                    mmaps.push(Some(mmap));
                    filter_to_part_col_idx[col_idx] = Some(part_col_idx);
                } else {
                    mmaps.push(None);
                }
            }
            let mmaps = Arc::new(mmaps);
            let filter_to_part_col_idx = Arc::new(filter_to_part_col_idx);

            let part_mask: Result<Vec<Vec<GranuleMask>>> = marks_to_scan
                .iter()
                .enumerate()
                .collect::<Vec<_>>()
                .par_chunks(5) // todo: move constant to cfg
                .map(|chunk_granule_marks| {
                    let mut data_bytes = vec![None; columns_to_filter.len()]; // todo: instead of new, have an average number of bytes for each column type decompressed
                    let mut mask: Vec<GranuleMask> = Vec::with_capacity(chunk_granule_marks.len());

                    for &(granule_idx, &mark_infos) in chunk_granule_marks {
                        if accepted_rows_count.load(Ordering::Relaxed) >= strategy.lines_to_read {
                            break;
                        }

                        let mut row_count = None;

                        for (file_idx, mmap) in mmaps.iter().enumerate() {
                            if let Some(mmap) = &mmap {
                                let part_col_idx = filter_to_part_col_idx[file_idx]
                                    .expect("mmap exists but mapping doesn't");
                                let granule_bytes = TablePartInfo::get_granule_bytes_decompressed(
                                    mmap,
                                    &mark_infos[part_col_idx],
                                    &columns_to_filter[file_idx].constraints.compression_type,
                                )?;
                                if row_count.is_none() {
                                    let archived: &ArchivedVec<ArchivedValue> =
                                        unsafe { rkyv::access_unchecked(&granule_bytes) };
                                    row_count = Some(archived.len());
                                }
                                data_bytes[file_idx] = Some(granule_bytes);
                            }
                        }

                        let row_count = row_count.unwrap_or(ScanLogic::get_granule_rows_fallback(
                            table_def, part_info, mark_infos,
                        )?);

                        let mask_part = FilterLogic::generate_mask(
                            &compiled_filter,
                            &data_bytes,
                            &col_def_mappings,
                            row_count,
                        );

                        mask.push(GranuleMask {
                            granule_id: granule_idx,
                            mask: mask_part,
                        });

                        accepted_rows_count.fetch_add(row_count, Ordering::Relaxed);
                    }

                    Ok(mask)
                })
                .collect();
            let part_mask: Vec<_> = part_mask?.into_iter().flatten().collect();
            total_parts_mask.push((*part_info, part_mask));
        }

        Ok(total_parts_mask)
    }

    fn no_fiter_fallback<'a>(
        table_def: &TableDef,
        table_config: &'a TableConfig,
    ) -> Result<Vec<(&'a TablePartInfo, Vec<GranuleMask>)>> {
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
                    let mmap =
                        Column::open_as_mmap(&part_info.get_column_path(table_def, col_def))?;

                    Column::validate_mmap(&mmap, &col_def.name)?;

                    let granule_bytes = TablePartInfo::get_granule_bytes_decompressed(
                        &mmap,
                        mark.info
                            .first()
                            .ok_or(Error::PartDoesNotHaveColumns(part_info.name.clone()))?,
                        &col_def.constraints.compression_type,
                    )?;
                    let archived: &ArchivedVec<ArchivedValue> =
                        unsafe { rkyv::access_unchecked(&granule_bytes) };

                    total_parts_mask[part_idx].1.push(GranuleMask {
                        granule_id: mark_idx,
                        mask: vec![true; archived.len()],
                    });

                    break;
                }
                total_parts_mask[part_idx].1.push(GranuleMask {
                    granule_id: mark_idx,
                    mask: vec![true; table_config.metadata.settings.index_granularity as usize],
                });
            }
        }

        Ok(total_parts_mask)
    }

    pub fn get_filter_columns<'a>(
        compiled_filter: &CompiledFilter,
        table_col_defs: &'a [ColumnDef],
    ) -> Vec<&'a ColumnDef> {
        let mut columns_to_filter = Vec::new();

        compiled_filter.get_column_indexes(&mut columns_to_filter);

        columns_to_filter
            .into_iter()
            .map(|col_idx| &table_col_defs[col_idx])
            .collect()
    }
    pub fn should_use_compiler_optimization(
        columns_to_filter: &[&ColumnDef],
        pk_col_defs: &[ColumnDef],
    ) -> bool {
        columns_to_filter
            .iter()
            .all(|col_def| pk_col_defs.contains(col_def))
    }

    pub fn filter_granules<'a>(
        compiled_filter: &CompiledFilter,
        marks: &'a [Mark],
        use_filter_optimization: bool,
        pk_col_defs: &[ColumnDef],
        table_col_defs: &[ColumnDef],
    ) -> Vec<&'a Vec<MarkInfo>> {
        if use_filter_optimization {
            let marks_indexes = Self::parse_complex_filter_granule(
                marks,
                compiled_filter,
                pk_col_defs,
                table_col_defs,
            );
            marks_indexes
                .into_iter()
                .map(|mark_idx| &marks[mark_idx].info)
                .collect()
        } else {
            marks.iter().map(|mark| &mark.info).collect()
        }
    }

    // todo: remove this fn...
    fn find_values<'a>(
        marks: &'a [Mark],
        pk_col_defs: &[ColumnDef],
        col_def: &ColumnDef,
    ) -> Vec<&'a Value> {
        let idx = pk_col_defs
            .iter()
            .position(|pk_col_def| pk_col_def == col_def);

        marks
            .iter()
            .map(|mark| {
                if let Some(idx) = idx {
                    &mark.index[idx]
                } else {
                    &Value::Null
                }
            })
            .collect()
    }

    fn parse_complex_filter_granule(
        marks: &[Mark],
        filter: &CompiledFilter,
        pk_col_defs: &[ColumnDef],
        table_col_defs: &[ColumnDef],
    ) -> Vec<usize> {
        match filter {
            CompiledFilter::Compare { col_idx, op, value } => {
                let values = Self::find_values(marks, pk_col_defs, &table_col_defs[*col_idx]);

                match *op {
                    BinOp::Eq => {
                        let start = values.partition_point(|&v| v < value);
                        let start = start.saturating_sub(1);
                        let end = values.partition_point(|&v| v <= value);
                        (start..end).collect()
                    }
                    BinOp::NotEq => (0..marks.len()).collect(), // cannot determine if it's present without reading
                    BinOp::Lt => {
                        let end = values.partition_point(|&v| v < value);
                        (0..end).collect()
                    }
                    BinOp::LtEq => {
                        let end = values.partition_point(|&v| v <= value);
                        (0..end).collect()
                    }
                    BinOp::Gt => {
                        let start = values.partition_point(|&v| v <= value);
                        let start = start.saturating_sub(1);
                        (start..marks.len()).collect()
                    }
                    BinOp::GtEq => {
                        let start = values.partition_point(|&v| v < value);
                        let start = start.saturating_sub(1);
                        (start..marks.len()).collect()
                    }
                }
            }
            CompiledFilter::CompareColumns {
                left_idx,
                op,
                right_idx,
            } => {
                let left_values = Self::find_values(marks, pk_col_defs, &table_col_defs[*left_idx]);
                let right_values =
                    Self::find_values(marks, pk_col_defs, &table_col_defs[*right_idx]);

                left_values
                    .into_iter()
                    .zip(right_values)
                    .enumerate()
                    .filter_map(|(idx, (a, b))| {
                        if CompiledFilter::cmp_vals(a, b, op) {
                            Some(idx)
                        } else {
                            None
                        }
                    })
                    .collect()
            }
            CompiledFilter::Or(a, b) => {
                let mut left =
                    Self::parse_complex_filter_granule(marks, a, pk_col_defs, table_col_defs);
                let right =
                    Self::parse_complex_filter_granule(marks, b, pk_col_defs, table_col_defs);

                for i in right {
                    if !left.contains(&i) {
                        left.push(i);
                    }
                }

                left
            }
            CompiledFilter::And(a, b) => {
                let mut left =
                    Self::parse_complex_filter_granule(marks, a, pk_col_defs, table_col_defs);
                let right =
                    Self::parse_complex_filter_granule(marks, b, pk_col_defs, table_col_defs);

                left.retain(|idx| right.contains(idx));
                left
            }
            CompiledFilter::Not(inner) => {
                let result =
                    Self::parse_complex_filter_granule(marks, inner, pk_col_defs, table_col_defs);
                (0..marks.len()).filter(|x| !result.contains(x)).collect()
            }
            CompiledFilter::Const(value) => {
                if *value {
                    (0..marks.len()).collect()
                } else {
                    Vec::new()
                }
            }
            CompiledFilter::Column(col_idx) => {
                let left_values = Self::find_values(marks, pk_col_defs, &table_col_defs[*col_idx]);

                left_values
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, &value)| {
                        if let Value::Bool(val) = value
                            && *val
                        {
                            Some(idx)
                        } else {
                            None
                        }
                    })
                    .collect()
            }
        }
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

    // todo: currently, because of most defined types, compiler struggles to vectorize comparison.
    // it's better to split them into (cmp_i32, cmp_u8s, ...) - vectorizable
    // and (cmp_strings, cmp_uuids, ...) - not vectorizable
    fn generate_mask(
        filter: &CompiledFilter,
        granule_bytes: &[Option<Vec<u8>>],
        col_mapping: &[Option<usize>],
        row_count: usize,
    ) -> Vec<bool> {
        match filter {
            CompiledFilter::CompareColumns { .. } => unimplemented!(),

            CompiledFilter::Compare { col_idx, op, value } => {
                if let Some(data_idx) = col_mapping[*col_idx]
                    && let Some(col_data) = &granule_bytes[data_idx]
                {
                    let values =
                        unsafe { rkyv::access_unchecked::<ArchivedVec<ArchivedValue>>(col_data) };
                    values
                        .iter()
                        .map(|row_value| CompiledFilter::cmp_vals(row_value, value, op))
                        .collect()
                } else {
                    vec![false; row_count]
                }
            }
            CompiledFilter::And(left, right) => {
                let left_mask = Self::generate_mask(left, granule_bytes, col_mapping, row_count);
                let right_mask = Self::generate_mask(right, granule_bytes, col_mapping, row_count);

                left_mask
                    .into_iter()
                    .zip(right_mask)
                    .map(|(l, r)| l && r)
                    .collect()
            }
            CompiledFilter::Or(left, right) => {
                let left_mask = Self::generate_mask(left, granule_bytes, col_mapping, row_count);
                let right_mask = Self::generate_mask(right, granule_bytes, col_mapping, row_count);

                left_mask
                    .into_iter()
                    .zip(right_mask)
                    .map(|(l, r)| l || r)
                    .collect()
            }
            CompiledFilter::Not(inner) => {
                let mask = Self::generate_mask(inner, granule_bytes, col_mapping, row_count);

                mask.into_iter().map(|b| !b).collect()
            }
            CompiledFilter::Column(col_idx) => {
                if let Some(data_idx) = col_mapping[*col_idx]
                    && let Some(col_data) = &granule_bytes[data_idx]
                {
                    let values =
                        unsafe { rkyv::access_unchecked::<ArchivedVec<ArchivedValue>>(col_data) };

                    values
                        .iter()
                        .map(|value| {
                            if let ArchivedValue::Bool(value) = value
                                && *value
                            {
                                true
                            } else {
                                false
                            }
                        })
                        .collect()
                } else {
                    vec![false; row_count]
                }
            }
            CompiledFilter::Const(value) => vec![*value; row_count],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::ValueType;

    fn str_col_def(name: &str) -> ColumnDef {
        ColumnDef {
            name: name.to_string(),
            field_type: ValueType::String,
            constraints: Default::default(),
        }
    }

    mod mapping {
        use crate::sql::execution::select::filter::FilterLogic;
        use crate::sql::execution::select::filter::tests::str_col_def;

        #[test]
        fn test_1() {
            let table_col_defs = vec![
                str_col_def("0"),
                str_col_def("1"),
                str_col_def("2"),
                str_col_def("3"),
            ];

            let filter_col_defs = vec![
                str_col_def("0"),
                str_col_def("1"),
                str_col_def("2"),
                str_col_def("3"),
            ];
            let filter_col_defs: Vec<_> = filter_col_defs.iter().collect();

            assert_eq!(
                FilterLogic::create_filter_cols_table_cols_mapping(
                    &filter_col_defs,
                    &table_col_defs
                )
                .unwrap(),
                vec![Some(0), Some(1), Some(2), Some(3)]
            );
        }

        #[test]
        fn test_2() {
            let table_col_defs = vec![
                str_col_def("0"),
                str_col_def("1"),
                str_col_def("2"),
                str_col_def("3"),
            ];

            let filter_col_defs = vec![
                str_col_def("3"),
                str_col_def("0"),
                str_col_def("1"),
                str_col_def("2"),
            ];
            let filter_col_defs: Vec<_> = filter_col_defs.iter().collect();

            assert_eq!(
                FilterLogic::create_filter_cols_table_cols_mapping(
                    &filter_col_defs,
                    &table_col_defs
                )
                .unwrap(),
                vec![Some(1), Some(2), Some(3), Some(0)]
            );
        }

        #[test]
        fn test_3() {
            let table_col_defs = vec![
                str_col_def("0"),
                str_col_def("1"),
                str_col_def("2"),
                str_col_def("3"),
            ];

            let filter_col_defs = vec![str_col_def("3"), str_col_def("1")];
            let filter_col_defs: Vec<_> = filter_col_defs.iter().collect();

            assert_eq!(
                FilterLogic::create_filter_cols_table_cols_mapping(
                    &filter_col_defs,
                    &table_col_defs
                )
                .unwrap(),
                vec![None, Some(1), None, Some(0)]
            );
        }

        #[test]
        fn test_4() {
            let table_col_defs = vec![
                str_col_def("0"),
                str_col_def("1"),
                str_col_def("2"),
                str_col_def("3"),
            ];

            let filter_col_defs = vec![str_col_def("1"), str_col_def("2")];
            let filter_col_defs: Vec<_> = filter_col_defs.iter().collect();

            assert_eq!(
                FilterLogic::create_filter_cols_table_cols_mapping(
                    &filter_col_defs,
                    &table_col_defs
                )
                .unwrap(),
                vec![None, Some(0), Some(1), None]
            );
        }
    }
}
