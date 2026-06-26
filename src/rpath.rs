//! The rpath/runpath operations: read, set, add, remove and shrink. DT_RUNPATH
//! is preferred over the obsolete DT_RPATH unless --force-rpath is given.

use std::path::Path;

use crate::error::Result;
use crate::ir::{self, dt, ElfImage};
use crate::ops::{dynstr_append, dynstr_section, needed, require_dynamic, Modifiers};

/// DT_RUNPATH takes precedence over the obsolete DT_RPATH.
pub fn read(image: &ElfImage) -> Result<String> {
    require_dynamic(image)?;
    let Some(strtab) = image.dynstr() else {
        return Ok(String::new());
    };
    let get = |off: u64| ir::cstr(strtab, off as u32).unwrap_or_default().to_owned();
    let mut rpath = String::new();
    for d in &image.dynamic {
        match d.tag {
            dt::RUNPATH => return Ok(get(d.val)),
            dt::RPATH => rpath = get(d.val),
            dt::NULL => break,
            _ => {}
        }
    }
    Ok(rpath)
}

pub fn remove(image: &mut ElfImage) {
    image
        .dynamic
        .retain(|d| d.tag != dt::RPATH && d.tag != dt::RUNPATH);
}

/// Append `path` to the current rpath (colon-joined), then set it.
pub fn add(image: &mut ElfImage, path: &str, force: bool) -> Result<()> {
    let cur = read(image)?;
    let combined = if cur.is_empty() {
        path.to_string()
    } else {
        format!("{cur}:{path}")
    };
    set(image, &combined, force)
}

/// Drop rpath directories that contain none of the needed libraries (matching
/// the binary's machine type), and any rejected by --allowed-rpath-prefixes.
/// Non-absolute entries such as $ORIGIN are always kept.
pub fn shrink(image: &mut ElfImage, mods: &Modifiers) -> Result<()> {
    let cur = read(image)?;
    if cur.is_empty() {
        return Ok(());
    }
    let needed = needed(image)?;
    let machine = image.ehdr.machine;
    let mut found = vec![false; needed.len()];
    let mut kept: Vec<&str> = Vec::new();

    for dir in cur.split(':').filter(|s| !s.is_empty()) {
        if !dir.starts_with('/') {
            kept.push(dir);
            continue;
        }
        let allowed = &mods.allowed_rpath_prefixes;
        if !allowed.is_empty() && !allowed.iter().any(|p| dir.starts_with(p)) {
            continue;
        }
        let mut dir_useful = false;
        for (j, lib) in needed.iter().enumerate() {
            if found[j] {
                continue;
            }
            if elf_machine(&Path::new(dir).join(lib)) == Some(machine) {
                found[j] = true;
                dir_useful = true;
            }
        }
        if dir_useful {
            kept.push(dir);
        }
    }

    let new = kept.join(":");
    set(image, &new, mods.force_rpath)
}

/// Set DT_RUNPATH (or DT_RPATH when `force`). Reuses the existing string slot
/// when the new value fits, otherwise appends to .dynstr; adds the dynamic
/// entry when absent. Growth is resolved later by the layout engine.
pub fn set(image: &mut ElfImage, new: &str, force: bool) -> Result<()> {
    require_dynamic(image)?;
    let dynstr_idx = dynstr_section(image)?;

    let existing: Vec<usize> = image
        .dynamic
        .iter()
        .take_while(|d| d.tag != dt::NULL)
        .enumerate()
        .filter(|(_, d)| d.tag == dt::RPATH || d.tag == dt::RUNPATH)
        .map(|(i, _)| i)
        .collect();

    // Match patchelf's tag policy: prefer DT_RUNPATH, unless --force-rpath.
    let has_runpath = existing
        .iter()
        .any(|&i| image.dynamic[i].tag == dt::RUNPATH);
    let convert_to = if force { dt::RPATH } else { dt::RUNPATH };
    let needs_convert = if force { has_runpath } else { !has_runpath };
    if needs_convert {
        for &i in &existing {
            image.dynamic[i].tag = convert_to;
        }
    }

    // Try an in-place overwrite when the new path fits the current slot.
    if let Some(&first) = existing.first() {
        let off = image.dynamic[first].val as usize;
        let old_len = ir::cstr(&image.section_data[dynstr_idx], off as u32)
            .map(str::len)
            .unwrap_or(0);
        if new.len() <= old_len {
            let buf = &mut image.section_data[dynstr_idx];
            buf[off..off + new.len()].copy_from_slice(new.as_bytes());
            buf[off + new.len()] = 0;
            return Ok(());
        }
    }

    let str_off = dynstr_append(&mut image.section_data[dynstr_idx], new);
    if existing.is_empty() {
        image.dynamic.insert(
            0,
            ir::DynEntry {
                tag: convert_to,
                val: str_off,
            },
        );
    } else {
        for &i in &existing {
            image.dynamic[i].val = str_off;
        }
    }
    Ok(())
}

/// Read an ELF file's e_machine, or None if it isn't a readable ELF. Reads only
/// the leading header bytes so probing large shared libraries stays cheap.
fn elf_machine(path: &Path) -> Option<u16> {
    use std::io::Read;
    let mut head = [0u8; 20];
    std::fs::File::open(path).ok()?.read_exact(&mut head).ok()?;
    if &head[..4] != b"\x7fELF" {
        return None;
    }
    let m = [head[18], head[19]];
    Some(if head[5] == 1 {
        u16::from_le_bytes(m) // EI_DATA: 1 = little-endian
    } else {
        u16::from_be_bytes(m)
    })
}
