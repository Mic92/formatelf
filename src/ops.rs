//! Pure transforms on [`ir::ElfImage`]. Never compute offsets or touch I/O;
//! relayout happens once afterwards in the layout engine.

use crate::cli::Operation;
use crate::error::{Error, Result};
use crate::ir::{self, dt, ElfImage};

/// Output from `--print-*` operations.
#[derive(Debug, Default)]
pub struct Report {
    pub lines: Vec<String>,
}

impl Report {
    fn push(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }
}

pub fn apply(image: &mut ElfImage, op: &Operation, report: &mut Report) -> Result<()> {
    match op {
        Operation::PrintInterpreter => report.push(interpreter(image)?),
        Operation::PrintOsAbi => report.push(os_abi_name(image.ehdr.os_abi)),
        Operation::PrintSoname => {
            if let Some(s) = soname(image) {
                report.push(s);
            }
        }
        Operation::PrintRpath => report.push(rpath(image)?),
        Operation::PrintNeeded => {
            for lib in needed(image)? {
                report.push(lib);
            }
        }
        Operation::PrintExecstack => report.push(format!("execstack: {}", execstack(image))),
        Operation::RemoveRpath => remove_rpath(image),
        Operation::SetRpath(path) => set_rpath(image, path, false)?,
        Operation::ForceRpath => {}
        Operation::SetInterpreter(p) => set_interpreter(image, p)?,
        Operation::SetSoname(name) => set_soname(image, name)?,
        Operation::AddNeeded(lib) => add_needed(image, lib)?,
        Operation::RemoveNeeded(lib) => remove_needed(image, lib)?,
        Operation::ReplaceNeeded { old, new } => replace_needed(image, old, new)?,
        other => return Err(Error::Unsupported(format!("{other:?}"))),
    }
    Ok(())
}

/// Drop DT_RPATH/DT_RUNPATH. The trailing DT_NULL is retained, so the encoded
/// array only shrinks and the edit stays in place.
fn remove_rpath(image: &mut ElfImage) {
    image
        .dynamic
        .retain(|d| d.tag != dt::RPATH && d.tag != dt::RUNPATH);
}

/// Set DT_RUNPATH (or DT_RPATH when `force`). Reuses the existing string slot
/// when the new value fits, otherwise appends to .dynstr; adds the dynamic
/// entry when absent. Growth is resolved later by the layout engine.
fn set_rpath(image: &mut ElfImage, new: &str, force: bool) -> Result<()> {
    require_dynamic(image)?;
    let dynstr_idx = image
        .find_section(".dynstr")
        .ok_or_else(|| Error::Missing("cannot find section .dynstr".into()))?;

    let existing: Vec<usize> = image
        .dynamic
        .iter()
        .take_while(|d| d.tag != dt::NULL)
        .enumerate()
        .filter(|(_, d)| d.tag == dt::RPATH || d.tag == dt::RUNPATH)
        .map(|(i, _)| i)
        .collect();

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
        let tag = if force { dt::RPATH } else { dt::RUNPATH };
        image.dynamic.insert(0, ir::DynEntry { tag, val: str_off });
    } else {
        for &i in &existing {
            image.dynamic[i].val = str_off;
        }
    }
    Ok(())
}

/// Append a NUL-terminated string to a string table, returning its offset.
fn dynstr_append(buf: &mut Vec<u8>, s: &str) -> u64 {
    let off = buf.len() as u64;
    buf.extend_from_slice(s.as_bytes());
    buf.push(0);
    off
}

fn dynstr_section(image: &ElfImage) -> Result<usize> {
    image
        .find_section(".dynstr")
        .ok_or_else(|| Error::Missing("cannot find section .dynstr".into()))
}

fn set_interpreter(image: &mut ElfImage, new: &str) -> Result<()> {
    let idx = image
        .find_section(".interp")
        .ok_or_else(|| Error::Missing("cannot find section .interp".into()))?;
    let mut bytes = new.as_bytes().to_vec();
    bytes.push(0);
    image.section_data[idx] = bytes;
    Ok(())
}

/// Only meaningful for shared objects; a no-op on executables, matching patchelf.
fn set_soname(image: &mut ElfImage, name: &str) -> Result<()> {
    if image.ehdr.e_type != ir::et::DYN {
        return Ok(());
    }
    require_dynamic(image)?;
    let dynstr_idx = dynstr_section(image)?;
    let off = dynstr_append(&mut image.section_data[dynstr_idx], name);
    match image.dynamic.iter().position(|d| d.tag == dt::SONAME) {
        Some(i) => image.dynamic[i].val = off,
        None => image.dynamic.insert(
            0,
            ir::DynEntry {
                tag: dt::SONAME,
                val: off,
            },
        ),
    }
    Ok(())
}

fn add_needed(image: &mut ElfImage, lib: &str) -> Result<()> {
    require_dynamic(image)?;
    let dynstr_idx = dynstr_section(image)?;
    let off = dynstr_append(&mut image.section_data[dynstr_idx], lib);
    image.dynamic.insert(
        0,
        ir::DynEntry {
            tag: dt::NEEDED,
            val: off,
        },
    );
    Ok(())
}

fn remove_needed(image: &mut ElfImage, lib: &str) -> Result<()> {
    require_dynamic(image)?;
    let dynstr_idx = dynstr_section(image)?;
    let targets: Vec<u64> = image
        .dynamic
        .iter()
        .filter(|d| d.tag == dt::NEEDED)
        .filter(|d| ir::cstr(&image.section_data[dynstr_idx], d.val as u32) == Some(lib))
        .map(|d| d.val)
        .collect();
    image
        .dynamic
        .retain(|d| d.tag != dt::NEEDED || !targets.contains(&d.val));
    Ok(())
}

fn replace_needed(image: &mut ElfImage, old: &str, new: &str) -> Result<()> {
    require_dynamic(image)?;
    let dynstr_idx = dynstr_section(image)?;
    let matches: Vec<usize> = image
        .dynamic
        .iter()
        .enumerate()
        .filter(|(_, d)| d.tag == dt::NEEDED)
        .filter(|(_, d)| ir::cstr(&image.section_data[dynstr_idx], d.val as u32) == Some(old))
        .map(|(i, _)| i)
        .collect();
    if matches.is_empty() {
        return Ok(());
    }
    let off = dynstr_append(&mut image.section_data[dynstr_idx], new);
    for i in matches {
        image.dynamic[i].val = off;
    }
    Ok(())
}

fn require_dynamic(image: &ElfImage) -> Result<()> {
    if image.has_dynamic() {
        Ok(())
    } else {
        Err(Error::Missing("no .dynamic section".into()))
    }
}

fn interpreter(image: &ElfImage) -> Result<String> {
    let idx = image
        .find_section(".interp")
        .ok_or_else(|| Error::Missing("cannot find section .interp".into()))?;
    Ok(ir::cstr(&image.section_data[idx], 0)
        .unwrap_or_default()
        .to_owned())
}

/// Value of the first `tag` dynamic entry resolved against `.dynstr`.
fn dyn_string(image: &ElfImage, tag: i64) -> Option<String> {
    let strtab = image.dynstr()?;
    let entry = image.dynamic.iter().find(|d| d.tag == tag)?;
    ir::cstr(strtab, entry.val as u32).map(str::to_owned)
}

fn soname(image: &ElfImage) -> Option<String> {
    if image.ehdr.e_type != ir::et::DYN {
        return None;
    }
    dyn_string(image, dt::SONAME).filter(|s| !s.is_empty())
}

/// DT_RUNPATH takes precedence over the obsolete DT_RPATH.
fn rpath(image: &ElfImage) -> Result<String> {
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

fn needed(image: &ElfImage) -> Result<Vec<String>> {
    require_dynamic(image)?;
    let Some(strtab) = image.dynstr() else {
        return Ok(Vec::new());
    };
    Ok(image
        .dynamic
        .iter()
        .take_while(|d| d.tag != dt::NULL)
        .filter(|d| d.tag == dt::NEEDED)
        .filter_map(|d| ir::cstr(strtab, d.val as u32).map(str::to_owned))
        .collect())
}

fn execstack(image: &ElfImage) -> char {
    match image.phdrs.iter().find(|p| p.p_type == ir::pt::GNU_STACK) {
        Some(p) if p.flags & ir::pf::X != 0 => 'X',
        Some(_) => '-',
        None => '?',
    }
}

fn os_abi_name(abi: u8) -> String {
    let name = match abi {
        0 => "System V",
        1 => "HP-UX",
        2 => "NetBSD",
        3 => "Linux",
        4 => "GNU Hurd",
        6 => "Solaris",
        7 => "AIX",
        8 => "IRIX",
        9 => "FreeBSD",
        10 => "Tru64",
        12 => "OpenBSD",
        13 => "OpenVMS",
        _ => return format!("0x{abi:02X}"),
    };
    name.to_owned()
}
