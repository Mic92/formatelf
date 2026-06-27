//! Decode raw ELF bytes into [`ir::ElfImage`]. One of two places aware of
//! class/endianness; uses `object` raw structs so the Elf32/Elf64 field-reorder
//! and width differences stay in one audited spot.

use crate::codec;
use crate::error::{Error, Result};
use crate::ir::{dt, pt, sht, DynEntry, Ehdr, ElfImage, Encoding, Phdr, Shdr};

fn slice<'a>(data: &'a [u8], off: u64, len: u64, what: &str) -> Result<&'a [u8]> {
    let off = off as usize;
    let end = off
        .checked_add(len as usize)
        .ok_or_else(|| Error::Parse(format!("{what}: offset overflow")))?;
    data.get(off..end)
        .ok_or_else(|| Error::Parse(format!("{what}: out of bounds")))
}

/// Validate that a `count`-entry table of `size`-byte records fits at `off`,
/// returning its bytes. Bounding the span before allocating caps `count` to
/// the file size, so an attacker's huge header count cannot drive a giant
/// Vec::with_capacity.
fn table<'a>(data: &'a [u8], off: u64, count: u64, size: u64, what: &str) -> Result<&'a [u8]> {
    let len = count
        .checked_mul(size)
        .ok_or_else(|| Error::Parse(format!("{what}: count overflow")))?;
    slice(data, off, len, what)
}

pub fn parse(data: &[u8]) -> Result<ElfImage<'_>> {
    let (mut ehdr, enc, raw_phnum, raw_shnum) = codec::read_ehdr(data)?;
    let (phnum, shnum) = resolve_counts(data, enc, &mut ehdr, raw_phnum, raw_shnum)?;

    let phsize = codec::phdr_size(enc.class) as u64;
    let phbytes = table(data, ehdr.phoff, phnum, phsize, "program headers")?;
    let mut phdrs = Vec::with_capacity(phnum as usize);
    for chunk in phbytes.chunks_exact(phsize as usize) {
        phdrs.push(codec::read_phdr(enc, chunk)?);
    }

    let shsize = codec::shdr_size(enc.class) as u64;
    let shbytes = table(data, ehdr.shoff, shnum, shsize, "section headers")?;
    let mut shdrs = Vec::with_capacity(shnum as usize);
    for chunk in shbytes.chunks_exact(shsize as usize) {
        shdrs.push(codec::read_shdr(enc, chunk)?);
    }

    let mut section_data = Vec::with_capacity(shdrs.len());
    for s in &shdrs {
        if s.sh_type == sht::NOBITS {
            section_data.push(std::borrow::Cow::Borrowed(&[][..]));
        } else {
            section_data.push(std::borrow::Cow::Borrowed(slice(
                data, s.offset, s.size, "section",
            )?));
        }
    }

    let dynamic = read_dynamic(data, enc, &shdrs, &phdrs)?;
    let dynstr_fallback = recover_dynstr(data, &phdrs, &shdrs, &dynamic);
    let interp_fallback = recover_interp(data, &phdrs);

    Ok(ElfImage {
        enc,
        ehdr,
        phdrs,
        shdrs,
        section_data,
        dynamic,
        dynstr_fallback,
        interp_fallback,
    })
}

/// Decode a dynamic array of `filesz` bytes starting at `off`, stopping at the
/// terminating DT_NULL.
fn read_dyn_array(data: &[u8], enc: Encoding, off: u64, filesz: u64) -> Result<Vec<DynEntry>> {
    let dsize = codec::dyn_size(enc.class) as u64;
    let bytes = slice(data, off, filesz - filesz % dsize, "dynamic")?;
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

/// When there is no `.dynstr` section (stripped section headers) but the
/// dynamic array is present, recover the string-table bytes by mapping
/// DT_STRTAB's virtual address through the PT_LOAD segments.
fn recover_dynstr(
    data: &[u8],
    phdrs: &[Phdr],
    shdrs: &[Shdr],
    dynamic: &[DynEntry],
) -> Option<Vec<u8>> {
    let has_section = shdrs.iter().any(|s| s.sh_type == sht::DYNAMIC);
    if has_section || dynamic.is_empty() {
        return None;
    }
    let strtab = dynamic.iter().find(|d| d.tag == dt::STRTAB)?.val;
    let strsz = dynamic.iter().find(|d| d.tag == dt::STRSZ)?.val;
    let off = vaddr_to_off(phdrs, strtab)?;
    slice(data, off, strsz, "dynstr").ok().map(<[u8]>::to_vec)
}

/// Recover the interpreter path from PT_INTERP. interpreter() prefers the
/// `.interp` section when present and falls back to this for stripped binaries.
fn recover_interp(data: &[u8], phdrs: &[Phdr]) -> Option<Vec<u8>> {
    let p = phdrs.iter().find(|p| p.p_type == pt::INTERP)?;
    slice(data, p.offset, p.filesz, "interp")
        .ok()
        .map(<[u8]>::to_vec)
}

fn vaddr_to_off(phdrs: &[Phdr], vaddr: u64) -> Option<u64> {
    phdrs
        .iter()
        .filter(|p| p.p_type == pt::LOAD)
        .find(|p| vaddr >= p.vaddr && vaddr < p.vaddr.saturating_add(p.filesz))
        .map(|p| p.offset.saturating_add(vaddr - p.vaddr))
}

/// Resolve the PN_XNUM / SHN_XINDEX escapes. When the 16-bit header fields
/// can't hold the real counts, section 0's header carries them: sh_info for
/// program headers, sh_size for sections, sh_link for the name string table.
fn resolve_counts(
    data: &[u8],
    enc: Encoding,
    ehdr: &mut Ehdr,
    raw_phnum: u16,
    raw_shnum: u16,
) -> Result<(u64, u64)> {
    let needs_escape =
        raw_phnum == codec::PN_XNUM || raw_shnum == 0 || ehdr.shstrndx == codec::SHN_XINDEX as u32;
    let sec0 = if ehdr.shoff != 0 && needs_escape {
        let shsize = codec::shdr_size(enc.class) as u64;
        Some(codec::read_shdr(
            enc,
            slice(data, ehdr.shoff, shsize, "shdr0")?,
        )?)
    } else {
        None
    };

    let phnum = if raw_phnum == codec::PN_XNUM {
        sec0.as_ref().map_or(raw_phnum as u64, |s| s.info as u64)
    } else {
        raw_phnum as u64
    };
    // e_shnum == 0 with a section table present means the count is in sh_size.
    let shnum = match (raw_shnum, &sec0) {
        (0, Some(s)) => s.size,
        _ => raw_shnum as u64,
    };
    if ehdr.shstrndx == codec::SHN_XINDEX as u32 {
        if let Some(s) = &sec0 {
            ehdr.shstrndx = s.link;
        }
    }
    Ok((phnum, shnum))
}

fn read_dynamic(
    data: &[u8],
    enc: Encoding,
    shdrs: &[Shdr],
    phdrs: &[Phdr],
) -> Result<Vec<DynEntry>> {
    let dsize = codec::dyn_size(enc.class) as u64;
    if let Some(dyn_sh) = shdrs.iter().find(|s| s.sh_type == sht::DYNAMIC) {
        if dyn_sh.size % dsize != 0 {
            return Err(Error::Parse("malformed .dynamic".into()));
        }
        return read_dyn_array(data, enc, dyn_sh.offset, dyn_sh.size);
    }
    // Stripped section headers: fall back to the PT_DYNAMIC segment.
    if let Some(p) = phdrs.iter().find(|p| p.p_type == pt::DYNAMIC) {
        return read_dyn_array(data, enc, p.offset, p.filesz);
    }
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: a PT_LOAD with vaddr near u64::MAX overflowed the
    /// `vaddr + filesz` range check while recovering .dynstr from segments.
    #[test]
    fn vaddr_to_off_handles_address_overflow() {
        let huge = u64::MAX - 0xff;
        let load = Phdr {
            p_type: pt::LOAD,
            flags: 0,
            offset: 0x1000,
            vaddr: huge,
            paddr: 0,
            filesz: 0x1000,
            memsz: 0x1000,
            align: 1,
        };
        // The requested vaddr falls inside the segment, forcing the upper-bound
        // check that used to overflow. It must return, not panic.
        assert_eq!(vaddr_to_off(&[load], huge), Some(0x1000));
    }
}
