use crate::engines::{Engine, EngineConfig};
use crate::error::{Error, Result};
use crate::sql::Projection;
use crate::storage::{ColumnDef, OutputColumn, Value};

use std::cmp::Ordering;

/// Standard engine for most needs.
/// Does not perform any changes to data. Just keeps it sorted in ASC by ORDER BY
/// If two rows have the same ORDER BY values, their positions in terms of each other are not deterministic.
pub struct MergeTreeEngine {
    _config: EngineConfig,
}

impl MergeTreeEngine {
    /// Creates a new `MergeTree` engine with the given configuration.
    pub const fn new(config: EngineConfig) -> Self {
        Self { _config: config }
    }
}

impl Engine for MergeTreeEngine {
    /// Orders columns by sorting rows according to ORDER BY column definitions.
    ///
    /// Returns:
    ///   * Ok: `Vec<OutputColumn>` with rows sorted in ascending order by `ORDER BY` columns.
    ///   * Error when:
    ///     1. ORDER BY is empty or columns is empty: `NoColumnsSpecified`.
    ///     2. Column lengths mismatch: `InvalidColumnsSpecified`.
    ///     3. ORDER BY column not found: `InvalidColumnsSpecified`.
    fn order_columns(
        &self,
        mut columns: Vec<OutputColumn>,
        order_by: &[Projection],
        _primary_key: &[ColumnDef],
    ) -> Result<Vec<OutputColumn>> {
        if order_by.is_empty() || columns.is_empty() {
            return Err(Error::NoColumnsSpecified);
        }

        let row_count = columns[0].data.len();

        if columns.iter().any(|col| col.data.len() != row_count) {
            return Err(Error::InvalidColumnsSpecified);
        }

        let mut order_by_indices = Vec::with_capacity(order_by.len());
        for order_proj in order_by {
            let Some(idx) = columns.iter().position(|col| *order_proj == col.proj) else {
                return Err(Error::InvalidColumnsSpecified);
            };
            order_by_indices.push(idx);
        }

        let mut indices: Vec<usize> = (0..row_count).collect();

        indices.sort_unstable_by(|&a, &b| {
            for &col_idx in &order_by_indices {
                let col_a = &columns[col_idx].data[a];
                let col_b = &columns[col_idx].data[b];

                let cmp = col_a
                    .partial_cmp(col_b)
                    .expect("Values in the same column are of the same type and ARE comparable");

                if cmp != Ordering::Equal {
                    return cmp;
                }
            }
            Ordering::Equal
        });

        for column in &mut columns {
            apply_permutation_in_place(&mut column.data, &indices);
        }

        Ok(columns)
    }
}

fn apply_permutation_in_place(data: &mut [Value], indices: &[usize]) {
    let mut visited = vec![false; data.len()];

    for cycle_start in 0..data.len() {
        if visited[cycle_start] {
            continue;
        }

        let mut current = cycle_start;
        let mut next = indices[current];

        while next != cycle_start {
            visited[current] = true;
            data.swap(current, next);
            current = next;
            next = indices[current];
        }
        visited[current] = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::ProjectionValue;
    use crate::storage::ValueType;

    macro_rules! value {
        (S $x:literal) => {
            vec![Value::String($x.to_string())]
        };
        (I $x:literal) => {
            vec![Value::Int32(i32::from($x))]
        };
        (S $($x:literal),*) => {
            vec![$(Value::String($x.to_string())),*]
        };
        (I $($x:literal),*) => {
            vec![$(Value::Int32(i32::from($x))),*]
        }
    }

    fn str_col_def() -> ColumnDef {
        ColumnDef {
            name: "test_str".to_string(),
            field_type: ValueType::String,
            constraints: Default::default(),
        }
    }

    fn int_col_def() -> ColumnDef {
        ColumnDef {
            name: "test_int".to_string(),
            field_type: ValueType::Int32,
            constraints: Default::default(),
        }
    }

    #[test]
    fn test_empty() {
        let engine = MergeTreeEngine::new(EngineConfig::default());
        let columns = Vec::new();

        assert_eq!(
            engine.order_columns(columns, &[], &[]).unwrap_err(),
            Error::NoColumnsSpecified
        );
    }

    #[test]
    fn test_single_row_single_column() {
        let engine = MergeTreeEngine::new(EngineConfig::default());
        let columns = vec![OutputColumn {
            proj: Projection {
                alias: None,
                source: ProjectionValue::ColumnDef(str_col_def()),
            },
            data: value!(S "1"),
        }];

        assert_eq!(
            engine
                .order_columns(
                    columns.clone(),
                    &[Projection {
                        alias: None,
                        source: ProjectionValue::ColumnDef(str_col_def())
                    }],
                    &[str_col_def()]
                )
                .unwrap(),
            columns
        )
    }

    #[test]
    fn test_multiple_row_single_column() {
        let engine = MergeTreeEngine::new(EngineConfig::default());
        let columns = vec![OutputColumn {
            proj: Projection {
                alias: None,
                source: ProjectionValue::ColumnDef(int_col_def()),
            },
            data: value!(I 1, 2, 4, 3, 2),
        }];

        assert_eq!(
            engine
                .order_columns(
                    columns.clone(),
                    &[Projection {
                        alias: None,
                        source: ProjectionValue::ColumnDef(int_col_def())
                    }],
                    &[int_col_def()]
                )
                .unwrap(),
            vec![OutputColumn {
                proj: Projection {
                    alias: None,
                    source: ProjectionValue::ColumnDef(int_col_def())
                },
                data: value!(I 1, 2, 2, 3, 4),
            }]
        );
    }

    #[test]
    fn test_single_row_multiple_column() {
        let engine = MergeTreeEngine::new(EngineConfig::default());
        let columns = vec![
            OutputColumn {
                proj: Projection {
                    alias: None,
                    source: ProjectionValue::ColumnDef(int_col_def()),
                },
                data: value!(I 1),
            },
            OutputColumn {
                proj: Projection {
                    alias: None,
                    source: ProjectionValue::ColumnDef(str_col_def()),
                },
                data: value!(S "1"),
            },
        ];

        assert_eq!(
            engine
                .order_columns(
                    columns.clone(),
                    &[Projection {
                        alias: None,
                        source: ProjectionValue::ColumnDef(int_col_def())
                    }],
                    &[int_col_def()]
                )
                .unwrap(),
            columns
        );
    }

    #[test]
    fn test_multiple_row_multiple_column_eq() {
        let engine = MergeTreeEngine::new(EngineConfig::default());
        let columns = vec![
            OutputColumn {
                proj: Projection {
                    alias: None,
                    source: ProjectionValue::ColumnDef(int_col_def()),
                },
                data: value!(I 1, 5, 3, 2, 4),
            },
            OutputColumn {
                proj: Projection {
                    alias: None,
                    source: ProjectionValue::ColumnDef(str_col_def()),
                },
                data: value!(S "1", "5", "3", "2", "4"),
            },
        ];

        assert_eq!(
            engine
                .order_columns(
                    columns.clone(),
                    &[Projection {
                        alias: None,
                        source: ProjectionValue::ColumnDef(int_col_def())
                    }],
                    &[int_col_def()]
                )
                .unwrap(),
            vec![
                OutputColumn {
                    proj: Projection {
                        alias: None,
                        source: ProjectionValue::ColumnDef(int_col_def())
                    },
                    data: value!(I 1, 2, 3, 4, 5),
                },
                OutputColumn {
                    proj: Projection {
                        alias: None,
                        source: ProjectionValue::ColumnDef(str_col_def())
                    },
                    data: value!(S "1", "2", "3", "4", "5"),
                }
            ]
        )
    }

    #[test]
    fn test_multiple_row_multiple_column_not_eq() {
        let engine = MergeTreeEngine::new(EngineConfig::default());
        let columns = vec![
            OutputColumn {
                proj: Projection {
                    alias: None,
                    source: ProjectionValue::ColumnDef(int_col_def()),
                },
                data: value!(I 1, 2, 3, 2, 4),
            },
            OutputColumn {
                proj: Projection {
                    alias: None,
                    source: ProjectionValue::ColumnDef(str_col_def()),
                },
                data: value!(S "1", "5", "3", "2", "4"),
            },
        ];

        assert_eq!(
            engine
                .order_columns(
                    columns.clone(),
                    &[
                        Projection {
                            alias: None,
                            source: ProjectionValue::ColumnDef(int_col_def())
                        },
                        Projection {
                            alias: None,
                            source: ProjectionValue::ColumnDef(str_col_def())
                        },
                    ],
                    &[int_col_def(), str_col_def()]
                )
                .unwrap(),
            vec![
                OutputColumn {
                    proj: Projection {
                        alias: None,
                        source: ProjectionValue::ColumnDef(int_col_def())
                    },
                    data: value!(I 1, 2, 2, 3, 4),
                },
                OutputColumn {
                    proj: Projection {
                        alias: None,
                        source: ProjectionValue::ColumnDef(str_col_def())
                    },
                    data: value!(S "1", "2", "5", "3", "4"),
                }
            ]
        )
    }
}
