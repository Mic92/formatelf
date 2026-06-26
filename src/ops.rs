//! Pure transforms on [`ir::ElfImage`]. Never compute offsets or touch I/O;
//! relayout happens once afterwards in the layout engine.

use std::collections::BTreeMap;
use std::path::Path;

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

/// Global modifiers that affect how the rpath operations behave, mirroring
/// patchelf's --force-rpath and --allowed-rpath-prefixes flags.
#[derive(Debug, Default)]
pub struct Modifiers {
    pub force_rpath: bool,
    pub allowed_rpath_prefixes: Vec<String>,
    pub debug: bool,
}

pub fn apply(
    image: &mut ElfImage,
    op: &Operation,
    mods: &Modifiers,
    report: &mut Report,
) -> Result<()> {
    if mods.debug {
        eprintln!("formatelf: applying {op:?}");
    }
    match op {
        Operation::PrintInterpreter => report.push(interpreter(image)?),
        Operation::PrintOsAbi => report.push(os_abi_name(image.ehdr.os_abi)),
        Operation::PrintSoname => {
            if let Some(s) = soname(image) {
                report.push(s);
            }
        }
        Operation::PrintRpath => report.push(crate::rpath::read(image)?),
        Operation::PrintNeeded => {
            for lib in needed(image)? {
                report.push(lib);
            }
        }
        Operation::PrintExecstack => report.push(format!("execstack: {}", execstack(image))),
        Operation::RemoveRpath => crate::rpath::remove(image),
        Operation::SetRpath(path) => crate::rpath::set(image, path, mods.force_rpath)?,
        Operation::AddRpath(path) => crate::rpath::add(image, path, mods.force_rpath)?,
        Operation::ShrinkRpath => crate::rpath::shrink(image, mods)?,
        Operation::ForceRpath | Operation::AllowedRpathPrefixes(_) => {}
        Operation::SetInterpreter(p) => set_interpreter(image, p)?,
        Operation::SetSoname(name) => set_soname(image, name)?,
        Operation::AddNeeded(lib) => add_needed(image, lib)?,
        Operation::RemoveNeeded(lib) => remove_needed(image, lib)?,
        Operation::ReplaceNeeded { old, new } => replace_needed(image, old, new)?,
        Operation::SetOsAbi(name) => set_os_abi(image, name)?,
        Operation::NoDefaultLib => no_default_lib(image)?,
        Operation::AddDebugTag => add_debug_tag(image)?,
        Operation::ClearExecstack => modify_execstack(image, false)?,
        Operation::SetExecstack => modify_execstack(image, true)?,
        Operation::ClearSymbolVersion(sym) => clear_symbol_version(image, sym)?,
        Operation::RenameDynamicSymbols(path) => {
            crate::symbols::rename_dynamic_symbols(image, &parse_symbol_map(path)?)?
        }
        Operation::BuildResolutionCache => crate::ldcache::build(image)?,
    }
    Ok(())
}

/// Drop DT_RPATH/DT_RUNPATH. The trailing DT_NULL is retained, so the encoded
/// array only shrinks and the edit stays in place.
/// Parse a symbol rename map: whitespace-separated `old new` pairs, one per
/// line, blank lines ignored (the patchelf NAME_MAP_FILE format).
fn parse_symbol_map(path: &Path) -> Result<BTreeMap<String, String>> {
    let text = std::fs::read_to_string(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        match (it.next(), it.next()) {
            (Some(old), Some(new)) => {
                map.insert(old.to_string(), new.to_string());
            }
            (None, _) => {}
            (Some(_), None) => {
                return Err(Error::Cli(format!("malformed symbol map line: {line:?}")))
            }
        }
    }
    Ok(map)
}

pub(crate) fn dynstr_append(buf: &mut Vec<u8>, s: &str) -> u64 {
    let off = buf.len() as u64;
    buf.extend_from_slice(s.as_bytes());
    buf.push(0);
    off
}

/// Store `s` in the dyn-string table, reusing the slot at `reuse` when `s`
/// fits, otherwise appending. Returns the offset where `s` now lives. Reusing
/// the slot keeps re-setting the same value free of growth (and idempotent).
pub(crate) fn dynstr_set(buf: &mut Vec<u8>, reuse: Option<usize>, s: &str) -> u64 {
    if let Some(off) = reuse {
        let old_len = ir::cstr(buf, off as u32).map(str::len).unwrap_or(0);
        if s.len() <= old_len {
            buf[off..off + s.len()].copy_from_slice(s.as_bytes());
            buf[off + s.len()] = 0;
            return off as u64;
        }
    }
    dynstr_append(buf, s)
}

pub(crate) fn dynstr_section(image: &ElfImage) -> Result<usize> {
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
    let existing = image.dynamic.iter().position(|d| d.tag == dt::SONAME);
    let reuse = existing.map(|i| image.dynamic[i].val as usize);
    let off = dynstr_set(&mut image.section_data[dynstr_idx], reuse, name);
    match existing {
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

/// Set the `.gnu.version` entry to 1 (VER_NDX_GLOBAL) for every dynamic symbol
/// named `sym`. In-place: the versym array keeps its size.
fn clear_symbol_version(image: &mut ElfImage, sym: &str) -> Result<()> {
    let dynsym = image
        .find_section(".dynsym")
        .ok_or_else(|| Error::Missing("cannot find section .dynsym".into()))?;
    let dynstr = dynstr_section(image)?;
    let versym = image
        .find_section(".gnu.version")
        .ok_or_else(|| Error::Missing("cannot find section .gnu.version".into()))?;

    let e = image.enc.endian;
    let sym_size = if image.enc.class == ir::Class::Elf64 {
        24
    } else {
        16
    };
    let count = image.section_data[dynsym].len() / sym_size;
    if image.section_data[versym].len() < count * 2 {
        return Err(Error::Parse("versym smaller than dynsym".into()));
    }

    // st_name (u32) is the first field of both Elf32_Sym and Elf64_Sym.
    const VER_NDX_GLOBAL: u16 = 1;
    for i in 0..count {
        let st_name = e.read_u32(&image.section_data[dynsym], i * sym_size);
        if ir::cstr(&image.section_data[dynstr], st_name) == Some(sym) {
            e.write_u16(&mut image.section_data[versym], i * 2, VER_NDX_GLOBAL);
        }
    }
    Ok(())
}

fn dyn_insert_front(image: &mut ElfImage, tag: i64, val: u64) {
    image.dynamic.insert(0, ir::DynEntry { tag, val });
}

fn no_default_lib(image: &mut ElfImage) -> Result<()> {
    require_dynamic(image)?;
    match image.dynamic.iter_mut().find(|d| d.tag == dt::FLAGS_1) {
        Some(d) => d.val |= ir::df1::NODEFLIB,
        None => dyn_insert_front(image, dt::FLAGS_1, ir::df1::NODEFLIB),
    }
    Ok(())
}

fn add_debug_tag(image: &mut ElfImage) -> Result<()> {
    require_dynamic(image)?;
    if !image.dynamic.iter().any(|d| d.tag == dt::DEBUG) {
        dyn_insert_front(image, dt::DEBUG, 0);
    }
    Ok(())
}

fn set_os_abi(image: &mut ElfImage, name: &str) -> Result<()> {
    let abi = abi_code(name).ok_or_else(|| Error::Cli("unrecognized OS ABI".into()))?;
    image.ehdr.ident[7] = abi; // EI_OSABI; written verbatim by the codec
    image.ehdr.os_abi = abi;
    Ok(())
}

/// EI_OSABI code for a human name, or None if unrecognized.
fn abi_code(name: &str) -> Option<u8> {
    Some(match name.trim().to_ascii_lowercase().as_str() {
        "system v" | "system-v" | "sysv" => 0,
        "hp-ux" => 1,
        "netbsd" => 2,
        "linux" | "gnu" => 3,
        "gnu hurd" | "gnu-hurd" | "hurd" => 4,
        "solaris" => 6,
        "aix" => 7,
        "irix" => 8,
        "freebsd" => 9,
        "tru64" => 10,
        "openbsd" => 12,
        "openvms" => 13,
        _ => return None,
    })
}

/// Toggle PF_X on PT_GNU_STACK. When the segment is absent, reuse a spare
/// PT_NULL slot if there is one, else append a new entry (the layout engine
/// relocates the program header table to make room). PT_GNU_STACK carries no
/// file content, so a fresh entry needs no offset/address assignment.
fn modify_execstack(image: &mut ElfImage, set: bool) -> Result<()> {
    if let Some(p) = image
        .phdrs
        .iter_mut()
        .find(|p| p.p_type == ir::pt::GNU_STACK)
    {
        p.flags = with_execstack(p.flags, set);
        return Ok(());
    }
    let new = ir::Phdr {
        p_type: ir::pt::GNU_STACK,
        flags: with_execstack(ir::pf::R | ir::pf::W, set),
        offset: 0,
        vaddr: 0,
        paddr: 0,
        filesz: 0,
        memsz: 0,
        align: 1,
    };
    match image.phdrs.iter_mut().find(|p| p.p_type == ir::pt::NULL) {
        Some(p) => *p = new,
        None => image.phdrs.push(new),
    }
    Ok(())
}

pub(crate) fn require_dynamic(image: &ElfImage) -> Result<()> {
    if image.has_dynamic() {
        Ok(())
    } else {
        Err(Error::Missing("no .dynamic section".into()))
    }
}

fn interpreter(image: &ElfImage) -> Result<String> {
    let bytes = match image.find_section(".interp") {
        Some(idx) => &image.section_data[idx],
        // Stripped section headers: fall back to PT_INTERP.
        None => image
            .interp_fallback
            .as_deref()
            .ok_or_else(|| Error::Missing("cannot find section .interp".into()))?,
    };
    Ok(ir::cstr(bytes, 0).unwrap_or_default().to_owned())
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

pub(crate) fn needed(image: &ElfImage) -> Result<Vec<String>> {
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

/// Set or clear PF_X, leaving the other flag bits untouched.
fn with_execstack(flags: u32, set: bool) -> u32 {
    if set {
        flags | ir::pf::X
    } else {
        flags & !ir::pf::X
    }
}

fn execstack(image: &ElfImage) -> char {
    execstack_char(&image.phdrs)
}

fn execstack_char(phdrs: &[ir::Phdr]) -> char {
    match phdrs.iter().find(|p| p.p_type == ir::pt::GNU_STACK) {
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

#[cfg(test)]
mod tests {
    use super::{abi_code, execstack_char, os_abi_name, with_execstack};
    use crate::ir::{pf, pt, Phdr};

    /// Each ABI name maps to a distinct code, and that code names it back.
    #[test]
    fn os_abi_table_is_consistent() {
        let table = [
            ("sysv", 0u8, "System V"),
            ("hp-ux", 1, "HP-UX"),
            ("netbsd", 2, "NetBSD"),
            ("linux", 3, "Linux"),
            ("gnu-hurd", 4, "GNU Hurd"),
            ("solaris", 6, "Solaris"),
            ("aix", 7, "AIX"),
            ("irix", 8, "IRIX"),
            ("freebsd", 9, "FreeBSD"),
            ("tru64", 10, "Tru64"),
            ("openbsd", 12, "OpenBSD"),
            ("openvms", 13, "OpenVMS"),
        ];
        for (name, code, display) in table {
            assert_eq!(abi_code(name), Some(code), "code for {name}");
            assert_eq!(os_abi_name(code), display, "name for {code}");
        }
        assert_eq!(abi_code("nonesuch"), None);
        assert_eq!(os_abi_name(99), "0x63");
    }

    fn gnu_stack(flags: u32) -> Phdr {
        Phdr {
            p_type: pt::GNU_STACK,
            flags,
            offset: 0,
            vaddr: 0,
            paddr: 0,
            filesz: 0,
            memsz: 0,
            align: 1,
        }
    }

    #[test]
    fn execstack_char_reflects_pf_x() {
        assert_eq!(execstack_char(&[]), '?');
        assert_eq!(execstack_char(&[gnu_stack(pf::R | pf::W)]), '-');
        assert_eq!(execstack_char(&[gnu_stack(pf::R | pf::W | pf::X)]), 'X');
    }

    #[test]
    fn with_execstack_toggles_only_pf_x() {
        let rw = pf::R | pf::W;
        assert_eq!(with_execstack(rw, true), rw | pf::X);
        assert_eq!(with_execstack(rw, false), rw);
        // Setting is idempotent, and clearing preserves the other bits.
        assert_eq!(with_execstack(rw | pf::X, true), rw | pf::X);
        assert_eq!(with_execstack(rw | pf::X, false), rw);
    }
}
