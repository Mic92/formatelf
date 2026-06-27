//! The rpath/runpath operations: read, set, add, remove and shrink. `DT_RUNPATH`
//! is preferred over the obsolete `DT_RPATH` unless --force-rpath is given.

use std::path::Path;

use crate::error::Result;
use crate::ir::{self, ElfImage, dt};
use crate::ops::{Modifiers, dynstr_section, dynstr_set, needed, require_dynamic};

/// `DT_RUNPATH` takes precedence over the obsolete `DT_RPATH`.
pub fn read(image: &ElfImage<'_>) -> Result<String> {
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

pub fn remove(image: &mut ElfImage<'_>) {
    image
        .dynamic
        .retain(|d| d.tag != dt::RPATH && d.tag != dt::RUNPATH);
}

/// Append `path` to the current rpath (colon-joined), then set it.
pub fn add(image: &mut ElfImage<'_>, path: &str, force: bool) -> Result<()> {
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
pub fn shrink(image: &mut ElfImage<'_>, mods: &Modifiers) -> Result<()> {
    let cur = read(image)?;
    if cur.is_empty() {
        return Ok(());
    }
    let needed = needed(image)?;
    let machine = image.ehdr.machine;
    // A directory is useful if it supplies a needed lib of the right machine
    // not yet found in an earlier directory.
    let mut found = vec![false; needed.len()];
    let keep = |dir: &str| {
        let mut useful = false;
        for (j, lib) in needed.iter().enumerate() {
            if !found[j] && elf_machine(&Path::new(dir).join(lib)) == Some(machine) {
                found[j] = true;
                useful = true;
            }
        }
        useful
    };
    let new = filter_dirs(&cur, &mods.allowed_rpath_prefixes, keep);
    set(image, &new, mods.force_rpath)
}

/// Filter a colon-separated rpath: drop empty components, keep non-absolute
/// entries (e.g. $ORIGIN) verbatim, and keep an absolute entry only when it
/// passes the `allowed` prefixes (if any) and `keep` accepts it. Order is
/// preserved. Pure, so the splitting/prefix logic is property-tested below.
fn filter_dirs(cur: &str, allowed: &[String], mut keep: impl FnMut(&str) -> bool) -> String {
    let mut out: Vec<&str> = Vec::new();
    for dir in cur.split(':').filter(|s| !s.is_empty()) {
        let allowed_ok = allowed.is_empty() || allowed.iter().any(|p| dir.starts_with(p));
        if !dir.starts_with('/') || (allowed_ok && keep(dir)) {
            out.push(dir);
        }
    }
    out.join(":")
}

/// Read an ELF file's `e_machine`, or None if it isn't a readable ELF. Reads only
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

/// Set `DT_RUNPATH` (or `DT_RPATH` when `force`). Reuses the existing string slot
/// when the new value fits, otherwise appends to .dynstr; adds the dynamic
/// entry when absent. Growth is resolved later by the layout engine.
pub fn set(image: &mut ElfImage<'_>, new: &str, force: bool) -> Result<()> {
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

    let reuse = existing.first().map(|&i| image.dynamic[i].val as usize);
    let off = dynstr_set(image.section_data[dynstr_idx].to_mut(), reuse, new);
    if existing.is_empty() {
        image.dynamic.insert(
            0,
            ir::DynEntry {
                tag: convert_to,
                val: off,
            },
        );
    } else {
        for &i in &existing {
            image.dynamic[i].val = off;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::filter_dirs;
    use proptest::prelude::*;

    /// A colon-joined rpath of mixed empty / absolute / relative components.
    fn rpath() -> impl Strategy<Value = String> {
        let component = prop_oneof![
            Just(String::new()),
            "/[a-z]{1,5}(/[a-z]{1,5})*",
            "[a-z]{1,5}",
            Just("$ORIGIN".to_string()),
        ];
        prop::collection::vec(component, 0..8).prop_map(|v| v.join(":"))
    }

    proptest! {
        /// With everything accepted, filtering only drops empty components.
        #[test]
        fn keeps_all_nonempty_when_accepted(cur in rpath()) {
            let got = filter_dirs(&cur, &[], |_| true);
            let want: Vec<&str> = cur.split(':').filter(|s| !s.is_empty()).collect();
            prop_assert_eq!(got, want.join(":"));
        }

        /// Rejecting every absolute dir leaves only the relative ones.
        #[test]
        fn drops_absolute_when_rejected(cur in rpath()) {
            let got = filter_dirs(&cur, &[], |_| false);
            prop_assert!(got.split(':').all(|d| d.is_empty() || !d.starts_with('/')));
        }
    }
}
