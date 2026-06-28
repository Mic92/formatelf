//! Resolve and wire up shared-library dependencies of ELF files, the way
//! nixpkgs' auto-patchelf hook does, but driven entirely from Rust: discover
//! ELF files, look their `DT_NEEDED` entries up in a set of library
//! directories, and set the interpreter and RUNPATH accordingly.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::cli::Operation;
use crate::error::{Error, Result};
use crate::ir::{Class, ElfImage, Endian, et, sht};
use crate::ops::{Modifiers, needed};
use crate::{parser, patch, rpath};

/// Target identity a candidate library must match: machine plus ELF class.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Arch {
    machine: u16,
    elf64: bool,
}

impl Arch {
    fn of(image: &ElfImage<'_>) -> Self {
        Arch {
            machine: image.ehdr.machine,
            elf64: image.enc.class == Class::Elf64,
        }
    }
}

const ELFOSABI_SYSV: u8 = 0;
const ELFOSABI_FREEBSD: u8 = 9;
const ELFOSABI_OPENBSD: u8 = 12;
/// `.note.dlopen` note type and owner from the ELF dlopen-metadata standard.
const NT_FDO_DLOPEN: u32 = 0x407c_0c0a;

/// The base ABI (`ELFOSABI_SYSV`, 0) is treated as compatible with everything,
/// matching `auto-patchelf`.
fn osabi_compatible(wanted: u8, got: u8) -> bool {
    wanted == got || wanted == ELFOSABI_SYSV || got == ELFOSABI_SYSV
}

#[derive(Default)]
struct Cache {
    /// (soname, arch) -> directories holding a match, with that match's OS ABI.
    by_soname: HashMap<(String, Arch), Vec<(PathBuf, u8)>>,
    visited: HashSet<PathBuf>,
}

fn is_shared_object(path: &Path) -> bool {
    // A plain `.so` or any versioned `.so.N`; sonames are always lowercase.
    let unversioned = path.extension().is_some_and(|e| e == "so");
    let versioned = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.contains(".so."));
    unversioned || versioned
}

/// objects from `separateDebugInfo` keep the section headers but turn `.text`
/// into NOBITS; they must not be treated as real libraries or patch targets.
fn is_separate_debug(image: &ElfImage<'_>) -> bool {
    image
        .find_section(".text")
        .is_some_and(|i| image.shdrs[i].sh_type == sht::NOBITS)
}

/// A parsed shared object, ready to be folded into the cache. Parsing happens
/// on worker threads, so this carries owned data only.
struct ParsedLib {
    name: String,
    arch: Arch,
    osabi: u8,
    dir: PathBuf,
    /// Non-`$ORIGIN` RUNPATH directories to follow next, with the recursion
    /// flag inherited from the library that named them.
    rpath_dirs: Vec<(PathBuf, bool)>,
}

/// Parse a single shared object for indexing. Returns `None` for anything that
/// is not a usable library (unreadable, non-ELF, or a separate-debug object).
fn parse_lib(path: &Path, recursive: bool) -> Option<ParsedLib> {
    let data = read_if_elf(path)?;
    let image = parser::parse(&data).ok()?;
    if is_separate_debug(&image) {
        return None;
    }
    let name = path.file_name()?.to_str()?.to_owned();
    let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let rpath_dirs = rpath::read(&image)
        .ok()
        .into_iter()
        .flat_map(|rp| {
            rp.split(':')
                .filter(|e| !e.is_empty() && !e.contains("$ORIGIN"))
                .map(|e| (PathBuf::from(e), recursive))
                .collect::<Vec<_>>()
        })
        .collect();
    Some(ParsedLib {
        name,
        arch: Arch::of(&image),
        osabi: image.ehdr.os_abi,
        dir,
        rpath_dirs,
    })
}

impl Cache {
    /// Index every shared object reachable from `roots` (each a path paired
    /// with its recursion flag). Libraries are parsed in parallel, while the
    /// cheap directory walk, RUNPATH following, and map merge stay on this
    /// thread. Non-`$ORIGIN` RUNPATH entries are followed, mirroring the loader.
    fn populate(&mut self, roots: &[(PathBuf, bool)], workers: usize) {
        // Symlinks resolve to the real store path, not one a later GC could
        // invalidate; cache the result per directory to avoid repeat syscalls.
        let mut dir_canon: HashMap<PathBuf, PathBuf> = HashMap::new();
        let mut canon = |dir: &Path| -> PathBuf {
            dir_canon
                .entry(dir.to_path_buf())
                .or_insert_with(|| std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()))
                .clone()
        };

        let mut frontier: Vec<(PathBuf, bool)> = roots.to_vec();
        while !frontier.is_empty() {
            let mut to_parse: Vec<(PathBuf, bool)> = Vec::new();
            let mut next: Vec<(PathBuf, bool)> = Vec::new();
            for (path, recursive) in std::mem::take(&mut frontier) {
                if !self.visited.insert(path.clone()) {
                    continue;
                }
                if path.is_file() {
                    to_parse.push((path, recursive));
                    continue;
                }
                let Ok(entries) = std::fs::read_dir(&path) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let child = entry.path();
                    if child.is_dir() {
                        if recursive {
                            next.push((child, recursive));
                        }
                    } else if is_shared_object(&child) {
                        to_parse.push((child, recursive));
                    }
                }
            }

            let libs = par_map(&to_parse, workers, |(p, rec)| parse_lib(p, *rec));
            for lib in libs {
                let dir = canon(&lib.dir);
                self.by_soname
                    .entry((lib.name, lib.arch))
                    .or_default()
                    .push((dir, lib.osabi));
                next.extend(lib.rpath_dirs);
            }
            frontier = next;
        }
    }

    fn find(&self, soname: &str, arch: Arch, osabi: u8) -> Option<&Path> {
        self.by_soname
            .get(&(soname.to_owned(), arch))?
            .iter()
            .find(|(_, libabi)| osabi_compatible(osabi, *libabi))
            .map(|(dir, _)| dir.as_path())
    }
}

/// Resolved configuration for an auto-formatelf run.
pub struct Config {
    pub paths: Vec<PathBuf>,
    pub libs: Vec<PathBuf>,
    pub runtime_deps: Vec<PathBuf>,
    pub append_rpaths: Vec<PathBuf>,
    pub ignore_missing: Vec<String>,
    pub recursive: bool,
    pub keep_libc: bool,
    pub add_existing: bool,
    pub interpreter: PathBuf,
    pub libc_lib: Option<PathBuf>,
    pub page_size: Option<u64>,
    /// Worker count. 0 means auto-detect (`NIX_BUILD_CORES` or all cores).
    pub jobs: usize,
}

/// Parse auto-formatelf's command line. The interpreter and libc default to
/// the cc-wrapper's `nix-support` metadata (via `$NIX_BINTOOLS`), matching the
/// nixpkgs hook, and can be overridden with `--interpreter`/`--libc`.
///
/// # Errors
/// Returns an error on an unknown flag or when the interpreter cannot be
/// determined.
pub fn parse_args<I>(raw: I) -> Result<Config>
where
    I: IntoIterator<Item = std::ffi::OsString>,
{
    use lexopt::prelude::{Long, Short};

    let mut cfg = Config {
        paths: Vec::new(),
        libs: Vec::new(),
        runtime_deps: Vec::new(),
        append_rpaths: Vec::new(),
        ignore_missing: Vec::new(),
        recursive: true,
        keep_libc: false,
        add_existing: true,
        interpreter: PathBuf::new(),
        libc_lib: None,
        page_size: None,
        jobs: 0,
    };
    let mut interpreter: Option<PathBuf> = None;
    let mut jobs: Option<usize> = None;
    let mut p = lexopt::Parser::from_iter(raw);
    let cli = |e: lexopt::Error| Error::Cli(format!("auto-formatelf: {e}"));

    while let Some(arg) = p.next().map_err(cli)? {
        // These options take zero or more values (argparse nargs="*"); lexopt's
        // values() errors when none follow, so treat that as an empty list.
        let multi = |p: &mut lexopt::Parser| -> Vec<std::ffi::OsString> {
            p.values().map(Iterator::collect).unwrap_or_default()
        };
        let paths = |p: &mut lexopt::Parser| multi(p).into_iter().map(PathBuf::from);
        match arg {
            Long("paths") => cfg.paths.extend(paths(&mut p)),
            Long("libs") => cfg.libs.extend(paths(&mut p)),
            Long("runtime-dependencies") => cfg.runtime_deps.extend(paths(&mut p)),
            Long("append-rpaths") => cfg.append_rpaths.extend(paths(&mut p)),
            Long("ignore-missing") => cfg
                .ignore_missing
                .extend(multi(&mut p).iter().map(|v| v.to_string_lossy().into_owned())),
            Long("no-recurse") => cfg.recursive = false,
            Long("keep-libc") => cfg.keep_libc = true,
            Long("ignore-existing") => cfg.add_existing = false,
            Long("interpreter") => interpreter = Some(p.value().map_err(cli)?.into()),
            Long("libc") => cfg.libc_lib = Some(p.value().map_err(cli)?.into()),
            Short('j') | Long("jobs") => {
                let s = p.value().map_err(cli)?;
                jobs = Some(
                    s.to_string_lossy()
                        .parse()
                        .map_err(|_| Error::Cli("auto-formatelf: invalid --jobs".into()))?,
                );
            }
            Long("page-size") => {
                let s = p.value().map_err(cli)?;
                cfg.page_size = Some(
                    s.to_string_lossy()
                        .parse()
                        .map_err(|_| Error::Cli("auto-formatelf: invalid --page-size".into()))?,
                );
            }
            // Trailing patchelf flags; formatelf needs none of them, so drop.
            Long("extra-args") => while p.value().is_ok() {},
            _ => return Err(Error::Cli(format!("auto-formatelf: unexpected {arg:?}"))),
        }
    }

    cfg.interpreter = match interpreter {
        Some(p) => p,
        None => default_interpreter()?,
    };
    if cfg.libc_lib.is_none() {
        cfg.libc_lib = default_libc();
    }
    // Absent an explicit -j, honor Nix's job budget; 0 (or unset) means auto.
    cfg.jobs = jobs.unwrap_or_else(|| {
        std::env::var("NIX_BUILD_CORES")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0)
    });
    Ok(cfg)
}

fn nix_support() -> Option<PathBuf> {
    std::env::var_os("NIX_BINTOOLS").map(|b| Path::new(&b).join("nix-support"))
}

fn default_interpreter() -> Result<PathBuf> {
    let file = nix_support()
        .map(|s| s.join("dynamic-linker"))
        .ok_or_else(|| {
            Error::Cli("auto-formatelf: no --interpreter and no $NIX_BINTOOLS".into())
        })?;
    let text = read(&file)?;
    Ok(PathBuf::from(String::from_utf8_lossy(&text).trim()))
}

fn default_libc() -> Option<PathBuf> {
    let file = nix_support()?.join("orig-libc");
    let text = std::fs::read(&file).ok()?;
    Some(Path::new(String::from_utf8_lossy(&text).trim()).join("lib"))
}

/// The interpreter identity every patch target is matched against.
struct Target {
    arch: Arch,
    osabi: u8,
    interpreter: String,
}

/// Patch every ELF file under `cfg.paths`, resolving dependencies against
/// `cfg.libs`.
///
/// # Errors
/// Returns an error listing the dependencies that could not be satisfied
/// (after applying the `ignore_missing` globs), or if a file cannot be patched.
pub fn run(cfg: &Config) -> Result<()> {
    if cfg.paths.is_empty() {
        return Err(Error::Cli("auto-formatelf: no paths to patch".into()));
    }

    let interp_data = read(&cfg.interpreter)?;
    let interp = parser::parse(&interp_data)?;
    let target = Target {
        arch: Arch::of(&interp),
        osabi: interp.ehdr.os_abi,
        interpreter: cfg
            .interpreter
            .to_str()
            .ok_or_else(|| Error::Cli("interpreter path is not UTF-8".into()))?
            .to_owned(),
    };

    let workers = resolve_workers(cfg.jobs);
    let mut cache = Cache::default();
    let mut roots: Vec<(PathBuf, bool)> = Vec::new();
    if cfg.add_existing {
        roots.extend(cfg.paths.iter().map(|p| (p.clone(), cfg.recursive)));
    }
    roots.extend(cfg.libs.iter().map(|l| (l.clone(), false)));
    cache.populate(&roots, workers);

    let mods = Modifiers::default();
    let files = collect_files(&cfg.paths, cfg.recursive);
    let missing = patch_all(&files, &target, &cache, cfg, &mods, workers)?;

    report_missing(&missing, &cfg.ignore_missing)
}

/// Resolve the worker count: explicit `--jobs`, else the available
/// parallelism. Zero means "all cores". Never less than one.
fn resolve_workers(jobs: usize) -> usize {
    if jobs == 0 {
        std::thread::available_parallelism().map_or(1, usize::from)
    } else {
        jobs
    }
    .max(1)
}

/// Apply `f` to every item across up to `workers` threads pulling from a shared
/// cursor, so a few heavy items do not stall the rest. Results are collected in
/// arbitrary order; `None` results are dropped.
fn par_map<T, R, F>(items: &[T], workers: usize, f: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> Option<R> + Sync,
{
    let workers = workers.min(items.len());
    if workers <= 1 {
        return items.iter().filter_map(&f).collect();
    }
    let cursor = std::sync::atomic::AtomicUsize::new(0);
    let mut out = Vec::new();
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut local = Vec::new();
                    loop {
                        let i = cursor.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(item) = items.get(i) else { break };
                        local.extend(f(item));
                    }
                    local
                })
            })
            .collect();
        for h in handles {
            out.extend(h.join().expect("worker panicked"));
        }
    });
    out
}

/// Patch every file, distributing them across workers that pull from a shared
/// cursor so a few large objects do not stall the others. Returns the merged
/// list of unresolved dependencies, or the first patch error encountered.
fn patch_all(
    files: &[PathBuf],
    target: &Target,
    cache: &Cache,
    cfg: &Config,
    mods: &Modifiers,
    workers: usize,
) -> Result<Vec<(PathBuf, String)>> {
    let workers = workers.min(files.len());
    if workers <= 1 {
        let mut missing = Vec::new();
        for file in files {
            patch_one(file, target, cache, cfg, mods, &mut missing)?;
        }
        return Ok(missing);
    }

    let cursor = std::sync::atomic::AtomicUsize::new(0);
    let mut results: Vec<Result<Vec<(PathBuf, String)>>> = Vec::new();
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut missing = Vec::new();
                    loop {
                        let i = cursor.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(file) = files.get(i) else { break };
                        patch_one(file, target, cache, cfg, mods, &mut missing)?;
                    }
                    Ok(missing)
                })
            })
            .collect();
        results.extend(handles.into_iter().map(|h| h.join().expect("worker panicked")));
    });

    let mut missing = Vec::new();
    for r in results {
        missing.extend(r?);
    }
    Ok(missing)
}

fn patch_one(
    file: &Path,
    target: &Target,
    cache: &Cache,
    cfg: &Config,
    mods: &Modifiers,
    missing: &mut Vec<(PathBuf, String)>,
) -> Result<()> {
    let Some(data) = read_if_elf(file) else {
        return Ok(());
    };
    let Ok(image) = parser::parse(&data) else {
        return Ok(());
    };

    let dynamic_exe = image.find_section(".interp").is_some();
    let static_exe = image.ehdr.e_type == et::EXEC && !dynamic_exe;
    if static_exe || image.phdrs.is_empty() || is_separate_debug(&image) {
        return Ok(());
    }
    let arch = Arch::of(&image);
    let osabi = image.ehdr.os_abi;
    if arch != target.arch || !osabi_compatible(target.osabi, osabi) {
        return Ok(());
    }

    let mut ops = Vec::new();
    let mut rpaths: Vec<PathBuf> = Vec::new();
    if dynamic_exe {
        ops.push(Operation::SetInterpreter(target.interpreter.clone()));
        rpaths.extend(cfg.runtime_deps.iter().cloned());
    }

    // BSD ships ld.so separately from libc, so libc must stay in the rpath.
    let keep_libc = cfg.keep_libc || matches!(osabi, ELFOSABI_FREEBSD | ELFOSABI_OPENBSD);

    // DT_NEEDED is one soname each; a .note.dlopen entry lists alternatives.
    let mut deps: Vec<Vec<String>> = needed(&image)
        .unwrap_or_default()
        .into_iter()
        .map(|n| vec![n])
        .collect();
    deps.extend(dlopen_deps(&image));

    for alternatives in &deps {
        let mut found = false;
        for cand in alternatives {
            let path = Path::new(cand);
            let in_libc = cfg
                .libc_lib
                .as_ref()
                .is_some_and(|l| l.join(cand).is_file());
            if (path.is_absolute() && path.is_file()) || (in_libc && !keep_libc) {
                found = true;
            } else if let Some(dir) = cache.find(cand, arch, osabi) {
                rpaths.push(dir.to_path_buf());
                found = true;
            } else if in_libc {
                found = true;
            }
            if found {
                break;
            }
        }
        if !found {
            let name = match alternatives.as_slice() {
                [one] => one.clone(),
                many => format!("any({})", many.join(", ")),
            };
            missing.push((file.to_path_buf(), name));
        }
    }

    rpaths.extend(cfg.append_rpaths.iter().cloned());
    if let Some(joined) = join_rpaths(&rpaths) {
        ops.push(Operation::SetRpath(joined));
    }
    if !ops.is_empty() {
        patch::patch_data(file, &data, &ops, mods, cfg.page_size)?;
    }
    Ok(())
}

/// Deduplicate while preserving order, then colon-join. Returns `None` when no
/// directory was collected (so the caller leaves the existing RUNPATH alone).
fn join_rpaths(rpaths: &[PathBuf]) -> Option<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for p in rpaths {
        let s = p.to_string_lossy().into_owned();
        if seen.insert(s.clone()) {
            out.push(s);
        }
    }
    (!out.is_empty()).then(|| out.join(":"))
}

fn report_missing(missing: &[(PathBuf, String)], ignore: &[String]) -> Result<()> {
    let mut failed = false;
    for (file, dep) in missing {
        if ignore.iter().any(|pat| glob_match(pat, dep)) {
            eprintln!(
                "auto-formatelf: ignoring missing {dep} wanted by {}",
                file.display()
            );
        } else {
            eprintln!(
                "auto-formatelf: {dep} not found, wanted by {}",
                file.display()
            );
            failed = true;
        }
    }
    if failed {
        return Err(Error::Missing(
            "auto-formatelf could not satisfy all dependencies; add them to --libs \
             or pass --ignore-missing"
                .into(),
        ));
    }
    Ok(())
}

/// Recursively list regular files (not symlinks) under each path. A path that
/// is itself a file is returned as-is.
fn collect_files(paths: &[PathBuf], recursive: bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut dirs = Vec::new();
    for p in paths {
        match std::fs::symlink_metadata(p) {
            Ok(m) if m.file_type().is_symlink() => {}
            Ok(m) if m.is_dir() => dirs.push(p.clone()),
            Ok(_) => out.push(p.clone()),
            Err(_) => {}
        }
    }
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            // file_type() is served from the directory entry (getdents d_type)
            // without an extra stat on the common filesystems.
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                if recursive {
                    dirs.push(entry.path());
                }
            } else {
                out.push(entry.path());
            }
        }
    }
    out
}

/// Minimal shell-style glob (`*` any run, `?` one char) for `--ignore-missing`
/// soname patterns like `libfoo.so.*`.
fn glob_match(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, usize::MAX, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            mark = ti;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Libraries a binary may `dlopen`, declared in a `.note.dlopen` note as a
/// FreeDesktop-standard JSON array. Each entry yields one list of soname
/// alternatives. Returns empty for the common case of no such note.
fn dlopen_deps(image: &ElfImage<'_>) -> Vec<Vec<String>> {
    let Some(idx) = image.find_section(".note.dlopen") else {
        return Vec::new();
    };
    if image.shdrs[idx].sh_type != sht::NOTE {
        return Vec::new();
    }
    let data = &image.section_data[idx];
    let big = image.enc.endian == Endian::Big;
    let rd = |o: usize| -> Option<u32> {
        let b: [u8; 4] = data.get(o..o + 4)?.try_into().ok()?;
        Some(if big {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        })
    };

    let align = |n: usize| (n + 3) & !3;
    let mut out = Vec::new();
    let mut o = 0;
    while let (Some(namesz), Some(descsz), Some(ntype)) = (rd(o), rd(o + 4), rd(o + 8)) {
        o += 12;
        let name = data.get(o..o + namesz as usize);
        o = align(o + namesz as usize);
        let desc = data.get(o..o + descsz as usize);
        o = align(o + descsz as usize);
        match (name, desc) {
            (Some(name), Some(desc)) if ntype == NT_FDO_DLOPEN && name.starts_with(b"FDO") => {
                if let Ok(json) = std::str::from_utf8(desc) {
                    out.extend(parse_dlopen_json(json.trim_end_matches('\0')));
                }
            }
            _ => {}
        }
    }
    out
}

/// Extract the `soname` string arrays from the dlopen note's JSON. The schema
/// is a flat array of objects, so scanning for each `"soname"` key and reading
/// the following `[...]` of quoted strings is enough; sonames never contain
/// quotes or commas.
fn parse_dlopen_json(json: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut rest = json;
    while let Some(key) = rest.find("\"soname\"") {
        rest = &rest[key + "\"soname\"".len()..];
        let Some(open) = rest.find('[') else { break };
        let Some(close) = rest[open..].find(']') else {
            break;
        };
        let sonames: Vec<String> = rest[open + 1..open + close]
            .split(',')
            .filter_map(|t| {
                let t = t.trim();
                t.strip_prefix('"')?.strip_suffix('"').map(str::to_owned)
            })
            .collect();
        if !sonames.is_empty() {
            out.push(sonames);
        }
        rest = &rest[open + close..];
    }
    out
}

fn read(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Read a file only when it starts with the ELF magic, so the many non-ELF
/// resources in a typical package are dismissed after a 4-byte read instead
/// of being slurped whole just to fail parsing.
fn read_if_elf(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read;

    let mut file = std::fs::File::open(path).ok()?;
    let mut magic = [0u8; 4];
    if file.read_exact(&mut magic).is_err() || &magic != b"\x7fELF" {
        return None;
    }
    let mut data = Vec::from(magic);
    file.read_to_end(&mut data).ok()?;
    Some(data)
}

#[cfg(test)]
mod tests {
    use super::parse_dlopen_json;

    #[test]
    fn dlopen_json_groups_alternatives() {
        let json = r#"[{"soname":["libfoo.so.1"]},{"soname":["libbar.so","libbar.so.2"]}]"#;
        assert_eq!(
            parse_dlopen_json(json),
            vec![
                vec!["libfoo.so.1".to_string()],
                vec!["libbar.so".to_string(), "libbar.so.2".to_string()],
            ]
        );
    }

    #[test]
    fn dlopen_json_tolerates_whitespace_and_other_keys() {
        let json = r#"[ { "feature": "x", "soname" : [ "libz.so.1" ] } ]"#;
        assert_eq!(parse_dlopen_json(json), vec![vec!["libz.so.1".to_string()]]);
    }

    #[test]
    fn dlopen_json_empty_when_absent() {
        assert!(parse_dlopen_json("[]").is_empty());
    }
}
