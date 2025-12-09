mod accumulate_function;
mod filter;
mod scan;

use accumulate_function::AccumulateFunction;
use filter::FilterLogic;
use scan::ScanLogic;

use crate::error::{Error, Result};
use crate::runtime_config::TABLE_DATA;
use crate::sql::CommandRunner;
use crate::sql::output_table::OutputTable;
use crate::sql::sql_parser::{Projection, ScanSource};

use crate::engines::EngineName;
use sqlparser::ast::Expr;

impl CommandRunner {
    pub fn select(
        table_def: ScanSource,
        projections: Vec<Projection>,
        filter_expr: Option<Expr>,
        order_by_vec: Option<Vec<Vec<Projection>>>,
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



        // filter all rows
        let row_parts_mask = FilterLogic::filter_marks(&table_config, filter_expr, &table_def)?;

        // scan all rows
        let proj_len = projections.len();
        let mut output_columns = ScanLogic::get_archived_values(
            &table_def,
            row_parts_mask,
            projections,
            vec![Vec::new(); proj_len],
            AccumulateFunction::filter_fn,
            table_config.metadata.settings.index_granularity as usize,
        )?;

        if let Some(order_by_vec) = order_by_vec {
            for order_by in order_by_vec {
                // todo: if final is specified, use table engine
                output_columns = EngineName::MergeTree
                    .get_engine(Default::default())
                    .order_columns(
                        output_columns,
                        &order_by,
                        &table_config.metadata.schema.primary_key,
                    )?
            }
        }

        for out_col in &mut output_columns {
            out_col.data.drain(0..(offset as usize));
        }

        if let Some(col_length) = output_columns.first().map(|x| x.data.len()) {
            let final_length = col_length.saturating_sub(limit.unwrap() as usize);
            for out_col in &mut output_columns {
                out_col.data.truncate(final_length);
            }
        }

        Ok(OutputTable::new(output_columns))
    }
}
