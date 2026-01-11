mod accumulate_function;
mod filter;
mod scan;
mod strategy;

pub use filter::FilterLogic;
pub use scan::ScanLogic;
pub use strategy::Strategy;

use crate::error::{Error, Result};
use crate::runtime_config::TABLE_DATA;
use crate::sql::CommandRunner;
use crate::sql::execution::select::accumulate_function::{AccumulateFn, CollectFn};
use crate::sql::output_table::OutputTable;
use crate::sql::sql_parser::{Projection, ScanSource};

use crate::engines::{EngineConfig, EngineName};
use crate::storage::value::ArchivedValue;
use crate::storage::{ColumnDef, MarkInfo, PhysicalColumn, TableDef, TablePartInfo};
use rkyv::vec::ArchivedVec;
use sqlparser::ast::Expr;

pub struct Granule {
    pub granule_idx: usize,
    pub mask: Vec<bool>,
}

impl CommandRunner {
    pub fn select(
        table_def: ScanSource,
        projections: Vec<Projection>,
        filter_expr: Option<Expr>,
        order_by: Option<Vec<Vec<Projection>>>,
        limit: Option<usize>,
        offset: usize,
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

        let optimal_strategy = Strategy::new(
            limit,
            offset,
            table_config
                .infos
                .iter()
                .map(|x| x.row_count as usize)
                .sum(),
            order_by.is_some(),
        );

        // filter all marks
        let row_parts_mask =
            FilterLogic::filter_marks(&table_config, filter_expr, &table_def, &optimal_strategy)?;

        // scan all rows
        let proj_len = projections.len();
        let mut output_columns = ScanLogic::get_out_cols(
            &table_def,
            row_parts_mask,
            projections,
            vec![Vec::new(); proj_len],
            &CollectFn::new(),
            table_config.metadata.settings.index_granularity as usize,
            &optimal_strategy,
        )?;

        if let Some(order_by_vec) = order_by {
            for order_by in order_by_vec {
                // todo: if final is specified, use table engine
                output_columns = EngineName::default()
                    .get_engine(EngineConfig::default())
                    .order_columns(
                        output_columns,
                        &order_by,
                        &table_config.metadata.schema.primary_key,
                    )?;
            }
        }

        if let Some(col_length) = output_columns.first().map(|x| x.data.len()) {
            let final_offset = col_length.min(offset);
            for out_col in &mut output_columns {
                out_col.data.drain(0..final_offset);
            }
        }

        if let Some(col_length) = output_columns.first().map(|x| x.data.len())
            && let Some(limit) = limit
        {
            let final_length = col_length.min(limit);
            for out_col in &mut output_columns {
                out_col.data.truncate(final_length);
            }
        }

        Ok(OutputTable::new(output_columns))
    }
}

pub fn create_proj_to_part_cols_mapping(
    projections: &[Projection],
    part_col_defs: &[ColumnDef],
) -> Vec<Option<usize>> {
    let mut mapping = vec![None; projections.len()];

    for (projection_idx, projection) in projections.iter().enumerate() {
        if let Some(col_def) = projection.source.get_col_def()
            && let Some(col_idx) = part_col_defs
                .iter()
                .position(|p_col_def| p_col_def == col_def)
        {
            mapping[projection_idx] = Some(col_idx);
        }
    }

    mapping
}

pub fn get_granule_rows_fallback(
    table_def: &TableDef,
    part_info: &TablePartInfo,
    mark_info: &[MarkInfo],
) -> Result<usize> {
    let Some(first_mark_info) = mark_info.first() else {
        return Err(Error::NoColumnsSpecified);
    };
    let Some(col_def) = part_info.column_defs.first() else {
        return Err(Error::NoColumnsSpecified);
    };
    let mmap = PhysicalColumn::open_as_mmap(&part_info.get_column_path(table_def, col_def))?;
    PhysicalColumn::validate_mmap(&mmap, &col_def.name)?;

    let granule_bytes = TablePartInfo::get_granule_bytes_decompressed(
        &mmap,
        first_mark_info,
        &col_def.constraints.compression_type,
    )?;
    let archived: &ArchivedVec<ArchivedValue> = unsafe { rkyv::access_unchecked(&granule_bytes) };

    Ok(archived.len())
}
