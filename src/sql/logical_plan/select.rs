use crate::error::{Error, Result};
use crate::runtime_config::TABLE_DATA;
use crate::sql::{
    AggregateProjection, LogicalPlan, Projection, ProjectionValue, RawProjection, ScanSource,
};
use crate::storage::TableDef;

use sqlparser::ast::{
    Expr, GroupByExpr, LimitClause, Offset, OrderBy, OrderByKind, Query, SelectItem, SetExpr,
    TableFactor, Value as SQLValue,
};

impl LogicalPlan {
    /// Parses SELECT query into a logical plan tree.
    ///
    /// Builds a tree of `LogicalPlan` nodes: Scan -> Filter -> Projection -> `OrderBy` -> Limit.
    ///
    /// Returns:
    ///   * Ok when:
    ///     1. Query has single FROM table/subquery, valid projections, optional WHERE/ORDER BY/LIMIT: `LogicalPlan` tree
    ///   * Error when:
    ///     1. Query is not a SELECT statement: `UnsupportedCommand`.
    ///     2. Multiple tables in FROM clause: `UnsupportedCommand`.
    ///     3. JOIN clause present: `UnsupportedCommand`.
    ///     4. Empty projection: `UnsupportedCommand`.
    ///     5. Multiple wildcards or columns after wildcard: `UnsupportedCommand`.
    ///     6. Non-identifier expressions in projection: `UnsupportedCommand`.
    ///     7. Duplicate column in projection: `DuplicateColumn`.
    ///     8. Column not found in table: `ColumnNotFound`.
    ///     9. Invalid LIMIT/OFFSET value: `InvalidLimitValue`.
    pub fn from_query(query: &Query) -> Result<Self> {
        let SetExpr::Select(select) = &*query.body else {
            return Err(Error::UnsupportedCommand(
                "Only SELECT queries are supported".to_string(),
            ));
        };

        Self::validate_from_clause(select)?;

        let scan_source = Self::build_scan_source(&select.from[0])?;
        let mut plan = Self::Scan {
            source: scan_source,
        };

        let available_projections = Self::extract_columns_from_plan(&plan)?;
        let (read_columns, aggregate_projections) =
            Self::parse_projections(&select.projection, &available_projections)?;

        plan = Self::apply_filter(plan, select.selection.as_ref());
        plan = Self::apply_projection(plan, read_columns.clone());
        plan = Self::apply_group_by(
            plan,
            &select.group_by,
            read_columns.clone(),
            aggregate_projections,
            &available_projections,
        )?;
        plan = Self::apply_having(plan, select.having.as_ref());
        plan = Self::apply_order_by(
            plan,
            query.order_by.as_ref(),
            &read_columns,
            &available_projections,
        )?;
        plan = Self::apply_limit(plan, query.limit_clause.as_ref())?;

        Ok(plan)
    }

    fn validate_from_clause(select: &sqlparser::ast::Select) -> Result<()> {
        if select.from.len() != 1 {
            return Err(Error::UnsupportedCommand(
                "Currently do not support multiple table selects".to_string(),
            ));
        }

        let table = &select.from[0];
        if !table.joins.is_empty() {
            return Err(Error::UnsupportedCommand(
                "JOIN clauses are not currently supported".to_string(),
            ));
        }

        Ok(())
    }

    fn build_scan_source(
        table: &sqlparser::ast::TableWithJoins,
    ) -> Result<ScanSource<LogicalPlan>> {
        match &table.relation {
            TableFactor::Table { name, .. } => {
                let table_def = TableDef::try_from(name)?;
                Ok(ScanSource::Table(table_def))
            }
            TableFactor::Derived { subquery, .. } => {
                let subquery_plan = Self::from_query(subquery)?;
                Ok(ScanSource::Subquery(Box::new(subquery_plan)))
            }
            _ => Err(Error::UnsupportedCommand(
                "Only simple table references and subqueries are supported".to_string(),
            )),
        }
    }

    fn parse_projections(
        projection_items: &[SelectItem],
        available_projections: &Vec<Projection>,
    ) -> Result<(Vec<Projection>, Vec<AggregateProjection>)> {
        if projection_items.is_empty() {
            return Err(Error::UnsupportedCommand(
                "No projection specified.".to_string(),
            ));
        }

        let mut read_columns: Vec<Projection> = Vec::with_capacity(projection_items.len() / 2);
        let mut aggregate_projections: Vec<AggregateProjection> =
            Vec::with_capacity(projection_items.len() / 2);
        let mut wildcard = None;

        for (idx, projection) in projection_items.iter().enumerate() {
            match projection {
                SelectItem::Wildcard(_) => {
                    if wildcard.is_some() {
                        return Err(Error::UnsupportedCommand(
                            "Multiple wildcards are not supported".to_string(),
                        ));
                    }
                    wildcard = Some(idx);
                }
                SelectItem::UnnamedExpr(expr) => {
                    Self::validate_no_wildcard_before(wildcard)?;
                    let (proj, aggr) =
                        Self::parse_projection_expr(expr, None, available_projections)?;
                    Self::add_projection(proj, aggr, &mut read_columns, &mut aggregate_projections);
                }
                SelectItem::ExprWithAlias { expr, alias } => {
                    Self::validate_no_wildcard_before(wildcard)?;
                    let (proj, aggr) = Self::parse_projection_expr(
                        expr,
                        Some(alias.value.clone()),
                        available_projections,
                    )?;
                    Self::add_projection(proj, aggr, &mut read_columns, &mut aggregate_projections);
                }
                SelectItem::QualifiedWildcard(..) => {
                    return Err(Error::UnsupportedCommand(
                        "Only simple column projections and wildcards are supported".to_string(),
                    ));
                }
            }
        }

        Self::expand_wildcard(wildcard, &mut read_columns, available_projections);

        Ok((read_columns, aggregate_projections))
    }

    fn validate_no_wildcard_before(wildcard: Option<usize>) -> Result<()> {
        if wildcard.is_some() {
            return Err(Error::UnsupportedCommand(
                "Columns after wildcard are not supported".to_string(),
            ));
        }
        Ok(())
    }

    fn parse_projection_expr(
        expr: &Expr,
        alias: Option<String>,
        available_projections: &[Projection],
    ) -> Result<(Option<Projection>, Option<AggregateProjection>)> {
        let mut raw_projection = RawProjection::try_from(expr, available_projections)?;
        if let Some(alias) = alias {
            raw_projection.set_alias(alias);
        }

        match raw_projection {
            RawProjection::Projection(projection) => Ok((Some(projection), None)),
            RawProjection::AggregateProjection(projection) => Ok((None, Some(projection))),
        }
    }

    fn add_projection(
        proj: Option<Projection>,
        aggr: Option<AggregateProjection>,
        read_columns: &mut Vec<Projection>,
        aggregate_projections: &mut Vec<AggregateProjection>,
    ) {
        if let Some(proj) = proj {
            read_columns.push(proj);
        }
        if let Some(aggr) = aggr {
            aggregate_projections.push(aggr);
        }
    }

    fn expand_wildcard(
        wildcard: Option<usize>,
        read_columns: &mut Vec<Projection>,
        available_projections: &Vec<Projection>,
    ) {
        if let Some(idx) = wildcard {
            if idx == 0 {
                read_columns.clone_from(available_projections);
            } else {
                for proj in available_projections {
                    if !read_columns.contains(proj) {
                        read_columns.push(proj.clone());
                    }
                }
            }
        }
    }

    fn apply_filter(plan: LogicalPlan, selection: Option<&Expr>) -> LogicalPlan {
        match selection {
            Some(expr) => LogicalPlan::Filter {
                expr: Box::new(expr.clone()),
                plan: Box::new(plan),
            },
            None => plan,
        }
    }

    fn apply_projection(plan: LogicalPlan, projs: Vec<Projection>) -> LogicalPlan {
        LogicalPlan::Projection {
            projs,
            plan: Box::new(plan),
        }
    }

    fn apply_group_by(
        plan: LogicalPlan,
        group_by: &GroupByExpr,
        read_columns: Vec<Projection>,
        aggregate_projections: Vec<AggregateProjection>,
        available_projections: &[Projection],
    ) -> Result<LogicalPlan> {
        match group_by {
            GroupByExpr::All(modifiers) => {
                Self::validate_no_modifiers(modifiers)?;
                Ok(LogicalPlan::Aggregate {
                    aggr_proj: aggregate_projections,
                    group_by: read_columns,
                    plan: Box::new(plan),
                })
            }
            GroupByExpr::Expressions(expressions, modifiers) => {
                Self::validate_no_modifiers(modifiers)?;

                if expressions.is_empty() && aggregate_projections.is_empty() {
                    return Ok(plan);
                }

                let group_by =
                    Self::parse_group_by_expressions(expressions, available_projections)?;
                Self::validate_all_columns_in_group_by(&read_columns, &group_by)?;

                Ok(LogicalPlan::Aggregate {
                    aggr_proj: aggregate_projections,
                    group_by,
                    plan: Box::new(plan),
                })
            }
        }
    }

    fn validate_no_modifiers(modifiers: &[sqlparser::ast::GroupByWithModifier]) -> Result<()> {
        if !modifiers.is_empty() {
            return Err(Error::UnsupportedCommand(format!(
                "Modifiers ({modifiers:?}) are not supported in GROUP BY clause"
            )));
        }
        Ok(())
    }

    fn parse_group_by_expressions(
        expressions: &[Expr],
        available_projections: &[Projection],
    ) -> Result<Vec<Projection>> {
        let mut group_by = Vec::with_capacity(expressions.len());

        for expr in expressions {
            let proj = match RawProjection::try_from(expr, available_projections)? {
                RawProjection::Projection(proj) => proj,
                RawProjection::AggregateProjection(aggr_proj) => {
                    return Err(Error::InvalidSource(format!(
                        "Expected projection in GROUP BY, got ({aggr_proj:?}) instead.",
                    )));
                }
            };
            group_by.push(proj);
        }

        Ok(group_by)
    }

    fn validate_all_columns_in_group_by(
        read_columns: &[Projection],
        group_by: &[Projection],
    ) -> Result<()> {
        if let Some(proj) = read_columns.iter().find(|proj| !group_by.contains(proj)) {
            return Err(Error::ColumnNotInGroupBy(proj.to_string()));
        }
        Ok(())
    }

    fn apply_having(plan: LogicalPlan, having: Option<&Expr>) -> LogicalPlan {
        match having {
            Some(expr) => LogicalPlan::Having {
                expr: Box::new(expr.clone()),
                plan: Box::new(plan),
            },
            None => plan,
        }
    }

    fn apply_order_by(
        plan: LogicalPlan,
        order_by: Option<&OrderBy>,
        read_columns: &[Projection],
        available_projections: &[Projection],
    ) -> Result<LogicalPlan> {
        let Some(order_by) = order_by else {
            return Ok(plan);
        };

        let projs = match &order_by.kind {
            OrderByKind::All(_params) => vec![read_columns.to_vec()],
            OrderByKind::Expressions(order_by_given) => {
                Self::parse_order_by_expressions(order_by_given, available_projections)?
            }
        };

        Ok(LogicalPlan::OrderBy {
            projs,
            plan: Box::new(plan),
        })
    }

    fn parse_order_by_expressions(
        order_by_given: &[sqlparser::ast::OrderByExpr],
        available_projections: &[Projection],
    ) -> Result<Vec<Vec<Projection>>> {
        let mut order_by_all = Vec::with_capacity(order_by_given.len());
        for order_by_expr in order_by_given {
            let order_by_cols = Self::parse_order_by(&order_by_expr.expr, available_projections)?;
            order_by_all.push(order_by_cols);
        }
        Ok(order_by_all)
    }

    fn apply_limit(plan: LogicalPlan, limit_clause: Option<&LimitClause>) -> Result<LogicalPlan> {
        let Some(limit_clause) = limit_clause else {
            return Ok(plan);
        };

        let LimitClause::LimitOffset {
            limit: limit_expr,
            offset: offset_expr,
            ..
        } = limit_clause
        else {
            return Err(Error::InvalidLimitValue(
                "Only LIMIT OFFSET clause is supported".to_string(),
            ));
        };

        let limit = Self::parse_limit_value(limit_expr.as_ref())?;
        let offset = Self::parse_offset_value(offset_expr.as_ref())?;

        Ok(LogicalPlan::Limit {
            limit,
            offset,
            plan: Box::new(plan),
        })
    }

    fn parse_limit_value(limit_expr: Option<&Expr>) -> Result<Option<usize>> {
        let Some(limit_expr) = limit_expr else {
            return Ok(None);
        };

        let Expr::Value(limit_expr) = limit_expr else {
            return Err(Error::InvalidLimitValue(
                "LIMIT must be a literal value".to_string(),
            ));
        };

        let SQLValue::Number(limit_expr, _) = &limit_expr.value else {
            return Err(Error::InvalidLimitValue(
                "LIMIT must be a number".to_string(),
            ));
        };

        let limit = limit_expr
            .parse()
            .map_err(|_| Error::InvalidLimitValue(limit_expr.clone()))?;

        Ok(Some(limit))
    }

    fn parse_offset_value(offset_expr: Option<&Offset>) -> Result<usize> {
        let Some(offset_expr) = offset_expr else {
            return Ok(0);
        };

        let Expr::Value(offset_expr) = &offset_expr.value else {
            return Err(Error::InvalidLimitValue(
                "OFFSET must be a literal value".to_string(),
            ));
        };

        let SQLValue::Number(offset_expr, _) = &offset_expr.value else {
            return Err(Error::InvalidLimitValue(
                "OFFSET must be a number".to_string(),
            ));
        };

        offset_expr
            .parse()
            .map_err(|_| Error::InvalidLimitValue(offset_expr.clone()))
    }

    /// Extracts column definitions from a logical plan.
    ///
    /// Recursively traverses the plan tree to find available columns.
    ///
    /// Returns:
    ///   * Ok when:
    ///     1. Plan is Projection: columns from projection.
    ///     2. Plan is Filter/OrderBy/Limit: columns from inner plan.
    ///     3. Plan is Scan with Table: columns from table metadata.
    ///     4. Plan is Scan with Subquery: columns from subquery plan.
    ///   * Error when:
    ///     1. Table not found in runtime config: `TableNotFound`.
    ///     2. Unsupported plan type: `UnsupportedCommand`.
    fn extract_columns_from_plan(plan: &LogicalPlan) -> Result<Vec<Projection>> {
        match plan {
            LogicalPlan::Projection { projs: columns, .. } => Ok(columns.clone()),
            LogicalPlan::Filter { plan, .. }
            | LogicalPlan::OrderBy { plan, .. }
            | LogicalPlan::Limit { plan, .. } => Self::extract_columns_from_plan(plan),
            LogicalPlan::Scan { source } => match source {
                ScanSource::Table(table_def) => {
                    let Some(table_config) = TABLE_DATA.get(table_def) else {
                        return Err(Error::TableNotFound);
                    };
                    Ok(table_config
                        .metadata
                        .schema
                        .columns
                        .iter()
                        .map(|col_def| Projection {
                            alias: None,
                            source: ProjectionValue::ColumnDef(col_def.clone()),
                        })
                        .collect())
                }
                ScanSource::Subquery(subquery_plan) => {
                    Self::extract_columns_from_plan(subquery_plan)
                }
            },
            _ => Err(Error::UnsupportedCommand(
                "Cannot extract columns from this plan type".to_string(),
            )),
        }
    }

    /// Tries to parse ORDER BY columns.
    ///
    /// Returns
    ///   * Ok when:
    ///     1. All columns are unique, exist in the pool of ALL columns: `Vec<ColumnDef>`
    ///   * Error when:
    //     1. If no ORDER BY was provided: `InvalidOrderBy`.
    //     2. If ORDER BY is empty: `InvalidOrderBy`.
    //     3. If column name is not an identifier: `InvalidOrderBy`.
    //     4. If column, not found in all columns, is found in ORDER BY: `InvalidOrderBy`.
    //     5. If the same column is added: `InvalidOrderBy`.
    pub fn parse_order_by(order_by: &Expr, projections: &[Projection]) -> Result<Vec<Projection>> {
        match order_by {
            Expr::Identifier(order_by) => {
                Projection::try_from_ident(order_by, projections).map(|x| vec![x])
            }
            Expr::Tuple(order_by) => {
                let mut order_by_total = Vec::with_capacity(order_by.len());
                for key in order_by {
                    let Expr::Identifier(ident) = key else {
                        return Err(Error::InvalidOrderBy(format!("Invalid specifier: {key}")));
                    };
                    order_by_total.push(Projection::try_from_ident(ident, projections)?);
                }

                Ok(order_by_total)
            }
            Expr::Nested(order_by) => {
                // Added, because `sqlparser-rs` believes single element tuples are `Expr::Nested`
                if let Expr::Identifier(order_by) = order_by.as_ref() {
                    Projection::try_from_ident(order_by, projections).map(|x| vec![x])
                } else {
                    Err(Error::InvalidOrderBy(
                        "Nested primary keys are unsupported".to_string(),
                    ))
                }
            }
            _ => Err(Error::InvalidOrderBy(order_by.to_string())),
        }
    }
}
