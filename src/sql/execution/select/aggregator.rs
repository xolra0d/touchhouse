// use crate::error::Result;
// use crate::storage::{ToValue, Value};

// pub trait AccumulateFn {
//     fn accumulate(acc: Vec<Vec<Value>>, granule: Vec<Vec<impl ToValue>>)
//     -> Result<Vec<Vec<Value>>>;
// }

// pub struct CollectFn;

// impl AccumulateFn for CollectFn {
//     fn accumulate(
//         mut acc: Vec<Vec<Value>>,
//         granule: Vec<Vec<impl ToValue>>,
//     ) -> Result<Vec<Vec<Value>>> {
//         for (granule_values, acc_values) in granule.into_iter().zip(&mut acc) {
//             let granule_values = granule_values
//                 .into_iter()
//                 .map(|x| x.to_value())
//                 .collect::<Result<Vec<_>>>()?;
//             acc_values.extend(granule_values);
//         }
//         Ok(acc)
//     }
// }

use sqlparser::ast::Expr;

use crate::sql::{AggregateProjection, Projection};

#[derive(Debug, Clone)]
pub struct Aggregator {
    projections: Vec<Projection>,
    aggregate_cols: Vec<AggregateProjection>,
    group_by: Vec<Projection>,
    having: Option<Box<Expr>>,
}

impl Aggregator {
    pub fn new(
        projections: Vec<Projection>,
        aggregate_cols: Vec<AggregateProjection>,
        group_by: Vec<Projection>,
        having: Option<Box<Expr>>,
        // todo: add order of projections,aggregate_cols
    ) -> Self {
        Self {
            projections,
            aggregate_cols,
            group_by,
            having,
        }
    }
}
