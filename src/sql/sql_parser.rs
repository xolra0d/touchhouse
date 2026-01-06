use serde::Serialize;
use sqlparser::ast::{
    BinaryOperator, Expr, Function as SQLFunction, FunctionArg, FunctionArgExpr, FunctionArguments,
    Ident, ObjectNamePart, Statement,
};
use sqlparser::dialect::ClickHouseDialect;
use sqlparser::parser::Parser;

use crate::error::{Error, Result};
use crate::sql::output_table::OutputColumn;
use crate::storage::table_metadata::TableSettings;
use crate::storage::{ColumnDef, TableDef, Value};

/// Source for a Scan operation
#[derive(Debug, Clone, PartialEq)]
pub enum ScanSource {
    Table(TableDef),
    Subquery(Box<LogicalPlan>),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum ProjectionValue {
    Value(Value),
    ColumnDef(ColumnDef),
    Function(Function),
}

impl ProjectionValue {
    pub fn get_col_def(&self) -> Option<&ColumnDef> {
        match self {
            ProjectionValue::ColumnDef(col_def) => Some(col_def),
            ProjectionValue::Function(function) => match function {
                Function::Avg(projection)
                | Function::Max(projection)
                | Function::Min(projection)
                | Function::Sum(projection) => projection.source.get_col_def(),
            },
            ProjectionValue::Value(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Projection {
    pub alias: Option<String>,
    pub source: ProjectionValue,
}

impl Projection {
    pub fn try_from_ident(ident: &Ident, available_projections: &[Projection]) -> Result<Self> {
        let Some(projection) = available_projections
            .iter()
            .find(|proj| {
                if let Some(alias) = &proj.alias {
                    *alias == ident.value
                } else if let ProjectionValue::ColumnDef(column_def) = &proj.source {
                    column_def.name == ident.value
                } else {
                    false
                }
            })
            .cloned()
        else {
            return Err(Error::ColumnNotFound(ident.to_string()));
        };

        Ok(projection)
    }

    pub fn try_from_expr(expr: &Expr, available_projections: &[Self]) -> Result<Self> {
        match expr {
            Expr::Identifier(ident) => {
                let projection = Self::try_from_ident(ident, available_projections)?;
                Ok(projection)
            }
            Expr::Value(value) => {
                let value = Value::try_from_untyped(value.value.clone())?;
                Ok(Self {
                    source: ProjectionValue::Value(value),
                    alias: None,
                })
            }
            Expr::Function(function) => Ok(Self {
                source: ProjectionValue::Function(Function::try_parse(
                    function,
                    available_projections,
                )?),
                alias: None,
            }),
            _ => Err(Error::UnsupportedCommand(
                "Only column identifiers and values are supported in projections".to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Function {
    Min(Box<Projection>),
    Max(Box<Projection>),
    Sum(Box<Projection>),
    Avg(Box<Projection>),
}

impl Function {
    fn find_function(function_name: &str, mut args: Vec<Projection>) -> Result<Self> {
        let function_name = function_name.to_lowercase();

        match function_name.as_str() {
            "min" => {
                if args.len() != 1 {
                    return Err(Error::InvalidFunctionParams(format!(
                        "for `min` function expected only 1 argument, but received: {}",
                        args.len()
                    )));
                }

                Ok(Function::Min(Box::new(args.remove(0))))
            }
            "max" => {
                if args.len() != 1 {
                    return Err(Error::InvalidFunctionParams(format!(
                        "for `max` function expected only 1 argument, but received: {}",
                        args.len()
                    )));
                }

                Ok(Function::Max(Box::new(args.remove(0))))
            }
            "sum" => {
                if args.len() != 1 {
                    return Err(Error::InvalidFunctionParams(format!(
                        "for `sum` function expected only 1 argument, but received: {}",
                        args.len()
                    )));
                }

                Ok(Function::Sum(Box::new(args.remove(0))))
            }
            "avg" => {
                if args.len() != 1 {
                    return Err(Error::InvalidFunctionParams(format!(
                        "for `avg` function expected only 1 argument, but received: {}",
                        args.len()
                    )));
                }

                Ok(Function::Avg(Box::new(args.remove(0))))
            }
            _ => Err(Error::UnknownFunction(format!(
                "Unknown function name: {function_name}"
            ))),
        }
    }

    pub fn try_parse(function: &SQLFunction, available_projections: &[Projection]) -> Result<Self> {
        if function.name.0.len() != 1 {
            return Err(Error::UnknownFunction(function.name.to_string()));
        }
        let ObjectNamePart::Identifier(function_ident) = &function.name.0[0] else {
            return Err(Error::UnknownFunction(format!(
                "function name ({}) should be identifier, not another function.",
                function.name.0[0]
            )));
        };
        let function_name = &function_ident.value;

        if function.parameters != FunctionArguments::None {
            return Err(Error::InvalidFunctionParams(format!(
                "Function parameters ({}) are not allowed.",
                function.parameters
            )));
        }

        let args = match &function.args {
            FunctionArguments::None => Vec::new(),
            FunctionArguments::Subquery(subquery) => {
                return Err(Error::InvalidFunctionParams(format!(
                    "subqueries ({subquery}) are not supported as function parameters"
                )));
            }
            FunctionArguments::List(args_list) => {
                if !args_list.clauses.is_empty() {
                    return Err(Error::InvalidFunctionParams(format!(
                        "clauses ({:?}) are not supported as function params",
                        args_list.clauses
                    )));
                }
                if args_list.duplicate_treatment.is_some() {
                    return Err(Error::InvalidFunctionParams(format!(
                        "`[ ALL | DISTINCT ]` ({:?}) is not supported as function params",
                        args_list.duplicate_treatment
                    )));
                }
                let mut arguments = Vec::with_capacity(args_list.args.len());
                for arg in &args_list.args {
                    let argument = match arg {
                        FunctionArg::Unnamed(arg_expr) => match arg_expr {
                            FunctionArgExpr::Expr(expr) => {
                                Projection::try_from_expr(expr, available_projections)?
                            }
                            FunctionArgExpr::Wildcard => available_projections[0].clone(),
                            FunctionArgExpr::QualifiedWildcard(qualified_wildcard) => {
                                return Err(Error::InvalidFunctionParams(format!(
                                    "Currently, qualified wildcards ({qualified_wildcard}) are not supported"
                                )));
                            }
                        },
                        FunctionArg::Named { .. } => {
                            return Err(Error::InvalidFunctionParams(
                                "Currently functions do not have/support named parameters."
                                    .to_string(),
                            ));
                        }
                        FunctionArg::ExprNamed { .. } => unreachable!(
                            "ClickHouseDialect::supports_named_fn_args_with_expr_name is false"
                        ),
                    };
                    arguments.push(argument);
                }

                arguments
            }
        };

        Self::find_function(function_name, args)
    }
}

/// High level representation of the SQL query.
#[derive(Debug, Clone, PartialEq)]
pub enum LogicalPlan {
    /// No tasks need to be done. Skip.
    Skip,

    /// Create a database.
    CreateDatabase {
        name: String,
    },

    /// Create a table.
    CreateTable {
        name: TableDef,
        columns: Vec<ColumnDef>,
        settings: TableSettings,
        order_by: Vec<ColumnDef>,
        primary_key: Vec<ColumnDef>,
    },

    /// Insert values.
    Insert {
        table_def: TableDef,
        columns: Vec<OutputColumn>,
    },

    DropDatabase {
        name: String,
        if_exists: bool,
    },

    DropTable {
        name: TableDef,
        if_exists: bool,
    },

    Scan {
        source: ScanSource,
    },

    Projection {
        columns: Vec<Projection>,
        plan: Box<LogicalPlan>,
    },

    Filter {
        expr: Box<Expr>,
        plan: Box<LogicalPlan>,
    },

    OrderBy {
        column_defs: Vec<Vec<Projection>>,
        plan: Box<LogicalPlan>,
    },

    Limit {
        limit: Option<u64>,
        offset: u64, // default 0
        plan: Box<LogicalPlan>,
    },
}

/// Tries to convert SQL to `LogicalPlan` by using Datafusion `SQLParser`
/// Currently supported commands
///   1. `CREATE DATABASE`
///   2. `CREATE TABLE`
///   3. `INSERT INTO`
impl TryFrom<&str> for LogicalPlan {
    type Error = Error;

    fn try_from(sql: &str) -> Result<Self> {
        let dialect = ClickHouseDialect {};
        let ast = Parser::parse_sql(&dialect, sql)
            .map_err(|error| Error::SqlToAstConversion(error.to_string()))?;
        if ast.len() != 1 {
            return Err(Error::SqlToAstConversion(
                "Currently support only statement per request".to_string(),
            ));
        }

        match &ast[0] {
            Statement::CreateDatabase {
                db_name,
                if_not_exists,
                ..
            } => Self::from_create_database(db_name, *if_not_exists),
            Statement::CreateTable(create_table) => Self::from_create_table(create_table),

            Statement::Insert(insert) => Self::from_insert(insert),
            Statement::Query(query) => Self::from_query(query),

            Statement::Drop {
                object_type,
                if_exists,
                names,
                ..
            } => Self::from_drop(*object_type, *if_exists, names),

            statement => Err(Error::UnsupportedCommand(statement.to_string())),
        }
    }
}

/// Lower level representation of the Logical Plan.
#[derive(Debug, Clone, PartialEq)]
pub enum PhysicalPlan {
    /// No tasks need to be done. Skip.
    Skip,

    /// Create a database.
    CreateDatabase {
        name: String,
    },

    /// Create a table.
    CreateTable {
        name: TableDef,
        columns: Vec<ColumnDef>,
        settings: TableSettings,
        order_by: Vec<ColumnDef>,
        primary_key: Vec<ColumnDef>,
    },

    /// Insert values.
    Insert {
        table_def: TableDef,
        columns: Vec<OutputColumn>,
    },

    DropDatabase {
        name: String,
        if_exists: bool,
    },

    DropTable {
        name: TableDef,
        if_exists: bool,
    },

    /// Select columns from table.
    Select {
        scan_source: ScanSource,
        columns: Vec<Projection>,
        filter: Option<Box<Expr>>,
        sort_by: Option<Vec<Vec<Projection>>>,
        limit: Option<u64>,
        offset: u64,
    },
}

impl From<LogicalPlan> for PhysicalPlan {
    fn from(plan: LogicalPlan) -> Self {
        match plan {
            LogicalPlan::Skip => Self::Skip,
            LogicalPlan::CreateDatabase { name } => Self::CreateDatabase { name },
            LogicalPlan::CreateTable {
                name,
                columns,
                settings,
                order_by,
                primary_key,
            } => Self::CreateTable {
                name,
                columns,
                settings,
                order_by,
                primary_key,
            },
            LogicalPlan::Insert { table_def, columns } => Self::Insert { table_def, columns },
            LogicalPlan::DropDatabase { name, if_exists } => Self::DropDatabase { name, if_exists },
            LogicalPlan::DropTable { name, if_exists } => Self::DropTable { name, if_exists },

            LogicalPlan::Scan { source } => {
                Self::Select {
                    scan_source: source,
                    columns: Vec::new(), // to be filled,
                    filter: None,
                    sort_by: None,
                    limit: None,
                    offset: 0,
                }
            }
            plan @ (LogicalPlan::Projection { .. }
            | LogicalPlan::Filter { .. }
            | LogicalPlan::OrderBy { .. }
            | LogicalPlan::Limit { .. }) => {
                let mut current = plan;
                let mut columns = None;
                let mut filter = None;
                let mut sort_by = None;
                let mut limit = None;
                let mut offset = 0;

                loop {
                    match current {
                        LogicalPlan::Limit {
                            limit: limit_val,
                            offset: offset_val,
                            plan: inner,
                        } => {
                            limit = limit_val;
                            offset = offset_val;
                            current = *inner;
                        }
                        LogicalPlan::OrderBy {
                            column_defs,
                            plan: inner,
                        } => {
                            sort_by = Some(column_defs);
                            current = *inner;
                        }
                        LogicalPlan::Projection {
                            columns: cols,
                            plan: inner,
                        } => {
                            columns = Some(cols);
                            current = *inner;
                        }
                        LogicalPlan::Filter { expr, plan: inner } => {
                            filter = match filter {
                                None => Some(expr),
                                Some(value) => Some(Box::new(Expr::BinaryOp {
                                    left: value,
                                    op: BinaryOperator::And,
                                    right: expr,
                                })),
                            };
                            current = *inner;
                        }
                        LogicalPlan::Scan { source } => {
                            return Self::Select {
                                scan_source: source,
                                columns: columns.unwrap_or_default(),
                                filter,
                                sort_by,
                                limit,
                                offset,
                            };
                        }
                        unexpected => unreachable!("Unexpected plan node in query: {unexpected:?}"),
                    }
                }
            }
        }
    }
}

impl PhysicalPlan {
    pub fn get_complexity(&self) -> usize {
        match self {
            PhysicalPlan::Skip => 0,
            PhysicalPlan::CreateDatabase { .. }
            | PhysicalPlan::CreateTable { .. }
            | PhysicalPlan::DropDatabase { .. }
            | PhysicalPlan::DropTable { .. } => 1,
            PhysicalPlan::Insert { .. } => 2,
            PhysicalPlan::Select { .. } => 4,
        }
    }
}
