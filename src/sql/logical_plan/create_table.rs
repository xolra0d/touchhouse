use std::collections::HashSet;

use sqlparser::ast::{
    ColumnOption, ColumnOptionDef, CreateTable, CreateTableOptions, Expr, Ident,
    OneOrManyWithParens, SqlOption,
};

use crate::engines::EngineName;
use crate::error::{Error, Result};
use crate::sql::{LogicalPlan, validate_name};
use crate::storage::{ColumnDef, Constraints, TableDef, TableSettings, Value, ValueType};

impl LogicalPlan {
    /// Create a table as directory and .metadata file.
    ///
    /// Returns
    ///   * Ok when:
    ///     1. Database name and table name (does not exist) provided, columns, their types and order by are valid: `LogicalPlan::CreateTable`
    ///     2. Database name and table name (exists and `IF NOT EXISTS` specified) provided, columns, their types and order by are valid: `LogicalPlan::Skip`
    ///   * Error when:
    ///     1. Could not parse table and database names from query: `UnsupportedCommand`.
    ///     2. Table already exists and `IF NOT EXISTS` is not passed: `TableAlreadyExists`.
    ///     3. Any column name provided is invalid: `InvalidColumnName`.
    ///     4. Any column name is repeated in specification: `InvalidColumnName`.
    ///     5. Unsupported column type was provided: `UnsupportedColumnType`.
    ///     6. `parse_column_constraints` returns error.
    ///     7. `parse_order_by` returns error.
    pub fn from_create_table(create_table: &CreateTable) -> Result<Self> {
        let table_def = TableDef::try_from(&create_table.name)?;

        if !validate_name(&table_def.table) {
            return Err(Error::InvalidTableName);
        }

        if create_table.columns.is_empty() {
            return Err(Error::NoColumnsSpecified);
        }

        let mut columns: Vec<ColumnDef> = Vec::with_capacity(create_table.columns.len());
        let mut columns_names: HashSet<&String> =
            HashSet::with_capacity(create_table.columns.len());

        for table_column in &create_table.columns {
            let column_name = &table_column.name.value;

            if !validate_name(column_name) {
                return Err(Error::InvalidColumnName(column_name.to_owned()));
            }
            if !columns_names.insert(column_name) {
                return Err(Error::InvalidColumnName(column_name.to_owned()));
            }

            let field_type = ValueType::try_from(&table_column.data_type)?;

            let constraints = Self::parse_column_constraints(&table_column.options, &field_type)?;

            columns.push(ColumnDef {
                name: column_name.clone(),
                field_type,
                constraints,
            });
        }

        let settings = Self::parse_table_options(&create_table.table_options)?;

        let (order_by, primary_key) = match (&create_table.order_by, &create_table.primary_key) {
            (Some(order_by), Some(primary_key)) => {
                let order_by = Self::create_parse_order_by(order_by, &columns)?;
                let primary_key = Self::parse_primary_key(primary_key, &columns)?;

                if primary_key.len() > order_by.len() {
                    return Err(Error::InvalidOrderByPrimaryKeyPair);
                }

                for (column_primary, column_order_by) in primary_key.iter().zip(order_by.iter()) {
                    if column_primary != column_order_by {
                        return Err(Error::InvalidOrderByPrimaryKeyPair);
                    }
                }

                (order_by, primary_key)
            }
            (Some(order_by), None) => {
                let order_by = Self::create_parse_order_by(order_by, &columns)?;
                let primary_key = order_by.clone();
                (order_by, primary_key)
            }
            (None, Some(primary_key)) => {
                let primary_key = Self::parse_primary_key(primary_key, &columns)?;
                let order_by = primary_key.clone();
                (order_by, primary_key)
            }
            (None, None) => (vec![columns[0].clone()], vec![columns[0].clone()]),
        };

        Ok(Self::CreateTable {
            name: table_def,
            if_not_exists: create_table.if_not_exists,
            columns,
            settings,
            order_by,
            primary_key,
        })
    }

    /// Tries to parse `EngineName` from create table tree.
    ///
    /// Returns:
    ///   * Ok when:
    ///     1. None is provided: `EngineName::MergeTree`.
    ///     2. `"Engine".lowercase()` option is provided and name is valid: `EngineName::{SPECIFIED_ENGINE_NAME}`
    ///   * Error when:
    ///     1. More than 1 option is provided: `InvalidEngineName`
    ///     2. When option name is not `"Engine".lowercase()`: `InvalidEngineName`
    ///     3. When engine name is not valid, return error from `EngineName::try_from`
    fn parse_table_options(table_options: &CreateTableOptions) -> Result<TableSettings> {
        match table_options {
            CreateTableOptions::None => Ok(TableSettings::default()),
            CreateTableOptions::Plain(options) => {
                let mut table_settings = TableSettings::default();

                for option in options {
                    let SqlOption::NamedParenthesizedList(option) = option else {
                        return Err(Error::InvalidEngineName);
                    };
                    let name = option.key.value.to_lowercase();

                    match name.as_str() {
                        "engine" => {
                            let key = option.name.as_ref().ok_or(Error::InvalidEngineName)?;
                            table_settings.engine = EngineName::try_from(key.value.as_str())?;
                            Ok(())
                        }
                        _ => Err(Error::UnsupportedTableOption(name)),
                    }?;
                }
                Ok(table_settings)
            }
            _ => Err(Error::InvalidEngineName),
        }
    }

    /// Tries to parse ORDER BY column names.
    ///
    /// Returns
    ///   * Ok when:
    ///     1. All columns are unique, exist in the pool of ALL columns: `Vec<ColumnDef>`
    ///   * Error when:
    ///     1. If no ORDER BY was provided: `InvalidOrderBy`.
    ///     2. If ORDER BY is empty: `InvalidOrderBy`.
    ///     3. If column name is not an identifier: `InvalidOrderBy`.
    ///     4. If column, not found in all columns, is found in ORDER BY: `InvalidOrderBy`.
    ///     5. If the same column is added: `InvalidOrderBy`.
    fn create_parse_order_by(
        order_by_params: &OneOrManyWithParens<Expr>,
        columns: &[ColumnDef],
    ) -> Result<Vec<ColumnDef>> {
        if order_by_params.is_empty() {
            return Err(Error::OrderByColumnsNotFound);
        }

        let mut order_by = Vec::with_capacity(order_by_params.len());
        let mut order_by_names = HashSet::with_capacity(order_by_params.len());

        for param in order_by_params {
            let Expr::Identifier(param_ident) = param else {
                return Err(Error::InvalidOrderBy(format!(
                    "Could not parse order by param: {param}"
                )));
            };
            let column_name = &param_ident.value;

            let column_def = columns.iter().find(|col| col.name == *column_name).ok_or(
                Error::InvalidOrderBy(format!("Could not find column: {column_name}")),
            )?;

            if !order_by_names.insert(column_name) {
                return Err(Error::InvalidOrderBy(format!(
                    "Duplicate column: {column_name}"
                )));
            }

            order_by.push((*column_def).clone());
        }
        Ok(order_by)
    }

    /// Tries to parse PRIMARY KEY column names.
    ///
    /// Returns
    ///   * Ok when:
    ///     1. All columns are unique, exist in the pool of ALL columns: `Vec<ColumnDef>`
    ///   * Error when:
    ///     1. If no ORDER BY was provided: `InvalidOrderBy`.
    ///     2. If ORDER BY is empty: `InvalidOrderBy`.
    ///     3. If column name is not an identifier: `InvalidOrderBy`.
    ///     4. If column, not found in all columns, is found in ORDER BY: `InvalidOrderBy`.
    ///     5. If the same column is added: `InvalidOrderBy`.
    pub fn parse_primary_key(primary_key: &Expr, columns: &[ColumnDef]) -> Result<Vec<ColumnDef>> {
        match primary_key {
            Expr::Identifier(primary_key) => {
                parse_column_def_ident(primary_key, columns).map(|x| vec![x])
            }
            Expr::Tuple(primary_keys) => {
                let mut primary_key = Vec::with_capacity(primary_keys.len());
                for key in primary_keys {
                    let Expr::Identifier(ident) = key else {
                        return Err(Error::InvalidPrimaryKey(format!(
                            "Invalid specifier: {key}"
                        )));
                    };
                    primary_key.push(parse_column_def_ident(ident, columns)?);
                }

                Ok(primary_key)
            }
            Expr::Nested(primary_key) => {
                // Added, because `sqlparser-rs` believes single element tuples are `Expr::Nested`
                if let Expr::Identifier(primary_key) = primary_key.as_ref() {
                    parse_column_def_ident(primary_key, columns).map(|x| vec![x])
                } else {
                    Err(Error::InvalidPrimaryKey(
                        "Nested primary keys are unsupported".to_string(),
                    ))
                }
            }
            _ => Err(Error::InvalidPrimaryKey(format!(
                "Invalid primary key: {primary_key}"
            ))),
        }
    }

    /// Tries to parse column constraints for each column.
    ///
    /// Returns:
    ///   * Ok when:
    ///     1. When provided valid constraint(s): `Constraints`
    ///   * Error when:
    ///     1. Both NULL and NOT NULL are supplied for the column: `UnsupportedColumnConstraint`
    ///     2. Unsupported column constraint is provided: `UnsupportedColumnConstraint`
    pub fn parse_column_constraints(
        options: &[ColumnOptionDef],
        column_type: &ValueType,
    ) -> Result<Constraints> {
        let mut nullable = None;
        let mut default = None;
        let compression_type = column_type.get_optimal_compression(); // currently `sqlparser` does not support `CODEC(compression_type)` param

        for option in options {
            match &option.option {
                constraint @ (ColumnOption::Null | ColumnOption::NotNull) => {
                    if nullable.is_some() {
                        return Err(Error::UnsupportedColumnConstraint(
                            "Invalid NULL constraint".to_string(),
                        ));
                    }
                    nullable = Some(matches!(constraint, ColumnOption::Null));
                }
                ColumnOption::Default(expr) => {
                    let Expr::Value(value) = expr else {
                        return Err(Error::UnsupportedColumnConstraint(
                            "Non-literal default values are not supported".to_string(),
                        ));
                    };
                    let value = Value::try_from((value.value.clone(), column_type))?;
                    default = Some(value);
                }
                _ => {
                    return Err(Error::UnsupportedColumnConstraint(
                        option.option.to_string(),
                    ));
                }
            }
        }

        Ok(Constraints {
            nullable: nullable.unwrap_or(true),
            default,
            compression_type,
        })
    }
}

/// Parses an identifier and finds matching column definition.
///
/// Returns:
///   * Ok: `ColumnDef` when column with matching name is found.
///   * Error: `ColumnNotFound` when no matching column exists.
fn parse_column_def_ident(ident: &Ident, columns: &[ColumnDef]) -> Result<ColumnDef> {
    if let Some(column_def) = columns.iter().find(|col| col.name == ident.value) {
        Ok(column_def.clone())
    } else {
        Err(Error::ColumnNotFound(format!(
            "Column specified ({}) was not found",
            ident.value
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::ast::Ident;

    #[test]
    fn test_parse_column_constraints_valid() {
        let not_null_option = ColumnOptionDef {
            name: None,
            option: ColumnOption::NotNull,
        };
        let null_option = ColumnOptionDef {
            name: None,
            option: ColumnOption::Null,
        };

        let result = LogicalPlan::parse_column_constraints(&[not_null_option], &ValueType::String);
        assert_eq!(
            result.unwrap(),
            Constraints {
                nullable: false,
                default: None,
                compression_type: ValueType::String.get_optimal_compression(),
            }
        );

        let result = LogicalPlan::parse_column_constraints(&[null_option], &ValueType::String);
        assert_eq!(
            result.unwrap(),
            Constraints {
                nullable: true,
                default: None,
                compression_type: ValueType::String.get_optimal_compression(),
            }
        );

        let result = LogicalPlan::parse_column_constraints(&[], &ValueType::String);
        assert_eq!(result.unwrap(), Constraints::default());
    }

    #[test]
    fn test_parse_column_constraints_invalid() {
        let not_null_option = ColumnOptionDef {
            name: None,
            option: ColumnOption::NotNull,
        };
        let null_option = ColumnOptionDef {
            name: None,
            option: ColumnOption::Null,
        };

        let result = LogicalPlan::parse_column_constraints(
            &[not_null_option, null_option],
            &ValueType::String,
        );
        assert!(result.is_err());

        let unique_option = ColumnOptionDef {
            name: None,
            option: ColumnOption::Unique {
                is_primary: false,
                characteristics: None,
            },
        };
        let result = LogicalPlan::parse_column_constraints(&[unique_option], &ValueType::String);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_order_by_invalid() {
        let col1 = ColumnDef {
            name: "id".to_string(),
            field_type: ValueType::UInt32,
            constraints: Constraints::default(),
        };
        let col2 = ColumnDef {
            name: "name".to_string(),
            field_type: ValueType::String,
            constraints: Constraints::default(),
        };
        let columns = vec![col1, col2];

        let empty_order_by = OneOrManyWithParens::Many(vec![]);
        let result = LogicalPlan::create_parse_order_by(&empty_order_by, &columns);
        assert!(result.is_err());

        let invalid_column = OneOrManyWithParens::Many(vec![Expr::Identifier(Ident::new(
            "nonexistent".to_string(),
        ))]);
        let result = LogicalPlan::create_parse_order_by(&invalid_column, &columns);
        assert!(result.is_err());

        let duplicate = OneOrManyWithParens::Many(vec![
            Expr::Identifier(Ident::new("id".to_string())),
            Expr::Identifier(Ident::new("id".to_string())),
        ]);
        let result = LogicalPlan::create_parse_order_by(&duplicate, &columns);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_order_by_valid() {
        let col1 = ColumnDef {
            name: "id".to_string(),
            field_type: ValueType::UInt32,
            constraints: Constraints::default(),
        };
        let col2 = ColumnDef {
            name: "name".to_string(),
            field_type: ValueType::String,
            constraints: Constraints::default(),
        };
        let columns = vec![col1, col2];

        let order_by =
            OneOrManyWithParens::Many(vec![Expr::Identifier(Ident::new("id".to_string()))]);
        let result = LogicalPlan::create_parse_order_by(&order_by, &columns);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 1);

        let order_by = OneOrManyWithParens::Many(vec![
            Expr::Identifier(Ident::new("id".to_string())),
            Expr::Identifier(Ident::new("name".to_string())),
        ]);
        let result = LogicalPlan::create_parse_order_by(&order_by, &columns);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 2);
    }

    #[test]
    fn test_parse_table_options_default() {
        let result = LogicalPlan::parse_table_options(&CreateTableOptions::None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().engine, EngineName::MergeTree);
    }
}
