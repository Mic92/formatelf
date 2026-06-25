//! Decode raw ELF bytes into [`ir::ElfImage`]. One of two places aware of
//! class/endianness; uses `object` raw structs so the Elf32/Elf64 field-reorder
//! and width differences stay in one audited spot.

use crate::codec;
use crate::error::{Error, Result};
use crate::ir::{DynEntry, ElfImage, Encoding, Shdr};

const SHT_NOBITS: u32 = 8;
const SHT_DYNAMIC: u32 = 6;

fn slice<'a>(data: &'a [u8], off: u64, len: u64, what: &str) -> Result<&'a [u8]> {
    let off = off as usize;
    let end = off
        .checked_add(len as usize)
        .ok_or_else(|| Error::Parse(format!("{what}: offset overflow")))?;
    data.get(off..end)
        .ok_or_else(|| Error::Parse(format!("{what}: out of bounds")))
}

pub fn parse(data: &[u8]) -> Result<ElfImage> {
    let (ehdr, enc, phnum, shnum) = codec::read_ehdr(data)?;

    let phsize = codec::phdr_size(enc.class) as u64;
    let mut phdrs = Vec::with_capacity(phnum as usize);
    for i in 0..phnum {
        let off = ehdr.phoff + i as u64 * phsize;
        phdrs.push(codec::read_phdr(enc, slice(data, off, phsize, "phdr")?)?);
    }

    let shsize = codec::shdr_size(enc.class) as u64;
    let mut shdrs = Vec::with_capacity(shnum as usize);
    for i in 0..shnum {
        let off = ehdr.shoff + i as u64 * shsize;
        shdrs.push(codec::read_shdr(enc, slice(data, off, shsize, "shdr")?)?);
    }

    let mut section_data = Vec::with_capacity(shdrs.len());
    for s in &shdrs {
        if s.sh_type == SHT_NOBITS {
            section_data.push(Vec::new());
        } else {
            section_data.push(slice(data, s.offset, s.size, "section")?.to_vec());
        }
    }

    let dynamic = read_dynamic(data, enc, &shdrs)?;

    Ok(ElfImage {
        enc,
        ehdr,
        phdrs,
        shdrs,
        section_data,
        dynamic,
    })
}

fn read_dynamic(data: &[u8], enc: Encoding, shdrs: &[Shdr]) -> Result<Vec<DynEntry>> {
    let Some(dyn_sh) = shdrs.iter().find(|s| s.sh_type == SHT_DYNAMIC) else {
        return Ok(Vec::new());
    };
    let dsize = codec::dyn_size(enc.class) as u64;
    if dyn_sh.size % dsize != 0 {
        return Err(Error::Parse("malformed .dynamic".into()));
    }
    let bytes = slice(data, dyn_sh.offset, dyn_sh.size, "dynamic")?;
    let mut out = Vec::new();
    for chunk in bytes.chunks_exact(dsize as usize) {
        let entry = codec::read_dyn(enc, chunk)?;
        let done = entry.tag == 0; // DT_NULL terminates the array
        out.push(entry);
        if done {
            break;
        }
    }
    Ok(out)
}
