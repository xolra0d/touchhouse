use sqlparser::ast::{ObjectName, ObjectNamePart};

use crate::error::{Error, Result};
use crate::sql::{LogicalPlan, validate_name};

impl LogicalPlan {
    /// Creates a database as directory.
    ///
    /// Returns
    ///   * Ok when:
    ///     1. Folder (database) already exists and if `if_not_exists`.
    ///     2. Folder (database) does not exist.
    ///   * Error when:
    ///     1. Table name was specified also (e.g., `db_name.TABLE_NAME`): `InvalidDatabaseName`.
    ///     2. Function passed instead of name: `InvalidDatabaseName`.
    ///     3. Name has invalid characters: `InvalidDatabaseName`.
    ///     4. Folder (database) already exists and `IF NOT EXISTS` is not passed: `DatabaseAlreadyExists`.
    pub fn from_create_database(db_name: &ObjectName, if_not_exists: bool) -> Result<Self> {
        if db_name.0.len() != 1 {
            return Err(Error::InvalidDatabaseName);
        }

        let ObjectNamePart::Identifier(name) = &db_name.0[0] else {
            return Err(Error::InvalidDatabaseName);
        };

        let name = &name.value;
        if !validate_name(name) {
            return Err(Error::InvalidDatabaseName);
        }

        Ok(Self::CreateDatabase {
            name: name.clone(),
            if_not_exists,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::ast::Ident;

    fn build_from_string_one(name: &str) -> ObjectName {
        ObjectName(vec![ObjectNamePart::Identifier(Ident::new(
            name.to_string(),
        ))])
    }

    fn build_from_string_two(name: &str) -> ObjectName {
        ObjectName(vec![
            ObjectNamePart::Identifier(Ident::new(name.to_string())),
            ObjectNamePart::Identifier(Ident::new(name.to_string())),
        ])
    }

    #[test]
    fn test_create_database_invalid() {
        let mut invalid = build_from_string_one("invalid*");
        assert!(LogicalPlan::from_create_database(&invalid, false).is_err());
        assert!(LogicalPlan::from_create_database(&invalid, true).is_err());
        invalid = build_from_string_two("invalid`");
        assert!(LogicalPlan::from_create_database(&invalid, false).is_err());
        assert!(LogicalPlan::from_create_database(&invalid, true).is_err());
    }

    #[test]
    fn test_create_database_valid() {
        let mut valid = build_from_string_one("amsterdam_places");
        assert_eq!(
            LogicalPlan::from_create_database(&valid, false),
            Ok(LogicalPlan::CreateDatabase {
                name: "amsterdam_places".to_string(),
                if_not_exists: false
            })
        );

        valid = build_from_string_one("_all_Data144");

        assert_eq!(
            LogicalPlan::from_create_database(&valid, false),
            Ok(LogicalPlan::CreateDatabase {
                name: "_all_Data144".to_string(),
                if_not_exists: false
            })
        );
    }
}
