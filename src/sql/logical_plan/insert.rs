use std::collections::HashSet;

use sqlparser::ast::{
    Expr, Ident, Insert, Query, SetExpr, TableObject, UnaryOperator, Value as SQLValue, Values,
};

use crate::error::{Error, Result};
use crate::runtime_config::{TABLE_DATA, TableConfig};
use crate::sql::LogicalPlan;
use crate::storage::{ColumnDef, PhysicalColumn, TableDef, Value};

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
        let table_def = Self::extract_table_def(&insert.table)?;
        let table_config = TABLE_DATA.get(&table_def).ok_or(Error::TableNotFound)?;

        Self::validate_no_duplicate_columns(&insert.columns)?;

        let insert_columns = Self::resolve_insert_columns(&insert.columns, &table_config)?;
        let insert_column_set: HashSet<_> =
            insert_columns.iter().map(|col| col.name.clone()).collect();

        Self::validate_required_columns(&insert_column_set, &table_config)?;

        let mut columns: Vec<PhysicalColumn> = insert_columns
            .into_iter()
            .map(PhysicalColumn::from)
            .collect();

        let source = Self::extract_values_source(insert.source.as_deref())?;
        Self::validate_and_populate_columns(&mut columns, source)?;
        Self::add_default_columns(
            &mut columns,
            &insert_column_set,
            &table_config,
            source.rows.len(),
        );

        Ok(LogicalPlan::Insert { table_def, columns })
    }

    fn extract_table_def(table: &TableObject) -> Result<TableDef> {
        let TableObject::TableName(table) = table else {
            return Err(Error::UnsupportedCommand(
                "Currently not supporting table functions".to_string(),
            ));
        };
        TableDef::try_from(table)
    }

    fn validate_no_duplicate_columns(columns: &[Ident]) -> Result<()> {
        if columns.is_empty() {
            return Err(Error::NoColumnsSpecified);
        }

        let mut seen = std::collections::HashSet::new();
        for col in columns {
            if !seen.insert(&col.value) {
                return Err(Error::InvalidColumnName(format!(
                    "Duplicate column: {}",
                    col.value
                )));
            }
        }
        Ok(())
    }

    fn resolve_insert_columns(
        input_columns: &[Ident],
        table_config: &TableConfig,
    ) -> Result<Vec<ColumnDef>> {
        let mut insert_columns = Vec::with_capacity(input_columns.len());

        for input_column in input_columns {
            let column_def = table_config
                .metadata
                .schema
                .columns
                .iter()
                .find(|x| x.name == input_column.value)
                .ok_or(Error::InvalidColumnName(input_column.value.clone()))?;
            insert_columns.push(column_def.clone());
        }

        Ok(insert_columns)
    }

    fn validate_required_columns(
        insert_column_set: &HashSet<String>,
        table_config: &TableConfig,
    ) -> Result<()> {
        let missing_not_null = table_config
            .metadata
            .schema
            .columns
            .iter()
            .filter(|col| !insert_column_set.contains(&col.name))
            .find(|col| !col.constraints.nullable && col.constraints.default.is_none());

        if let Some(col_def) = missing_not_null {
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

        Ok(())
    }

    fn extract_values_source(source: Option<&Query>) -> Result<&Values> {
        let Some(source) = source else {
            return Err(Error::InvalidSource(
                "No source of values was specified.".to_string(),
            ));
        };

        let SetExpr::Values(values) = source.body.as_ref() else {
            return Err(Error::InvalidSource("Provide direct values".to_string()));
        };

        Ok(values)
    }

    fn validate_and_populate_columns(
        columns: &mut [PhysicalColumn],
        source: &Values,
    ) -> Result<()> {
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
                let sql_value = Self::extract_sql_value(expr)?;
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

        Ok(())
    }

    fn extract_sql_value(expr: &Expr) -> Result<SQLValue> {
        match expr {
            Expr::Value(sql_value) => Ok(sql_value.value.clone()),
            Expr::UnaryOp { op, expr } => {
                let Expr::Value(inner) = expr.as_ref() else {
                    return Err(Error::InvalidSource(format!(
                        "Expected direct value, received: {expr}"
                    )));
                };

                match (op, &inner.value) {
                    (UnaryOperator::Minus, SQLValue::Number(n, exact)) => {
                        Ok(SQLValue::Number(format!("-{n}"), *exact))
                    }
                    (UnaryOperator::Plus, SQLValue::Number(n, exact)) => {
                        Ok(SQLValue::Number(n.clone(), *exact))
                    }
                    _ => Err(Error::InvalidSource(format!(
                        "Expected plus or minus as operator and a number, received: {} and {}",
                        op, inner.value
                    ))),
                }
            }
            _ => Err(Error::InvalidSource(format!(
                "Expected a value, received: {expr}"
            ))),
        }
    }

    fn add_default_columns(
        columns: &mut Vec<PhysicalColumn>,
        insert_column_set: &HashSet<String>,
        table_config: &TableConfig,
        row_count: usize,
    ) {
        for column_def in &table_config.metadata.schema.columns {
            if insert_column_set.contains(&column_def.name) {
                continue;
            }

            let default_value_ref =
                if let Some(default_value) = column_def.constraints.default.as_ref() {
                    default_value
                } else if column_def.constraints.nullable {
                    &Value::Null
                } else {
                    continue;
                };

            columns.push(PhysicalColumn {
                column_def: column_def.clone(),
                data: vec![default_value_ref.clone(); row_count],
            });
        }
    }
}
