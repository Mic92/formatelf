//! Validate a [`layout::Plan`]: make loadable-ELF invariants explicit instead
//! of relying on them holding by construction. Run before serialization and as
//! a test assertion.

use crate::error::Result;
use crate::ir::ElfImage;
use crate::layout::Plan;

/// Checks: PT_LOAD non-overlap, no W+X, page-congruent offset/vaddr, phdr
/// containment, alignment.
pub fn validate(_image: &ElfImage, _plan: &Plan) -> Result<()> {
    todo!("validate layout invariants")
}
