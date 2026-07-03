//! Loadable-ELF invariants checked on the finalized image before writing, so
//! layout bugs surface as errors instead of silently corrupt binaries.

use crate::error::{Error, Result};
use crate::ir::{ElfImage, Phdr, dt, et, pf, pt, sht};

fn fail(msg: impl Into<String>) -> Error {
    Error::Constraint(msg.into())
}

/// Dynamic tags whose value is a virtual address, not a size, string offset, or
/// flag.
const ADDR_TAGS: &[i64] = &[
    dt::PLTGOT,
    dt::HASH,
    dt::GNU_HASH,
    dt::STRTAB,
    dt::SYMTAB,
    dt::RELA,
    dt::REL,
    dt::JMPREL,
    dt::INIT,
    dt::FINI,
    dt::INIT_ARRAY,
    dt::FINI_ARRAY,
    dt::PREINIT_ARRAY,
    dt::RELR,
    dt::VERDEF,
    dt::VERNEED,
    dt::VERSYM,
];

/// # Errors
/// Returns an error if the image violates a structural invariant the loader
/// relies on. `total` is the size of the file that will be written.
pub fn run(image: &ElfImage<'_>, total: u64) -> Result<()> {
    let loads: Vec<_> = image
        .phdrs
        .iter()
        .filter(|p| p.p_type == pt::LOAD)
        .collect();

    for p in &image.phdrs {
        // p_align must be 0, 1, or a power of two; the loader takes it modulo.
        if p.align > 1 && !p.align.is_power_of_two() {
            return Err(fail("program header alignment is not a power of two"));
        }
    }

    for p in &loads {
        if p.align > 1 && (p.vaddr % p.align) != (p.offset % p.align) {
            return Err(fail("PT_LOAD offset/vaddr not page-congruent"));
        }
        if p.flags & pf::W != 0 && p.flags & pf::X != 0 {
            return Err(fail("PT_LOAD is both writable and executable"));
        }
        if p.filesz > p.memsz {
            return Err(fail("PT_LOAD file size exceeds memory size"));
        }
    }

    // PT_LOAD virtual address ranges must not overlap.
    let mut ranges: Vec<(u64, u64)> = loads
        .iter()
        .map(|p| (p.vaddr, p.vaddr.saturating_add(p.memsz)))
        .collect();
    ranges.sort_by_key(|r| r.0);
    for w in ranges.windows(2) {
        if w[0].1 > w[1].0 {
            return Err(fail("PT_LOAD segments overlap in memory"));
        }
    }

    // The program header table must live inside some PT_LOAD.
    let pht_end = image
        .ehdr
        .phoff
        .saturating_add((image.phdrs.len() as u64).saturating_mul(u64::from(image.ehdr.phentsize)));
    let covered = loads
        .iter()
        .any(|p| image.ehdr.phoff >= p.offset && pht_end <= p.offset.saturating_add(p.filesz));
    if !covered {
        return Err(fail("program header table not covered by a PT_LOAD"));
    }

    phdr_anchor(image, &loads)?;
    file_bounds(image, total)?;
    section_links(image)?;
    interp(image, &loads)?;
    entry(image, &loads)?;
    dynamic_consistency(image)?;
    dynamic_addrs(image, &loads)?;
    Ok(())
}

/// Nothing a header references may extend past the file we are about to write.
fn file_bounds(image: &ElfImage<'_>, total: u64) -> Result<()> {
    let pht_end = image
        .ehdr
        .phoff
        .saturating_add((image.phdrs.len() as u64).saturating_mul(u64::from(image.ehdr.phentsize)));
    if image.ehdr.phoff != 0 && pht_end > total {
        return Err(fail("program header table runs past end of file"));
    }
    let sht_end = image
        .ehdr
        .shoff
        .saturating_add((image.shdrs.len() as u64).saturating_mul(u64::from(image.ehdr.shentsize)));
    if image.ehdr.shoff != 0 && sht_end > total {
        return Err(fail("section header table runs past end of file"));
    }
    for (i, p) in image.phdrs.iter().enumerate() {
        let end = p.offset.saturating_add(p.filesz);
        if end > total {
            return Err(fail(format!(
                "segment {i} ends at {end:#x}, past end of file {total:#x}"
            )));
        }
    }
    for (i, s) in image.shdrs.iter().enumerate() {
        // SHT_NOBITS (.bss) occupies no file space, so its offset is irrelevant.
        let end = s.offset.saturating_add(s.size);
        if s.sh_type != sht::NOBITS && end > total {
            let name = image.section_name(i).unwrap_or("?");
            return Err(fail(format!(
                "section {i} ({name}) ends at {end:#x}, past end of file {total:#x}"
            )));
        }
    }
    Ok(())
}

/// `e_shstrndx` and the `sh_link` of tables that carry one must index a real
/// section.
fn section_links(image: &ElfImage<'_>) -> Result<()> {
    let n = image.shdrs.len();
    if n == 0 {
        return Ok(());
    }
    if image.ehdr.shstrndx as usize >= n {
        return Err(fail("e_shstrndx out of range"));
    }
    for s in &image.shdrs {
        let uses_link = matches!(
            s.sh_type,
            sht::SYMTAB | sht::DYNSYM | sht::DYNAMIC | sht::RELA | sht::REL
        );
        if uses_link && s.link as usize >= n {
            return Err(fail(format!(
                "section sh_link {} out of range (0..{n})",
                s.link
            )));
        }
    }
    Ok(())
}

/// A stale `PT_INTERP` after growing the interpreter hands the kernel a
/// truncated or over-long path, so it must frame the NUL-terminated `.interp`
/// inside a load.
fn interp(image: &ElfImage<'_>, loads: &[&Phdr]) -> Result<()> {
    let Some(seg) = image.phdrs.iter().find(|p| p.p_type == pt::INTERP) else {
        return Ok(());
    };
    if seg.filesz == 0 {
        return Err(fail("PT_INTERP is empty"));
    }
    if !loads.iter().any(|p| {
        seg.offset >= p.offset
            && seg.offset.saturating_add(seg.filesz) <= p.offset.saturating_add(p.filesz)
    }) {
        return Err(fail("PT_INTERP not covered by a PT_LOAD"));
    }
    if let Some(i) = image.find_section(".interp") {
        let s = &image.shdrs[i];
        if seg.offset != s.offset || seg.filesz != s.size {
            return Err(fail("PT_INTERP does not frame the .interp section"));
        }
        if image.section_data[i].last() != Some(&0) {
            return Err(fail("interpreter path is not NUL-terminated"));
        }
    }
    Ok(())
}

/// The entry point must land in an executable `PT_LOAD`, or the kernel jumps to
/// unmapped memory.
fn entry(image: &ElfImage<'_>, loads: &[&Phdr]) -> Result<()> {
    if image.ehdr.entry == 0 || (image.ehdr.e_type != et::EXEC && image.ehdr.e_type != et::DYN) {
        return Ok(());
    }
    let e = image.ehdr.entry;
    if !loads
        .iter()
        .any(|p| p.flags & pf::X != 0 && e >= p.vaddr && e < p.vaddr.saturating_add(p.memsz))
    {
        return Err(fail("entry point not inside an executable PT_LOAD"));
    }
    Ok(())
}

/// The classic patchelf failure: a section moves during relayout but the
/// address-valued DT_ tag pointing at it keeps its stale address, leaving the
/// loader reading hash/symbol/reloc tables from unmapped memory. Each such
/// pointer must resolve inside some `PT_LOAD`.
fn dynamic_addrs(image: &ElfImage<'_>, loads: &[&Phdr]) -> Result<()> {
    if loads.is_empty() {
        return Ok(());
    }
    let mapped = |a: u64| {
        loads
            .iter()
            .any(|p| a >= p.vaddr && a < p.vaddr.saturating_add(p.memsz))
    };
    for d in &image.dynamic {
        if ADDR_TAGS.contains(&d.tag) && d.val != 0 && !mapped(d.val) {
            return Err(fail(format!(
                "dynamic tag {:#x} points at {:#x}, outside every PT_LOAD",
                d.tag, d.val
            )));
        }
    }
    Ok(())
}

/// The kernel passes `AT_PHDR` = `base_vaddr` + `e_phoff` and the loader derives its
/// load bias from `PT_PHDR.p_vaddr`, so for `ET_DYN` the table must satisfy
/// `PT_PHDR.p_vaddr` == `base_vaddr` + `e_phoff` (base = the offset-0 `PT_LOAD`).
/// Getting this wrong loads fine on lenient `x86_64` glibc but aborts elsewhere.
fn phdr_anchor(image: &ElfImage<'_>, loads: &[&Phdr]) -> Result<()> {
    let Some(phdr) = image.phdrs.iter().find(|p| p.p_type == pt::PHDR) else {
        return Ok(());
    };
    let Some(base) = loads.iter().min_by_key(|p| p.offset) else {
        return Err(fail("PT_PHDR present but no PT_LOAD"));
    };
    if phdr.offset != image.ehdr.phoff {
        return Err(fail("PT_PHDR offset disagrees with e_phoff"));
    }
    if phdr.vaddr
        != base
            .vaddr
            .saturating_add(image.ehdr.phoff.saturating_sub(base.offset))
    {
        return Err(fail("PT_PHDR vaddr breaks the AT_PHDR load-bias invariant"));
    }
    Ok(())
}

/// Cross-check the dynamic array against the sections and segments it must
/// agree with. These caught a real bug where `DT_STRTAB` kept pointing at the
/// pre-relayout .dynstr address.
fn dynamic_consistency(image: &ElfImage<'_>) -> Result<()> {
    let dval = |tag: i64| image.dynamic.iter().find(|d| d.tag == tag).map(|d| d.val);

    if let Some(i) = image.find_section(".dynstr") {
        let s = &image.shdrs[i];
        if dval(dt::STRTAB).is_some_and(|v| v != s.addr) {
            return Err(fail("DT_STRTAB does not point at the .dynstr address"));
        }
        if dval(dt::STRSZ).is_some_and(|v| v > s.size) {
            return Err(fail("DT_STRSZ exceeds the .dynstr section size"));
        }
    }

    // String-valued tags hold byte offsets into the table; a bad .dynstr-append
    // would push one past DT_STRSZ.
    if let Some(strsz) = dval(dt::STRSZ) {
        const STR_TAGS: [i64; 4] = [dt::NEEDED, dt::SONAME, dt::RPATH, dt::RUNPATH];
        if image
            .dynamic
            .iter()
            .any(|d| STR_TAGS.contains(&d.tag) && d.val >= strsz)
        {
            return Err(fail("dynamic string offset past DT_STRSZ"));
        }
    }

    // PT_DYNAMIC and the SHT_DYNAMIC section must describe the same bytes.
    let dyn_seg = image.phdrs.iter().find(|p| p.p_type == pt::DYNAMIC);
    let dyn_sec = image.shdrs.iter().find(|s| s.sh_type == sht::DYNAMIC);
    if let (Some(seg), Some(sec)) = (dyn_seg, dyn_sec)
        && (seg.vaddr != sec.addr || seg.offset != sec.offset)
    {
        return Err(fail("PT_DYNAMIC and .dynamic disagree on location"));
    }

    Ok(())
}
