use crate::storage::Value;
use crate::storage::value::ArchivedValue;
use rkyv::rancor;
use rkyv::vec::ArchivedVec;

pub struct AccumulateFunction;

impl AccumulateFunction {
    pub fn filter_fn(
        mut acc: Vec<Vec<Value>>,
        values: &Vec<Option<(Vec<u8>, &[bool])>>,
        row_count: usize,
    ) -> Vec<Vec<Value>> {
        for (col_idx, col_values) in values.iter().enumerate() {
            if let Some(col_values) = col_values {
                let archived_values: &ArchivedVec<ArchivedValue> =
                    unsafe { rkyv::access_unchecked(&col_values.0) };
                for (value_idx, archived_value) in archived_values.iter().enumerate() {
                    if col_values.1[value_idx] {
                        let value =
                            rkyv::deserialize::<Value, rancor::Error>(archived_value).unwrap();
                        acc[col_idx].push(value);
                    }
                }
            } else {
                acc[col_idx].extend(vec![Value::Null; row_count])
            }
        }
        acc
    }
}
