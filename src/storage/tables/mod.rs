mod native;
mod r#virtual;

use crate::error::Result;
use crate::sql::Projection;
use crate::storage::{PhysicalColumn, TableSchema, ToValue};

pub use self::{native::NativeStorage, r#virtual::VirtualStorage};

pub struct Immutable;
pub struct Mutable;

mod sealed {
    pub trait SealedMode {}
    impl SealedMode for super::Immutable {}
    impl SealedMode for super::Mutable {}
}

pub trait StorageRead {
    fn get_total_rows(&self) -> usize;
    fn get_schema(&self) -> &TableSchema;
    fn load_next_chunk(&mut self) -> Result<Option<()>>;
    fn access_chunk_column(&self, proj: &Projection) -> Result<Vec<impl ToValue>>;
}

pub trait StorageWrite {
    fn insert(&mut self, columns: Vec<PhysicalColumn>) -> Result<()>;
    fn drop(self, if_exists: bool) -> Result<()>;
}
