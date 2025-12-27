use derive_more::Display;
use serde::Serialize;

pub type Result<T> = std::result::Result<T, Error>;

/// Universal error.
#[derive(Serialize, Debug, Display, PartialEq, Eq)]
pub enum Error {
    // mod storage
    #[display("System time went backword. Try again later.")]
    SystemTimeWentBackword,
    #[display("Database not found.")]
    DatabaseNotFound,
    #[display("Table not found.")]
    TableNotFound,
    #[display("Invalid database name.")]
    InvalidDatabaseName,
    #[display("Invalid column name: {_0}")]
    InvalidColumnName(String),
    #[display("Database already exists.")]
    DatabaseAlreadyExists,
    #[display("Table already exists.")]
    TableAlreadyExists,

    // mod sql
    #[display("Couldn't parse SQL: {_0}")]
    SqlToAstConversion(String),
    #[display("Unsupported command: {_0}.")]
    UnsupportedCommand(String),
    #[display("Unsupported column name: {_0}.")]
    UnsupportedColumnType(String),
    #[display("Invalid engine name.")]
    InvalidEngineName,
    #[display("Unsupported table option: {_0}")]
    UnsupportedTableOption(String),
    #[display("Invalid ORDER BY: {_0}")]
    InvalidOrderBy(String),
    #[display("Invalid PRIMARY KEY: {_0}")]
    InvalidPrimaryKey(String),
    #[display("Invalid pair of ORDER BY and PRIMARY KEY. PRIMARY KEY should be prefix of ORDER BY")]
    InvalidOrderByPrimaryKeyPair,
    #[display("Invalid table name.")]
    InvalidTableName,
    #[display("No columns specified.")]
    NoColumnsSpecified,
    #[display("Invalid columns specified.")]
    InvalidColumnsSpecified,
    #[display("Invalid source of values: {_0}")]
    InvalidSource(String),
    #[display("Unsupported column constraint: {_0}")]
    UnsupportedColumnConstraint(String),
    #[display("Could not insert data: {_0}.")]
    CouldNotInsertData(String),
    #[display("Could not read data: {_0}.")]
    CouldNotReadData(String),
    #[display("Could not create table: {_0}.")]
    CouldNotCreateTable(String),
    #[display("No values provided")]
    EmptySource,
    #[display("Permission denied")]
    PermissionDenied,
    #[display("Unsupported filter: {_0}")]
    UnsupportedFilter(String),
    #[display("Column not found: {_0}")]
    ColumnNotFound(String),
    #[display("Duplicate column in projection: {_0}")]
    DuplicateColumn(String),
    #[display("Invalid limit value: {_0}")]
    InvalidLimitValue(String),
    #[display("Invalid number of params specified: {_0}")]
    InvalidNumberOfParamsSpecified(String),
    #[display("Unknown function: {_0}")]
    UnknownFunction(String),
    #[display("Nested functions are not supported: {_0}")]
    UnsupportedNestedFunctions(String),
    #[display("Invalid function paramaters: {_0}")]
    InvalidFunctionParams(String),
    #[display("Part ({_0}) does not have any columns.")]
    PartDoesNotHaveColumns(String),

    // mod engines
    #[display("No ORDER BY columns found")]
    OrderByColumnsNotFound,

    // mod main
    SendResponse, // does not need display
    #[display("Internal error happened: {_0}")]
    Internal(String),
}
