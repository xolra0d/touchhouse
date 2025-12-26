mod command_runner;
mod compiled_filter;
mod execution;
mod logical_plan;
mod output_table;
mod plan_optimization;
mod sql_parser;

pub use command_runner::CommandRunner;
pub use output_table::{OutputColumn, OutputTable};
pub use sql_parser::{Projection, ProjectionValue};

use crate::error::{Error, Result};
use crate::storage::ColumnDef;

use sqlparser::ast::Ident;

/// Validates the name of fields, databases, columns.
///
/// Returns:
///   * `true` when: name is non-empty and consists only of ASCII alphanumeric characters or underscore.
///   * `false` when: name is empty or contains invalid characters.
pub fn validate_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// Parses an identifier and finds matching column definition.
///
/// Returns:
///   * Ok: `ColumnDef` when column with matching name is found.
///   * Error: `ColumnNotFound` when no matching column exists.
pub fn parse_column_def_ident(ident: &Ident, columns: &[ColumnDef]) -> Result<ColumnDef> {
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
pub mod tests {
    use crate::sql::validate_name;

    #[test]
    fn test_invalid_names() {
        assert!(!validate_name("*"));
        assert!(!validate_name("csji="));
        assert!(!validate_name("csji122yrd01/"));
        assert!(!validate_name(""));
    }

    #[test]
    fn test_valid_names() {
        assert!(validate_name("coffee_shop"));
        assert!(validate_name("amsterdam"));
        assert!(validate_name("John_Data"));
    }
}
