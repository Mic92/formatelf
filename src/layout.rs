//! Assign on-disk offsets after ops change section sizes, then hand off to the
//! serializer. Mirrors patchelf's `rewriteSectionsLibrary` with the program
//! header table relocated to the end of the file: grown sections, a fresh copy
//! of the (extended) PHT, and the SHT are appended in a new PT_LOAD segment.
//!
//! This works for both ET_DYN and ET_EXEC: the relocated sections are reached
//! through DT_* tags and PT_* segments (which we fix up), not absolute code
//! references, so placing them at a fresh high virtual address is safe.
//! patchelf uses a separate shifting strategy for ET_EXEC only to keep the PHT
//! at the start for very old kernels; we relocate it like the library path.

use crate::codec;
use crate::constraints;
use crate::error::{Error, Result};
use crate::ir::{dt, et, pf, pt, sht, ElfImage, Phdr};
use crate::serialize;

fn round_up(v: u64, align: u64) -> u64 {
    if align <= 1 {
        v
    } else {
        v.div_ceil(align) * align
    }
}

fn dyn_idx(image: &ElfImage) -> Option<usize> {
    image.shdrs.iter().position(|s| s.sh_type == sht::DYNAMIC)
}

/// Final on-disk size of a section. The `.dynamic` section is driven by the
/// `dynamic` array (re-encoded later by `sync_dynamic`); shrinking it keeps the
/// original size with a zeroed tail, so only genuine growth counts here.
fn section_len(image: &ElfImage, i: usize) -> u64 {
    if Some(i) == dyn_idx(image) {
        let encoded = image.dynamic.len() as u64 * codec::dyn_size(image.enc.class) as u64;
        encoded.max(image.shdrs[i].size)
    } else {
        image.section_data[i].len() as u64
    }
}

/// Re-encode the dynamic array into its section data, zero-padding to the
/// header size so a shrunk array's trailing DT_NULL still terminates. Called
/// once, after address fixups, so it captures the final entry values.
fn sync_dynamic(image: &mut ElfImage) -> Result<()> {
    let Some(idx) = dyn_idx(image) else {
        return Ok(());
    };
    let mut bytes = Vec::new();
    for d in &image.dynamic {
        codec::write_dyn(image.enc, d, &mut bytes)?;
    }
    bytes.resize((image.shdrs[idx].size as usize).max(bytes.len()), 0);
    image.section_data[idx] = bytes;
    Ok(())
}

fn grown_sections(image: &ElfImage) -> Vec<usize> {
    (0..image.shdrs.len())
        .filter(|&i| {
            image.shdrs[i].sh_type != sht::NOBITS && section_len(image, i) != image.shdrs[i].size
        })
        .collect()
}

pub fn finalize(image: &mut ElfImage, original: &[u8], page_size: Option<u64>) -> Result<Vec<u8>> {
    let grown = grown_sections(image);
    if grown.is_empty() {
        sync_dynamic(image)?;
        return serialize::write(image, original.to_vec());
    }
    if image.ehdr.e_type != et::DYN && image.ehdr.e_type != et::EXEC {
        return Err(Error::Unsupported(
            "unsupported ELF type for relayout".into(),
        ));
    }
    relayout(image, original, grown, page_size)
}

fn page_size_for(image: &ElfImage, forced: Option<u64>) -> u64 {
    if let Some(p) = forced {
        return p;
    }
    // Per-arch minimum page sizes, mirroring patchelf/gold.
    const EM_PPC: u16 = 20;
    const EM_PPC64: u16 = 21;
    const EM_AARCH64: u16 = 183;
    const EM_MIPS: u16 = 8;
    match image.ehdr.machine {
        EM_PPC | EM_PPC64 | EM_AARCH64 | EM_MIPS => 0x10000,
        _ => 0x1000,
    }
}

fn relayout(
    image: &mut ElfImage,
    original: &[u8],
    grown: Vec<usize>,
    page_size: Option<u64>,
) -> Result<Vec<u8>> {
    let sec_align = codec::dyn_size(image.enc.class) as u64; // sizeof(Elf_Off)
    let phdr_size = codec::phdr_size(image.enc.class) as u64;
    let shdr_size = codec::shdr_size(image.enc.class) as u64;
    let ehdr_size = image.ehdr.ehsize as u64;

    let page = page_size_for(image, page_size);
    let mut align = page;
    let mut start_page = 0u64;
    let mut have_phdr_seg = false;
    for p in &image.phdrs {
        start_page = start_page.max(p.vaddr + p.memsz);
        align = align.max(p.align);
        if p.p_type == pt::PHDR {
            have_phdr_seg = true;
        }
    }
    if !have_phdr_seg {
        return Err(Error::Unsupported(
            "no PT_PHDR segment; cannot relocate program headers".into(),
        ));
    }
    start_page = round_up(start_page, align);

    // One extra PHT entry for the new PT_LOAD.
    let new_phnum = image.phdrs.len() as u64 + 1;
    let pht_size = round_up(new_phnum * phdr_size + ehdr_size, sec_align);
    let sht_size = round_up(image.shdrs.len() as u64 * shdr_size, sec_align);

    let mut needed = pht_size + sht_size;
    for &i in &grown {
        needed += round_up(section_len(image, i), sec_align);
    }

    let start_off = round_up(original.len() as u64, align);
    // +1: older binutils readelf rejects a PT_DYNAMIC as large as the file.
    let mut buf = original.to_vec();
    buf.resize((start_off + needed + 1) as usize, 0);

    // Lay out the appended region: PHT, SHT, then the grown sections.
    image.ehdr.phoff = start_off;
    image.ehdr.shoff = start_off + pht_size;
    let mut cur = start_off + pht_size + sht_size;
    for &i in &grown {
        let len = section_len(image, i);
        image.shdrs[i].offset = cur;
        image.shdrs[i].addr = start_page + (cur - start_off);
        image.shdrs[i].size = len;
        cur += round_up(len, sec_align);
    }

    image.phdrs.push(Phdr {
        p_type: pt::LOAD,
        flags: pf::R | pf::W,
        offset: start_off,
        vaddr: start_page,
        paddr: start_page,
        filesz: needed,
        memsz: needed,
        align,
    });

    sync_segments(image);
    fixup_dynamic_addrs(image);
    sync_dynamic(image)?;

    constraints::validate(image)?;
    serialize::write(image, buf)
}

/// Re-point PT_PHDR/PT_DYNAMIC/PT_INTERP at their (possibly moved) targets.
fn sync_segments(image: &mut ElfImage) {
    let phoff = image.ehdr.phoff;
    let pht_bytes = image.phdrs.len() as u64 * codec::phdr_size(image.enc.class) as u64;
    // PT_PHDR's vaddr maps the relocated table; it sits at the region start,
    // whose vaddr is the new PT_LOAD vaddr (the last pushed segment).
    let phdr_vaddr = image.phdrs.last().unwrap().vaddr;
    let extent = |i: usize| {
        let s = &image.shdrs[i];
        (s.offset, s.addr, s.size)
    };
    let interp = image.find_section(".interp").map(extent);
    let dynamic = image
        .shdrs
        .iter()
        .position(|s| s.sh_type == sht::DYNAMIC)
        .map(extent);
    for p in &mut image.phdrs {
        match p.p_type {
            pt::PHDR => set_segment(p, phoff, phdr_vaddr, pht_bytes),
            pt::INTERP => {
                if let Some((off, addr, size)) = interp {
                    set_segment(p, off, addr, size);
                }
            }
            pt::DYNAMIC => {
                if let Some((off, addr, size)) = dynamic {
                    set_segment(p, off, addr, size);
                }
            }
            _ => {}
        }
    }
}

fn set_segment(p: &mut Phdr, off: u64, addr: u64, size: u64) {
    p.offset = off;
    p.vaddr = addr;
    p.paddr = addr;
    p.filesz = size;
    p.memsz = size;
}

/// Update DT_* entries whose value is the address/size of a moved section.
/// Sections that did not move resolve to their unchanged address, so these
/// assignments are no-ops for them.
fn fixup_dynamic_addrs(image: &mut ElfImage) {
    let addr = |name: &str| image.find_section(name).map(|i| image.shdrs[i].addr);
    let size = |name: &str| image.find_section(name).map(|i| image.shdrs[i].size);
    let dynstr_addr = addr(".dynstr");
    let dynstr_size = size(".dynstr");
    let map: &[(i64, Option<u64>)] = &[
        (dt::STRTAB, dynstr_addr),
        (dt::STRSZ, dynstr_size),
        (dt::SYMTAB, addr(".dynsym")),
        (dt::HASH, addr(".hash")),
        (dt::GNU_HASH, addr(".gnu.hash")),
        (dt::VERNEED, addr(".gnu.version_r")),
        (dt::VERSYM, addr(".gnu.version")),
        (dt::JMPREL, addr(".rela.plt").or_else(|| addr(".rel.plt"))),
        (dt::RELA, addr(".rela.dyn")),
        (dt::REL, addr(".rel.dyn")),
    ];
    for d in &mut image.dynamic {
        if let Some((_, Some(v))) = map.iter().find(|(t, _)| *t == d.tag) {
            d.val = *v;
        }
    }
}
