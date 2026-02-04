use crate::{
    error::{Error, Result},
    sql::BinOp,
};

use rkyv::{
    Archive as RkyvArchive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize, rancor,
};
use serde::Serialize;
use sqlparser::ast::{DataType as SQLDatatype, Value as SQLValue};
use std::{
    cmp::Ordering,
    fmt,
    hash::{Hash, Hasher},
};
use uuid::Uuid;

/// Trait to allow zero-copy deserialization during filtering and aggregation.
pub trait ToValue: PartialEq<Value> + PartialOrd<Value> + PartialEq + PartialOrd + Clone {
    fn to_value(self) -> Result<Value>;
    fn is_true(&self) -> bool;
    fn as_f64(&self) -> Option<f64>;

    fn fits_op<T: ToValue>(&self, val: &T, op: &BinOp) -> bool
    where
        Self: PartialOrd<T>,
    {
        match op {
            BinOp::Gt => self > val,
            BinOp::GtEq => self >= val,
            BinOp::Lt => self < val,
            BinOp::LtEq => self <= val,
            BinOp::Eq => self == val,
            BinOp::NotEq => self != val,
        }
    }
}

impl ToValue for Value {
    fn to_value(self) -> Result<Value> {
        Ok(self)
    }

    fn is_true(&self) -> bool {
        if let Value::Bool(val) = self
            && *val
        {
            true
        } else {
            false
        }
    }

    fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Int8(v) => Some(f64::from(*v)),
            Self::Int16(v) => Some(f64::from(*v)),
            Self::Int32(v) => Some(f64::from(*v)),
            Self::Int64(v) => Some(*v as f64),
            Self::UInt8(v) => Some(f64::from(*v)),
            Self::UInt16(v) => Some(f64::from(*v)),
            Self::UInt32(v) => Some(f64::from(*v)),
            Self::UInt64(v) => Some(*v as f64),
            Self::F32(v) => Some(f64::from(*v)),
            Self::F64(v) => Some(*v),
            Self::String(_) | Self::Null | Self::Bool(_) | Self::Uuid(_) => None,
        }
    }
}

impl ToValue for &ArchivedValue {
    fn to_value(self) -> Result<Value> {
        rkyv::deserialize::<Value, rancor::Error>(self).map_err(|error| {
            Error::CouldNotReadData(format!("Could not deserialize value ({self:?}): {error}"))
        })
    }

    fn is_true(&self) -> bool {
        if let ArchivedValue::Bool(val) = self
            && *val
        {
            true
        } else {
            false
        }
    }

    fn as_f64(&self) -> Option<f64> {
        match self {
            ArchivedValue::Int8(v) => Some(f64::from(*v)),
            ArchivedValue::Int16(v) => Some(f64::from(v.to_native())),
            ArchivedValue::Int32(v) => Some(f64::from(v.to_native())),
            ArchivedValue::Int64(v) => Some(v.to_native() as f64),
            ArchivedValue::UInt8(v) => Some(f64::from(*v)),
            ArchivedValue::UInt16(v) => Some(f64::from(v.to_native())),
            ArchivedValue::UInt32(v) => Some(f64::from(v.to_native())),
            ArchivedValue::UInt64(v) => Some(v.to_native() as f64),
            ArchivedValue::F32(v) => Some(f64::from(v.to_native())),
            ArchivedValue::F64(v) => Some(v.to_native()),
            ArchivedValue::String(_)
            | ArchivedValue::Null
            | ArchivedValue::Bool(_)
            | ArchivedValue::Uuid(_) => None,
        }
    }
}

/// Represents a parsed value in our custom protocol
#[derive(
    Clone, Debug, Default, PartialEq, Serialize, RkyvSerialize, RkyvArchive, RkyvDeserialize,
)]
#[rkyv(derive(Debug), compare(PartialEq))]
pub enum Value {
    #[default]
    Null,
    String(String),
    Uuid(Uuid),
    Bool(bool),

    Int8(i8),
    Int16(i16),
    Int32(i32),
    Int64(i64),

    UInt8(u8),
    UInt16(u16),
    UInt32(u32),
    UInt64(u64),

    F32(f32),
    F64(f64),
}

impl Eq for Value {}

impl Hash for Value {
    fn hash<H: Hasher>(&self, state: &mut H) {
        core::mem::discriminant(self).hash(state);
        match self {
            Self::Null => {}
            Self::String(val) => val.hash(state),
            Self::Uuid(val) => val.hash(state),
            Self::Bool(val) => val.hash(state),
            Self::Int8(val) => val.hash(state),
            Self::Int16(val) => val.hash(state),
            Self::Int32(val) => val.hash(state),
            Self::Int64(val) => val.hash(state),
            Self::UInt8(val) => val.hash(state),
            Self::UInt16(val) => val.hash(state),
            Self::UInt32(val) => val.hash(state),
            Self::UInt64(val) => val.hash(state),
            Self::F32(val) => {
                let val = if val.is_nan() {
                    0x7fc0_0000
                } else if *val == 0. {
                    0
                } else {
                    val.to_bits()
                };

                val.hash(state);
            }
            Self::F64(val) => {
                let val = if val.is_nan() {
                    0x7fc0_0000
                } else if *val == 0. {
                    0
                } else {
                    val.to_bits()
                };

                val.hash(state);
            }
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => write!(f, "null"),
            Self::String(val) => write!(f, "String({val})"),
            Self::Uuid(val) => write!(f, "Uuid({val})"),
            Self::Bool(val) => write!(f, "Bool({val})"),
            Self::Int8(val) => write!(f, "Int8({val})"),
            Self::Int16(val) => write!(f, "Int16({val})"),
            Self::Int32(val) => write!(f, "Int32({val})"),
            Self::Int64(val) => write!(f, "Int64({val})"),
            Self::UInt8(val) => write!(f, "UInt8({val})"),
            Self::UInt16(val) => write!(f, "UInt16({val})"),
            Self::UInt32(val) => write!(f, "UInt32({val})"),
            Self::UInt64(val) => write!(f, "UInt64({val})"),
            Self::F32(val) => write!(f, "Float32({val})"),
            Self::F64(val) => write!(f, "Float64({val})"),
        }
    }
}

impl TryFrom<(SQLValue, &ValueType)> for Value {
    type Error = Error;
    fn try_from(value: (SQLValue, &ValueType)) -> Result<Self> {
        let (sql_value, value_type) = value;

        match sql_value {
            SQLValue::Null => Ok(Self::Null),
            SQLValue::SingleQuotedString(string)
            | SQLValue::TripleSingleQuotedString(string)
            | SQLValue::TripleDoubleQuotedString(string) => {
                if value_type == &ValueType::String {
                    Ok(Self::String(string))
                } else if value_type == &ValueType::Uuid {
                    let uuid = Uuid::parse_str(&string).map_err(|error| {
                        Error::InvalidSource(format!("Could not parse uuid: {error}"))
                    })?;
                    Ok(Self::Uuid(uuid))
                } else {
                    Err(Error::InvalidSource(format!(
                        "Could not convert {string} to {value_type:?}",
                    )))
                }
            }
            SQLValue::Number(number, _) => match value_type {
                ValueType::Int8 => Ok(Self::Int8(number.parse().map_err(|error| {
                    Error::InvalidSource(format!("Could not parse number ({number}): {error}"))
                })?)),
                ValueType::Int16 => Ok(Self::Int16(number.parse().map_err(|error| {
                    Error::InvalidSource(format!("Could not parse number ({number}): {error}"))
                })?)),
                ValueType::Int32 => Ok(Self::Int32(number.parse().map_err(|error| {
                    Error::InvalidSource(format!("Could not parse number ({number}): {error}"))
                })?)),
                ValueType::Int64 => Ok(Self::Int64(number.parse().map_err(|error| {
                    Error::InvalidSource(format!("Could not parse number ({number}): {error}"))
                })?)),
                ValueType::UInt8 => Ok(Self::UInt8(number.parse().map_err(|error| {
                    Error::InvalidSource(format!("Could not parse number ({number}): {error}"))
                })?)),
                ValueType::UInt16 => Ok(Self::UInt16(number.parse().map_err(|error| {
                    Error::InvalidSource(format!("Could not parse number ({number}): {error}"))
                })?)),
                ValueType::UInt32 => Ok(Self::UInt32(number.parse().map_err(|error| {
                    Error::InvalidSource(format!("Could not parse number ({number}): {error}"))
                })?)),
                ValueType::UInt64 => Ok(Self::UInt64(number.parse().map_err(|error| {
                    Error::InvalidSource(format!("Could not parse number ({number}): {error}"))
                })?)),
                ValueType::F32 => Ok(Self::F32(number.parse().map_err(|error| {
                    Error::InvalidSource(format!("Could not parse number ({number}): {error}"))
                })?)),
                ValueType::F64 => Ok(Self::F64(number.parse().map_err(|error| {
                    Error::InvalidSource(format!("Could not parse number ({number}): {error}"))
                })?)),
                _ => Err(Error::UnsupportedColumnType(format!(
                    "Cannot convert number to {value_type:?}",
                ))),
            },
            SQLValue::Boolean(bool_value) => {
                if value_type != &ValueType::Bool {
                    return Err(Error::InvalidSource(format!(
                        "Could not convert boolean value to {value_type:?}"
                    )));
                }
                Ok(Self::Bool(bool_value))
            }
            SQLValue::Placeholder(_) => Err(Error::UnsupportedColumnType(
                "Plan to add placeholder support".to_string(),
            )),
            column_type => Err(Error::UnsupportedColumnType(column_type.to_string())),
        }
    }
}

impl Value {
    pub fn try_from_untyped(value: SQLValue) -> Result<Value> {
        match value {
            SQLValue::Null => Ok(Value::Null),
            SQLValue::SingleQuotedString(s)
            | SQLValue::TripleSingleQuotedString(s)
            | SQLValue::TripleDoubleQuotedString(s) => Ok(Value::String(s)),
            SQLValue::Number(number, _) => Ok(Value::F64(number.parse().map_err(|error| {
                Error::InvalidSource(format!(
                    "Failed to parse number as Float64 ({number}): {error}"
                ))
            })?)),
            SQLValue::Boolean(b) => Ok(Value::Bool(b)),
            _ => Err(Error::InvalidSource(format!(
                "Unsupported SQL value type: {value:?}"
            ))),
        }
    }
}

#[derive(
    Debug, Clone, Hash, PartialEq, Eq, Serialize, RkyvSerialize, RkyvArchive, RkyvDeserialize,
)]
pub enum ValueType {
    Null,
    String,
    Uuid,
    Bool,

    Int8,
    Int16,
    Int32,
    Int64,

    UInt8,
    UInt16,
    UInt32,
    UInt64,

    F32,
    F64,
}

impl TryFrom<&SQLDatatype> for ValueType {
    type Error = Error;

    fn try_from(value: &SQLDatatype) -> Result<Self> {
        match value {
            SQLDatatype::String(_) => Ok(Self::String),
            SQLDatatype::Uuid => Ok(Self::Uuid),
            SQLDatatype::Bool => Ok(Self::Bool),
            SQLDatatype::Int8(_) => Ok(Self::Int8),
            SQLDatatype::Int16 => Ok(Self::Int16),
            SQLDatatype::Int32 => Ok(Self::Int32),
            SQLDatatype::Int64 => Ok(Self::Int64),
            SQLDatatype::UInt8 => Ok(Self::UInt8),
            SQLDatatype::UInt16 => Ok(Self::UInt16),
            SQLDatatype::UInt32 => Ok(Self::UInt32),
            SQLDatatype::UInt64 => Ok(Self::UInt64),
            SQLDatatype::Float32 => Ok(Self::F32),
            SQLDatatype::Float64 => Ok(Self::F64),
            column_type => Err(Error::UnsupportedColumnType(column_type.to_string())),
        }
    }
}

impl Value {
    /// Returns the `ValueType` corresponding to this value.
    pub fn get_type(&self) -> ValueType {
        match &self {
            Value::Null => ValueType::Null,
            Value::String(_) => ValueType::String,
            Value::Uuid(_) => ValueType::Uuid,
            Value::Bool(_) => ValueType::Bool,
            Value::Int8(_) => ValueType::Int8,
            Value::Int16(_) => ValueType::Int16,
            Value::Int32(_) => ValueType::Int32,
            Value::Int64(_) => ValueType::Int64,
            Value::UInt8(_) => ValueType::UInt8,
            Value::UInt16(_) => ValueType::UInt16,
            Value::UInt32(_) => ValueType::UInt32,
            Value::UInt64(_) => ValueType::UInt64,
            Value::F32(_) => ValueType::F32,
            Value::F64(_) => ValueType::F64,
        }
    }
}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (Value::Null, Value::Null) => Some(Ordering::Equal),
            (Value::String(l), Value::String(r)) => l.partial_cmp(r),
            (Value::Bool(l), Value::Bool(r)) => l.partial_cmp(r),
            (Value::Uuid(l), Value::Uuid(r)) => l.partial_cmp(r),
            (Value::Int8(l), Value::Int8(r)) => l.partial_cmp(r),
            (Value::Int16(l), Value::Int16(r)) => l.partial_cmp(r),
            (Value::Int32(l), Value::Int32(r)) => l.partial_cmp(r),
            (Value::Int64(l), Value::Int64(r)) => l.partial_cmp(r),
            (Value::UInt8(l), Value::UInt8(r)) => l.partial_cmp(r),
            (Value::UInt16(l), Value::UInt16(r)) => l.partial_cmp(r),
            (Value::UInt32(l), Value::UInt32(r)) => l.partial_cmp(r),
            (Value::UInt64(l), Value::UInt64(r)) => l.partial_cmp(r),
            (Value::F32(l), Value::F32(r)) => l.partial_cmp(r),
            (Value::F64(l), Value::F64(r)) => l.partial_cmp(r),
            _ => None,
        }
    }
}

impl PartialOrd<ArchivedValue> for Value {
    fn partial_cmp(&self, rhs: &ArchivedValue) -> Option<Ordering> {
        match (self, rhs) {
            (Self::Null, ArchivedValue::Null) => Some(Ordering::Equal),
            (Self::String(l), ArchivedValue::String(r)) => l.partial_cmp(r),
            (Self::Uuid(l), ArchivedValue::Uuid(r)) => l.partial_cmp(r),
            (Self::Bool(l), ArchivedValue::Bool(r)) => l.partial_cmp(r),
            (Self::Int8(l), ArchivedValue::Int8(r)) => l.partial_cmp(r),
            (Self::Int16(l), ArchivedValue::Int16(r)) => l.partial_cmp(&r.to_native()),
            (Self::Int32(l), ArchivedValue::Int32(r)) => l.partial_cmp(&r.to_native()),
            (Self::Int64(l), ArchivedValue::Int64(r)) => l.partial_cmp(&r.to_native()),
            (Self::UInt8(l), ArchivedValue::UInt8(r)) => l.partial_cmp(r),
            (Self::UInt16(l), ArchivedValue::UInt16(r)) => l.partial_cmp(&r.to_native()),
            (Self::UInt32(l), ArchivedValue::UInt32(r)) => l.partial_cmp(&r.to_native()),
            (Self::UInt64(l), ArchivedValue::UInt64(r)) => l.partial_cmp(&r.to_native()),
            (Self::F32(l), ArchivedValue::F32(r)) => l.partial_cmp(&r.to_native()),
            (Self::F64(l), ArchivedValue::F64(r)) => l.partial_cmp(&r.to_native()),
            _ => None,
        }
    }
}

impl PartialOrd<Value> for &ArchivedValue {
    fn partial_cmp(&self, rhs: &Value) -> Option<Ordering> {
        match (self, rhs) {
            (ArchivedValue::Null, Value::Null) => Some(Ordering::Equal),
            (ArchivedValue::String(l), Value::String(r)) => l.partial_cmp(r),
            (ArchivedValue::Uuid(l), Value::Uuid(r)) => l.partial_cmp(r),
            (ArchivedValue::Bool(l), Value::Bool(r)) => l.partial_cmp(r),
            (ArchivedValue::Int8(l), Value::Int8(r)) => l.partial_cmp(r),
            (ArchivedValue::Int16(l), Value::Int16(r)) => l.partial_cmp(r),
            (ArchivedValue::Int32(l), Value::Int32(r)) => l.partial_cmp(r),
            (ArchivedValue::Int64(l), Value::Int64(r)) => l.partial_cmp(r),
            (ArchivedValue::UInt8(l), Value::UInt8(r)) => l.partial_cmp(r),
            (ArchivedValue::UInt16(l), Value::UInt16(r)) => l.partial_cmp(r),
            (ArchivedValue::UInt32(l), Value::UInt32(r)) => l.partial_cmp(r),
            (ArchivedValue::UInt64(l), Value::UInt64(r)) => l.partial_cmp(r),
            (ArchivedValue::F32(l), Value::F32(r)) => l.partial_cmp(r),
            (ArchivedValue::F64(l), Value::F64(r)) => l.partial_cmp(r),
            _ => None,
        }
    }
}

impl PartialEq<Value> for &ArchivedValue {
    fn eq(&self, other: &Value) -> bool {
        match (other, self) {
            (Value::Null, ArchivedValue::Null) => true,
            (Value::String(l), ArchivedValue::String(r)) => l == r,
            (Value::Uuid(l), ArchivedValue::Uuid(r)) => l == r,
            (Value::Bool(l), ArchivedValue::Bool(r)) => l == r,
            (Value::Int8(l), ArchivedValue::Int8(r)) => l == r,
            (Value::Int16(l), ArchivedValue::Int16(r)) => l == r,
            (Value::Int32(l), ArchivedValue::Int32(r)) => l == r,
            (Value::Int64(l), ArchivedValue::Int64(r)) => l == r,
            (Value::UInt8(l), ArchivedValue::UInt8(r)) => l == r,
            (Value::UInt16(l), ArchivedValue::UInt16(r)) => l == r,
            (Value::UInt32(l), ArchivedValue::UInt32(r)) => l == r,
            (Value::UInt64(l), ArchivedValue::UInt64(r)) => l == r,
            (Value::F32(l), ArchivedValue::F32(r)) => l == r,
            (Value::F64(l), ArchivedValue::F64(r)) => l == r,
            _ => false,
        }
    }
}

impl PartialEq<ArchivedValue> for ArchivedValue {
    fn eq(&self, rhs: &ArchivedValue) -> bool {
        match (self, rhs) {
            (Self::Null, ArchivedValue::Null) => true,
            (Self::String(l), ArchivedValue::String(r)) => l == r,
            (Self::Uuid(l), ArchivedValue::Uuid(r)) => l == r,
            (Self::Bool(l), ArchivedValue::Bool(r)) => l == r,
            (Self::Int8(l), ArchivedValue::Int8(r)) => l == r,
            (Self::Int16(l), ArchivedValue::Int16(r)) => l == r,
            (Self::Int32(l), ArchivedValue::Int32(r)) => l == r,
            (Self::Int64(l), ArchivedValue::Int64(r)) => l == r,
            (Self::UInt8(l), ArchivedValue::UInt8(r)) => l == r,
            (Self::UInt16(l), ArchivedValue::UInt16(r)) => l == r,
            (Self::UInt32(l), ArchivedValue::UInt32(r)) => l == r,
            (Self::UInt64(l), ArchivedValue::UInt64(r)) => l == r,
            (Self::F32(l), ArchivedValue::F32(r)) => l == r,
            (Self::F64(l), ArchivedValue::F64(r)) => l == r,
            _ => false,
        }
    }
}

impl PartialOrd<ArchivedValue> for ArchivedValue {
    fn partial_cmp(&self, rhs: &ArchivedValue) -> Option<Ordering> {
        match (self, rhs) {
            (ArchivedValue::Null, ArchivedValue::Null) => Some(Ordering::Equal),
            (Self::String(l), ArchivedValue::String(r)) => l.partial_cmp(r),
            (Self::Uuid(l), ArchivedValue::Uuid(r)) => l.partial_cmp(r),
            (Self::Bool(l), ArchivedValue::Bool(r)) => l.partial_cmp(r),
            (Self::Int8(l), ArchivedValue::Int8(r)) => l.partial_cmp(r),
            (Self::Int16(l), ArchivedValue::Int16(r)) => l.partial_cmp(r),
            (Self::Int32(l), ArchivedValue::Int32(r)) => l.partial_cmp(r),
            (Self::Int64(l), ArchivedValue::Int64(r)) => l.partial_cmp(r),
            (Self::UInt8(l), ArchivedValue::UInt8(r)) => l.partial_cmp(r),
            (Self::UInt16(l), ArchivedValue::UInt16(r)) => l.partial_cmp(r),
            (Self::UInt32(l), ArchivedValue::UInt32(r)) => l.partial_cmp(r),
            (Self::UInt64(l), ArchivedValue::UInt64(r)) => l.partial_cmp(r),
            (Self::F32(l), ArchivedValue::F32(r)) => l.partial_cmp(r),
            (Self::F64(l), ArchivedValue::F64(r)) => l.partial_cmp(r),
            _ => None,
        }
    }
}
