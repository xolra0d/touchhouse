use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Strategy to read rows.
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
        should_read_all_rows: bool,
    ) -> Self {
        let lines_to_read = if should_read_all_rows {
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

    pub fn set_lines_read(&self, num: usize) {
        self.lines_read.store(num, Ordering::Relaxed);
    }

    pub fn should_read_next_chunk(&self) -> bool {
        self.lines_to_read > self.lines_read.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::Strategy;

    #[test]
    fn should_read_whole() {
        let strategy = Strategy::new(None, 0, 123, true);
        assert_eq!(strategy.lines_to_read, 123);
    }

    #[test]
    fn should_read_only_part() {
        let strategy = Strategy::new(Some(30), 10, 123, false);
        dbg!(strategy.lines_to_read);
        assert_eq!(strategy.lines_to_read, 40);
    }
}
