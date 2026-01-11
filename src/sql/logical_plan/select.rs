use crate::error::{Error, Result};
use crate::runtime_config::TABLE_DATA;
use crate::sql::sql_parser::{
    AggregateProjection, LogicalPlan, Projection, ProjectionValue, RawProjection, ScanSource,
};
use crate::storage::TableDef;

use sqlparser::ast::{
    Expr, GroupByExpr, LimitClause, OrderByKind, Query, SelectItem, SetExpr, TableFactor,
    Value as SQLValue,
};

impl LogicalPlan {
    /// Parses SELECT query into a logical plan tree.
    ///
    /// Builds a tree of `LogicalPlan` nodes: Scan -> Filter -> Projection -> OrderBy -> Limit.
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
        // dbg!(query);
        // panic!();
        let SetExpr::Select(select) = &*query.body else {
            return Err(Error::UnsupportedCommand(
                "Only SELECT queries are supported".to_string(),
            ));
        };

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
        let scan_source = match &table.relation {
            TableFactor::Table { name, .. } => {
                let table_def = TableDef::try_from(name)?;
                ScanSource::Table(table_def)
            }
            TableFactor::Derived { subquery, .. } => {
                let subquery_plan = Self::from_query(subquery)?;
                ScanSource::Subquery(Box::new(subquery_plan))
            }
            _ => {
                return Err(Error::UnsupportedCommand(
                    "Only simple table references and subqueries are supported".to_string(),
                ));
            }
        };

        if select.projection.is_empty() {
            return Err(Error::UnsupportedCommand(
                "No projection specified.".to_string(),
            ));
        }

        let mut plan = Self::Scan {
            source: scan_source,
        };

        let mut read_columns: Vec<Projection> = Vec::with_capacity(select.projection.len() / 2);
        let mut aggregate_projections: Vec<AggregateProjection> =
            Vec::with_capacity(select.projection.len() / 2);

        let available_projections = Self::extract_columns_from_plan(&plan)?;

        // Allow either
        // * Wildcard only, meaning all columns.
        // * Wildcard at the end, meaning all columns which are not specified.
        // * No wildcard.
        let mut wildcard = None;

        for (idx, projection) in select.projection.iter().enumerate() {
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
                    if wildcard.is_some() {
                        return Err(Error::UnsupportedCommand(
                            "Columns after wildcard are not supported".to_string(),
                        ));
                    }
                    let raw_projection = RawProjection::try_from(expr, &available_projections)?;
                    match raw_projection {
                        RawProjection::Projection(projection) => read_columns.push(projection),
                        RawProjection::AggregateProjection(projection) => {
                            aggregate_projections.push(projection)
                        }
                    };
                }
                SelectItem::ExprWithAlias { expr, alias } => {
                    if wildcard.is_some() {
                        return Err(Error::UnsupportedCommand(
                            "Columns after wildcard are not supported".to_string(),
                        ));
                    }
                    let mut raw_projection = RawProjection::try_from(expr, &available_projections)?;
                    raw_projection.set_alias(alias.value.clone());
                    match raw_projection {
                        RawProjection::Projection(projection) => read_columns.push(projection),
                        RawProjection::AggregateProjection(projection) => {
                            aggregate_projections.push(projection)
                        }
                    };
                }
                SelectItem::QualifiedWildcard(..) => {
                    return Err(Error::UnsupportedCommand(
                        "Only simple column projections and wildcards are supported".to_string(),
                    ));
                }
            }
        }

        if let Some(idx) = wildcard {
            if idx == 0 {
                read_columns.clone_from(&available_projections);
            } else {
                for proj in &available_projections {
                    if !read_columns.contains(proj) {
                        read_columns.push(proj.clone());
                    }
                }
            }
        }

        if let Some(ref selection) = select.selection {
            plan = LogicalPlan::Filter {
                expr: Box::new(selection.clone()),
                plan: Box::new(plan),
            };
        }

        plan = LogicalPlan::Projection {
            columns: read_columns.clone(),
            plan: Box::new(plan),
        };

        match &select.group_by {
            GroupByExpr::All(modifiers) => {
                if !modifiers.is_empty() {
                    return Err(Error::UnsupportedCommand(format!(
                        "Modifiers ({:?}) are not supported in GROUP BY clause",
                        modifiers
                    )));
                }

                plan = LogicalPlan::Aggregate {
                    aggr_proj: aggregate_projections,
                    group_by: read_columns.clone(),
                    plan: Box::new(plan),
                };
            }
            GroupByExpr::Expressions(expressions, modifiers) => {
                if !modifiers.is_empty() {
                    return Err(Error::UnsupportedCommand(format!(
                        "Modifiers ({:?}) are not supported in GROUP BY clause",
                        modifiers
                    )));
                }

                let mut group_by = Vec::with_capacity(expressions.len());

                for expr in expressions {
                    let proj = match RawProjection::try_from(expr, &available_projections)? {
                        RawProjection::Projection(proj) => proj,
                        RawProjection::AggregateProjection(aggr_proj) => {
                            return Err(Error::InvalidSource(format!(
                                "Expected projection in ORDER BY, got ({aggr_proj:?}) instead.",
                            )));
                        }
                    };

                    group_by.push(proj);
                }

                if let Some(proj) = read_columns.iter().find(|proj| !group_by.contains(proj)) {
                    return Err(Error::ColumnNotInGroupBy(proj.to_string()));
                }

                plan = LogicalPlan::Aggregate {
                    aggr_proj: aggregate_projections,
                    group_by,
                    plan: Box::new(plan),
                };
            }
        }

        if let Some(expr) = &select.having {
            plan = LogicalPlan::Having {
                expr: Box::new(expr.clone()),
                plan: Box::new(plan),
            };
        }

        if let Some(order_by) = &query.order_by {
            match &order_by.kind {
                OrderByKind::All(_params) => {
                    plan = LogicalPlan::OrderBy {
                        projs: vec![read_columns],
                        plan: Box::new(plan),
                    };
                }
                OrderByKind::Expressions(order_by_given) => {
                    let mut order_by_all = Vec::with_capacity(order_by_given.len());
                    for order_by_expr in order_by_given {
                        let order_by_cols =
                            Self::parse_order_by(&order_by_expr.expr, &available_projections)?; // OrderBy cols is interpreted in the same way as PK in `CREATE TABLE`
                        order_by_all.push(order_by_cols);
                    }

                    plan = LogicalPlan::OrderBy {
                        projs: order_by_all,
                        plan: Box::new(plan),
                    };
                }
            }
        }
        if let Some(limit_clause) = &query.limit_clause {
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

            let mut limit = None;
            let mut offset = 0;

            if let Some(limit_expr) = limit_expr {
                let Expr::Value(limit_expr) = &limit_expr else {
                    return Err(Error::InvalidLimitValue(
                        "LIMIT must be a literal value".to_string(),
                    ));
                };
                let SQLValue::Number(limit_expr, _) = &limit_expr.value else {
                    return Err(Error::InvalidLimitValue(
                        "LIMIT must be a number".to_string(),
                    ));
                };

                limit = Some(
                    limit_expr
                        .parse()
                        .map_err(|_| Error::InvalidLimitValue(limit_expr.clone()))?,
                );
            }

            if let Some(offset_expr) = offset_expr {
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

                offset = offset_expr
                    .parse()
                    .map_err(|_| Error::InvalidLimitValue(offset_expr.clone()))?;
            }

            plan = LogicalPlan::Limit {
                limit,
                offset,
                plan: Box::new(plan),
            };
        }

        Ok(plan)
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
            LogicalPlan::Projection { columns, .. } => Ok(columns.clone()),
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
