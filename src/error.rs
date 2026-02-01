use std::fmt;

use serde::Serialize;

pub type Result<T> = std::result::Result<T, Error>;

/// Universal error.
#[derive(Serialize, Debug, PartialEq, Eq)]
pub enum Error {
    // mod storage
    SystemTimeWentBackword,
    DatabaseNotFound,
    TableNotFound,
    InvalidDatabaseName,
    InvalidColumnName(String),
    DatabaseAlreadyExists,
    TableAlreadyExists,
    // mod sql
    SqlToAstConversion(String),
    UnsupportedCommand(String),
    UnsupportedColumnType(String),
    InvalidEngineName,
    UnsupportedTableOption(String),
    InvalidOrderBy(String),
    InvalidPrimaryKey(String),
    InvalidOrderByPrimaryKeyPair,
    InvalidTableName,
    NoColumnsSpecified,
    InvalidColumnsSpecified,
    InvalidSource(String),
    UnsupportedColumnConstraint(String),
    CouldNotInsertData(String),
    CouldNotReadData(String),
    CouldNotCreateTable(String),
    EmptySource,
    PermissionDenied,
    UnsupportedFilter(String),
    ColumnNotFound(String),
    DuplicateColumn(String),
    InvalidLimitValue(String),
    InvalidNumberOfParamsSpecified(String),
    UnknownFunction(String),
    UnsupportedNestedFunctions(String),
    InvalidFunctionParams(String),
    PartDoesNotHaveColumns(String),
    ColumnNotInGroupBy(String),
    // mod engines
    OrderByColumnsNotFound,
    // mod main
    SendResponse,
    Internal(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // mod storage
            Error::SystemTimeWentBackword => {
                write!(f, "System time went backword. Try again later.")
            }
            Error::DatabaseNotFound => write!(f, "Database not found."),
            Error::TableNotFound => write!(f, "Table not found."),
            Error::InvalidDatabaseName => write!(f, "Invalid database name."),
            Error::InvalidColumnName(name) => write!(f, "Invalid column name: {name}"),
            Error::DatabaseAlreadyExists => write!(f, "Database already exists."),
            Error::TableAlreadyExists => write!(f, "Table already exists."),
            // mod sql
            Error::SqlToAstConversion(msg) => write!(f, "Couldn't parse SQL: {msg}"),
            Error::UnsupportedCommand(cmd) => write!(f, "Unsupported command: {cmd}."),
            Error::UnsupportedColumnType(typ) => write!(f, "Unsupported column name: {typ}."),
            Error::InvalidEngineName => write!(f, "Invalid engine name."),
            Error::UnsupportedTableOption(opt) => write!(f, "Unsupported table option: {opt}"),
            Error::InvalidOrderBy(msg) => write!(f, "Invalid ORDER BY: {msg}"),
            Error::InvalidPrimaryKey(msg) => write!(f, "Invalid PRIMARY KEY: {msg}"),
            Error::InvalidOrderByPrimaryKeyPair => write!(
                f,
                "Invalid pair of ORDER BY and PRIMARY KEY. PRIMARY KEY should be prefix of ORDER BY"
            ),
            Error::InvalidTableName => write!(f, "Invalid table name."),
            Error::NoColumnsSpecified => write!(f, "No columns specified."),
            Error::InvalidColumnsSpecified => write!(f, "Invalid columns specified."),
            Error::InvalidSource(src) => write!(f, "Invalid source of values: {src}"),
            Error::UnsupportedColumnConstraint(con) => {
                write!(f, "Unsupported column constraint: {con}")
            }
            Error::CouldNotInsertData(msg) => write!(f, "Could not insert data: {msg}."),
            Error::CouldNotReadData(msg) => write!(f, "Could not read data: {msg}."),
            Error::CouldNotCreateTable(msg) => write!(f, "Could not create table: {msg}."),
            Error::EmptySource => write!(f, "No values provided"),
            Error::PermissionDenied => write!(f, "Permission denied"),
            Error::UnsupportedFilter(flt) => write!(f, "Unsupported filter: {flt}"),
            Error::ColumnNotFound(col) => write!(f, "Column not found: {col}"),
            Error::DuplicateColumn(col) => write!(f, "Duplicate column in projection: {col}"),
            Error::InvalidLimitValue(val) => write!(f, "Invalid limit value: {val}"),
            Error::InvalidNumberOfParamsSpecified(msg) => {
                write!(f, "Invalid number of params specified: {msg}")
            }
            Error::UnknownFunction(func) => write!(f, "Unknown function: {func}"),
            Error::UnsupportedNestedFunctions(msg) => {
                write!(f, "Nested functions are not supported: {msg}")
            }
            Error::InvalidFunctionParams(msg) => write!(f, "Invalid function paramaters: {msg}"),
            Error::PartDoesNotHaveColumns(part) => {
                write!(f, "Part ({part}) does not have any columns.")
            }
            Error::ColumnNotInGroupBy(col) => write!(
                f,
                "Column ({col}) is not under aggregate function and not in GROUP BY keys."
            ),
            // mod engines
            Error::OrderByColumnsNotFound => write!(f, "No ORDER BY columns found"),
            // mod main
            Error::SendResponse => write!(f, "SendResponse"),
            Error::Internal(msg) => write!(f, "Internal error happened: {msg}"),
        }
    }
}
