use crate::error::{Error, Result};
use crate::storage::{ColumnDef, Mark, Value, ValueType, value::ArchivedValue};
use rkyv::vec::ArchivedVec;
use sqlparser::ast::{BinaryOperator, Expr, UnaryOperator, Value as SQLValue};

pub enum BinOp {
    Gt,
    Lt,
    GtEq,
    LtEq,
    Eq,
    NotEq,
}

pub enum CompiledFilter {
    Compare {
        col_idx: usize,
        op: BinOp,
        value: Value,
    },
    CompareColumns {
        left_idx: usize,
        op: BinOp,
        right_idx: usize,
    },
    And(Box<CompiledFilter>, Box<CompiledFilter>),
    Or(Box<CompiledFilter>, Box<CompiledFilter>),
    Not(Box<CompiledFilter>),
    Column(usize),
    Const(bool),
}

impl CompiledFilter {
    /// Collects all column indices referenced by this filter.
    ///
    /// Recursively traverses the filter tree and adds unique column indices to the output vector.
    pub fn get_column_indexes(&self, col_def_idxs: &mut Vec<usize>) {
        match self {
            CompiledFilter::CompareColumns {
                left_idx,
                right_idx,
                ..
            } => {
                if !col_def_idxs.contains(left_idx) {
                    col_def_idxs.push(*left_idx);
                }
                if !col_def_idxs.contains(right_idx) {
                    col_def_idxs.push(*right_idx);
                }
            }
            CompiledFilter::And(left, right) | CompiledFilter::Or(left, right) => {
                left.get_column_indexes(col_def_idxs);
                right.get_column_indexes(col_def_idxs);
            }
            CompiledFilter::Not(filter) => {
                filter.get_column_indexes(col_def_idxs);
            }
            CompiledFilter::Compare { col_idx, .. } | CompiledFilter::Column(col_idx) => {
                if !col_def_idxs.contains(col_idx) {
                    col_def_idxs.push(*col_idx);
                }
            }
            CompiledFilter::Const(_) => {}
        }
    }

    /// Allow cmp for
    /// * `Value` and `Value`
    /// * `Value` and `ArchivedValue`
    /// * `ArchivedValue` and `Value`
    /// * `ArchivedValue` and `ArchivedValue`
    pub fn cmp_vals<T, K>(a: &T, b: &K, op: &BinOp) -> bool
    where
        T: PartialEq<K> + PartialOrd<K> + PartialEq + PartialOrd,
        K: PartialEq<T> + PartialOrd<T> + PartialEq + PartialOrd,
    {
        match op {
            BinOp::Gt => a > b,
            BinOp::Lt => a < b,
            BinOp::GtEq => a >= b,
            BinOp::LtEq => a <= b,
            BinOp::Eq => a == b,
            BinOp::NotEq => a != b,
        }
    }

    /// Compiles a SQL expression into a `CompiledFilter` for efficient evaluation.
    ///
    /// Supports: AND, OR, NOT, comparison operators, column references, and literal values.
    /// Performs constant folding for boolean expressions.
    ///
    /// Returns:
    ///   * Ok: `CompiledFilter` representing the compiled expression.
    ///   * Error when:
    ///     1. Column not found in table: `ColumnNotFound`.
    ///     2. Unsupported expression type: `UnsupportedFilter` or `InvalidSource`.
    ///     3. Value conversion fails: type conversion error.
    pub fn try_compile(filter: Expr, table_column_defs: &[ColumnDef]) -> Result<Self> {
        match filter {
            Expr::BinaryOp { op, left, right } => match op {
                BinaryOperator::And => {
                    let left = Self::try_compile(*left, table_column_defs)?;

                    if let Self::Const(false) = left {
                        return Ok(Self::Const(false));
                    }
                    if let Self::Const(true) = left {
                        return Self::try_compile(*right, table_column_defs);
                    }

                    let right = Self::try_compile(*right, table_column_defs)?;

                    if let Self::Const(false) = right {
                        return Ok(Self::Const(false));
                    }
                    if let Self::Const(true) = right {
                        return Ok(left);
                    }

                    Ok(Self::And(Box::new(left), Box::new(right)))
                }
                BinaryOperator::Or => {
                    let left = Self::try_compile(*left, table_column_defs)?;

                    if let Self::Const(true) = left {
                        return Ok(Self::Const(true));
                    }
                    if let Self::Const(false) = left {
                        return Self::try_compile(*right, table_column_defs);
                    }

                    let right = Self::try_compile(*right, table_column_defs)?;

                    if let Self::Const(true) = right {
                        return Ok(Self::Const(true));
                    }
                    if let Self::Const(false) = right {
                        return Ok(left);
                    }

                    Ok(Self::Or(Box::new(left), Box::new(right)))
                }
                _ => {
                    let op = BinOp::try_from(op)?;
                    match (*left, *right) {
                        (Expr::Identifier(left), Expr::Value(right)) => {
                            let left = table_column_defs
                                .iter()
                                .position(|col_def| *col_def.name == left.value)
                                .ok_or(Error::ColumnNotFound(left.value.clone()))?;
                            let right = Value::try_from((
                                right.value,
                                &table_column_defs[left].field_type,
                            ))?;

                            Ok(Self::Compare {
                                col_idx: left,
                                op,
                                value: right,
                            })
                        }
                        (Expr::Value(left), Expr::Identifier(right)) => {
                            let right = table_column_defs
                                .iter()
                                .position(|col_def| *col_def.name == right.value)
                                .ok_or(Error::ColumnNotFound(right.value.clone()))?;
                            let left = Value::try_from((
                                left.value,
                                &table_column_defs[right].field_type,
                            ))?;

                            Ok(Self::Compare {
                                col_idx: right,
                                op: op.flip(),
                                value: left,
                            })
                        }
                        (Expr::Value(left), Expr::Value(right)) => {
                            let left = Value::try_from_untyped(left.value)?;
                            let right = Value::try_from_untyped(right.value)?;

                            Ok(Self::Const(Self::cmp_vals(&left, &right, &op)))
                        }
                        (Expr::Identifier(left), Expr::Identifier(right)) => {
                            let left_idx = table_column_defs
                                .iter()
                                .position(|col_def| *col_def.name == left.value)
                                .ok_or(Error::ColumnNotFound(left.value.clone()))?;
                            let right_idx = table_column_defs
                                .iter()
                                .position(|col_def| *col_def.name == right.value)
                                .ok_or(Error::ColumnNotFound(right.value.clone()))?;
                            Ok(Self::CompareColumns {
                                left_idx,
                                op,
                                right_idx,
                            })
                        }
                        (left, right) => Err(Error::InvalidSource(format!(
                            "Unsupported comparison operands in filter: ({left}) and ({right})"
                        ))),
                    }
                }
            },
            Expr::UnaryOp { op, expr } => {
                if let UnaryOperator::Not = op {
                    Ok(Self::try_compile(*expr, table_column_defs)?.invert_self())
                } else {
                    Err(Error::InvalidSource(
                        "Currently do not support filters with unary operators except NOT"
                            .to_string(),
                    ))
                }
            }
            Expr::Value(value) => {
                if let SQLValue::Boolean(value) = value.value {
                    Ok(Self::Const(value))
                } else {
                    Err(Error::InvalidSource(format!(
                        "Could not filter on NOT boolean value {value}"
                    )))
                }
            }
            Expr::Identifier(ident) => {
                let col_idx = table_column_defs
                    .iter()
                    .position(|col_def| *col_def.name == ident.value)
                    .ok_or(Error::ColumnNotFound(ident.value.clone()))?;

                if table_column_defs[col_idx].field_type == ValueType::Bool {
                    Ok(Self::Column(col_idx))
                } else {
                    Err(Error::InvalidSource(format!(
                        "Column '{}' has type {:?}, but boolean expected in filter expression",
                        ident.value, table_column_defs[col_idx].field_type
                    )))
                }
            }
            expr => Err(Error::UnsupportedFilter(format!(
                "Unsupported expression type in filter: {expr}"
            ))),
        }
    }

    pub fn invert_self(self) -> Self {
        match self {
            CompiledFilter::Column(col_idx) => {
                CompiledFilter::Not(Box::new(CompiledFilter::Column(col_idx)))
            }
            CompiledFilter::Compare { col_idx, op, value } => CompiledFilter::Compare {
                col_idx,
                op: op.flip(),
                value,
            },
            CompiledFilter::CompareColumns {
                left_idx,
                op,
                right_idx,
            } => CompiledFilter::CompareColumns {
                left_idx,
                op: op.flip(),
                right_idx,
            },
            CompiledFilter::Not(inner) => *inner,
            CompiledFilter::And(left, right) => {
                CompiledFilter::Or(Box::new(left.invert_self()), Box::new(right.invert_self()))
            }
            CompiledFilter::Or(left, right) => {
                CompiledFilter::And(Box::new(left.invert_self()), Box::new(right.invert_self()))
            }
            CompiledFilter::Const(val) => CompiledFilter::Const(!val),
        }
    }

    // todo: currently, because of most defined types, compiler struggles to vectorize comparison.
    // it's better to split them into (cmp_i32, cmp_u8s, ...) - vectorizable
    // and (cmp_strings, cmp_uuids, ...) - not vectorizable
    pub fn generate_mask(
        &self,
        granule_bytes: &[Option<Vec<u8>>],
        col_mapping: &[Option<usize>],
        row_count: usize,
    ) -> Vec<bool> {
        match self {
            CompiledFilter::CompareColumns { .. } => unimplemented!(),

            CompiledFilter::Compare { col_idx, op, value } => {
                if let Some(data_idx) = col_mapping[*col_idx]
                    && let Some(col_data) = &granule_bytes[data_idx]
                {
                    let values =
                        unsafe { rkyv::access_unchecked::<ArchivedVec<ArchivedValue>>(col_data) };
                    values
                        .iter()
                        .map(|row_value| CompiledFilter::cmp_vals(row_value, value, op))
                        .collect()
                } else {
                    vec![false; row_count]
                }
            }
            CompiledFilter::And(left, right) => {
                let left_mask = Self::generate_mask(left, granule_bytes, col_mapping, row_count);
                let right_mask = Self::generate_mask(right, granule_bytes, col_mapping, row_count);

                left_mask
                    .into_iter()
                    .zip(right_mask)
                    .map(|(l, r)| l && r)
                    .collect()
            }
            CompiledFilter::Or(left, right) => {
                let left_mask = Self::generate_mask(left, granule_bytes, col_mapping, row_count);
                let right_mask = Self::generate_mask(right, granule_bytes, col_mapping, row_count);

                left_mask
                    .into_iter()
                    .zip(right_mask)
                    .map(|(l, r)| l || r)
                    .collect()
            }
            CompiledFilter::Not(inner) => {
                let mask = Self::generate_mask(inner, granule_bytes, col_mapping, row_count);

                mask.into_iter().map(|b| !b).collect()
            }
            CompiledFilter::Column(col_idx) => {
                if let Some(data_idx) = col_mapping[*col_idx]
                    && let Some(col_data) = &granule_bytes[data_idx]
                {
                    let values =
                        unsafe { rkyv::access_unchecked::<ArchivedVec<ArchivedValue>>(col_data) };

                    values
                        .iter()
                        .map(|value| {
                            if let ArchivedValue::Bool(value) = value
                                && *value
                            {
                                true
                            } else {
                                false
                            }
                        })
                        .collect()
                } else {
                    vec![false; row_count]
                }
            }
            CompiledFilter::Const(value) => vec![*value; row_count],
        }
    }

    pub fn get_col_defs_inside<'a>(&self, table_col_defs: &'a [ColumnDef]) -> Vec<&'a ColumnDef> {
        let mut columns_to_filter = Vec::new();

        self.get_column_indexes(&mut columns_to_filter);

        columns_to_filter
            .into_iter()
            .map(|col_idx| &table_col_defs[col_idx])
            .collect()
    }

    pub fn filter_marks(
        &self,
        marks: &[Mark],
        use_filter_optimization: bool,
        pk_col_defs: &[ColumnDef],
        table_col_defs: &[ColumnDef],
    ) -> Vec<usize> {
        if use_filter_optimization {
            self.filter_marks_impl(marks, pk_col_defs, table_col_defs)
        } else {
            (0..marks.len()).collect()
        }
    }

    // todo: remove this fn...
    fn find_values<'a>(
        marks: &'a [Mark],
        pk_col_defs: &[ColumnDef],
        col_def: &ColumnDef,
    ) -> Vec<&'a Value> {
        let idx = pk_col_defs
            .iter()
            .position(|pk_col_def| pk_col_def == col_def);

        marks
            .iter()
            .map(|mark| {
                if let Some(idx) = idx {
                    &mark.index[idx]
                } else {
                    &Value::Null
                }
            })
            .collect()
    }

    fn filter_marks_impl(
        &self,
        marks: &[Mark],
        pk_col_defs: &[ColumnDef],
        table_col_defs: &[ColumnDef],
    ) -> Vec<usize> {
        match self {
            CompiledFilter::Compare { col_idx, op, value } => {
                let values = Self::find_values(marks, pk_col_defs, &table_col_defs[*col_idx]);

                match *op {
                    BinOp::Eq => {
                        let start = values.partition_point(|&v| v < value);
                        let start = start.saturating_sub(1);
                        let end = values.partition_point(|&v| v <= value);
                        (start..end).collect()
                    }
                    BinOp::NotEq => (0..marks.len()).collect(), // cannot determine if it's present without reading
                    BinOp::Lt => {
                        let end = values.partition_point(|&v| v < value);
                        (0..end).collect()
                    }
                    BinOp::LtEq => {
                        let end = values.partition_point(|&v| v <= value);
                        (0..end).collect()
                    }
                    BinOp::Gt => {
                        let start = values.partition_point(|&v| v <= value);
                        let start = start.saturating_sub(1);
                        (start..marks.len()).collect()
                    }
                    BinOp::GtEq => {
                        let start = values.partition_point(|&v| v < value);
                        let start = start.saturating_sub(1);
                        (start..marks.len()).collect()
                    }
                }
            }
            CompiledFilter::CompareColumns {
                left_idx,
                op,
                right_idx,
            } => {
                let left_values = Self::find_values(marks, pk_col_defs, &table_col_defs[*left_idx]);
                let right_values =
                    Self::find_values(marks, pk_col_defs, &table_col_defs[*right_idx]);

                left_values
                    .into_iter()
                    .zip(right_values)
                    .enumerate()
                    .filter_map(|(idx, (a, b))| {
                        if CompiledFilter::cmp_vals(a, b, op) {
                            Some(idx)
                        } else {
                            None
                        }
                    })
                    .collect()
            }
            CompiledFilter::Or(a, b) => {
                let mut left = a.filter_marks_impl(marks, pk_col_defs, table_col_defs);
                let right = b.filter_marks_impl(marks, pk_col_defs, table_col_defs);

                for i in right {
                    if !left.contains(&i) {
                        left.push(i);
                    }
                }

                left
            }
            CompiledFilter::And(a, b) => {
                let mut left = a.filter_marks_impl(marks, pk_col_defs, table_col_defs);
                let right = b.filter_marks_impl(marks, pk_col_defs, table_col_defs);

                left.retain(|idx| right.contains(idx));
                left
            }
            CompiledFilter::Not(inner) => {
                let result = inner.filter_marks_impl(marks, pk_col_defs, table_col_defs);
                (0..marks.len()).filter(|x| !result.contains(x)).collect()
            }
            CompiledFilter::Const(value) => {
                if *value {
                    (0..marks.len()).collect()
                } else {
                    Vec::new()
                }
            }
            CompiledFilter::Column(col_idx) => {
                let left_values = Self::find_values(marks, pk_col_defs, &table_col_defs[*col_idx]);

                left_values
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, &value)| {
                        if let Value::Bool(val) = value
                            && *val
                        {
                            Some(idx)
                        } else {
                            None
                        }
                    })
                    .collect()
            }
        }
    }
}

impl TryFrom<BinaryOperator> for BinOp {
    type Error = Error;

    fn try_from(value: BinaryOperator) -> Result<Self> {
        match value {
            BinaryOperator::Gt => Ok(Self::Gt),
            BinaryOperator::Lt => Ok(Self::Lt),
            BinaryOperator::GtEq => Ok(Self::GtEq),
            BinaryOperator::LtEq => Ok(Self::LtEq),
            BinaryOperator::Eq => Ok(Self::Eq),
            BinaryOperator::NotEq => Ok(Self::NotEq),
            _ => Err(Error::UnsupportedFilter(value.to_string())),
        }
    }
}

impl BinOp {
    fn flip(self) -> Self {
        match self {
            Self::Gt => Self::Lt,
            Self::Lt => Self::Gt,
            Self::GtEq => Self::LtEq,
            Self::LtEq => Self::GtEq,
            Self::Eq => Self::Eq,
            Self::NotEq => Self::NotEq,
        }
    }
}
