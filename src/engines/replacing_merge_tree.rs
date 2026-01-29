use crate::engines::{Engine, EngineConfig};
use crate::error::{Error, Result};
use crate::sql::{Projection, ProjectionValue};
use crate::storage::{ColumnDef, OutputColumn};
use std::cmp::Ordering;

/// Engine for editing rows. Sorts values in ASC order.
///
/// When it finds rows with the same PK values, it replaces with the newest row values.
///
/// # Example
///
/// ```text
/// PK indexes: [0, 1]
///
/// Row0: [1, 2, 3, 4] <- same PK values (1, 2)
/// Row1: [4, 2, 3, 4]
/// Row2: [1, 2, 33, 42] <- same PK values (1, 2)
///
/// Replaces row [1, 2, 3, 4] with new row: [1, 2, 33, 42]
///
/// Returns:
/// Row0: [1, 2, 33, 42]
/// Row1: [4, 2, 3, 4]
/// ```
pub struct ReplacingMergeTreeEngine {
    _config: EngineConfig,
}

impl ReplacingMergeTreeEngine {
    /// Creates a new `ReplacingMergeTree` engine with the given configuration.
    pub fn new(config: EngineConfig) -> Self {
        Self { _config: config }
    }
}

impl Engine for ReplacingMergeTreeEngine {
    /// Orders columns and deduplicates rows by PRIMARY KEY, keeping the latest row.
    ///
    /// Sorts rows in ascending order by ORDER BY columns, then removes duplicates
    /// based on PRIMARY KEY, keeping the row that appears last (newest).
    ///
    /// Returns:
    ///   * Ok: `Vec<OutputColumn>` with sorted and deduplicated rows.
    ///   * Error: `NoColumnsSpecified` if columns is empty.
    fn order_columns(
        &self,
        mut columns: Vec<OutputColumn>,
        order_by: &[Projection],
        primary_key: &[ColumnDef],
    ) -> Result<Vec<OutputColumn>> {
        let Some(total_rows) = columns.first().map(|col| col.data.len()) else {
            return Err(Error::NoColumnsSpecified);
        };

        let mut order_by_indexes = Vec::new();
        for proj in order_by {
            let Some(position) = columns.iter().position(|col| {
                // if let ProjectionValue::ColumnDef(col_def) = &proj.source {
                //     col.column_def == *col_def
                // } else {
                //     false
                // }
                *proj == col.proj
            }) else {
                continue;
            };
            order_by_indexes.push(position);
        }

        let mut pk_indexes = Vec::new();
        for pk_col_def in primary_key {
            let Some(position) = columns.iter().position(|col| {
                if let ProjectionValue::ColumnDef(col_def) = &col.proj.source {
                    pk_col_def == col_def
                } else {
                    false
                }
            }) else {
                continue;
            };
            pk_indexes.push(position);
        }

        let mut data_in_row_format: Vec<Vec<_>> = (0..total_rows)
            .map(|_| Vec::with_capacity(columns.len()))
            .collect();
        for col in &mut columns {
            for (idx, value) in col.data.drain(..).enumerate() {
                data_in_row_format[idx].push(value);
            }
        }

        data_in_row_format.sort_by(|left_vec, right_vec| {
            for &order_by_idx in &order_by_indexes {
                let col_a = &left_vec[order_by_idx];
                let col_b = &right_vec[order_by_idx];

                let cmp = col_a
                    .partial_cmp(col_b)
                    .expect("Values in the same column are of the same type and ARE comparable");

                if cmp != Ordering::Equal {
                    return cmp;
                }
            }
            Ordering::Equal
        });

        data_in_row_format.reverse();

        data_in_row_format.dedup_by(|a, b| pk_indexes.iter().all(|&pk_idx| a[pk_idx] == b[pk_idx]));

        data_in_row_format.reverse();

        for row in data_in_row_format {
            for (column, value) in columns.iter_mut().zip(row) {
                column.data.push(value);
            }
        }

        Ok(columns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{ColumnDef, Constraints, Value, ValueType};

    fn string_column(name: String, data: Vec<&str>) -> OutputColumn {
        OutputColumn {
            proj: Projection {
                alias: None,
                source: ProjectionValue::ColumnDef(ColumnDef {
                    name,
                    field_type: ValueType::String,
                    constraints: Constraints::default(),
                }),
            },
            data: data.iter().map(|x| Value::String(x.to_string())).collect(),
        }
    }

    fn get_engine() -> ReplacingMergeTreeEngine {
        ReplacingMergeTreeEngine::new(EngineConfig::default())
    }

    #[test]
    fn test_1() {
        let col_1 = string_column("col_1".to_string(), vec!["a", "b", "b", "c", "a", "d", "b"]);
        let col_2 = string_column("col_2".to_string(), vec!["q", "w", "e", "d", "q", "w", "w"]);
        let col_3 = string_column("col_3".to_string(), vec!["1", "2", "3", "4", "5", "6", "7"]);
        let order_by = vec![
            Projection {
                alias: None,
                source: col_1.proj.source.clone(),
            },
            Projection {
                alias: None,
                source: col_2.proj.source.clone(),
            },
            Projection {
                alias: None,
                source: col_3.proj.source.clone(),
            },
        ];

        let ProjectionValue::ColumnDef(col_1_col_def) = col_1.proj.source.clone() else {
            unreachable!()
        };
        let ProjectionValue::ColumnDef(col_2_col_def) = col_2.proj.source.clone() else {
            unreachable!()
        };

        let primary_key = vec![col_1_col_def, col_2_col_def];

        let merged = vec![
            string_column("col_1".to_string(), vec!["a", "b", "b", "c", "d"]),
            string_column("col_2".to_string(), vec!["q", "e", "w", "d", "w"]),
            string_column("col_3".to_string(), vec!["5", "3", "7", "4", "6"]),
        ];

        assert_eq!(
            get_engine()
                .order_columns(vec![col_1, col_2, col_3], &order_by, &primary_key)
                .unwrap(),
            merged
        );
    }

    #[test]
    fn test_2() {
        let col_1 = string_column("id".to_string(), vec!["1", "1", "2", "2", "3", "1"]);
        let col_2 = string_column(
            "version".to_string(),
            vec!["v1", "v2", "v1", "v3", "v1", "v3"],
        );
        let col_3 = string_column(
            "data".to_string(),
            vec!["old", "mid", "old", "new", "only", "newest"],
        );

        let order_by = vec![
            Projection {
                alias: None,
                source: col_1.proj.source.clone(),
            },
            Projection {
                alias: None,
                source: col_2.proj.source.clone(),
            },
        ];
        let ProjectionValue::ColumnDef(col_1_col_def) = col_1.proj.source.clone() else {
            unreachable!()
        };

        let primary_key = vec![col_1_col_def];

        let merged = vec![
            string_column("id".to_string(), vec!["1", "2", "3"]),
            string_column("version".to_string(), vec!["v3", "v3", "v1"]),
            string_column("data".to_string(), vec!["newest", "new", "only"]),
        ];

        assert_eq!(
            get_engine()
                .order_columns(vec![col_1, col_2, col_3], &order_by, &primary_key)
                .unwrap(),
            merged
        );
    }
}
