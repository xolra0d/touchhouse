use dashmap::DashMap;
use std::sync::atomic::AtomicUsize;

use crate::storage::{TableDef, TableMetadata, TablePartInfo};

#[derive(Debug, Clone)]
pub struct TableConfig {
    pub metadata: TableMetadata,
    pub infos: Vec<TablePartInfo>,
}

/// Stores preloaded each table configuration for quick access.
pub static TABLE_DATA: std::sync::LazyLock<DashMap<TableDef, TableConfig>> =
    std::sync::LazyLock::new(DashMap::default);

/// Signifies when it's ok to lock `TABLE_DATA` to merge `TablePart`.
/// Decrements is only done through implementing `Drop`.
pub static DATABASE_LOAD: std::sync::LazyLock<AtomicUsize> =
    std::sync::LazyLock::new(AtomicUsize::default);

/// Guard that decrements `DATABASE_LOAD` on drop.
///
/// Used to track query complexity and automatically release resources when query completes.
pub struct ComplexityGuard {
    complexity: usize,
}

impl ComplexityGuard {
    /// Creates a new complexity guard with the given complexity value.
    pub fn new(complexity: usize) -> Self {
        Self { complexity }
    }
}

impl Drop for ComplexityGuard {
    fn drop(&mut self) {
        DATABASE_LOAD.fetch_sub(self.complexity, std::sync::atomic::Ordering::Relaxed);
    }
}
