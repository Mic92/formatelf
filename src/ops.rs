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
        eprintln!("patchelf: applying {op:?}");
    }
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
        Operation::SetRpath(path) => modify_rpath(image, path, mods.force_rpath)?,
        Operation::AddRpath(path) => add_rpath(image, path, mods.force_rpath)?,
        Operation::ShrinkRpath => shrink_rpath(image, mods)?,
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
        Operation::BuildResolutionCache => build_resolution_cache(image)?,
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

/// Append `path` to the current rpath (colon-joined), then set it.
fn add_rpath(image: &mut ElfImage, path: &str, force: bool) -> Result<()> {
    let cur = rpath(image)?;
    let combined = if cur.is_empty() {
        path.to_string()
    } else {
        format!("{cur}:{path}")
    };
    modify_rpath(image, &combined, force)
}

/// Read an ELF file's e_machine, or None if it isn't a readable ELF. Reads only
/// the leading header bytes so probing large shared libraries stays cheap.
fn elf_machine(path: &std::path::Path) -> Option<u16> {
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

/// Drop rpath directories that contain none of the needed libraries (matching
/// the binary's machine type), and any rejected by --allowed-rpath-prefixes.
/// Non-absolute entries such as $ORIGIN are always kept.
fn shrink_rpath(image: &mut ElfImage, mods: &Modifiers) -> Result<()> {
    let cur = rpath(image)?;
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
            if elf_machine(&std::path::Path::new(dir).join(lib)) == Some(machine) {
                found[j] = true;
                dir_useful = true;
            }
        }
        if dir_useful {
            kept.push(dir);
        }
    }

    let new = kept.join(":");
    modify_rpath(image, &new, mods.force_rpath)
}

/// Set DT_RUNPATH (or DT_RPATH when `force`). Reuses the existing string slot
/// when the new value fits, otherwise appends to .dynstr; adds the dynamic
/// entry when absent. Growth is resolved later by the layout engine.
fn modify_rpath(image: &mut ElfImage, new: &str, force: bool) -> Result<()> {
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

/// Append a NUL-terminated string to a string table, returning its offset.
/// Bake resolved library paths into a NixOS ld-cache note: for each searched
/// DT_NEEDED entry, record where it resolves against the run path so the
/// loader can skip the search. Mirrors patchelf's buildResolutionCache.
fn build_resolution_cache(image: &mut ElfImage) -> Result<()> {
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

    let mut cache: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
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
        let unresolvable =
            dir.contains('$') || std::path::Path::new(dir).join("glibc-hwcaps").exists();
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

/// Append a NixOS ld-cache SHT_NOTE section plus a PT_NOTE that points at it;
/// the layout engine assigns its address and the covering PT_LOAD.
fn add_note_section(image: &mut ElfImage, desc: &[u8]) -> Result<()> {
    const NT_NIXOS_LD_CACHE: u32 = 0x63a8_6cb6;
    const SHT_NOTE: u32 = 7;
    const SHF_ALLOC: u64 = 2;
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

    let shstr = image.ehdr.shstrndx as usize;
    if image.section_data.get(shstr).is_none() {
        return Err(Error::Missing("no section header string table".into()));
    }
    let name_off = image.section_data[shstr].len() as u32;
    image.section_data[shstr].extend_from_slice(b".note.nixos.ldcache\0");

    image.shdrs.push(ir::Shdr {
        name: name_off,
        sh_type: SHT_NOTE,
        flags: SHF_ALLOC,
        addr: 0,
        offset: 0,
        size: 0, // 0 vs the data length marks the section for placement
        link: 0,
        info: 0,
        addralign: 4,
        entsize: 0,
    });
    image.section_data.push(note);

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

/// Parse a symbol rename map: whitespace-separated `old new` pairs, one per
/// line, blank lines ignored (the patchelf NAME_MAP_FILE format).
fn parse_symbol_map(path: &std::path::Path) -> Result<std::collections::BTreeMap<String, String>> {
    let text = std::fs::read_to_string(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut map = std::collections::BTreeMap::new();
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

    let big = image.enc.endian == ir::Endian::Big;
    let sym_size = if image.enc.class == ir::Class::Elf64 {
        24
    } else {
        16
    };
    let count = image.section_data[dynsym].len() / sym_size;
    if image.section_data[versym].len() < count * 2 {
        return Err(Error::Parse("versym smaller than dynsym".into()));
    }
    let rd_u32 = |b: &[u8]| {
        let a = [b[0], b[1], b[2], b[3]];
        if big {
            u32::from_be_bytes(a)
        } else {
            u32::from_le_bytes(a)
        }
    };
    let global = if big {
        1u16.to_be_bytes()
    } else {
        1u16.to_le_bytes()
    };

    // st_name (u32) is the first field of both Elf32_Sym and Elf64_Sym.
    for i in 0..count {
        let st_name = rd_u32(&image.section_data[dynsym][i * sym_size..]);
        if ir::cstr(&image.section_data[dynstr], st_name) == Some(sym) {
            image.section_data[versym][i * 2..i * 2 + 2].copy_from_slice(&global);
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
    let abi = match name.trim().to_ascii_lowercase().as_str() {
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
        _ => return Err(Error::Cli("unrecognized OS ABI".into())),
    };
    image.ehdr.ident[7] = abi; // EI_OSABI; written verbatim by the codec
    image.ehdr.os_abi = abi;
    Ok(())
}

/// Toggle PF_X on PT_GNU_STACK. When the segment is absent, reuse a spare
/// PT_NULL slot if there is one, else append a new entry (the layout engine
/// relocates the program header table to make room). PT_GNU_STACK carries no
/// file content, so a fresh entry needs no offset/address assignment.
fn modify_execstack(image: &mut ElfImage, set: bool) -> Result<()> {
    let flip = |flags: u32| {
        if set {
            flags | ir::pf::X
        } else {
            flags & !ir::pf::X
        }
    };
    if let Some(p) = image
        .phdrs
        .iter_mut()
        .find(|p| p.p_type == ir::pt::GNU_STACK)
    {
        p.flags = flip(p.flags);
        return Ok(());
    }
    let new = ir::Phdr {
        p_type: ir::pt::GNU_STACK,
        flags: flip(ir::pf::R | ir::pf::W),
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
