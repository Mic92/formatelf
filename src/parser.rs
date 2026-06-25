//! Decode raw ELF bytes into [`ir::ElfImage`]. One of two places aware of
//! class/endianness; uses `object` raw structs so the Elf32/Elf64 field-reorder
//! and width differences stay in one audited spot.

use crate::error::Result;
use crate::ir::ElfImage;

pub fn parse(_bytes: &[u8]) -> Result<ElfImage> {
    todo!("decode raw ELF into native IR via object raw structs")
}
