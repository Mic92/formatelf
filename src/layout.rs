//! Pure layout planner: [`ir::ElfImage`] + [`Policy`] -> [`Plan`]. No I/O, no
//! mutation. Isolates patchelf's executable-vs-library relayout logic so it can
//! be unit- and property-tested.

use crate::error::Result;
use crate::ir::ElfImage;

#[derive(Debug, Clone)]
pub struct Policy {
    pub page_size: Option<u64>,
    pub sort_headers: bool,
    pub clobber_old_sections: bool,
}

#[derive(Debug, Default)]
pub struct Plan {
    pub total_size: u64,
    pub phoff: u64,
    pub shoff: u64,
    /// Parallel to `image.shdrs`.
    pub section_offset: Vec<u64>,
}

pub fn plan(_image: &ElfImage, _policy: &Policy) -> Result<Plan> {
    todo!("compute relayout plan")
}
