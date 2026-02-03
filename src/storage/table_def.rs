use sqlparser::ast::{ObjectName, ObjectNamePart};
use std::convert::TryFrom;
use std::fmt;
use std::path::PathBuf;

use crate::config::CONFIG;
use crate::error::{Error, Result};
use crate::storage::table_metadata::TABLE_METADATA_FILENAME;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TableDef {
    pub table: String,
    pub database: String,
}

impl fmt::Display for TableDef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.database, self.table)
    }
}

impl TableDef {
    /// Returns filesystem path for this table.
    pub fn get_path(&self) -> PathBuf {
        CONFIG.get_db_dir().join(&self.database).join(&self.table)
    }

    /// Checks if table exists by verifying database directory and `TABLE_METADATA_FILENAME` file.
    ///
    /// Returns: Ok or DatabaseNotFound/TableNotFound error
    pub fn exists_or_err(&self) -> Result<()> {
        let mut path = CONFIG.get_db_dir().join(&self.database);
        if !path.exists() {
            return Err(Error::DatabaseNotFound);
        }

        path.push(&self.table);
        path.push(TABLE_METADATA_FILENAME);

        if !path.exists() {
            return Err(Error::TableNotFound);
        }

        Ok(())
    }
}

impl TryFrom<&ObjectName> for TableDef {
    type Error = Error;
    fn try_from(object_name: &ObjectName) -> Result<Self> {
        let names = &object_name.0;
        if names.len() != 2 {
            return Err(Error::UnsupportedCommand(
                "You should provide table name in form `database_name.table_name`".to_string(),
            ));
        }

        let ObjectNamePart::Identifier(database) = &names[0] else {
            return Err(Error::UnsupportedCommand(
                "Currently unimplemented.".to_string(),
            ));
        };
        let database = database.value.clone();

        let ObjectNamePart::Identifier(table) = &names[1] else {
            return Err(Error::UnsupportedCommand(
                "Currently unimplemented.".to_string(),
            ));
        };
        let table = table.value.clone();

        let table_def = Self { table, database };

        Ok(table_def)
    }
}
