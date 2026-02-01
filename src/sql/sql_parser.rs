use log::error;
use serde::Serialize;
use sqlparser::ast::{
    Expr, Function as SQLFunction, FunctionArg, FunctionArgExpr, FunctionArguments, Ident,
    ObjectNamePart, Statement,
};
use sqlparser::dialect::ClickHouseDialect;
use sqlparser::parser::Parser;
use std::fmt;

use crate::error::{Error, Result};
use crate::storage::{ColumnDef, PhysicalColumn, TableDef, TableSettings, Value, ValueType};

/// Source for a Scan operation
#[derive(Debug, Clone, PartialEq)]
pub enum ScanSource<Plan> {
    Table(TableDef),
    Subquery(Box<Plan>),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum ProjectionValue {
    Value(Value),
    ColumnDef(ColumnDef),
}

impl ProjectionValue {
    pub fn get_col_def(&self) -> Option<&ColumnDef> {
        match self {
            ProjectionValue::ColumnDef(col_def) => Some(col_def),
            ProjectionValue::Value(_) => None,
        }
    }

    pub fn get_field_type(&self) -> ValueType {
        match self {
            ProjectionValue::Value(value) => value.get_type(),
            ProjectionValue::ColumnDef(col_def) => col_def.field_type.clone(),
        }
    }

    pub fn add_aggr_fn_around(self, name: &str) -> Self {
        match self {
            Self::Value(value) => Self::Value(Value::String(format!("{name}({value})"))),
            Self::ColumnDef(col_def) => Self::Value(Value::String(format!("{name}({col_def})"))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Projection {
    pub alias: Option<String>,
    pub source: ProjectionValue,
}

impl Projection {
    pub fn try_from_ident(ident: &Ident, available_projections: &[Self]) -> Result<Self> {
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
}

impl From<ColumnDef> for Projection {
    fn from(value: ColumnDef) -> Self {
        Projection {
            alias: None,
            source: ProjectionValue::ColumnDef(value),
        }
    }
}

impl fmt::Display for ProjectionValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::result::Result<(), fmt::Error> {
        match &self {
            Self::Value(value) => write!(f, "{value}"),
            Self::ColumnDef(col_def) => write!(f, "{}", col_def.name),
        }
    }
}

impl fmt::Display for Projection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::result::Result<(), fmt::Error> {
        if let Some(alias) = &self.alias {
            return write!(f, "{alias}");
        }
        self.source.fmt(f)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AggregateProjection {
    pub alias: Option<String>,
    pub source: AggregateFunction,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum AggregateFunction {
    Min(Projection),
    Max(Projection),
    Sum(Projection),
    Avg(Projection),
    Count(Projection),
}

impl AggregateFunction {
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

                Ok(AggregateFunction::Min(args.remove(0)))
            }
            "max" => {
                if args.len() != 1 {
                    return Err(Error::InvalidFunctionParams(format!(
                        "for `max` function expected only 1 argument, but received: {}",
                        args.len()
                    )));
                }

                Ok(AggregateFunction::Max(args.remove(0)))
            }
            "sum" => {
                if args.len() != 1 {
                    return Err(Error::InvalidFunctionParams(format!(
                        "for `sum` function expected only 1 argument, but received: {}",
                        args.len()
                    )));
                }

                Ok(AggregateFunction::Sum(args.remove(0)))
            }
            "avg" => {
                if args.len() != 1 {
                    return Err(Error::InvalidFunctionParams(format!(
                        "for `avg` function expected only 1 argument, but received: {}",
                        args.len()
                    )));
                }

                Ok(AggregateFunction::Avg(args.remove(0)))
            }
            "count" => {
                if args.len() != 1 {
                    return Err(Error::InvalidFunctionParams(format!(
                        "for `count` function expected only 1 argument, but received: {}",
                        args.len()
                    )));
                }

                Ok(AggregateFunction::Count(args.remove(0)))
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
                                match RawProjection::try_from(expr, available_projections)? {
                                    RawProjection::Projection(proj) => proj,
                                    RawProjection::AggregateProjection(aggr_proj) => {
                                        return Err(Error::InvalidSource(format!(
                                            "Expected projection in ORDER BY, got ({aggr_proj:?}) instead.",
                                        )));
                                    }
                                }
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

/// Struct for converting any `sql_parser::Expr` projection inside of `sql_parser::Query` into either `Projection` or `AggregateProjection`
pub enum RawProjection {
    Projection(Projection),
    AggregateProjection(AggregateProjection),
}

impl RawProjection {
    pub fn try_from(expr: &Expr, available_projections: &[Projection]) -> Result<Self> {
        match expr {
            Expr::Identifier(ident) => {
                let projection = Projection::try_from_ident(ident, available_projections)?;
                Ok(Self::Projection(projection))
            }
            Expr::Value(value) => {
                let value = Value::try_from_untyped(value.value.clone())?;
                Ok(Self::Projection(Projection {
                    source: ProjectionValue::Value(value),
                    alias: None,
                }))
            }
            Expr::Function(function) => Ok(Self::AggregateProjection(AggregateProjection {
                source: AggregateFunction::try_parse(function, available_projections)?,
                alias: None,
            })),
            _ => Err(Error::UnsupportedCommand(
                "Only column identifiers and values are supported in projections".to_string(),
            )),
        }
    }

    pub fn set_alias(&mut self, alias: String) {
        match self {
            Self::Projection(proj) => proj.alias = Some(alias),
            Self::AggregateProjection(proj) => proj.alias = Some(alias),
        }
    }
}

/// High level representation of the SQL query.
#[derive(Debug, Clone, PartialEq)]
pub enum LogicalPlan {
    /// Create a database.
    CreateDatabase {
        name: String,
        if_not_exists: bool,
    },

    /// Create a table.
    CreateTable {
        name: TableDef,
        if_not_exists: bool,
        columns: Vec<ColumnDef>,
        settings: TableSettings,
        order_by: Vec<ColumnDef>,
        primary_key: Vec<ColumnDef>,
    },

    /// Insert values.
    Insert {
        table_def: TableDef,
        columns: Vec<PhysicalColumn>,
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
        source: ScanSource<LogicalPlan>,
    },

    Projection {
        projs: Vec<Projection>,
        plan: Box<LogicalPlan>,
    },

    Filter {
        expr: Box<Expr>,
        plan: Box<LogicalPlan>,
    },

    OrderBy {
        projs: Vec<Vec<Projection>>,
        plan: Box<LogicalPlan>,
    },

    Limit {
        limit: Option<usize>,
        offset: usize, // default 0
        plan: Box<LogicalPlan>,
    },

    Aggregate {
        aggr_proj: Vec<AggregateProjection>,
        group_by: Vec<Projection>, // todo: if GROUP BY ALL, then just store it as variant, like GroupBy::All, or GroupBy::Projections(Vec<Projection>)
        plan: Box<LogicalPlan>,
    },

    Having {
        expr: Box<Expr>,
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
    /// Create a database.
    CreateDatabase {
        name: String,
        if_not_exists: bool,
    },

    /// Create a table.
    CreateTable {
        name: TableDef,
        if_not_exists: bool,
        columns: Vec<ColumnDef>,
        settings: TableSettings,
        order_by: Vec<ColumnDef>,
        primary_key: Vec<ColumnDef>,
    },

    /// Insert values.
    Insert {
        table_def: TableDef,
        columns: Vec<PhysicalColumn>,
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
    Select(SelectNode),
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelectNode {
    pub scan_source: ScanSource<SelectNode>,
    pub columns: Vec<Projection>,
    pub filter: Option<Box<Expr>>,
    pub aggregate_cols: Vec<AggregateProjection>,
    pub group_by: Vec<Projection>,
    pub having: Option<Box<Expr>>,
    pub order_by: Option<Vec<Vec<Projection>>>,
    pub limit: Option<usize>,
    pub offset: usize,
}

impl TryFrom<LogicalPlan> for SelectNode {
    type Error = Error;
    fn try_from(mut current_plan: LogicalPlan) -> Result<Self> {
        let mut columns = Vec::new();
        let mut filter = None;
        let mut aggregate_cols = Vec::new();
        let mut group_by = Vec::new();
        let mut having = None;
        let mut order_by = None;
        let mut limit = None;
        let mut offset = 0;

        loop {
            match current_plan {
                LogicalPlan::Scan { source } => {
                    let inner = match source {
                        ScanSource::Table(table_def) => ScanSource::Table(table_def),
                        ScanSource::Subquery(plan) => {
                            ScanSource::Subquery(Box::new(SelectNode::try_from(*plan)?))
                        }
                    };

                    return Ok(SelectNode {
                        scan_source: inner,
                        columns,
                        filter,
                        aggregate_cols,
                        group_by,
                        having,
                        order_by,
                        limit,
                        offset,
                    });
                }
                LogicalPlan::Projection { projs, plan } => {
                    columns = projs;
                    current_plan = *plan;
                }
                LogicalPlan::Filter { expr, plan } => {
                    filter = Some(expr);
                    current_plan = *plan;
                }
                LogicalPlan::OrderBy { projs, plan } => {
                    order_by = Some(projs);
                    current_plan = *plan;
                }
                LogicalPlan::Limit {
                    limit: limit_inner,
                    offset: offset_inner,
                    plan,
                } => {
                    limit = limit_inner;
                    offset = offset_inner;
                    current_plan = *plan;
                }
                LogicalPlan::Aggregate {
                    aggr_proj,
                    group_by: group_by_inner,
                    plan,
                } => {
                    aggregate_cols = aggr_proj;
                    group_by = group_by_inner;
                    current_plan = *plan;
                }
                LogicalPlan::Having { expr, plan } => {
                    having = Some(expr);
                    current_plan = *plan;
                }
                LogicalPlan::CreateDatabase { .. }
                | LogicalPlan::CreateTable { .. }
                | LogicalPlan::Insert { .. }
                | LogicalPlan::DropDatabase { .. }
                | LogicalPlan::DropTable { .. } => {
                    error!("Unexpected logical node: {current_plan:?}");
                    return Err(Error::Internal(format!(
                        "Unexpected logical node: {current_plan:?}"
                    )));
                }
            }
        }
    }
}

impl TryFrom<LogicalPlan> for PhysicalPlan {
    type Error = Error;
    fn try_from(plan: LogicalPlan) -> Result<Self> {
        match plan {
            LogicalPlan::CreateDatabase {
                name,
                if_not_exists,
            } => Ok(Self::CreateDatabase {
                name,
                if_not_exists,
            }),
            LogicalPlan::CreateTable {
                name,
                if_not_exists,
                columns,
                settings,
                order_by,
                primary_key,
            } => Ok(Self::CreateTable {
                name,
                if_not_exists,
                columns,
                settings,
                order_by,
                primary_key,
            }),
            LogicalPlan::Insert { table_def, columns } => Ok(Self::Insert { table_def, columns }),
            LogicalPlan::DropDatabase { name, if_exists } => {
                Ok(Self::DropDatabase { name, if_exists })
            }
            LogicalPlan::DropTable { name, if_exists } => Ok(Self::DropTable { name, if_exists }),
            LogicalPlan::Scan { .. }
            | LogicalPlan::Projection { .. }
            | LogicalPlan::Filter { .. }
            | LogicalPlan::OrderBy { .. }
            | LogicalPlan::Limit { .. }
            | LogicalPlan::Aggregate { .. }
            | LogicalPlan::Having { .. } => Ok(Self::Select(SelectNode::try_from(plan)?)),
        }
    }
}

impl PhysicalPlan {
    pub fn get_complexity(&self) -> usize {
        match self {
            PhysicalPlan::CreateDatabase { .. }
            | PhysicalPlan::CreateTable { .. }
            | PhysicalPlan::DropDatabase { .. }
            | PhysicalPlan::DropTable { .. } => 1,
            PhysicalPlan::Insert { .. } => 2,
            PhysicalPlan::Select { .. } => 4,
        }
    }
}
