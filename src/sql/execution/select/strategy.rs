use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Clone)]
pub struct Strategy {
    pub lines_to_read: usize,
    pub lines_read: Arc<AtomicUsize>,
}

impl Strategy {
    pub fn new(
        limit: Option<usize>,
        offset: usize,
        in_table_lines: usize,
        have_order_by: bool,
    ) -> Self {
        let lines_to_read = if have_order_by {
            in_table_lines
        } else if let Some(limit) = limit {
            in_table_lines.min(limit) + offset
        } else {
            in_table_lines
        };

        Self {
            lines_to_read,
            lines_read: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn set_read_lines(&self, num: usize) {
        self.lines_read.store(num, Ordering::Relaxed);
    }

    pub fn should_read_next_chunk(&self) -> bool {
        self.lines_to_read > self.lines_read.load(Ordering::Relaxed)
    }
}
