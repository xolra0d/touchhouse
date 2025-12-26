use sqlparser::ast::{BinaryOperator, Expr, Statement};
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

#[derive(Debug, Clone, PartialEq)]
pub enum ProjectionValue {
    Value(Value),
    ColumnDef(ColumnDef),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Projection {
    pub alias: Option<String>,
    pub source: ProjectionValue,
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
            } => Self::from_drop(object_type, *if_exists, names),

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
