use crate::sql::sql_parser::LogicalPlan;

mod query_flattening;

impl LogicalPlan {
    /// Optimizes a logical plan by flattening nested structures.
    ///
    /// Merges subqueries, filters, projections, order by, and limit clauses.
    ///
    /// Returns: Optimized `LogicalPlan`.
    pub fn optimize(self) -> LogicalPlan {
        self.flatten()
    }
}
