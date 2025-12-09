use crate::error::{Error, Result};

use rkyv::{Archive as RkyvArchive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::Serialize;
use sqlparser::ast::{DataType as SQLDatatype, Value as SQLValue};
use std::cmp::Ordering;
use uuid::Uuid;

/// Represents a parsed value in our custom protocol
#[derive(
    Clone, Debug, PartialEq, Default, Serialize, RkyvSerialize, RkyvArchive, RkyvDeserialize,
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
            SQLValue::Number(number, _) => {
                let parse_err = |_| Error::InvalidSource("Could not parse number".to_string());
                match value_type {
                    ValueType::Int8 => Ok(Self::Int8(number.parse().map_err(parse_err)?)),
                    ValueType::Int16 => Ok(Self::Int16(number.parse().map_err(parse_err)?)),
                    ValueType::Int32 => Ok(Self::Int32(number.parse().map_err(parse_err)?)),
                    ValueType::Int64 => Ok(Self::Int64(number.parse().map_err(parse_err)?)),
                    ValueType::UInt8 => Ok(Self::UInt8(number.parse().map_err(parse_err)?)),
                    ValueType::UInt16 => Ok(Self::UInt16(number.parse().map_err(parse_err)?)),
                    ValueType::UInt32 => Ok(Self::UInt32(number.parse().map_err(parse_err)?)),
                    ValueType::UInt64 => Ok(Self::UInt64(number.parse().map_err(parse_err)?)),
                    _ => Err(Error::UnsupportedColumnType(format!(
                        "Cannot convert number to {value_type:?}",
                    ))),
                }
            }
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
            SQLValue::Number(number, _) => Ok(Value::Int64(number.parse().map_err(|_| {
                Error::InvalidSource(format!("Failed to parse number as Int64r: {number}"))
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
        }
    }
}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (Value::Null, Value::Null) => Some(Ordering::Equal), // todo: maybe replace with not eq..
            (Value::String(l), Value::String(r)) => Some(l.cmp(r)),
            (Value::Bool(l), Value::Bool(r)) => Some(l.cmp(r)),
            (Value::Uuid(l), Value::Uuid(r)) => Some(l.cmp(r)),
            (Value::Int8(l), Value::Int8(r)) => Some(l.cmp(r)),
            (Value::Int16(l), Value::Int16(r)) => Some(l.cmp(r)),
            (Value::Int32(l), Value::Int32(r)) => Some(l.cmp(r)),
            (Value::Int64(l), Value::Int64(r)) => Some(l.cmp(r)),
            (Value::UInt8(l), Value::UInt8(r)) => Some(l.cmp(r)),
            (Value::UInt16(l), Value::UInt16(r)) => Some(l.cmp(r)),
            (Value::UInt32(l), Value::UInt32(r)) => Some(l.cmp(r)),
            (Value::UInt64(l), Value::UInt64(r)) => Some(l.cmp(r)),
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
            _ => None,
        }
    }
}

impl PartialOrd<Value> for ArchivedValue {
    fn partial_cmp(&self, rhs: &Value) -> Option<Ordering> {
        match (self, rhs) {
            (Self::Null, Value::Null) => Some(Ordering::Equal),
            (Self::String(l), Value::String(r)) => l.partial_cmp(r),
            (Self::Uuid(l), Value::Uuid(r)) => l.partial_cmp(r),
            (Self::Bool(l), Value::Bool(r)) => l.partial_cmp(r),
            (Self::Int8(l), Value::Int8(r)) => l.partial_cmp(r),
            (Self::Int16(l), Value::Int16(r)) => l.to_native().partial_cmp(r),
            (Self::Int32(l), Value::Int32(r)) => l.to_native().partial_cmp(r),
            (Self::Int64(l), Value::Int64(r)) => l.to_native().partial_cmp(r),
            (Self::UInt8(l), Value::UInt8(r)) => l.partial_cmp(r),
            (Self::UInt16(l), Value::UInt16(r)) => l.to_native().partial_cmp(r),
            (Self::UInt32(l), Value::UInt32(r)) => l.to_native().partial_cmp(r),
            (Self::UInt64(l), Value::UInt64(r)) => l.to_native().partial_cmp(r),
            _ => None,
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
            _ => false,
        }
    }
}

impl PartialOrd<ArchivedValue> for ArchivedValue {
    fn partial_cmp(&self, rhs: &ArchivedValue) -> Option<Ordering> {
        match (self, rhs) {
            (ArchivedValue::Null, ArchivedValue::Null) => Some(Ordering::Equal), // todo: maybe replace with not eq..
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
            _ => None,
        }
    }
}
