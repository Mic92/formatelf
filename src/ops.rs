//! Pure transforms on [`ir::ElfImage`]. Never compute offsets or touch I/O;
//! relayout happens once afterwards in the layout engine.

use crate::cli::Operation;
use crate::error::Result;
use crate::ir::ElfImage;

/// Output from `--print-*` operations.
#[derive(Debug, Default)]
pub struct Report {
    pub lines: Vec<String>,
}

pub fn apply(_image: &mut ElfImage, _op: &Operation, _report: &mut Report) -> Result<()> {
    todo!("apply operation to IR")
}
