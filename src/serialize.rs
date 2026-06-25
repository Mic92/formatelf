//! Emit bytes from [`ir::ElfImage`] + [`layout::Plan`]. Single forward pass;
//! re-encodes to the source class/endianness. Disk writes use a sized temp file
//! (ftruncate + mmap) atomically renamed over the target.

use crate::error::Result;
use crate::ir::ElfImage;
use crate::layout::Plan;

pub fn serialize(_image: &ElfImage, _plan: &Plan) -> Result<Vec<u8>> {
    todo!("encode IR + plan into output bytes")
}
