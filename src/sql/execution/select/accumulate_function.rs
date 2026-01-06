use crate::error::{Error, Result};
use crate::storage::Value;
use crate::storage::value::ArchivedValue;

use rkyv::rancor;
use rkyv::vec::ArchivedVec;

pub trait AccumulateFn {
    fn new() -> Self;
    fn accumulate_raw(
        &self,
        acc: Vec<Vec<Value>>,
        values: &[Option<(Vec<u8>, &[bool])>],
        row_count: usize,
    ) -> Result<Vec<Vec<Value>>>;
    fn accumulate_values(
        &self,
        acc: Vec<Vec<Value>>,
        values: Vec<Vec<Value>>,
    ) -> Result<Vec<Vec<Value>>>;
}

pub struct CollectFn;

impl AccumulateFn for CollectFn {
    fn new() -> Self {
        CollectFn
    }

    fn accumulate_raw(
        &self,
        mut acc: Vec<Vec<Value>>,
        read_columns: &[Option<(Vec<u8>, &[bool])>],
        row_count: usize,
    ) -> Result<Vec<Vec<Value>>> {
        for (col_idx, col_data) in read_columns.iter().enumerate() {
            if let Some((col_values, bitmask)) = col_data {
                let archived_col_values: &ArchivedVec<ArchivedValue> =
                    unsafe { rkyv::access_unchecked(col_values) };
                for (value_idx, archived_value) in archived_col_values.iter().enumerate() {
                    if bitmask[value_idx] {
                        let value = rkyv::deserialize::<Value, rancor::Error>(archived_value)
                            .map_err(|error| {
                                Error::CouldNotReadData(format!(
                                    "Could not deserialize value ({archived_value:?}): {error}"
                                ))
                            })?;
                        acc[col_idx].push(value);
                    }
                }
            } else {
                acc[col_idx].extend(vec![Value::Null; row_count]);
            }
        }
        Ok(acc)
    }

    fn accumulate_values(
        &self,
        mut acc: Vec<Vec<Value>>,
        values: Vec<Vec<Value>>,
    ) -> Result<Vec<Vec<Value>>> {
        for (col_idx, col_values) in values.into_iter().enumerate() {
            acc[col_idx].extend(col_values);
        }
        Ok(acc)
    }
}
