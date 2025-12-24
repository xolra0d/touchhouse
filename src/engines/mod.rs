mod merge_tree;
mod replacing_merge_tree;

use crate::engines::merge_tree::MergeTreeEngine;
use crate::engines::replacing_merge_tree::ReplacingMergeTreeEngine;
use crate::error::{Error, Result};
use crate::sql::OutputColumn;
use crate::sql::Projection;
use crate::storage::ColumnDef;

use rkyv::{Archive as RkyvArchive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};

/// Interface for every engine to follow.
pub trait Engine {
    /// Orders columns for insert by `order_by`.
    fn order_columns(
        &self,
        columns: Vec<OutputColumn>,
        order_by: &[Projection],
        primary_key: &[ColumnDef],
    ) -> Result<Vec<OutputColumn>>;
}

/// Used for storing engine name in metadata.
#[derive(Debug, Eq, Hash, PartialEq, Clone, RkyvSerialize, RkyvArchive, RkyvDeserialize)]
pub enum EngineName {
    MergeTree,
    ReplacingMergeTree,
}

impl Default for EngineName {
    fn default() -> Self {
        Self::MergeTree
    }
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
#[derive(Default)]
pub struct EngineConfig {}

impl EngineName {
    /// Returns engine implementation for the given engine name.
    pub fn get_engine(&self, config: EngineConfig) -> Box<dyn Engine> {
        match self {
            EngineName::MergeTree => Box::new(MergeTreeEngine::new(config)),
            EngineName::ReplacingMergeTree => Box::new(ReplacingMergeTreeEngine::new(config)),
        }
    }
}
