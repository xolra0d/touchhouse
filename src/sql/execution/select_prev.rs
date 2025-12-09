use crate::engines::{EngineConfig, EngineName};
use crate::error::{Error, Result};
use crate::runtime_config::TABLE_DATA;
use crate::sql::CommandRunner;
use crate::sql::compiled_filter::{BinOp, CompiledFilter};
use crate::sql::sql_parser::{Projection, ProjectionValue, ScanSource};
use crate::storage::value::ArchivedValue;
use crate::storage::{Column, ColumnDef, Mark, TableDef, TablePartInfo, Value};

use crate::sql::output_table::{OutputColumn, OutputTable};
use rayon::prelude::*;
use rkyv::vec::ArchivedVec;
use sqlparser::ast::Expr;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

thread_local! {
    static LOCAL_BUFFER: RefCell<Vec<Vec<Value>>> = const { RefCell::new(Vec::new()) };
}

struct ScanConfig {
    result: Arc<RwLock<Vec<OutputColumn>>>,
    infos: Vec<TablePartInfo>,
    use_filter_optimization: bool,
    compiled_filter: Option<CompiledFilter>,
    table_col_defs: Vec<ColumnDef>,
    pk_col_defs: Vec<ColumnDef>,
    result_col_defs: Vec<ColumnDef>,
    index_granularity: usize,
    table_def: TableDef,
    limit: Option<u64>,
    offset: u64,
}

impl CommandRunner {
    /// Executes SELECT operation by scanning all table parts.
    ///
    /// Reads all table parts, optionally filters and orders data.
    ///
    /// Returns:
    ///   * Ok: `OutputTable` with success status
    ///   * Error: `TableNotFound`, `CouldNotReadData` or `Internal` on failure
    pub fn select(
        table_def: ScanSource,
        columns_to_read: Vec<Projection>,
        filter: Option<Box<Expr>>,
        order_by: Option<&Vec<Vec<Projection>>>,
        limit: Option<u64>,
        offset: u64,
    ) -> Result<OutputTable> {
        let table_def = match table_def {
            ScanSource::Table(table_def) => table_def,
            ScanSource::Subquery(_) => {
                return Err(Error::Internal(
                    "Subqueries should've been removed during optimization. Cannot proceed"
                        .to_string(),
                ));
            }
        };
        let Some(table_config) = TABLE_DATA.get(&table_def) else {
            return Err(Error::TableNotFound);
        };
        let index_granularity = table_config.metadata.settings.index_granularity as usize;

        let expected_rows = Self::estimate_avg_rows(limit, index_granularity);

        let mut scalar_projections: Vec<(usize, Projection)> = Vec::new();
        let disk_columns: Vec<Projection> = columns_to_read
            .iter()
            .enumerate()
            .filter_map(|(idx, proj)| match &proj.source {
                ProjectionValue::ColumnDef(_) => Some(proj.clone()),
                ProjectionValue::Value(_) => {
                    scalar_projections.push((idx, proj.clone()));
                    None
                }
            })
            .collect();

        let mut result: Vec<OutputColumn> = Vec::new();
        Self::add_projections(&mut result, disk_columns.clone(), expected_rows);

        let mut compiled_filter = None;
        let mut use_filter_optimization = false;

        if let Some(filter) = filter {
            let filter = CompiledFilter::compile(*filter, &table_config.metadata.schema.columns)?;

            let mut columns_to_filter = Vec::new();

            filter.get_column_defs(&mut columns_to_filter);
            compiled_filter = Some(filter);

            let columns_to_filter: Vec<_> = columns_to_filter
                .into_iter()
                .map(|col_idx| table_config.metadata.schema.columns[col_idx].clone())
                .collect();

            // TODO: allow partial cmp, e.g., part is in PK, part is not.
            if columns_to_filter
                .iter()
                .all(|col_def| table_config.metadata.schema.primary_key.contains(col_def))
            {
                use_filter_optimization = true;
            }
            Self::add_column_defs(&mut result, columns_to_filter, expected_rows);
        }

        if let Some(order_by) = &order_by {
            Self::add_projections(
                &mut result,
                order_by.iter().flatten().cloned().collect(),
                expected_rows,
            );
        }

        let result_col_defs: Vec<_> = result.iter().map(|col| col.column_def.clone()).collect();
        let result = Arc::new(RwLock::new(result));

        let total_size = Self::scan_table_parts(ScanConfig {
            result: Arc::clone(&result),
            infos: table_config.infos.clone(),
            use_filter_optimization,
            compiled_filter,
            table_col_defs: table_config.metadata.schema.columns.clone(),
            pk_col_defs: table_config.metadata.schema.primary_key.clone(),
            result_col_defs,
            index_granularity,
            table_def: table_def.clone(),
            limit,
            offset,
        })?;

        let result = Arc::try_unwrap(result)
            .map_err(|_| {
                Error::Internal("Some threads are leaked and have not finished.".to_string())
            })?
            .into_inner()
            .map_err(|error| Error::Internal(format!("Failed to get inner Arc data: {error}")))?;

        let result = Self::apply_post_processing(
            result,
            order_by,
            &table_config.metadata.settings.engine,
            &table_config.metadata.schema.primary_key,
            &disk_columns,
            &columns_to_read,
            scalar_projections,
            limit,
            offset,
            total_size
        )?;

        Ok(OutputTable::new(result))
    }

    fn add_column_defs(
        result: &mut Vec<OutputColumn>,
        column_defs: Vec<ColumnDef>,
        expected_rows: usize,
    ) {
        for column_def in column_defs {
            if !result.iter().any(|col| col.column_def == column_def) {
                result.push(OutputColumn {
                    alias: None,
                    column_def,
                    data: Vec::with_capacity(expected_rows),
                });
            }
        }
    }

    fn add_projections(
        result: &mut Vec<OutputColumn>,
        projections: Vec<Projection>,
        expected_rows: usize,
    ) {
        for projection in projections {
            if let ProjectionValue::ColumnDef(column_def) = projection.source {
                // Only add columns (not scalar values) for disk reading
                if !result.iter().any(|col| col.column_def == column_def) {
                    result.push(OutputColumn {
                        alias: projection.alias,
                        column_def,
                        data: Vec::with_capacity(expected_rows),
                    });
                }
            }
        }
    }

    fn scan_table_parts(config: ScanConfig) -> Result<usize> {
        let ScanConfig {
            result,
            infos,
            use_filter_optimization,
            compiled_filter,
            table_col_defs,
            pk_col_defs,
            result_col_defs,
            index_granularity,
            table_def,
            limit,
            offset,
        } = config;

        // if compiled_filter.is_none() &&

        let table_col_defs = &table_col_defs;
        let pk_col_defs = &pk_col_defs;
        let table_def = &table_def;
        let should_stop = Arc::new(AtomicBool::new(false));
        let result_col_defs = Arc::new(result_col_defs);
        let total_len = Arc::new(AtomicUsize::new(0));

        for part_info in &infos {
            if should_stop.load(Ordering::Relaxed) {
                break;
            }

            let mut file_mmaps = Vec::with_capacity(part_info.column_defs.len());

            // todo: not open for not needing..
            for col_def in &part_info.column_defs {
                let mmap = Column::open_as_mmap(&part_info.get_column_path(table_def, col_def))?;
                Column::validate_mmap(&mmap, &col_def.name)?;

                file_mmaps.push(mmap);
            }

            let file_mmaps = Arc::new(file_mmaps);

            let marks_to_scan: Vec<_> =
                if use_filter_optimization && let Some(compiled_filter) = &compiled_filter {
                    let marks_indexes = Self::parse_complex_filter_granule(
                        &part_info.marks,
                        compiled_filter,
                        pk_col_defs,
                        table_col_defs,
                    );
                    marks_indexes
                        .into_iter()
                        .map(|mark_idx| &part_info.marks[mark_idx].info)
                        .collect()
                } else {
                    part_info.marks.iter().map(|mark| &mark.info).collect()
                };
            if should_stop.load(Ordering::Relaxed) {
                break;
            }

            marks_to_scan
                .par_chunks(10)
                .try_for_each(|chunk_granule_marks| {
                    LOCAL_BUFFER.with(|buffer| {
                        let mut buffer = buffer.borrow_mut();
                        *buffer = vec![Vec::with_capacity(index_granularity); result_col_defs.len()];
                    });

                    if should_stop.load(Ordering::Relaxed) {
                        return Ok(());
                    }

                    let mut granule_buffer = GranuleBuffer {
                        data_bytes: vec![None; result_col_defs.len()],
                        mask: Vec::with_capacity(index_granularity),
                    };

                    for &granule_marks in chunk_granule_marks {
                        if should_stop.load(Ordering::Relaxed) {
                            return Ok(());
                        }

                        let mut row_count = None;

                        for (file_and_col_idx, file_mmap) in file_mmaps.iter().enumerate()
                        {
                            let result_idx = result_col_defs.iter().position(|col_def| {
                                *col_def == part_info.column_defs[file_and_col_idx]
                            });
                            if let Some(result_idx) = result_idx {
                                let granule_bytes = TablePartInfo::get_granule_bytes_decompressed(
                                    file_mmap,
                                    &granule_marks[file_and_col_idx],
                                    &result_col_defs[result_idx].constraints.compression_type,
                                )?;
                                if row_count.is_none() {
                                    row_count = Some(unsafe {
                                        rkyv::access_unchecked::<ArchivedVec<ArchivedValue>>(
                                            &granule_bytes,
                                        )
                                        .len()
                                    });
                                }
                                granule_buffer.data_bytes[result_idx] = Some(granule_bytes);
                            }
                        }

                        if let Some(row_count) = row_count {
                            if let Some(compiled_filter) = &compiled_filter {
                                granule_buffer.fill_mask(
                                    compiled_filter,
                                    &result_col_defs,
                                    table_col_defs,
                                    row_count,
                                )?;
                            }

                            let mut archived_values = Vec::with_capacity(granule_buffer.data_bytes.len());

                            for col in &granule_buffer.data_bytes {
                                if let Some(col_bytes) = col {
                                    let values = unsafe {
                                        rkyv::access_unchecked::<ArchivedVec<ArchivedValue>>(
                                            col_bytes,
                                        )
                                    };
                                    archived_values.push(Some(values));
                                } else {
                                    archived_values.push(None);
                                }
                            }
                            let allowed_count = granule_buffer.mask.iter().filter(|x| **x).count();
                            if should_stop.load(Ordering::Relaxed) {
                                return Ok(());
                            }

                            for (idx, col_values) in archived_values.iter().enumerate() {
                                let col_values = if let Some(col_values_) = col_values {
                                    let mut res = Vec::with_capacity(col_values_.len());
                                    for (val_idx, col_value) in col_values_.iter().enumerate() {
                                        if granule_buffer.mask.is_empty()
                                            || granule_buffer.mask[val_idx]
                                        {
                                            let col_values =
                                                rkyv::deserialize::<Value, rkyv::rancor::Error>(
                                                    col_value,
                                                )
                                                .map_err(|error| {
                                                    Error::CouldNotReadData(format!("Could not deserialize value in column ({}): {error}", result_col_defs[idx].name))
                                                })?;
                                            res.push(col_values);
                                        }
                                    }

                                    res
                                } else {
                                    vec![Value::Null; allowed_count]
                                };
                                LOCAL_BUFFER.with(|buffer| {
                                    let mut buffer = buffer.borrow_mut();
                                    buffer[idx].extend(col_values);
                                });
                            }

                            total_len.fetch_add(allowed_count, Ordering::Relaxed);

                            if let Some(limit) = limit && total_len.load(Ordering::Relaxed) as u64 >= limit.saturating_add(offset) {
                                should_stop.store(true, Ordering::Relaxed);
                                return Ok(());
                            }

                            for archived_vec in &mut granule_buffer.data_bytes {
                                *archived_vec = None;
                            }
                            granule_buffer.mask.clear();
                        }
                    }
                    let mut guard = result.write().map_err(|error| Error::Internal(format!("RwLock poisoning while reading: {error}")))?;
                    for (idx, col) in LOCAL_BUFFER.take().into_iter().enumerate() {
                        guard[idx].data.extend(col);
                    }

                    Ok(())
                })?;
        }

        Ok(total_len.load(Ordering::Relaxed))
    }

    fn apply_post_processing(
        mut result: Vec<OutputColumn>,
        order_by: Option<&Vec<Vec<Projection>>>,
        engine_name: &EngineName,
        pk_col_defs: &[ColumnDef],
        disk_columns: &[Projection],
        _columns_to_read: &[Projection],
        scalar_projections: Vec<(usize, Projection)>,
        limit: Option<u64>,
        offset: u64,
        mut total_size: usize
    ) -> Result<Vec<OutputColumn>> {
        if let Some(sort_by) = &order_by {
            let engine = engine_name.get_engine(EngineConfig::default());
            for order_by in *sort_by {
                result = engine.order_columns(result, order_by, pk_col_defs)?;
            }
        }

        result.retain(|col| {
            disk_columns.iter().any(|proj| {
                if let ProjectionValue::ColumnDef(col_def) = &proj.source {
                    col.column_def == *col_def
                } else {
                    false
                }
            })
        });

        let offset = offset.min(total_size as u64) as usize;
        total_size -= offset;
        for column in &mut result {
            column.data.drain(0..offset);
        }

        if let Some(limit) = limit {
            let limit = limit.min(total_size as u64);
            total_size = total_size.saturating_sub(limit as usize);
            for column in &mut result {
                column.data.truncate(limit as usize);
            }
        }

        // Add scalar columns at their correct positions
        for (position, projection) in scalar_projections {
            if let ProjectionValue::Value(value) = &projection.source {
                let column = OutputColumn {
                    alias: projection.alias.clone(),
                    column_def: ColumnDef {
                        name: projection
                            .alias
                            .clone()
                            .unwrap_or_else(|| format!("{:?}", value)),
                        field_type: value.get_type(),
                        constraints: Default::default(),
                    },
                    data: vec![value.clone(); total_size],
                };
                result.insert(position, column);
            }
        }

        Ok(result)
    }
}