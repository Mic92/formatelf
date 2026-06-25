//! Loadable-ELF invariants checked on the finalized image before writing, so
//! layout bugs surface as errors instead of silently corrupt binaries.

use crate::error::{Error, Result};
use crate::ir::{pf, pt, ElfImage};

fn fail(msg: impl Into<String>) -> Error {
    Error::Serialize(msg.into())
}

pub fn validate(image: &ElfImage) -> Result<()> {
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

    Ok(())
}
