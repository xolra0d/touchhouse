mod command_runner;
mod compiled_filter;
mod execution;
mod logical_plan;
mod plan_optimization;
mod sql_parser;

pub use self::command_runner::CommandRunner;
pub use self::compiled_filter::{BinOp, CompiledFilter};
pub use self::sql_parser::{
    AggregateFunction, AggregateProjection, LogicalPlan, PhysicalPlan, Projection, ProjectionValue,
    RawProjection, ScanSource, SelectNode,
};

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
