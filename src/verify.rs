//! Loadable-ELF invariants checked on the finalized image before writing, so
//! layout bugs surface as errors instead of silently corrupt binaries.

use crate::error::{Error, Result};
use crate::ir::{dt, pf, pt, sht, ElfImage, Phdr};

fn fail(msg: impl Into<String>) -> Error {
    Error::Serialize(msg.into())
}

pub fn run(image: &ElfImage) -> Result<()> {
    let loads: Vec<_> = image
        .phdrs
        .iter()
        .filter(|p| p.p_type == pt::LOAD)
        .collect();

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
    let mut ranges: Vec<(u64, u64)> = loads.iter().map(|p| (p.vaddr, p.vaddr + p.memsz)).collect();
    ranges.sort_by_key(|r| r.0);
    for w in ranges.windows(2) {
        if w[0].1 > w[1].0 {
            return Err(fail("PT_LOAD segments overlap in memory"));
        }
    }

    // The program header table must live inside some PT_LOAD.
    let pht_end = image.ehdr.phoff + image.phdrs.len() as u64 * image.ehdr.phentsize as u64;
    let covered = loads
        .iter()
        .any(|p| image.ehdr.phoff >= p.offset && pht_end <= p.offset + p.filesz);
    if !covered {
        return Err(fail("program header table not covered by a PT_LOAD"));
    }

    phdr_anchor(image, &loads)?;
    dynamic_consistency(image)?;
    Ok(())
}

/// The kernel passes AT_PHDR = base_vaddr + e_phoff and the loader derives its
/// load bias from PT_PHDR.p_vaddr, so for ET_DYN the table must satisfy
/// PT_PHDR.p_vaddr == base_vaddr + e_phoff (base = the offset-0 PT_LOAD).
/// Getting this wrong loads fine on lenient x86_64 glibc but aborts elsewhere.
fn phdr_anchor(image: &ElfImage, loads: &[&Phdr]) -> Result<()> {
    let Some(phdr) = image.phdrs.iter().find(|p| p.p_type == pt::PHDR) else {
        return Ok(());
    };
    let Some(base) = loads.iter().min_by_key(|p| p.offset) else {
        return Err(fail("PT_PHDR present but no PT_LOAD"));
    };
    if phdr.offset != image.ehdr.phoff {
        return Err(fail("PT_PHDR offset disagrees with e_phoff"));
    }
    if phdr.vaddr != base.vaddr + (image.ehdr.phoff - base.offset) {
        return Err(fail("PT_PHDR vaddr breaks the AT_PHDR load-bias invariant"));
    }
    Ok(())
}

/// Cross-check the dynamic array against the sections and segments it must
/// agree with. These caught a real bug where DT_STRTAB kept pointing at the
/// pre-relayout .dynstr address.
fn dynamic_consistency(image: &ElfImage) -> Result<()> {
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
    if let (Some(seg), Some(sec)) = (dyn_seg, dyn_sec) {
        if seg.vaddr != sec.addr || seg.offset != sec.offset {
            return Err(fail("PT_DYNAMIC and .dynamic disagree on location"));
        }
    }

    Ok(())
}
