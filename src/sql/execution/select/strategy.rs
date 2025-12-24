#[derive(Debug)]
pub struct Strategy {
    pub lines_to_read: usize,
}

impl Strategy {
    pub fn design_new(
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

        Self { lines_to_read }
    }
}
