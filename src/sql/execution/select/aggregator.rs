use std::{collections::HashMap, f64};

use sqlparser::ast::Expr;

use crate::{
    error::{Error, Result},
    sql::{AggregateProjection, Projection, ProjectionValue, sql_parser::AggregateFunction},
    storage::{OutputColumn, ToValue, Value},
};

#[derive(Debug, Clone)]
enum RawData {
    Sum(Option<f64>),
    Min(Option<f64>),
    Max(Option<f64>),
    Count(usize),
    Avg { sum: f64, count: usize },
    Values(Vec<Value>),
}

impl RawData {
    fn add_value(&mut self, val: impl ToValue) -> Result<()> {
        match self {
            RawData::Sum(sum) => {
                let v = val.as_f64().ok_or(Error::InvalidColumnsSpecified)?;
                *sum = Some(sum.map_or(v, |m| m + v));
            }
            RawData::Min(min) => {
                let v = val.as_f64().ok_or(Error::InvalidColumnsSpecified)?;
                *min = Some(min.map_or(v, |m| m.min(v)));
            }
            RawData::Max(max) => {
                let v = val.as_f64().ok_or(Error::InvalidColumnsSpecified)?;
                *max = Some(max.map_or(v, |m| m.max(v)));
            }
            RawData::Count(count) => {
                *count += 1;
            }
            RawData::Avg { sum, count } => {
                *count += 1;
                *sum += val.as_f64().ok_or(Error::InvalidColumnsSpecified)?;
            }
            RawData::Values(values) => {
                values.push(val.to_value()?);
            }
        }

        Ok(())
    }

    fn get_fn_name(&self) -> Option<&str> {
        match self {
            Self::Sum(_) => Some("sum"),
            Self::Min(_) => Some("min"),
            Self::Max(_) => Some("max"),
            Self::Count(_) => Some("count"),
            Self::Avg { .. } => Some("avg"),
            Self::Values(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
struct RawColumn {
    alias: Option<String>,
    proj: Projection,
    data: RawData,
}

impl RawColumn {
    fn finalize(self) -> OutputColumn {
        let RawColumn {
            alias,
            proj: mut inner_proj,
            data,
        } = self;

        let proj = match &data {
            RawData::Sum(_) => Projection {
                alias,
                source: ProjectionValue::Value(Value::String(format!("sum({inner_proj})"))),
            },
            RawData::Min(_) => Projection {
                alias,
                source: ProjectionValue::Value(Value::String(format!("min({inner_proj})"))),
            },
            RawData::Max(_) => Projection {
                alias,
                source: ProjectionValue::Value(Value::String(format!("max({inner_proj})"))),
            },
            RawData::Count(_) => Projection {
                alias,
                source: ProjectionValue::Value(Value::String(format!("count({inner_proj})"))),
            },
            RawData::Avg { .. } => Projection {
                alias,
                source: ProjectionValue::Value(Value::String(format!("avg({inner_proj})"))),
            },
            RawData::Values(_) => {
                inner_proj.alias = alias;
                inner_proj
            }
        };

        let data = match data {
            RawData::Sum(v) | RawData::Min(v) | RawData::Max(v) => {
                if let Some(v) = v {
                    vec![Value::F64(v)]
                } else {
                    vec![Value::Null]
                }
            }
            RawData::Count(count) => vec![Value::UInt64(count as u64)],
            RawData::Avg { sum, count } => {
                if count == 0 {
                    vec![Value::Null]
                } else {
                    vec![Value::F64(sum / (count as f64))]
                }
            }
            RawData::Values(values) => values,
        };

        OutputColumn { proj, data }
    }

    fn data_as_empty_vec(self) -> Self {
        let RawColumn {
            alias,
            proj: inner_proj,
            ..
        } = self;

        Self {
            alias,
            proj: inner_proj,
            data: RawData::Values(Vec::new()),
        }
    }
}

#[derive(Debug, Clone)]
enum AggregatorFormat {
    Simple(Vec<RawColumn>),
    Aggregate {
        group_by_cols: Vec<RawColumn>,
        aggregate_cols: Vec<RawColumn>,
        groups: HashMap<Vec<Value>, Vec<RawData>>,
    },
}

impl AggregatorFormat {
    fn get_projs_inside(&self) -> Vec<&Projection> {
        match self {
            Self::Simple(cols) => cols.iter().map(|x| &x.proj).collect(),
            Self::Aggregate {
                group_by_cols,
                aggregate_cols,
                ..
            } => group_by_cols
                .iter()
                .chain(aggregate_cols.iter())
                .map(|x| &x.proj)
                .collect(),
        }
    }

    fn finalize(self) -> Vec<OutputColumn> {
        match self {
            Self::Simple(cols) => cols.into_iter().map(RawColumn::finalize).collect(),
            Self::Aggregate {
                mut group_by_cols,
                aggregate_cols,
                groups,
            } => {
                if groups.is_empty() {
                    return group_by_cols
                        .into_iter()
                        .chain(aggregate_cols)
                        .map(|mut c| {
                            c.data = RawData::Values(vec![]);
                            c.finalize()
                        })
                        .collect();
                }

                let (key_rows, value_rows): (Vec<Vec<Value>>, Vec<Vec<RawData>>) =
                    groups.into_iter().unzip();

                for key_row in key_rows {
                    for (val, col) in key_row.into_iter().zip(group_by_cols.iter_mut()) {
                        col.data.add_value(val).unwrap();
                    }
                }

                let aggregate_cols: Vec<_> = aggregate_cols
                    .into_iter()
                    .map(|x| {
                        if let Some(fn_name) = x.data.get_fn_name().map(ToString::to_string) {
                            let RawColumn { alias, proj, data } = x;
                            let Projection {
                                alias: alias_proj,
                                source,
                            } = proj;
                            let source = source.add_aggr_fn_around(&fn_name);

                            RawColumn {
                                alias,
                                proj: Projection {
                                    alias: alias_proj,
                                    source,
                                },
                                data,
                            }
                        } else {
                            x
                        }
                    })
                    .collect();

                let mut aggregate_cols: Vec<_> = aggregate_cols
                    .into_iter()
                    .map(RawColumn::data_as_empty_vec)
                    .collect();

                for value_row in value_rows {
                    for (val, col) in value_row.into_iter().zip(aggregate_cols.iter_mut()) {
                        let val = match val {
                            RawData::Sum(v) | RawData::Min(v) | RawData::Max(v) => {
                                if let Some(v) = v {
                                    Value::F64(v)
                                } else {
                                    Value::Null
                                }
                            }
                            RawData::Count(c) => Value::UInt64(c as u64),
                            RawData::Avg { sum, count } => {
                                if count == 0 {
                                    Value::Null
                                } else {
                                    Value::F64(sum / count as f64)
                                }
                            }
                            RawData::Values(_) => unreachable!(),
                        };

                        col.data.add_value(val).unwrap();
                    }
                }

                group_by_cols
                    .into_iter()
                    .chain(aggregate_cols)
                    .map(RawColumn::finalize)
                    .collect()
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Aggregator {
    format: AggregatorFormat,
    rows: usize,
}

impl Aggregator {
    pub fn new(
        projections: Vec<Projection>,
        aggregate_cols: Vec<AggregateProjection>,
        group_by: Vec<Projection>,
        _having: Option<Box<Expr>>,
    ) -> Self {
        if group_by.is_empty() {
            let columns = if projections.is_empty() {
                aggregate_cols
                    .into_iter()
                    .map(|x| create_aggregate_column(x.alias, x.source))
                    .collect()
            } else {
                let mut columns = Vec::with_capacity(projections.len());

                for projection in projections {
                    columns.push(RawColumn {
                        alias: None,
                        proj: projection,
                        data: RawData::Values(Vec::new()),
                    });
                }

                columns
            };

            let format = AggregatorFormat::Simple(columns);
            Aggregator { format, rows: 0 }
        } else {
            let mut keys = Vec::with_capacity(group_by.len());
            for projection in group_by {
                keys.push(RawColumn {
                    alias: None,
                    proj: projection,
                    data: RawData::Values(Vec::new()),
                });
            }

            let values = aggregate_cols
                .into_iter()
                .map(|x| create_aggregate_column(x.alias, x.source))
                .collect();

            let format = AggregatorFormat::Aggregate {
                group_by_cols: keys,
                aggregate_cols: values,
                groups: HashMap::new(),
            };
            Aggregator { format, rows: 0 }
        }
    }

    pub fn get_projs_to_read(&self) -> Vec<&Projection> {
        self.format.get_projs_inside()
    }

    pub fn append_chunk(&mut self, chunk: Vec<Vec<impl ToValue>>) -> Result<usize> {
        let num_rows = chunk.first().map_or(0, Vec::len);

        if num_rows == 0 {
            return Ok(0);
        }

        self.rows += num_rows;

        match &mut self.format {
            AggregatorFormat::Simple(columns) => {
                for (col_data, raw_col) in chunk.into_iter().zip(columns.iter_mut()) {
                    for val in col_data {
                        raw_col.data.add_value(val)?;
                    }
                }
            }
            AggregatorFormat::Aggregate {
                group_by_cols,
                aggregate_cols,
                groups,
            } => {
                let num_group_by_projs = group_by_cols.len();
                for row_idx in 0..num_rows {
                    let key = chunk
                        .iter()
                        .take(num_group_by_projs)
                        .map(|col| col[row_idx].clone().to_value())
                        .collect::<Result<Vec<_>>>()?;

                    let group_aggregates = groups.entry(key).or_insert_with(|| {
                        aggregate_cols
                            .iter()
                            .map(|col| col.data.clone())
                            .collect::<Vec<RawData>>()
                    });

                    for (agg_idx, _) in aggregate_cols.iter().enumerate() {
                        let chunk_col_idx = num_group_by_projs + agg_idx;
                        let val = &chunk[chunk_col_idx][row_idx];
                        group_aggregates[agg_idx].add_value(val.clone())?;
                    }
                }
            }
        }

        Ok(num_rows)
    }

    pub fn finalize(self) -> Vec<OutputColumn> {
        self.format.finalize()
    }
}

fn create_aggregate_column(alias: Option<String>, source: AggregateFunction) -> RawColumn {
    let (proj, data) = match source {
        AggregateFunction::Sum(p) => (p, RawData::Sum(None)),
        AggregateFunction::Min(p) => (p, RawData::Min(None)),
        AggregateFunction::Max(p) => (p, RawData::Max(None)),
        AggregateFunction::Count(p) => (p, RawData::Count(0)),
        AggregateFunction::Avg(p) => (p, RawData::Avg { sum: 0., count: 0 }),
    };
    RawColumn { alias, proj, data }
}
