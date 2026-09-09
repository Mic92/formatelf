//! `--set-entry-stub FILE`: make position-independent code from FILE the
//! program's entry point. The code lands in a new executable section
//! (`.formatelf.entry`) that the layout engine maps in its own `R+X` segment,
//! and `e_entry` is pointed at it. So the stub can chain to the program, the
//! original `e_entry` (a link-time address, to be biased like `AT_ENTRY`) is
//! written as a 64-bit value in the file's byte order at offset 8 of the
//! section; execution starts at offset 16. FILE therefore needs at least 16
//! leading bytes of header it does not execute.
//!
//! Re-running replaces the stub and keeps chaining to the real program entry,
//! not to the previous stub.

use std::borrow::Cow;
use std::path::Path;

use crate::error::{Error, Result};
use crate::ir::{self, ElfImage, et, shf, sht};

pub const SECTION: &str = ".formatelf.entry";
/// Offset of the saved original entry address inside the section.
pub const SAVED_ENTRY_OFFSET: usize = 8;
/// Offset at which execution starts inside the section.
pub const CODE_OFFSET: u64 = 16;

pub(crate) fn set(image: &mut ElfImage<'_>, file: &Path) -> Result<()> {
    let blob = std::fs::read(file).map_err(|source| Error::Io {
        path: file.to_path_buf(),
        source,
    })?;
    set_bytes(image, blob)
}

pub(crate) fn set_bytes(image: &mut ElfImage<'_>, mut blob: Vec<u8>) -> Result<()> {
    if image.ehdr.e_type != et::EXEC && image.ehdr.e_type != et::DYN {
        return Err(Error::Unsupported(
            "--set-entry-stub needs an executable".into(),
        ));
    }
    if image.enc.class != ir::Class::Elf64 {
        return Err(Error::Unsupported("--set-entry-stub is ELF64-only".into()));
    }
    if (blob.len() as u64) <= CODE_OFFSET {
        return Err(Error::Cli(format!(
            "entry stub must be longer than its {CODE_OFFSET}-byte header"
        )));
    }

    let original_entry = match image.find_section(SECTION) {
        Some(idx) => saved_entry(image, idx)?,
        None => image.ehdr.entry,
    };
    let saved = match image.enc.endian {
        ir::Endian::Little => original_entry.to_le_bytes(),
        ir::Endian::Big => original_entry.to_be_bytes(),
    };
    blob[SAVED_ENTRY_OFFSET..SAVED_ENTRY_OFFSET + 8].copy_from_slice(&saved);

    if let Some(idx) = image.find_section(SECTION) {
        image.section_data[idx] = Cow::Owned(blob);
        image.shdrs[idx].size = 0; // force re-placement even if the length matches
        return Ok(());
    }
    let shstr = image.ehdr.shstrndx as usize;
    if image.section_data.get(shstr).is_none() {
        return Err(Error::Missing("no section header string table".into()));
    }
    let name_off = u32::try_from(image.section_data[shstr].len())
        .map_err(|_| Error::Unsupported(".shstrtab too large".into()))?;
    let shstrtab = image.section_data[shstr].to_mut();
    shstrtab.extend_from_slice(SECTION.as_bytes());
    shstrtab.push(0);
    image.shdrs.push(ir::Shdr {
        name: name_off,
        sh_type: sht::PROGBITS,
        flags: shf::ALLOC | shf::EXECINSTR,
        addr: 0,
        offset: 0,
        size: 0,
        link: 0,
        info: 0,
        addralign: 16,
        entsize: 0,
    });
    image.section_data.push(Cow::Owned(blob));
    Ok(())
}

/// The program entry the current stub chains to.
fn saved_entry(image: &ElfImage<'_>, idx: usize) -> Result<u64> {
    let data = &image.section_data[idx];
    let bytes: [u8; 8] = data
        .get(SAVED_ENTRY_OFFSET..SAVED_ENTRY_OFFSET + 8)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| Error::Parse(format!("{SECTION} shorter than its header")))?;
    Ok(match image.enc.endian {
        ir::Endian::Little => u64::from_le_bytes(bytes),
        ir::Endian::Big => u64::from_be_bytes(bytes),
    })
}
