//! The NixOS ld-cache note (`--build-resolution-cache`): bake the resolved
//! location of each searched `DT_NEEDED` entry into a note so the loader can skip
//! the run-path search. Mirrors patchelf's buildResolutionCache.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{Error, Result};
use crate::ir::{self, ElfImage, dt, shf, sht};
use crate::ops::{needed, require_dynamic};

pub(crate) fn build(image: &mut ElfImage<'_>) -> Result<()> {
    require_dynamic(image)?;
    let needed = needed(image)?;
    let strtab = image
        .dynstr()
        .ok_or_else(|| Error::Missing("no .dynstr".into()))?;
    let pick = |tag| {
        image
            .dynamic
            .iter()
            .find(|d| d.tag == tag)
            .and_then(|d| ir::cstr(strtab, d.val as u32))
    };
    let runpath = pick(dt::RUNPATH).or_else(|| pick(dt::RPATH)).unwrap_or("");
    let dirs: Vec<&str> = runpath.split(':').filter(|s| !s.is_empty()).collect();
    if needed.is_empty() || dirs.is_empty() {
        eprintln!("warning: --build-resolution-cache: nothing to resolve; no cache written");
        return Ok(());
    }

    let mut cache: BTreeMap<String, String> = BTreeMap::new();
    let mut add = |lib: &str, val: String| {
        let e = cache.entry(lib.to_string()).or_default();
        if !e.is_empty() {
            e.push(':');
        }
        e.push_str(&val);
    };
    for dir in &dirs {
        // Tokens ($ORIGIN, ...) and glibc-hwcaps dirs can't be resolved at
        // patch time, so record the directory as a hint instead.
        let unresolvable = dir.contains('$') || Path::new(dir).join("glibc-hwcaps").exists();
        // A DT_NEEDED with a slash is opened directly, bypassing the run path.
        for lib in needed.iter().filter(|l| !l.contains('/')) {
            if unresolvable {
                add(lib, format!("?{dir}"));
            } else {
                let p = format!("{dir}/{lib}");
                if std::fs::File::open(&p).is_ok() {
                    add(lib, format!("={p}"));
                }
            }
        }
    }
    if cache.is_empty() {
        eprintln!("warning: --build-resolution-cache: no libraries resolved; no cache written");
        return Ok(());
    }

    let mut desc = Vec::new();
    for (lib, path) in &cache {
        desc.extend_from_slice(lib.as_bytes());
        desc.push(0);
        desc.extend_from_slice(path.as_bytes());
        desc.push(0);
    }
    desc.push(0);
    add_note_section(image, &desc)
}

/// Append a NixOS ld-cache `SHT_NOTE` section plus a `PT_NOTE` that points at it;
/// the layout engine assigns its address and the covering `PT_LOAD`.
fn add_note_section(image: &mut ElfImage<'_>, desc: &[u8]) -> Result<()> {
    const NT_NIXOS_LD_CACHE: u32 = 0x63a8_6cb6;
    let big = image.enc.endian == ir::Endian::Big;
    let u32b = |v: u32| {
        if big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        }
    };

    let mut name = b"NixOS\0".to_vec();
    let namesz = name.len() as u32;
    while !name.len().is_multiple_of(4) {
        name.push(0);
    }
    let mut note = Vec::new();
    note.extend_from_slice(&u32b(namesz));
    note.extend_from_slice(&u32b(desc.len() as u32));
    note.extend_from_slice(&u32b(NT_NIXOS_LD_CACHE));
    note.extend_from_slice(&name);
    note.extend_from_slice(desc);
    while !note.len().is_multiple_of(4) {
        note.push(0);
    }

    // Re-running must refresh the existing note, not append a duplicate. The
    // layout engine re-anchors the covering PT_NOTE once the section moves.
    if let Some(idx) = image.find_section(".note.nixos.ldcache") {
        if image.section_data[idx].as_ref() == note.as_slice() {
            return Ok(()); // already up to date
        }
        image.section_data[idx] = std::borrow::Cow::Owned(note);
        image.shdrs[idx].size = 0; // force re-placement
        return Ok(());
    }

    let shstr = image.ehdr.shstrndx as usize;
    if image.section_data.get(shstr).is_none() {
        return Err(Error::Missing("no section header string table".into()));
    }
    let name_off = image.section_data[shstr].len() as u32;
    image.section_data[shstr]
        .to_mut()
        .extend_from_slice(b".note.nixos.ldcache\0");

    image.shdrs.push(ir::Shdr {
        name: name_off,
        sh_type: sht::NOTE,
        flags: shf::ALLOC,
        addr: 0,
        offset: 0,
        size: 0, // 0 vs the data length marks the section for placement
        link: 0,
        info: 0,
        addralign: 4,
        entsize: 0,
    });
    image.section_data.push(std::borrow::Cow::Owned(note));

    // Placeholder PT_NOTE (filesz 0) resynced onto the note once it is placed.
    image.phdrs.push(ir::Phdr {
        p_type: ir::pt::NOTE,
        flags: ir::pf::R,
        offset: 0,
        vaddr: 0,
        paddr: 0,
        filesz: 0,
        memsz: 0,
        align: 4,
    });
    Ok(())
}
