//! Assign on-disk offsets after ops change section sizes, then hand off to the
//! serializer. Mirrors patchelf's `rewriteSectionsLibrary` with the program
//! header table relocated to the end of the file: grown sections, a fresh copy
//! of the (extended) PHT, and the SHT are appended in a new `PT_LOAD` segment.
//!
//! This works for both `ET_DYN` and `ET_EXEC`: the relocated sections are reached
//! through DT_* tags and PT_* segments (which we fix up), not absolute code
//! references, so placing them at a fresh high virtual address is safe.
//! patchelf uses a separate shifting strategy for `ET_EXEC` only to keep the PHT
//! at the start for very old kernels; we relocate it like the library path.
//!
//! Binaries carrying a `.bun` section get a prepend layout instead. bun reuses
//! the patched binary as the template for `bun build --compile` output and
//! assumes the writable segment holding `.bun` is both the highest mapping and
//! the last loaded content in the file (everything after it is moved as an
//! opaque tail). Appending a `PT_LOAD` breaks both assumptions. The new region
//! therefore goes in front: the whole original file is shifted up and the
//! fresh PHT plus relocated sections land in a new low-vaddr segment before
//! the original image.

use crate::codec;
use crate::error::{Error, Result};
use crate::ir::{Class, ElfImage, Phdr, dt, et, pf, pt, shf, sht};
use crate::serialize;
use crate::verify;

/// A section relocated by relayout: its old address range and new address, used
/// to rebase the dynamic tags that pointed into it.
struct MovedSection {
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

fn dyn_idx(image: &ElfImage<'_>) -> Option<usize> {
    image.shdrs.iter().position(|s| s.sh_type == sht::DYNAMIC)
}

/// Final on-disk size of a section. The `.dynamic` section is driven by the
/// `dynamic` array (re-encoded later by `sync_dynamic`); shrinking it keeps the
/// original size with a zeroed tail, so only genuine growth counts here.
fn section_len(image: &ElfImage<'_>, i: usize) -> u64 {
    if Some(i) == dyn_idx(image) {
        let encoded = image.dynamic.len() as u64 * codec::dyn_size(image.enc.class) as u64;
        encoded.max(image.shdrs[i].size)
    } else {
        image.section_data[i].len() as u64
    }
}

/// Re-encode the dynamic array into its section data, zero-padding to the
/// header size so a shrunk array's trailing `DT_NULL` still terminates. Called
/// once, after address fixups, so it captures the final entry values.
fn sync_dynamic(image: &mut ElfImage<'_>) -> Result<()> {
    let Some(idx) = dyn_idx(image) else {
        return Ok(());
    };
    let mut bytes = Vec::new();
    for d in &image.dynamic {
        codec::write_dyn(image.enc, d, &mut bytes)?;
    }
    let want = crate::ir::try_usize(image.shdrs[idx].size)?;
    bytes.resize(want.max(bytes.len()), 0);
    image.section_data[idx] = std::borrow::Cow::Owned(bytes);
    Ok(())
}

fn grown_sections(image: &ElfImage<'_>) -> Vec<usize> {
    (0..image.shdrs.len())
        .filter(|&i| {
            image.shdrs[i].sh_type != sht::NOBITS && section_len(image, i) != image.shdrs[i].size
        })
        .collect()
}

/// # Errors
/// Returns an error for an ELF type that cannot be relaid out, or propagates a
/// verification or serialization failure.
pub fn finalize(
    image: &mut ElfImage<'_>,
    original: &[u8],
    page_size: Option<u64>,
    debug: bool,
    no_clobber: bool,
    out: &mut dyn std::io::Write,
) -> Result<()> {
    let grown = grown_sections(image);
    // A pushed program header (e.g. a new PT_GNU_STACK) does not grow any
    // section but still needs the PHT relocated to make room.
    let orig_phnum = codec::read_ehdr(original)?.2 as usize;
    let phdrs_grew = image.phdrs.len() > orig_phnum;
    if grown.is_empty() && !phdrs_grew {
        if debug {
            eprintln!("formatelf: no section grew; rewriting in place");
        }
        // Removing entries shifts later slots, so the slot-relative
        // DT_MIPS_RLD_MAP_REL offset can change without any section moving.
        fixup_mips_rld_map_rel(image);
        sync_dynamic(image)?;
        let total = original.len() as u64;
        verify::run(image, total)?;
        return serialize::write_to(image, original, total, out);
    }
    if debug {
        eprintln!("formatelf: {} section(s) grew; relaying out", grown.len());
    }
    let reclaim = !no_clobber;
    if image.ehdr.e_type != et::DYN && image.ehdr.e_type != et::EXEC {
        return Err(Error::Unsupported(
            "unsupported ELF type for relayout".into(),
        ));
    }
    if image.find_section(".bun").is_some() {
        // Work on a copy so an inapplicable prepend attempt (no room below the
        // image base) leaves the append fallback with a pristine image.
        let mut prepended = image.clone();
        if relayout_shift(&mut prepended, original, &grown, page_size, debug, out)? {
            return Ok(());
        }
    }
    relayout(image, original, &grown, page_size, reclaim, out)
}

fn page_size_for(image: &ElfImage<'_>, forced: Option<u64>) -> u64 {
    // Per-arch minimum page sizes, mirroring patchelf/gold.
    const EM_PPC: u16 = 20;
    const EM_PPC64: u16 = 21;
    const EM_AARCH64: u16 = 183;
    const EM_MIPS: u16 = 8;
    if let Some(p) = forced {
        return p;
    }
    match image.ehdr.machine {
        EM_PPC | EM_PPC64 | EM_AARCH64 | EM_MIPS => 0x10000,
        _ => 0x1000,
    }
}

/// Pick the sections to lay out fresh. When reclaiming, drop a region a prior
/// relayout appended (it starts at the relocated PHT, offset == phoff) and
/// return its start so its contents are placed anew alongside the grown
/// sections, reusing the space instead of stacking a new region.
fn relocation_set(
    image: &mut ElfImage<'_>,
    grown: &[usize],
    reclaim: bool,
) -> (Option<u64>, Vec<usize>) {
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
    let relocate = (0..image.shdrs.len())
        .filter(|&i| {
            let s = &image.shdrs[i];
            s.sh_type != sht::NOBITS
                && !image.section_data[i].is_empty()
                && (grown.contains(&i) || region_start.is_some_and(|r| s.offset >= r))
        })
        .collect();
    (region_start, relocate)
}

/// Prepend layout for bun templates (see module doc). Shifts every original
/// byte up by a segment-aligned amount and places the new PHT and the
/// relocated sections in a `PT_LOAD` mapped just below the original image.
/// Returns `Ok(false)` without touching `out` when the image has no room below
/// its base address, so the caller can fall back to the append layout.
fn relayout_shift(
    image: &mut ElfImage<'_>,
    original: &[u8],
    grown: &[usize],
    page_size: Option<u64>,
    debug: bool,
    out: &mut dyn std::io::Write,
) -> Result<bool> {
    let sec_align = codec::dyn_size(image.enc.class) as u64;
    let phdr_size = codec::phdr_size(image.enc.class) as u64;
    let shdr_size = codec::shdr_size(image.enc.class) as u64;
    let ehdr_size = u64::from(image.ehdr.ehsize);

    // Also re-place anything a prior append-relayout parked past the image, so
    // repatching does not leave a stale high `PT_LOAD` behind.
    let (_, relocate) = relocation_set(image, grown, true);

    let mut align = page_size_for(image, page_size);
    let mut base_vaddr = u64::MAX;
    for p in image.phdrs.iter().filter(|p| p.p_type == pt::LOAD) {
        align = align.max(p.align);
        base_vaddr = base_vaddr.min(p.vaddr);
    }
    if base_vaddr == u64::MAX || base_vaddr % align != 0 {
        return Ok(false);
    }

    // The region holds ehdr + PHT (one extra entry for the new PT_LOAD) +
    // relocated sections. Every original byte moves up by the region size.
    let pht_size = (image.phdrs.len() as u64 + 1) * phdr_size;
    let (region_offsets, needed) = region_layout(image, &relocate, ehdr_size + pht_size);
    let shift = round_up(needed, align);
    if base_vaddr < shift {
        // No address space below the image (e.g. a PIE loaded at 0).
        return Ok(false);
    }
    let region_vaddr = base_vaddr - shift;
    if debug {
        eprintln!("formatelf: .bun template; prepending {shift:#x} bytes");
    }

    let ldcache_phdr = ldcache_note_phdr(image);

    // Every original byte lands `shift` later. Sections re-placed in the low
    // region get their offsets there instead. Virtual addresses of everything
    // outside the region are unchanged, so only file offsets move.
    for p in &mut image.phdrs {
        p.offset += shift;
    }
    for s in &mut image.shdrs {
        s.offset += shift;
    }
    image.ehdr.phoff = ehdr_size;
    let moves = place_relocated(image, &relocate, &region_offsets, 0, region_vaddr)?;

    // bun requires the section header table past the loaded image, so keep it
    // at the very end of the grown file.
    image.ehdr.shoff = round_up(original.len() as u64 + shift, sec_align);
    let total = image.ehdr.shoff + image.shdrs.len() as u64 * shdr_size;

    // Keep PT_LOAD entries sorted by vaddr: the new region precedes the image.
    let first_load = image
        .phdrs
        .iter()
        .position(|p| p.p_type == pt::LOAD)
        .unwrap_or(image.phdrs.len());
    image.phdrs.insert(
        first_load,
        Phdr {
            p_type: pt::LOAD,
            flags: pf::R | pf::W,
            offset: 0,
            vaddr: region_vaddr,
            paddr: region_vaddr,
            filesz: shift,
            memsz: shift,
            align,
        },
    );

    sync_segments(image, ldcache_phdr, region_vaddr + image.ehdr.phoff);
    fixup_dynamic_addrs(image, &moves);
    sync_dynamic(image)?;

    verify::run(image, total)?;
    serialize::write_to_shifted(image, original, shift, total, out)?;
    Ok(true)
}

/// Assign region-relative offsets to the `relocate`d sections starting at
/// `start`, honoring each section's `sh_addralign` (at least the dynamic-entry
/// size). Returns the offsets and the region end. The region base is
/// page-aligned, so an aligned offset yields an aligned address.
fn region_layout(image: &ElfImage<'_>, relocate: &[usize], start: u64) -> (Vec<u64>, u64) {
    let sec_align = codec::dyn_size(image.enc.class) as u64;
    let mut cur = start;
    let mut offsets = Vec::with_capacity(relocate.len());
    for &i in relocate {
        cur = round_up(cur, sec_align.max(image.shdrs[i].addralign.max(1)));
        offsets.push(cur);
        cur += section_len(image, i);
    }
    (offsets, cur)
}

/// Move the `relocate`d sections to their assigned spots in the new region
/// (file offset `region_off + rel`, address `region_vaddr + rel`). Moving a
/// section corrupts anything that referenced its old address by value. DT_*
/// tags are fixed via the returned records, so refuse when relocations or
/// symbols point into a moved section.
fn place_relocated(
    image: &mut ElfImage<'_>,
    relocate: &[usize],
    region_offsets: &[u64],
    region_off: u64,
    region_vaddr: u64,
) -> Result<Vec<MovedSection>> {
    let ranges: Vec<(u64, u64)> = relocate
        .iter()
        .map(|&i| (image.shdrs[i].addr, image.shdrs[i].size))
        .filter(|&(addr, _)| addr != 0)
        .collect();
    assert_no_address_refs(image, &ranges)?;

    let mut moves = Vec::with_capacity(relocate.len());
    for (&i, &rel) in relocate.iter().zip(region_offsets) {
        let len = section_len(image, i);
        let s = &mut image.shdrs[i];
        if s.addr != 0 {
            moves.push(MovedSection {
                old_addr: s.addr,
                old_size: s.size,
                new_addr: region_vaddr + rel,
            });
        }
        s.offset = region_off + rel;
        s.addr = region_vaddr + rel;
        s.size = len;
    }
    Ok(moves)
}

/// The `PT_NOTE` covering the ld-cache note, identified while the file is
/// still consistent so it is not confused with immovable notes like
/// `.note.ABI-tag` after the move.
fn ldcache_note_phdr(image: &ElfImage<'_>) -> Option<usize> {
    let si = image.find_section(".note.nixos.ldcache")?;
    let off = image.shdrs[si].offset;
    image
        .phdrs
        .iter()
        .position(|p| p.p_type == pt::NOTE && p.offset == off)
}

fn relayout(
    image: &mut ElfImage<'_>,
    original: &[u8],
    grown: &[usize],
    page_size: Option<u64>,
    reclaim: bool,
    out: &mut dyn std::io::Write,
) -> Result<()> {
    let sec_align = codec::dyn_size(image.enc.class) as u64; // sizeof(Elf_Off)
    let phdr_size = codec::phdr_size(image.enc.class) as u64;
    let shdr_size = codec::shdr_size(image.enc.class) as u64;
    let ehdr_size = u64::from(image.ehdr.ehsize);
    let orig_len = original.len() as u64;

    let (region_start, relocate) = relocation_set(image, grown, reclaim);

    let page = page_size_for(image, page_size);
    let mut align = page;
    let mut start_page = 0u64;
    let mut base_vaddr = u64::MAX;
    for p in &image.phdrs {
        start_page = start_page.max(p.vaddr + p.memsz);
        align = align.max(p.align);
        if p.p_type == pt::LOAD {
            base_vaddr = base_vaddr.min(p.vaddr);
        }
    }
    let base_vaddr = if base_vaddr == u64::MAX {
        0
    } else {
        base_vaddr
    };
    start_page = round_up(start_page, align);

    // One extra PHT entry for the new PT_LOAD.
    let new_phnum = image.phdrs.len() as u64 + 1;
    let pht_size = round_up(new_phnum * phdr_size + ehdr_size, sec_align);
    let sht_size = round_up(image.shdrs.len() as u64 * shdr_size, sec_align);

    let (region_offsets, needed) = region_layout(image, &relocate, pht_size + sht_size);

    // The kernel passes AT_PHDR = base_vaddr + e_phoff, and the loader derives
    // its load bias from PT_PHDR.vaddr, so every byte in the appended region
    // must keep vaddr == base_vaddr + file_offset. Pick a file offset that also
    // places the region past all existing segments to avoid overlap.
    let min_off = start_page.saturating_sub(base_vaddr);
    let start_off = round_up(region_start.unwrap_or(orig_len).max(min_off), align);
    let region_vaddr = base_vaddr + start_off;
    // +1: older binutils readelf rejects a PT_DYNAMIC as large as the file.
    let total = start_off + needed + 1;

    let ldcache_phdr = ldcache_note_phdr(image);

    // Lay out the appended region: PHT, SHT, then the relocated sections at the
    // offsets assigned above.
    image.ehdr.phoff = start_off;
    image.ehdr.shoff = start_off + pht_size;
    let moves = place_relocated(image, &relocate, &region_offsets, start_off, region_vaddr)?;

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

    let phdr_vaddr = image.phdrs.last().unwrap().vaddr + (image.ehdr.phoff - start_off);
    sync_segments(image, ldcache_phdr, phdr_vaddr);
    fixup_dynamic_addrs(image, &moves);
    sync_dynamic(image)?;

    verify::run(image, total)?;
    serialize::write_to(image, original, total, out)
}

/// True if any fixed-stride record in `sec` has its `width`-byte field at
/// offset `fo` satisfying `pred`.
fn any_field(
    image: &ElfImage<'_>,
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
            _ => u64::from(e.read_u32(r, fo)),
        };
        pred(v)
    })
}

/// Error if any relocation target (`r_offset`) or symbol value (`st_value`) lands
/// inside one of the address ranges about to be relocated.
fn assert_no_address_refs(image: &ElfImage<'_>, ranges: &[(u64, u64)]) -> Result<()> {
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

/// Re-point `PT_PHDR/PT_DYNAMIC/PT_INTERP` and the ld-cache `PT_NOTE` at their
/// (possibly moved) targets. `phdr_vaddr` is the virtual address of the
/// relocated program header table.
fn sync_segments(image: &mut ElfImage<'_>, ldcache_phdr: Option<usize>, phdr_vaddr: u64) {
    let phoff = image.ehdr.phoff;
    let pht_bytes = image.phdrs.len() as u64 * codec::phdr_size(image.enc.class) as u64;
    let extent = |i: usize| {
        let s = &image.shdrs[i];
        (s.offset, s.addr, s.size)
    };
    let interp = image.find_section(".interp").map(extent);
    let abiflags = image.find_section(".MIPS.abiflags").map(extent);
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
            pt::MIPS_ABIFLAGS => {
                if let Some((off, addr, size)) = abiflags {
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
/// then refresh `DT_STRSZ`. The tag set mirrors glibc's `ADJUST_DYN_INFO` plus the
/// init/fini and array pointers: anything the loader treats as an address. By
/// keying on the old address range rather than a section name, every moved
/// section is handled, not just the few an operation is known to touch.
fn fixup_dynamic_addrs(image: &mut ElfImage<'_>, moves: &[MovedSection]) {
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
        dt::MIPS_XHASH,
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
    fixup_mips_rld_map_rel(image);
    // DT_STRSZ tracks the .dynstr size, the one string table that can grow.
    if let Some(size) = image.find_section(".dynstr").map(|i| image.shdrs[i].size) {
        for d in &mut image.dynamic {
            if d.tag == dt::STRSZ {
                d.val = size;
            }
        }
    }
}

/// `DT_MIPS_RLD_MAP_REL` holds the offset from the tag's own in-memory address to
/// the .`rld_map` debug pointer, so it must be recomputed whenever .dynamic or
/// .`rld_map` moves. Absent .`rld_map`, mirror patchelf and store 0.
fn fixup_mips_rld_map_rel(image: &mut ElfImage<'_>) {
    if !image.dynamic.iter().any(|d| d.tag == dt::MIPS_RLD_MAP_REL) {
        return;
    }
    let stride = codec::dyn_size(image.enc.class) as u64;
    let dyn_addr = dyn_idx(image).map_or(0, |i| image.shdrs[i].addr);
    let rld = image.find_section(".rld_map").map(|i| image.shdrs[i].addr);
    let mask = if image.enc.class == Class::Elf32 {
        0xffff_ffff
    } else {
        u64::MAX
    };
    for (i, d) in image.dynamic.iter_mut().enumerate() {
        if d.tag == dt::MIPS_RLD_MAP_REL {
            d.val = rld.map_or(0, |r| {
                r.wrapping_sub(i as u64 * stride).wrapping_sub(dyn_addr) & mask
            });
        }
    }
}
