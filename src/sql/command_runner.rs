use crate::error::Result;
use crate::runtime_config::{ComplexityGuard, DATABASE_LOAD};
use crate::sql::{LogicalPlan, PhysicalPlan};
use crate::storage::{OutputColumn, OutputTable};

/// Main runner struct which executes received command.
#[derive(Debug)]
pub struct CommandRunner;

impl CommandRunner {
    /// Handles full command execution pipeline.
    ///
    /// Parses SQL, optimizes logical plan, converts to physical plan, and executes.
    ///
    /// Returns:
    ///   * Ok: `OutputTable` with query results or success status.
    ///   * Error: Any error from parsing, optimization, or execution stages.
    pub fn execute_command(command: &str) -> Result<OutputTable> {
        let start = std::time::Instant::now();

        let logical_plan = LogicalPlan::try_from(command)?;

        let logical_plan = logical_plan.optimize();

        let physical_plan = PhysicalPlan::try_from(logical_plan)?;

        let complexity = physical_plan.get_complexity();
        DATABASE_LOAD.fetch_add(complexity, std::sync::atomic::Ordering::Relaxed);
        let _guard = ComplexityGuard::new(complexity);

        let output_columns = Self::execute_physical_plan(physical_plan)?;

        Ok(OutputTable::new(output_columns, start.elapsed()))
    }

    /// Executes a physical plan by dispatching to appropriate handler.
    ///
    /// Returns:
    ///   * Ok: `OutputTable` with query results or success status.
    ///   * Error: Handler-specific errors (e.g., `TableNotFound`, `CouldNotInsertData`).
    pub fn execute_physical_plan(plan: PhysicalPlan) -> Result<Vec<OutputColumn>> {
        match plan {
            PhysicalPlan::CreateDatabase {
                name,
                if_not_exists,
            } => Self::create_database(name, if_not_exists),
            PhysicalPlan::CreateTable {
                name: table_def,
                if_not_exists,
                columns,
                settings,
                order_by,
                primary_key,
            } => Self::create_table(
                &table_def,
                if_not_exists,
                columns,
                settings,
                order_by,
                primary_key,
            ),
            PhysicalPlan::Insert { table_def, columns } => Self::insert(&table_def, columns),
            PhysicalPlan::DropDatabase { name, if_exists } => Self::drop_database(&name, if_exists),
            PhysicalPlan::DropTable { name, if_exists } => Self::drop_table(&name, if_exists),
            PhysicalPlan::Select(select_node) => Self::select(select_node),
        }
    }
}
