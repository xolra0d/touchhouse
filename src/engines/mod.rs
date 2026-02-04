mod merge_tree;
mod replacing_merge_tree;

use rkyv::{Archive as RkyvArchive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};

use self::merge_tree::MergeTreeEngine;
use self::replacing_merge_tree::ReplacingMergeTreeEngine;
use crate::error::{Error, Result};
use crate::sql::Projection;
use crate::storage::{ColumnDef, OutputColumn};

/// Interface for every engine to follow.
pub trait Engine {
    /// Orders columns for insert by `order_by`.
    fn order_columns(&self, columns: Vec<OutputColumn>) -> Result<Vec<OutputColumn>>;
}

/// Used for storing engine name in metadata.
#[derive(
    Default, Debug, Eq, Hash, PartialEq, Clone, RkyvSerialize, RkyvArchive, RkyvDeserialize,
)]
pub enum EngineName {
    #[default]
    MergeTree,
    ReplacingMergeTree,
}

impl TryFrom<&str> for EngineName {
    type Error = Error;
    fn try_from(value: &str) -> Result<Self> {
        match value {
            "MergeTree" => Ok(Self::MergeTree),
            "ReplacingMergeTree" => Ok(Self::ReplacingMergeTree),
            _ => Err(Error::InvalidEngineName),
        }
    }
}

/// Engine configuration. Used to configure engine before running.
pub struct EngineConfig<'a> {
    order_by: &'a [Projection],
    primary_key: &'a [ColumnDef],
}

impl<'a> EngineConfig<'a> {
    pub fn new(order_by: &'a [Projection], primary_key: &'a [ColumnDef]) -> Self {
        Self {
            order_by,
            primary_key,
        }
    }
}

impl<'a> EngineName {
    /// Returns engine implementation for the given engine name.
    pub fn get_engine(&self, config: EngineConfig<'a>) -> Box<dyn Engine + 'a> {
        match self {
            EngineName::MergeTree => Box::new(MergeTreeEngine::new(config)),
            EngineName::ReplacingMergeTree => Box::new(ReplacingMergeTreeEngine::new(config)),
        }
    }
}
