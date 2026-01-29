use sqlparser::ast::{Expr, Insert, SetExpr, TableObject, UnaryOperator, Value as SQLValue};

use crate::error::{Error, Result};
use crate::runtime_config::TABLE_DATA;
use crate::sql::LogicalPlan;
use crate::storage::{PhysicalColumn, TableDef, Value};

impl LogicalPlan {
    /// Parses INSERT statement into `LogicalPlan::Insert` variant.
    ///
    /// Validates that:
    /// - Table exists and columns are valid
    /// - All NOT NULL and ORDER BY columns are provided
    /// - Values match column types
    ///
    /// Returns:
    ///   * Ok: `LogicalPlan::Insert` with validated columns and data
    ///   * Error: `TableNotFound`, `InvalidColumnName`, `InvalidColumnsSpecified`, `InvalidSource`, or `EmptySource`
    pub fn from_insert(insert: &Insert) -> Result<Self> {
        let TableObject::TableName(table) = &insert.table else {
            return Err(Error::UnsupportedCommand(
                "Currently not supporting table functions".to_string(),
            ));
        };
        let table_def = TableDef::try_from(table)?;

        let Some(table_config) = TABLE_DATA.get(&table_def) else {
            return Err(Error::TableNotFound);
        };

        if insert.columns.is_empty() {
            return Err(Error::NoColumnsSpecified);
        }
        let mut seen = std::collections::HashSet::new();
        for col in &insert.columns {
            if !seen.insert(&col.value) {
                return Err(Error::InvalidColumnName(format!(
                    "Duplicate column: {}",
                    col.value
                )));
            }
        }

        let mut insert_columns = Vec::with_capacity(insert.columns.len());
        let mut insert_column_set = std::collections::HashSet::new();
        for input_column in &insert.columns {
            let column_def = table_config
                .metadata
                .schema
                .columns
                .iter()
                .find(|x| x.name == input_column.value)
                .ok_or(Error::InvalidColumnName(input_column.value.clone()))?;
            insert_columns.push(column_def.clone());
            insert_column_set.insert(&column_def.name);
        }

        let missing_not_null_not_default = table_config
            .metadata
            .schema
            .columns
            .iter()
            .filter(|col| !insert_column_set.contains(&col.name))
            .find(|col| !col.constraints.nullable && col.constraints.default.is_none());

        if let Some(col_def) = missing_not_null_not_default {
            return Err(Error::InvalidSource(format!(
                "Column ({}) is not specified and is neither nullable nor have a default value.",
                col_def.name
            )));
        }

        for order_by_col in &table_config.metadata.schema.order_by {
            if !insert_column_set.contains(&order_by_col.name)
                && !order_by_col.constraints.nullable
                && order_by_col.constraints.default.is_none()
            {
                return Err(Error::InvalidColumnsSpecified);
            }
        }

        for pk_col in &table_config.metadata.schema.primary_key {
            if !insert_column_set.contains(&pk_col.name)
                && !pk_col.constraints.nullable
                && pk_col.constraints.default.is_none()
            {
                return Err(Error::InvalidColumnsSpecified);
            }
        }

        let mut columns: Vec<PhysicalColumn> = insert_columns
            .into_iter()
            .map(PhysicalColumn::from)
            .collect();

        let Some(source) = &insert.source else {
            return Err(Error::InvalidSource(
                "No source of values was specified.".to_string(),
            ));
        };
        let SetExpr::Values(source) = source.body.as_ref() else {
            return Err(Error::InvalidSource("Provide direct values".to_string()));
        };

        let Some(val_count) = source.rows.first().map(Vec::len) else {
            return Err(Error::EmptySource);
        };

        if source.rows.iter().any(|x| x.len() != val_count) {
            return Err(Error::InvalidSource("Columns length mismatch.".to_string()));
        }

        if columns.len() != val_count {
            return Err(Error::InvalidSource(format!(
                "Invalid number of values specified. Expected: {}, got: {}",
                columns.len(),
                val_count
            )));
        }

        for row in &source.rows {
            for (col_idx, expr) in row.iter().enumerate() {
                let sql_value = match expr {
                    Expr::Value(sql_value) => sql_value.value.clone(),
                    Expr::UnaryOp { op, expr } => {
                        let Expr::Value(inner) = expr.as_ref() else {
                            return Err(Error::InvalidSource(format!(
                                "Expected direct value, received: {expr}"
                            )));
                        };
                        match (&op, &inner.value) {
                            (UnaryOperator::Minus, SQLValue::Number(n, exact)) => {
                                SQLValue::Number(format!("-{n}"), *exact)
                            }
                            (UnaryOperator::Plus, SQLValue::Number(n, exact)) => {
                                SQLValue::Number(n.clone(), *exact)
                            }
                            _ => {
                                return Err(Error::InvalidSource(format!(
                                    "Expected plus or minus as operator and a number, received: {} and {}",
                                    op, inner.value
                                )));
                            }
                        }
                    }
                    _ => {
                        return Err(Error::InvalidSource(format!(
                            "Expected a value, received: {expr}"
                        )));
                    }
                };

                let column_def = &columns[col_idx].column_def;

                let value = Value::try_from((sql_value, &column_def.field_type))?;

                if value == Value::Null && !column_def.constraints.nullable {
                    return Err(Error::CouldNotInsertData(format!(
                        "NULL value not allowed for column '{}'",
                        column_def.name
                    )));
                }

                columns[col_idx].data.push(value);
            }
        }

        for column_def in &table_config.metadata.schema.columns {
            if insert_column_set.contains(&column_def.name) {
                continue;
            }
            let default_value_ref = {
                if let Some(default_value) = column_def.constraints.default.as_ref() {
                    default_value
                } else if column_def.constraints.nullable {
                    &Value::Null
                } else {
                    continue;
                }
            };
            columns.push(PhysicalColumn {
                column_def: column_def.clone(),
                data: vec![default_value_ref.clone(); source.rows.len()],
            });
        }

        Ok(LogicalPlan::Insert { table_def, columns })
    }
}
