use crate::error::{Error, Result};
use crate::storage::{ColumnDef, ToValue, Value, ValueType};
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
    pub fn compile(filter: Expr, table_column_defs: &[ColumnDef]) -> Result<Self> {
        match filter {
            Expr::BinaryOp { op, left, right } => match op {
                BinaryOperator::And => {
                    let left = Self::compile(*left, table_column_defs)?;

                    if let Self::Const(false) = left {
                        return Ok(Self::Const(false));
                    }
                    if let Self::Const(true) = left {
                        return Self::compile(*right, table_column_defs);
                    }

                    let right = Self::compile(*right, table_column_defs)?;

                    if let Self::Const(false) = right {
                        return Ok(Self::Const(false));
                    }
                    if let Self::Const(true) = right {
                        return Ok(left);
                    }

                    Ok(Self::And(Box::new(left), Box::new(right)))
                }
                BinaryOperator::Or => {
                    let left = Self::compile(*left, table_column_defs)?;

                    if let Self::Const(true) = left {
                        return Ok(Self::Const(true));
                    }
                    if let Self::Const(false) = left {
                        return Self::compile(*right, table_column_defs);
                    }

                    let right = Self::compile(*right, table_column_defs)?;

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

                            Ok(Self::Const(left.fits_op(&right, &op)))
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
                    Ok(Self::Not(Box::new(Self::compile(
                        *expr,
                        table_column_defs,
                    )?))) // todo: create .flip to flip without additional Not
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
    pub fn filter_granule<T: ToValue>(&self, columns: &mut Vec<Vec<T>>) -> Result<()> {
        let mask = self.get_granule_mask(columns)?;

        for column in columns {
            let mut idx = 0;
            column.retain(|_| {
                let flag = mask[idx];
                idx += 1;
                flag
            });
        }

        Ok(())
    }

    fn get_granule_mask<T: ToValue>(&self, columns: &[Vec<T>]) -> Result<Vec<bool>> {
        let mut mask = Vec::with_capacity(columns.first().map(|x| x.len()).unwrap_or(0));

        match self {
            Self::Column(col_idx) => {
                for val in &columns[*col_idx] {
                    if val.is_true() {
                        mask.push(true);
                    } else {
                        mask.push(false);
                    }
                }
            }
            Self::Compare { col_idx, op, value } => {
                for left in &columns[*col_idx] {
                    mask.push(left.fits_op(value, op))
                }
            }
            Self::CompareColumns {
                left_idx,
                op,
                right_idx,
            } => {
                let left_vals = &columns[*left_idx];
                let right_vals = &columns[*right_idx];

                for (l, r) in left_vals.iter().zip(right_vals) {
                    mask.push(l.fits_op(r, op))
                }
            }
            Self::And(l, r) => {
                let left = l.get_granule_mask(columns)?;
                let right = r.get_granule_mask(columns)?;

                for (l, r) in left.into_iter().zip(right) {
                    mask.push(l && r);
                }
            }
            Self::Or(l, r) => {
                let left = l.get_granule_mask(columns)?;
                let right = r.get_granule_mask(columns)?;

                for (l, r) in left.into_iter().zip(right) {
                    mask.push(l || r);
                }
            }
            Self::Not(inner) => {
                let inner = inner.get_granule_mask(columns)?;

                for val in inner {
                    mask.push(!val);
                }
            }
            Self::Const(val) => {
                let length = columns.first().map(|x| x.len()).unwrap_or(0);
                mask.extend(vec![*val; length]);
            }
        };

        Ok(mask)
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
