use crate::error::{Error, Result};
use crate::runtime_config::TableConfig;
use crate::sql::compiled_filter::{BinOp, CompiledFilter};
use crate::storage::value::ArchivedValue;
use crate::storage::{Column, ColumnDef, Mark, MarkInfo, TableDef, TablePartInfo, Value};
use memmap2::Mmap;
use rayon::prelude::*;
use rkyv::vec::ArchivedVec;
use sqlparser::ast::Expr;
use std::sync::Arc;

pub struct FilterLogic;

impl FilterLogic {
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
    pub fn if_use_compiler_optimization(
        columns_to_filter: &[&ColumnDef],
        pk_col_defs: &[ColumnDef],
    ) -> bool {
        if columns_to_filter
            .iter()
            .all(|col_def| pk_col_defs.contains(col_def))
        {
            true
        } else {
            false
        }
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

    fn load_values<'a>(
        marks: &'a [Mark],
        pk_col_defs: &[ColumnDef],
        col_def: &ColumnDef,
    ) -> Vec<&'a Value> {
        marks
            .iter()
            .map(|mark| {
                let idx = pk_col_defs
                    .iter()
                    .position(|pk_col_def| pk_col_def == col_def);
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
                let values = Self::load_values(marks, pk_col_defs, &table_col_defs[*col_idx]);

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
                let left_values = Self::load_values(marks, pk_col_defs, &table_col_defs[*left_idx]);
                let right_values =
                    Self::load_values(marks, pk_col_defs, &table_col_defs[*right_idx]);

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
                let left_values = Self::load_values(marks, pk_col_defs, &table_col_defs[*col_idx]);

                left_values
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, &value)| {
                        if let Value::Bool(val) = value
                            && !*val
                        {
                            None
                        } else {
                            Some(idx)
                        }
                    })
                    .collect()
            }
        }
    }

    pub fn create_filter_to_table_col_defs_mapping(
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

    pub fn generate_mask(
        data_bytes: &[Option<Vec<u8>>],
        filter: &CompiledFilter,
        col_mapping: &[Option<usize>],
        row_count: usize,
    ) -> Vec<bool> {
        Self::eval_filter_vectorized(filter, data_bytes, col_mapping, row_count)
    }

    // todo: currently, because of mot defined types, compiler struggles to vectorize computation.
    // it's better to split them into (cmp_i32, cmp_u8s, ...) - vectorizable
    // and (cmp_strings, cmp_uuids, ...) - not vectorizable
    fn eval_filter_vectorized(
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
                let left_mask =
                    Self::eval_filter_vectorized(left, granule_bytes, col_mapping, row_count);
                let right_mask =
                    Self::eval_filter_vectorized(right, granule_bytes, col_mapping, row_count);

                left_mask
                    .into_iter()
                    .zip(right_mask)
                    .map(|(l, r)| l && r)
                    .collect()
            }
            CompiledFilter::Or(left, right) => {
                let left_mask =
                    Self::eval_filter_vectorized(left, granule_bytes, col_mapping, row_count);
                let right_mask =
                    Self::eval_filter_vectorized(right, granule_bytes, col_mapping, row_count);

                left_mask
                    .into_iter()
                    .zip(right_mask)
                    .map(|(l, r)| l || r)
                    .collect()
            }
            CompiledFilter::Not(inner) => {
                let mask =
                    Self::eval_filter_vectorized(inner, granule_bytes, col_mapping, row_count);

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
                            let ArchivedValue::Bool(val) = value else {
                                unreachable!("filtered and mapped to false in compiled filter")
                            };
                            *val
                        })
                        .collect()
                } else {
                    vec![false; row_count]
                }
            }
            CompiledFilter::Const(value) => vec![*value; row_count],
        }
    }

    pub fn filter_marks<'a>(
        table_config: &'a TableConfig,
        filter_expr: Option<Expr>,
        table_def: &TableDef,
    ) -> Result<Vec<(&'a TablePartInfo, Vec<(usize, Vec<bool>)>)>> {
        if let Some(filter) = filter_expr {
            let compiled_filter =
                CompiledFilter::try_compile(filter, &table_config.metadata.schema.columns)?;

            let columns_to_filter = FilterLogic::get_filter_columns(
                &compiled_filter,
                &table_config.metadata.schema.columns,
            );
            let col_def_mappings = FilterLogic::create_filter_to_table_col_defs_mapping(
                &columns_to_filter,
                &table_config.metadata.schema.columns,
            )?;

            let use_filter_optimization = FilterLogic::if_use_compiler_optimization(
                &columns_to_filter,
                &table_config.metadata.schema.primary_key,
            );

            let mut sorted_infos: Vec<_> = table_config.infos.iter().collect();

            // sort DESC, so we will first parse parts with bigger num of rows, allowing more parallelism
            sorted_infos.sort_unstable_by(|a, b| b.row_count.cmp(&a.row_count));

            let mut parts_granule_marks = vec![Vec::new(); sorted_infos.len()];

            for (idx, &part_info) in sorted_infos.iter().enumerate() {
                let granules_to_scan = FilterLogic::filter_granules(
                    &compiled_filter,
                    &part_info.marks,
                    use_filter_optimization,
                    &table_config.metadata.schema.primary_key,
                    &table_config.metadata.schema.columns,
                );
                parts_granule_marks[idx] = granules_to_scan;
            }

            let mut total_parts_mask: Vec<(&TablePartInfo, Vec<(usize, Vec<bool>)>)> =
                Vec::with_capacity(sorted_infos.len());

            for (part_idx, marks_to_scan) in parts_granule_marks.iter().enumerate() {
                let part_info = sorted_infos[part_idx];
                let mut mmaps: Vec<Option<Mmap>> = Vec::with_capacity(columns_to_filter.len());
                let mut filter_to_part_col_idx: Vec<Option<usize>> =
                    vec![None; columns_to_filter.len()];

                for (col_idx, col_def) in columns_to_filter.iter().enumerate() {
                    if let Some(part_col_idx) =
                        part_info.column_defs.iter().position(|c| c == *col_def)
                    {
                        let mmap =
                            Column::open_as_mmap(&part_info.get_column_path(&table_def, col_def))?;
                        Column::validate_mmap(&mmap, &col_def.name)?;
                        mmaps.push(Some(mmap));
                        filter_to_part_col_idx[col_idx] = Some(part_col_idx);
                    } else {
                        mmaps.push(None);
                    }
                }
                let mmaps = Arc::new(mmaps);
                let filter_to_part_col_idx = Arc::new(filter_to_part_col_idx);

                // for each part we store idx of granule and mask for each row
                let part_mask: Result<Vec<Vec<(usize, Vec<bool>)>>> = marks_to_scan
                    .into_iter()
                    .enumerate()
                    .collect::<Vec<_>>()
                    .chunks(5) // todo: move constant to cfg
                    .map(|chunk_granule_marks| {
                        let mut data_bytes = vec![None; columns_to_filter.len()]; // todo: instead of new, have an average number of bytes for each column type decompressed
                        let mut mask: Vec<(usize, Vec<bool>)> =
                            Vec::with_capacity(chunk_granule_marks.len());

                        for &(granule_idx, granule_marks) in chunk_granule_marks {
                            let mut row_count = None;

                            for (file_idx, mmap) in mmaps.iter().enumerate() {
                                if let Some(mmap) = &mmap {
                                    let part_col_idx = filter_to_part_col_idx[file_idx]
                                        .expect("mmap exists but mapping doesn't");
                                    let granule_bytes =
                                        TablePartInfo::get_granule_bytes_decompressed(
                                            &mmap,
                                            &granule_marks[part_col_idx],
                                            &columns_to_filter[file_idx]
                                                .constraints
                                                .compression_type,
                                        )?;
                                    if row_count.is_none() {
                                        // access_unchecked is ~200ps
                                        let archived: &ArchivedVec<ArchivedValue> =
                                            unsafe { rkyv::access_unchecked(&granule_bytes) };
                                        row_count = Some(archived.len());
                                    }
                                    data_bytes[file_idx] = Some(granule_bytes);
                                }
                            }

                            let Some(row_count) = row_count else {
                                // part is missing all filter columns...
                                todo!()
                            };

                            let mask_part = FilterLogic::generate_mask(
                                &data_bytes,
                                &compiled_filter,
                                &col_def_mappings,
                                row_count,
                            );

                            mask.push((granule_idx, mask_part));
                        }

                        Ok(mask)
                    })
                    .collect();
                let part_mask = part_mask?.into_iter().flatten().collect::<Vec<_>>();
                total_parts_mask.push((&sorted_infos[part_idx], part_mask));
            }

            Ok(total_parts_mask)
        } else {
            let mut total_parts_mask: Vec<(&TablePartInfo, Vec<(usize, Vec<bool>)>)> =
                Vec::with_capacity(table_config.infos.len());

            for (part_idx, part_info) in table_config.infos.iter().enumerate() {
                total_parts_mask.push((&part_info, Vec::with_capacity(part_info.marks.len())));
                let last_mark = &part_info.marks[part_info.marks.len() - 1];

                let col_def = part_info
                    .column_defs
                    .first()
                    .ok_or(Error::PartDoesNotHaveColumns(part_info.name.to_string()))?;

                for (mark_idx, mark) in part_info.marks.iter().enumerate() {
                    if mark == last_mark {
                        let mmap =
                            Column::open_as_mmap(&part_info.get_column_path(&table_def, col_def))?;

                        Column::validate_mmap(&mmap, &col_def.name)?;

                        let granule_bytes = TablePartInfo::get_granule_bytes_decompressed(
                            &mmap,
                            mark.info
                                .first()
                                .ok_or(Error::PartDoesNotHaveColumns(part_info.name.to_string()))?,
                            &col_def.constraints.compression_type,
                        )?;
                        let archived: &ArchivedVec<ArchivedValue> =
                            unsafe { rkyv::access_unchecked(&granule_bytes) };

                        total_parts_mask[part_idx]
                            .1
                            .push((mark_idx, vec![true; archived.len()]));

                        break;
                    }
                    total_parts_mask[part_idx].1.push((
                        mark_idx,
                        vec![true; table_config.metadata.settings.index_granularity as usize],
                    ));
                }
            }

            Ok(total_parts_mask)
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
                FilterLogic::create_filter_to_table_col_defs_mapping(
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
                FilterLogic::create_filter_to_table_col_defs_mapping(
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
                FilterLogic::create_filter_to_table_col_defs_mapping(
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
                FilterLogic::create_filter_to_table_col_defs_mapping(
                    &filter_col_defs,
                    &table_col_defs
                )
                .unwrap(),
                vec![None, Some(0), Some(1), None]
            );
        }
    }
}
