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
        other => return Err(Error::Unsupported(format!("{other:?}"))),
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
