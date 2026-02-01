mod aggregator;
mod strategy;

use aggregator::Aggregator;
use strategy::Strategy;

use crate::engines::{EngineConfig, EngineName};
use crate::error::Result;
use crate::sql::{AggregateProjection, Projection, ScanSource, SelectNode};
use crate::sql::{CommandRunner, CompiledFilter};
use crate::storage::{NativeStorage, OutputColumn, StorageRead, VirtualStorage};

use sqlparser::ast::Expr;

struct QueryParams {
    projections: Vec<Projection>,
    filter: Option<Box<Expr>>,
    aggregate_cols: Vec<AggregateProjection>,
    group_by: Vec<Projection>,
    having: Option<Box<Expr>>,
    order_by: Option<Vec<Vec<Projection>>>,
    limit: Option<usize>,
    offset: usize,
}

impl CommandRunner {
    pub fn select(select: SelectNode) -> Result<Vec<OutputColumn>> {
        let SelectNode {
            scan_source,
            columns,
            filter,
            aggregate_cols,
            group_by,
            having,
            order_by,
            limit,
            offset,
        } = select;

        let scan_source = match scan_source {
            ScanSource::Table(table_def) => ScanSource::Table(table_def),
            ScanSource::Subquery(select_node) => {
                let output_table_inner = Self::select(*select_node)?;

                ScanSource::Subquery(Box::new(output_table_inner))
            }
        };

        let params = QueryParams {
            projections: columns,
            filter,
            aggregate_cols,
            group_by,
            having,
            order_by,
            limit,
            offset,
        };

        match scan_source {
            ScanSource::Table(table_def) => {
                let storage = NativeStorage::try_from(&table_def)?;
                Self::scan_from_storage(storage, params)
            }
            ScanSource::Subquery(virtual_storage) => {
                let storage = VirtualStorage::from(*virtual_storage);
                Self::scan_from_storage(storage, params)
            }
        }
    }

    fn scan_from_storage<S: StorageRead>(
        mut storage: S,
        params: QueryParams,
    ) -> Result<Vec<OutputColumn>> {
        let QueryParams {
            projections,
            filter,
            aggregate_cols,
            group_by,
            having,
            order_by,
            limit,
            offset,
        } = params;

        let total_storage_rows = storage.get_total_rows();
        let strategy = Strategy::new(
            limit,
            offset,
            total_storage_rows,
            order_by.is_some() || !group_by.is_empty(),
        );

        let filter = filter
            .map(|x| CompiledFilter::compile(*x, &storage.get_schema().columns))
            .transpose()?;

        let mut aggregator = Aggregator::new(projections, aggregate_cols, group_by, having);

        while strategy.should_read_next_chunk() && storage.load_next_chunk()?.is_some() {
            let projs_to_read = aggregator.get_projs_to_read();
            let mut granule_buffer = Vec::with_capacity(projs_to_read.len());

            for projection in projs_to_read {
                let col_values = storage.access_chunk_column(projection)?;
                granule_buffer.push(col_values);
            }

            if let Some(filter) = &filter {
                filter.filter_granule(&mut granule_buffer)?;
            }

            let rows_count = aggregator.append_chunk(granule_buffer)?;
            strategy.set_lines_read(rows_count);
        }

        let mut output_columns = aggregator.finalize();

        if let Some(order_by_vec) = order_by {
            for order_by in order_by_vec {
                // todo: if final is specified, use table engine
                output_columns = EngineName::default()
                    .get_engine(EngineConfig::default())
                    .order_columns(output_columns, &order_by, &storage.get_schema().primary_key)?;
            }
        }

        if let Some(col_length) = output_columns.first().map(|x| x.data.len()) {
            let final_offset = col_length.min(offset);
            for out_col in &mut output_columns {
                let _ = out_col.data.drain(0..final_offset);
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

        Ok(output_columns)
    }
}
