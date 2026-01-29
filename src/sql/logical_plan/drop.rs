use crate::error::{Error, Result};
use crate::sql::LogicalPlan;
use crate::storage::TableDef;
use sqlparser::ast::{ObjectName, ObjectNamePart, ObjectType};

impl LogicalPlan {
    /// Parses DROP statement into a logical plan.
    ///
    /// Supports dropping tables and databases.
    ///
    /// Returns:
    ///   * Ok when:
    ///     1. Object type is Table and valid table name provided: `LogicalPlan::DropTable`.
    ///     2. Object type is Database and valid database name provided: `LogicalPlan::DropDatabase`.
    ///   * Error when:
    ///     1. Multiple names provided for table: `InvalidDatabaseName`.
    ///     2. Multiple names provided for database: `InvalidNumberOfParamsSpecified`.
    ///     3. Database name has multiple parts: `InvalidNumberOfParamsSpecified`.
    ///     4. Database name is not an identifier: `InvalidDatabaseName`.
    ///     5. Unsupported object type: `UnsupportedCommand`.
    pub fn from_drop(
        object_type: ObjectType,
        if_exists: bool,
        names: &[ObjectName],
    ) -> Result<Self> {
        match object_type {
            ObjectType::Table => {
                if names.len() != 1 {
                    return Err(Error::InvalidDatabaseName);
                }
                let name = &names[0];

                let table_def = TableDef::try_from(name)?;

                Ok(Self::DropTable {
                    name: table_def,
                    if_exists,
                })
            }
            ObjectType::Database => {
                if names.len() != 1 {
                    return Err(Error::InvalidNumberOfParamsSpecified(format!(
                        "expected only one database name, got {}",
                        names.len()
                    )));
                }
                let name = &names[0];

                if name.0.len() != 1 {
                    return Err(Error::InvalidNumberOfParamsSpecified(format!(
                        "expected only database name, got {:?}",
                        name.0
                    )));
                }
                let ObjectNamePart::Identifier(ident) = &name.0[0] else {
                    return Err(Error::InvalidDatabaseName);
                };

                Ok(Self::DropDatabase {
                    name: ident.value.clone(),
                    if_exists,
                })
            }
            _ => Err(Error::UnsupportedCommand(
                "Currently can drop databases and tables".to_string(),
            )),
        }
    }
}
