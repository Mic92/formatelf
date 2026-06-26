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
use crate::error::{Error, Result};
use crate::ir::{dt, et, pf, pt, shf, sht, Class, ElfImage, Phdr};
use crate::serialize;
use crate::verify;

/// A section relocated by relayout: its old address range and new address, used
/// to rebase the dynamic tags that pointed into it.
struct MovedSection {
    idx: usize,
    old_addr: u64,
    old_size: u64,
    new_addr: u64,
}

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

pub fn finalize(
    image: &mut ElfImage,
    original: &[u8],
    page_size: Option<u64>,
    debug: bool,
    no_clobber: bool,
) -> Result<Vec<u8>> {
    let grown = grown_sections(image);
    // A pushed program header (e.g. a new PT_GNU_STACK) does not grow any
    // section but still needs the PHT relocated to make room.
    let orig_phnum = codec::read_ehdr(original)?.2 as usize;
    let phdrs_grew = image.phdrs.len() > orig_phnum;
    if grown.is_empty() && !phdrs_grew {
        if debug {
            eprintln!("patchelf: no section grew; rewriting in place");
        }
        sync_dynamic(image)?;
        verify::run(image)?;
        let total = original.len() as u64;
        return serialize::write(image, original, total);
    }
    if debug {
        eprintln!("patchelf: {} section(s) grew; relaying out", grown.len());
    }
    let reclaim = !no_clobber;
    if image.ehdr.e_type != et::DYN && image.ehdr.e_type != et::EXEC {
        return Err(Error::Unsupported(
            "unsupported ELF type for relayout".into(),
        ));
    }
    relayout(image, original, grown, page_size, reclaim)
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
    reclaim: bool,
) -> Result<Vec<u8>> {
    let sec_align = codec::dyn_size(image.enc.class) as u64; // sizeof(Elf_Off)
    let phdr_size = codec::phdr_size(image.enc.class) as u64;
    let shdr_size = codec::shdr_size(image.enc.class) as u64;
    let ehdr_size = image.ehdr.ehsize as u64;
    let orig_len = original.len() as u64;

    // Reclaim a region appended by a previous relayout rather than stacking a
    // fresh one: that segment begins exactly at the relocated PHT
    // (offset == phoff). Everything living in it is laid out anew together with
    // the newly grown sections, so repeated patching reuses the same space.
    let prior = reclaim
        .then(|| {
            image
                .phdrs
                .iter()
                .position(|p| p.p_type == pt::LOAD && p.offset == image.ehdr.phoff)
        })
        .flatten();
    let region_start = prior.map(|i| image.phdrs[i].offset);
    if let Some(i) = prior {
        image.phdrs.remove(i);
    }
    // Iterating in index order yields a sorted, duplicate-free list directly.
    let relocate: Vec<usize> = (0..image.shdrs.len())
        .filter(|&i| {
            let s = &image.shdrs[i];
            s.sh_type != sht::NOBITS
                && !image.section_data[i].is_empty()
                && (grown.contains(&i) || region_start.is_some_and(|r| s.offset >= r))
        })
        .collect();

    let page = page_size_for(image, page_size);
    let mut align = page;
    let mut start_page = 0u64;
    let mut base_vaddr = u64::MAX;
    let mut have_phdr_seg = false;
    for p in &image.phdrs {
        start_page = start_page.max(p.vaddr + p.memsz);
        align = align.max(p.align);
        if p.p_type == pt::LOAD {
            base_vaddr = base_vaddr.min(p.vaddr);
        }
        if p.p_type == pt::PHDR {
            have_phdr_seg = true;
        }
    }
    let base_vaddr = if base_vaddr == u64::MAX {
        0
    } else {
        base_vaddr
    };
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
    for &i in &relocate {
        needed += round_up(section_len(image, i), sec_align);
    }

    // The kernel passes AT_PHDR = base_vaddr + e_phoff, and the loader derives
    // its load bias from PT_PHDR.vaddr, so every byte in the appended region
    // must keep vaddr == base_vaddr + file_offset. Pick a file offset that also
    // places the region past all existing segments to avoid overlap.
    let min_off = start_page.saturating_sub(base_vaddr);
    let start_off = round_up(region_start.unwrap_or(orig_len).max(min_off), align);
    let region_vaddr = base_vaddr + start_off;
    // +1: older binutils readelf rejects a PT_DYNAMIC as large as the file.
    let total = start_off + needed + 1;

    // Moving a section to a new address corrupts anything that referenced its
    // old address by value. We fix DT_* tags below, so refuse to relocate a
    // section that relocations or symbols point into. Capture each moved
    // section's old (addr, size) now, before the layout loop overwrites them.
    let moves: Vec<MovedSection> = relocate
        .iter()
        .map(|&i| MovedSection {
            idx: i,
            old_addr: image.shdrs[i].addr,
            old_size: image.shdrs[i].size,
            new_addr: 0,
        })
        .filter(|m| m.old_addr != 0)
        .collect();
    let ranges: Vec<(u64, u64)> = moves.iter().map(|m| (m.old_addr, m.old_size)).collect();
    assert_no_address_refs(image, &ranges)?;

    // The ld-cache note moves with the appended region, so remember which
    // PT_NOTE covers it now (the file is still consistent) to re-anchor it
    // after the move. Identifying it up front avoids confusing it with
    // immovable notes like .note.ABI-tag.
    let ldcache_phdr = image.find_section(".note.nixos.ldcache").and_then(|si| {
        let off = image.shdrs[si].offset;
        image
            .phdrs
            .iter()
            .position(|p| p.p_type == pt::NOTE && p.offset == off)
    });

    // Lay out the appended region: PHT, SHT, then the relocated sections.
    image.ehdr.phoff = start_off;
    image.ehdr.shoff = start_off + pht_size;
    let mut cur = start_off + pht_size + sht_size;
    for &i in &relocate {
        let len = section_len(image, i);
        image.shdrs[i].offset = cur;
        image.shdrs[i].addr = region_vaddr + (cur - start_off);
        image.shdrs[i].size = len;
        cur += round_up(len, sec_align);
    }
    let moves: Vec<MovedSection> = moves
        .into_iter()
        .map(|m| MovedSection {
            new_addr: image.shdrs[m.idx].addr,
            ..m
        })
        .collect();

    image.phdrs.push(Phdr {
        p_type: pt::LOAD,
        flags: pf::R | pf::W,
        offset: start_off,
        vaddr: region_vaddr,
        paddr: region_vaddr,
        filesz: needed,
        memsz: needed,
        align,
    });

    sync_segments(image, ldcache_phdr);
    fixup_dynamic_addrs(image, &moves);
    sync_dynamic(image)?;

    verify::run(image)?;
    serialize::write(image, original, total)
}

/// True if any fixed-stride record in `sec` has its `width`-byte field at
/// offset `fo` satisfying `pred`.
fn any_field(
    image: &ElfImage,
    sec: usize,
    stride: usize,
    fo: usize,
    width: usize,
    pred: impl Fn(u64) -> bool,
) -> bool {
    let e = image.enc.endian;
    image.section_data[sec].chunks_exact(stride).any(|r| {
        let v = match width {
            8 => e.read_u64(r, fo),
            _ => e.read_u32(r, fo) as u64,
        };
        pred(v)
    })
}

/// Error if any relocation target (r_offset) or symbol value (st_value) lands
/// inside one of the address ranges about to be relocated.
fn assert_no_address_refs(image: &ElfImage, ranges: &[(u64, u64)]) -> Result<()> {
    if ranges.is_empty() {
        return Ok(());
    }
    let hits = |v: u64| ranges.iter().any(|&(a, sz)| v >= a && v < a + sz);
    let elf64 = image.enc.class == Class::Elf64;
    let ptr = if elf64 { 8 } else { 4 };
    // r_offset is the first field of Elf_Rel/Elf_Rela. st_value sits at offset
    // 8 (Elf64_Sym) or 4 (Elf32_Sym).
    let (rel_stride, rela_stride) = if elf64 { (16, 24) } else { (8, 12) };
    let (sym_stride, sym_off) = if elf64 { (24, 8) } else { (16, 4) };

    for (i, s) in image.shdrs.iter().enumerate() {
        // Only loaded tables matter at runtime; .symtab and friends are not
        // mapped and legitimately carry stale STT_SECTION values after a move.
        if s.flags & shf::ALLOC == 0 {
            continue;
        }
        let referenced = match s.sh_type {
            sht::RELA => any_field(image, i, rela_stride, 0, ptr, hits),
            sht::REL => any_field(image, i, rel_stride, 0, ptr, hits),
            sht::DYNSYM | sht::SYMTAB => any_field(image, i, sym_stride, sym_off, ptr, hits),
            _ => continue,
        };
        if referenced {
            return Err(Error::Unsupported(format!(
                "cannot relocate section referenced by {} entries at its address",
                image.section_name(i).unwrap_or("?")
            )));
        }
    }
    Ok(())
}

/// Re-point PT_PHDR/PT_DYNAMIC/PT_INTERP and the ld-cache PT_NOTE at their
/// (possibly moved) targets.
fn sync_segments(image: &mut ElfImage, ldcache_phdr: Option<usize>) {
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
    let ldcache = image.find_section(".note.nixos.ldcache").map(extent);
    if let (Some(pi), Some((off, addr, size))) = (ldcache_phdr, ldcache) {
        set_segment(&mut image.phdrs[pi], off, addr, size);
    }
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

/// Rebase the address-valued dynamic tags that point into a relocated section,
/// then refresh DT_STRSZ. The tag set mirrors glibc's ADJUST_DYN_INFO plus the
/// init/fini and array pointers: anything the loader treats as an address. By
/// keying on the old address range rather than a section name, every moved
/// section is handled, not just the few an operation is known to touch.
fn fixup_dynamic_addrs(image: &mut ElfImage, moves: &[MovedSection]) {
    const ADDR_TAGS: &[i64] = &[
        dt::HASH,
        dt::PLTGOT,
        dt::STRTAB,
        dt::SYMTAB,
        dt::RELA,
        dt::REL,
        dt::JMPREL,
        dt::VERSYM,
        dt::VERNEED,
        dt::VERDEF,
        dt::GNU_HASH,
        dt::RELR,
        dt::INIT,
        dt::FINI,
        dt::INIT_ARRAY,
        dt::FINI_ARRAY,
        dt::PREINIT_ARRAY,
    ];
    for d in &mut image.dynamic {
        if !ADDR_TAGS.contains(&d.tag) {
            continue;
        }
        if let Some(m) = moves
            .iter()
            .find(|m| d.val >= m.old_addr && d.val < m.old_addr + m.old_size)
        {
            d.val = m.new_addr + (d.val - m.old_addr);
        }
    }
    // DT_STRSZ tracks the .dynstr size, the one string table that can grow.
    if let Some(size) = image.find_section(".dynstr").map(|i| image.shdrs[i].size) {
        for d in &mut image.dynamic {
            if d.tag == dt::STRSZ {
                d.val = size;
            }
        }
    }
}
