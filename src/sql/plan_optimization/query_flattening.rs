use crate::sql::sql_parser::{LogicalPlan, ScanSource};

use crate::sql::Projection;
use sqlparser::ast::{BinaryOperator, Expr};

impl LogicalPlan {
    /// Flattens a logical plan by merging nested query structures.
    ///
    /// Applies optimizations: merge scans, filters, projections, order by, and limits.
    /// Non-query plans (Skip, `CreateDatabase`, `CreateTable`, `Insert`, `Drop`) are returned unchanged.
    ///
    /// Returns: Flattened `LogicalPlan`.
    pub fn flatten(self) -> Self {
        match self {
            Self::Skip
            | Self::CreateDatabase { .. }
            | Self::CreateTable { .. }
            | Self::Insert { .. }
            | Self::DropDatabase { .. }
            | Self::DropTable { .. } => self,
            plan => plan
                .merge_scans()
                .merge_filters(Vec::new())
                .merge_projections(Vec::new())
                .merge_order_by(Vec::new())
                .merge_offset_and_limit(None, 0),
        }
    }

    fn merge_scans(self) -> Self {
        match self {
            Self::Scan { source } => match source {
                ScanSource::Subquery(plan) => plan.merge_scans(),
                ScanSource::Table(_) => Self::Scan { source },
            },
            Self::Projection { columns, plan } => Self::Projection {
                columns,
                plan: Box::new(plan.merge_scans()),
            },
            Self::Filter { expr, plan } => Self::Filter {
                expr,
                plan: Box::new(plan.merge_scans()),
            },
            Self::OrderBy { column_defs, plan } => Self::OrderBy {
                column_defs,
                plan: Box::new(plan.merge_scans()),
            },
            Self::Limit {
                limit,
                offset,
                plan,
            } => Self::Limit {
                limit,
                offset,
                plan: Box::new(plan.merge_scans()),
            },
            Self::Skip
            | Self::CreateDatabase { .. }
            | Self::CreateTable { .. }
            | Self::Insert { .. }
            | Self::DropDatabase { .. }
            | Self::DropTable { .. } => unreachable!(), // it's already filtered by `flatten`
        }
    }

    fn merge_filters(self, mut filters: Vec<Expr>) -> Self {
        match self {
            Self::Filter { expr, plan } => {
                filters.push(*expr);
                plan.merge_filters(filters)
            }
            Self::Scan { .. } => {
                // we assume scans are merged, so they are 100% at the very bottom
                if filters.is_empty() {
                    self
                } else {
                    Self::Filter {
                        expr: Box::new(combine_filters(filters)),
                        plan: Box::new(self), // subquery was already removed in `merge_scans`
                    }
                }
            }
            Self::Projection { columns, plan } => Self::Projection {
                columns,
                plan: Box::new(plan.merge_filters(filters)),
            },
            Self::OrderBy { column_defs, plan } => Self::OrderBy {
                column_defs,
                plan: Box::new(plan.merge_filters(filters)),
            },
            Self::Limit {
                limit,
                offset,
                plan,
            } => Self::Limit {
                limit,
                offset,
                plan: Box::new(plan.merge_filters(filters)),
            },
            Self::Skip
            | Self::CreateDatabase { .. }
            | Self::CreateTable { .. }
            | Self::Insert { .. }
            | Self::DropDatabase { .. }
            | Self::DropTable { .. } => unreachable!(), // it's already filtered by `flatten`
        }
    }

    fn merge_projections(self, mut columns: Vec<Projection>) -> Self {
        match self {
            Self::Projection {
                columns: proj_cols,
                plan,
            } => {
                if columns.is_empty() {
                    columns = proj_cols;
                }
                plan.merge_projections(columns)
            }
            Self::Filter { .. } | Self::Scan { .. } => {
                // we assume filters and scans are merged, so they are 100% at the very bottom
                if columns.is_empty() {
                    self
                } else {
                    Self::Projection {
                        columns,
                        plan: Box::new(self), // subquery was already removed in `merge_scans`
                    }
                }
            }
            Self::OrderBy { column_defs, plan } => Self::OrderBy {
                column_defs,
                plan: Box::new(plan.merge_projections(columns)),
            },
            Self::Limit {
                limit,
                offset,
                plan,
            } => Self::Limit {
                limit,
                offset,
                plan: Box::new(plan.merge_projections(columns)),
            },
            Self::Skip
            | Self::CreateDatabase { .. }
            | Self::CreateTable { .. }
            | Self::Insert { .. }
            | Self::DropDatabase { .. }
            | Self::DropTable { .. } => unreachable!(), // it's already filtered by `flatten`
        }
    }

    fn merge_order_by(self, mut order_by: Vec<Vec<Projection>>) -> Self {
        match self {
            Self::OrderBy { column_defs, plan } => {
                // todo: remove unnecessary repeating order_by
                // todo: simplify
                for (idx, own_order_by) in column_defs.into_iter().enumerate() {
                    order_by.insert(idx, own_order_by);
                }
                plan.merge_order_by(order_by)
            }
            Self::Projection { .. } => {
                if order_by.is_empty() {
                    self
                } else {
                    Self::OrderBy {
                        column_defs: order_by,
                        plan: Box::new(self),
                    }
                }
            }
            Self::Limit {
                limit,
                offset,
                plan,
            } => Self::Limit {
                limit,
                offset,
                plan: Box::new(plan.merge_order_by(order_by)),
            },
            Self::Skip
            | Self::CreateDatabase { .. }
            | Self::CreateTable { .. }
            | Self::Insert { .. }
            | Self::DropDatabase { .. }
            | Self::DropTable { .. } => unreachable!(), // it's already filtered by `flatten`
            Self::Filter { .. } | Self::Scan { .. } => unreachable!(), // no need to check for filter/scan, as each select MUST have `Self::Projection`
        }
    }

    fn merge_offset_and_limit(self, mut limit: Option<u64>, mut offset: u64) -> Self {
        match self {
            Self::Limit {
                limit: limit_inner,
                offset: offset_inner,
                plan,
            } => {
                limit = match (limit, limit_inner) {
                    (Some(outer), Some(inner)) => Some(outer.min(inner)),
                    (None, Some(val)) | (Some(val), None) => Some(val),
                    (None, None) => None,
                };
                offset = offset.checked_add(offset_inner).expect("offset overflow"); // todo: consider checking in logical select

                plan.merge_offset_and_limit(limit, offset)
            }
            Self::OrderBy { .. } | Self::Projection { .. } => {
                if limit.is_none() && offset == 0 {
                    self
                } else {
                    Self::Limit {
                        limit,
                        offset,
                        plan: Box::new(self),
                    }
                }
            }
            Self::Skip
            | Self::CreateDatabase { .. }
            | Self::CreateTable { .. }
            | Self::Insert { .. }
            | Self::DropDatabase { .. }
            | Self::DropTable { .. } => unreachable!(), // it's already filtered by `flatten`
            Self::Filter { .. } | Self::Scan { .. } => unreachable!(), // no need to check for filter/scan, as each select MUST have `Self::Projection`
        }
    }
}

fn combine_filters(mut filters: Vec<Expr>) -> Expr {
    debug_assert_ne!(filters.len(), 0);

    if filters.len() == 1 {
        return filters.remove(0);
    }
    let mut result = filters.remove(0);

    for filter in filters {
        result = Expr::BinaryOp {
            left: Box::new(result),
            op: BinaryOperator::And,
            right: Box::new(filter),
        };
    }

    result
}

#[cfg(test)]
mod tests {
    use crate::sql::plan_optimization::query_flattening::combine_filters;
    use crate::sql::sql_parser::{LogicalPlan, ScanSource};
    use crate::storage::{ColumnDef, Constraints, TableDef, ValueType};

    use crate::sql::{Projection, ProjectionValue};
    use sqlparser::ast::{Expr, Ident};
    use sqlparser::tokenizer::Span;

    fn str_column(name: String) -> ColumnDef {
        ColumnDef {
            name,
            field_type: ValueType::String,
            constraints: Constraints::default(),
        }
    }

    fn projection(columns: Vec<Projection>, plan: LogicalPlan) -> LogicalPlan {
        LogicalPlan::Projection {
            columns,
            plan: Box::new(plan),
        }
    }

    fn filter(expr: Expr, plan: LogicalPlan) -> LogicalPlan {
        LogicalPlan::Filter {
            expr: Box::new(expr),
            plan: Box::new(plan),
        }
    }

    fn identifier(name: String) -> Expr {
        Expr::Identifier(Ident {
            value: name,
            quote_style: None,
            span: Span::empty(),
        })
    }

    fn table_def() -> TableDef {
        TableDef {
            table: "table_name".to_string(),
            database: "database_name".to_string(),
        }
    }

    fn scan(source: ScanSource) -> LogicalPlan {
        LogicalPlan::Scan { source }
    }

    fn order_by(column_defs: Vec<Vec<Projection>>, plan: LogicalPlan) -> LogicalPlan {
        LogicalPlan::OrderBy {
            column_defs,
            plan: Box::new(plan),
        }
    }

    fn limit(limit: u64, offset: u64, plan: LogicalPlan) -> LogicalPlan {
        LogicalPlan::Limit {
            limit: Some(limit),
            offset,
            plan: Box::new(plan),
        }
    }

    fn get_start_stage() -> LogicalPlan {
        // `SELECT age, name FROM (SELECT age, name, id FROM (SELECT age, name, id FROM table WHERE filter1 ORDER BY age, id LIMIT 4 OFFSET 6) WHERE filter2) WHERE filter3 ORDER BY age, name LIMIT 2 OFFSET 2`
        limit(
            2,
            2,
            order_by(
                vec![vec![
                    Projection {
                        alias: None,
                        source: ProjectionValue::ColumnDef(str_column("age".to_string())),
                    },
                    Projection {
                        alias: None,
                        source: ProjectionValue::ColumnDef(str_column("name".to_string())),
                    },
                ]],
                projection(
                    vec![
                        Projection {
                            alias: None,
                            source: ProjectionValue::ColumnDef(str_column("age".to_string())),
                        },
                        Projection {
                            alias: None,
                            source: ProjectionValue::ColumnDef(str_column("name".to_string())),
                        },
                    ],
                    filter(
                        identifier("filter3".to_string()),
                        scan(ScanSource::Subquery(Box::new(projection(
                            vec![
                                Projection {
                                    alias: None,
                                    source: ProjectionValue::ColumnDef(str_column(
                                        "age".to_string(),
                                    )),
                                },
                                Projection {
                                    alias: None,
                                    source: ProjectionValue::ColumnDef(str_column(
                                        "name".to_string(),
                                    )),
                                },
                                Projection {
                                    alias: None,
                                    source: ProjectionValue::ColumnDef(str_column(
                                        "id".to_string(),
                                    )),
                                },
                            ],
                            filter(
                                identifier("filter2".to_string()),
                                scan(ScanSource::Subquery(Box::new(limit(
                                    4,
                                    6,
                                    order_by(
                                        vec![vec![
                                            Projection {
                                                alias: None,
                                                source: ProjectionValue::ColumnDef(str_column(
                                                    "age".to_string(),
                                                )),
                                            },
                                            Projection {
                                                alias: None,
                                                source: ProjectionValue::ColumnDef(str_column(
                                                    "id".to_string(),
                                                )),
                                            },
                                        ]],
                                        projection(
                                            vec![
                                                Projection {
                                                    alias: None,
                                                    source: ProjectionValue::ColumnDef(str_column(
                                                        "age".to_string(),
                                                    )),
                                                },
                                                Projection {
                                                    alias: None,
                                                    source: ProjectionValue::ColumnDef(str_column(
                                                        "name".to_string(),
                                                    )),
                                                },
                                                Projection {
                                                    alias: None,
                                                    source: ProjectionValue::ColumnDef(str_column(
                                                        "id".to_string(),
                                                    )),
                                                },
                                            ],
                                            filter(
                                                identifier("filter1".to_string()),
                                                scan(ScanSource::Table(table_def())),
                                            ),
                                        ),
                                    ),
                                )))),
                            ),
                        )))),
                    ),
                ),
            ),
        )
    }

    #[test]
    fn test_merge_scans_mini() {
        // Scan: sub_query
        //  Scan: sub_query
        //      Scan: table
        //
        // INTO
        //
        // Scan: table

        let plan = scan(ScanSource::Subquery(Box::new(scan(ScanSource::Subquery(
            Box::new(scan(ScanSource::Table(table_def()))),
        )))));
        let merged = scan(ScanSource::Table(table_def()));

        assert_eq!(plan.merge_scans(), merged);
    }

    #[test]
    fn test_merge_scans_2() {
        // start plan
        //
        // INTO
        //
        // Limit: 2 Offset: 2
        //      Order by: [age, name]
        //          Projection: age, name
        //              Filter: filter3
        //                  Projection: age, name, id
        //                      Filter: filter2
        //                          Limit: 4 Offset 6
        //                              Order by: [age, id]
        //                                  Projection: age, name, id
        //                                      Filter: filter1
        //                                          Scan: table

        let plan = get_start_stage();

        let merged = limit(
            2,
            2,
            order_by(
                vec![vec![
                    Projection {
                        alias: None,
                        source: ProjectionValue::ColumnDef(str_column("age".to_string())),
                    },
                    Projection {
                        alias: None,
                        source: ProjectionValue::ColumnDef(str_column("name".to_string())),
                    },
                ]],
                projection(
                    vec![
                        Projection {
                            alias: None,
                            source: ProjectionValue::ColumnDef(str_column("age".to_string())),
                        },
                        Projection {
                            alias: None,
                            source: ProjectionValue::ColumnDef(str_column("name".to_string())),
                        },
                    ],
                    filter(
                        identifier("filter3".to_string()),
                        projection(
                            vec![
                                Projection {
                                    alias: None,
                                    source: ProjectionValue::ColumnDef(str_column(
                                        "age".to_string(),
                                    )),
                                },
                                Projection {
                                    alias: None,
                                    source: ProjectionValue::ColumnDef(str_column(
                                        "name".to_string(),
                                    )),
                                },
                                Projection {
                                    alias: None,
                                    source: ProjectionValue::ColumnDef(str_column(
                                        "id".to_string(),
                                    )),
                                },
                            ],
                            filter(
                                identifier("filter2".to_string()),
                                limit(
                                    4,
                                    6,
                                    order_by(
                                        vec![vec![
                                            Projection {
                                                alias: None,
                                                source: ProjectionValue::ColumnDef(str_column(
                                                    "age".to_string(),
                                                )),
                                            },
                                            Projection {
                                                alias: None,
                                                source: ProjectionValue::ColumnDef(str_column(
                                                    "id".to_string(),
                                                )),
                                            },
                                        ]],
                                        projection(
                                            vec![
                                                Projection {
                                                    alias: None,
                                                    source: ProjectionValue::ColumnDef(str_column(
                                                        "age".to_string(),
                                                    )),
                                                },
                                                Projection {
                                                    alias: None,
                                                    source: ProjectionValue::ColumnDef(str_column(
                                                        "name".to_string(),
                                                    )),
                                                },
                                                Projection {
                                                    alias: None,
                                                    source: ProjectionValue::ColumnDef(str_column(
                                                        "id".to_string(),
                                                    )),
                                                },
                                            ],
                                            filter(
                                                identifier("filter1".to_string()),
                                                scan(ScanSource::Table(table_def())),
                                            ),
                                        ),
                                    ),
                                ),
                            ),
                        ),
                    ),
                ),
            ),
        );

        assert_eq!(plan.merge_scans(), merged);
    }

    #[test]
    fn test_merge_filters() {
        // Limit: 2 Offset: 2
        //      Order by: [age, name]
        //          Projection: age, name
        //              Filter: filter3
        //                  Projection: age, name, id
        //                      Filter: filter2
        //                          Limit: 4 Offset 6
        //                              Order by: [age, id]
        //                                  Projection: age, name, id
        //                                      Filter: filter1
        //                                          Scan: table
        //
        // INTO
        //
        // Limit: 2 Offset: 2
        //      Order by: [age, name]
        //          Projection: age, name
        //              Projection: age, name, id
        //                  Limit: 4 Offset 6
        //                      Order by: [age, id]
        //                          Projection: age, name, id
        //                              Filter: filter3 + filter2 + filter1
        //                                  Scan: table

        let plan = get_start_stage().merge_scans();

        let merged = limit(
            2,
            2,
            order_by(
                vec![vec![
                    Projection {
                        alias: None,
                        source: ProjectionValue::ColumnDef(str_column("age".to_string())),
                    },
                    Projection {
                        alias: None,
                        source: ProjectionValue::ColumnDef(str_column("name".to_string())),
                    },
                ]],
                projection(
                    vec![
                        Projection {
                            alias: None,
                            source: ProjectionValue::ColumnDef(str_column("age".to_string())),
                        },
                        Projection {
                            alias: None,
                            source: ProjectionValue::ColumnDef(str_column("name".to_string())),
                        },
                    ],
                    projection(
                        vec![
                            Projection {
                                alias: None,
                                source: ProjectionValue::ColumnDef(str_column("age".to_string())),
                            },
                            Projection {
                                alias: None,
                                source: ProjectionValue::ColumnDef(str_column("name".to_string())),
                            },
                            Projection {
                                alias: None,
                                source: ProjectionValue::ColumnDef(str_column("id".to_string())),
                            },
                        ],
                        limit(
                            4,
                            6,
                            order_by(
                                vec![vec![
                                    Projection {
                                        alias: None,
                                        source: ProjectionValue::ColumnDef(str_column(
                                            "age".to_string(),
                                        )),
                                    },
                                    Projection {
                                        alias: None,
                                        source: ProjectionValue::ColumnDef(str_column(
                                            "id".to_string(),
                                        )),
                                    },
                                ]],
                                projection(
                                    vec![
                                        Projection {
                                            alias: None,
                                            source: ProjectionValue::ColumnDef(str_column(
                                                "age".to_string(),
                                            )),
                                        },
                                        Projection {
                                            alias: None,
                                            source: ProjectionValue::ColumnDef(str_column(
                                                "name".to_string(),
                                            )),
                                        },
                                        Projection {
                                            alias: None,
                                            source: ProjectionValue::ColumnDef(str_column(
                                                "id".to_string(),
                                            )),
                                        },
                                    ],
                                    filter(
                                        combine_filters(vec![
                                            identifier("filter3".to_string()),
                                            identifier("filter2".to_string()),
                                            identifier("filter1".to_string()),
                                        ]),
                                        scan(ScanSource::Table(table_def())),
                                    ),
                                ),
                            ),
                        ),
                    ),
                ),
            ),
        );

        assert_eq!(plan.merge_filters(Vec::new()), merged);
    }

    #[test]
    fn test_merge_projections() {
        // Limit: 2 Offset: 2
        //      Order by: [age, name]
        //          Projection: age, name
        //              Projection: age, name, id
        //                  Limit: 4 Offset 6
        //                      Order by: [age, id]
        //                          Projection: age, name, id
        //                              Filter: filter3 + filter2 + filter1
        //                                  Scan: table
        //
        // INTO
        //
        // Limit: 2 Offset: 2
        //      Order by: [age, name]
        //          Limit: 4 Offset 6
        //              Order by: [age, id]
        //                  Projection: age, name
        //                      Filter: filter3 + filter2 + filter1
        //                          Scan: table

        let plan = get_start_stage().merge_scans().merge_filters(Vec::new());

        let merged = limit(
            2,
            2,
            order_by(
                vec![vec![
                    Projection {
                        alias: None,
                        source: ProjectionValue::ColumnDef(str_column("age".to_string())),
                    },
                    Projection {
                        alias: None,
                        source: ProjectionValue::ColumnDef(str_column("name".to_string())),
                    },
                ]],
                limit(
                    4,
                    6,
                    order_by(
                        vec![vec![
                            Projection {
                                alias: None,
                                source: ProjectionValue::ColumnDef(str_column("age".to_string())),
                            },
                            Projection {
                                alias: None,
                                source: ProjectionValue::ColumnDef(str_column("id".to_string())),
                            },
                        ]],
                        projection(
                            vec![
                                Projection {
                                    alias: None,
                                    source: ProjectionValue::ColumnDef(str_column(
                                        "age".to_string(),
                                    )),
                                },
                                Projection {
                                    alias: None,
                                    source: ProjectionValue::ColumnDef(str_column(
                                        "name".to_string(),
                                    )),
                                },
                            ],
                            filter(
                                combine_filters(vec![
                                    identifier("filter3".to_string()),
                                    identifier("filter2".to_string()),
                                    identifier("filter1".to_string()),
                                ]),
                                scan(ScanSource::Table(table_def())),
                            ),
                        ),
                    ),
                ),
            ),
        );

        assert_eq!(plan.merge_projections(Vec::new()), merged);
    }

    #[test]
    fn test_merge_order() {
        // Limit: 2 Offset: 2
        //      Order by: [age, name]
        //          Limit: 4 Offset 6
        //              Order by: [age, id]
        //                  Projection: age, name
        //                      Filter: filter3 + filter2 + filter1
        //                          Scan: table
        //
        // INTO
        //
        // Limit: 2 Offset: 2
        //      Limit: 4 Offset 6
        //          Order by: [age, id], [age, name]
        //              Projection: age, name
        //                  Filter: filter3 + filter2 + filter1
        //                      Scan: table

        let plan = get_start_stage()
            .merge_scans()
            .merge_filters(Vec::new())
            .merge_projections(Vec::new());

        let merged = limit(
            2,
            2,
            limit(
                4,
                6,
                order_by(
                    vec![
                        vec![
                            Projection {
                                alias: None,
                                source: ProjectionValue::ColumnDef(str_column("age".to_string())),
                            },
                            Projection {
                                alias: None,
                                source: ProjectionValue::ColumnDef(str_column("id".to_string())),
                            },
                        ],
                        vec![
                            Projection {
                                alias: None,
                                source: ProjectionValue::ColumnDef(str_column("age".to_string())),
                            },
                            Projection {
                                alias: None,
                                source: ProjectionValue::ColumnDef(str_column("name".to_string())),
                            },
                        ],
                    ],
                    projection(
                        vec![
                            Projection {
                                alias: None,
                                source: ProjectionValue::ColumnDef(str_column("age".to_string())),
                            },
                            Projection {
                                alias: None,
                                source: ProjectionValue::ColumnDef(str_column("name".to_string())),
                            },
                        ],
                        filter(
                            combine_filters(vec![
                                identifier("filter3".to_string()),
                                identifier("filter2".to_string()),
                                identifier("filter1".to_string()),
                            ]),
                            scan(ScanSource::Table(table_def())),
                        ),
                    ),
                ),
            ),
        );

        assert_eq!(plan.merge_order_by(Vec::new()), merged);
    }

    #[test]
    fn test_merge_limit_and_offset() {
        // Limit: 2 Offset: 2
        //      Limit: 4 Offset 6
        //          Order by: [age, id], [age, name]
        //              Projection: age, name
        //                  Filter: filter3 + filter2 + filter1
        //                      Scan: table
        //
        // INTO
        // Limit: 2 Offset: 8
        //          Order by: [age, id], [age, name]
        //              Projection: age, name
        //                  Filter: filter3 + filter2 + filter1
        //                      Scan: table
        let plan = get_start_stage()
            .merge_scans()
            .merge_filters(Vec::new())
            .merge_projections(Vec::new())
            .merge_order_by(Vec::new());

        let merged = limit(
            2,
            8,
            order_by(
                vec![
                    vec![
                        Projection {
                            alias: None,
                            source: ProjectionValue::ColumnDef(str_column("age".to_string())),
                        },
                        Projection {
                            alias: None,
                            source: ProjectionValue::ColumnDef(str_column("id".to_string())),
                        },
                    ],
                    vec![
                        Projection {
                            alias: None,
                            source: ProjectionValue::ColumnDef(str_column("age".to_string())),
                        },
                        Projection {
                            alias: None,
                            source: ProjectionValue::ColumnDef(str_column("name".to_string())),
                        },
                    ],
                ],
                projection(
                    vec![
                        Projection {
                            alias: None,
                            source: ProjectionValue::ColumnDef(str_column("age".to_string())),
                        },
                        Projection {
                            alias: None,
                            source: ProjectionValue::ColumnDef(str_column("name".to_string())),
                        },
                    ],
                    filter(
                        combine_filters(vec![
                            identifier("filter3".to_string()),
                            identifier("filter2".to_string()),
                            identifier("filter1".to_string()),
                        ]),
                        scan(ScanSource::Table(table_def())),
                    ),
                ),
            ),
        );

        assert_eq!(plan.merge_offset_and_limit(None, 0), merged);
    }

    #[test]
    fn debug() {
        // SELECT name FROM (SELECT name, age FROM default.users WHERE id > 1) LIMIT 2
        let plan = limit(
            2,
            0,
            projection(
                vec![Projection {
                    alias: None,
                    source: ProjectionValue::ColumnDef(str_column("name".to_string())),
                }],
                scan(ScanSource::Subquery(Box::new(projection(
                    vec![
                        Projection {
                            alias: None,
                            source: ProjectionValue::ColumnDef(str_column("name".to_string())),
                        },
                        Projection {
                            alias: None,
                            source: ProjectionValue::ColumnDef(str_column("age".to_string())),
                        },
                    ],
                    filter(
                        identifier("filter1".to_string()),
                        scan(ScanSource::Table(table_def())),
                    ),
                )))),
            ),
        );

        let merged = limit(
            2,
            0,
            projection(
                vec![Projection {
                    alias: None,
                    source: ProjectionValue::ColumnDef(str_column("name".to_string())),
                }],
                filter(
                    identifier("filter1".to_string()),
                    scan(ScanSource::Table(table_def())),
                ),
            ),
        );

        assert_eq!(plan.flatten(), merged);
    }
}
