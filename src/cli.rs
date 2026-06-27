use std::fs;
use std::path::PathBuf;

use lexopt::prelude::*;

use crate::error::{Error, Result};

/// One requested mutation. Pure data: parsing never touches an ELF file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    SetInterpreter(String),
    PrintInterpreter,
    PrintOsAbi,
    SetOsAbi(String),
    PrintSoname,
    SetSoname(String),
    SetRpath(String),
    AddRpath(String),
    RemoveRpath,
    ShrinkRpath,
    AllowedRpathPrefixes(String),
    PrintRpath,
    ForceRpath,
    AddNeeded(String),
    RemoveNeeded(String),
    ReplaceNeeded { old: String, new: String },
    PrintNeeded,
    NoDefaultLib,
    ClearSymbolVersion(String),
    AddDebugTag,
    BuildResolutionCache,
    PrintExecstack,
    ClearExecstack,
    SetExecstack,
    RenameDynamicSymbols(PathBuf),
}

/// Global flags plus the ordered operation list and target files.
#[derive(Debug, Default, Clone)]
pub struct Args {
    pub ops: Vec<Operation>,
    pub files: Vec<PathBuf>,
    pub page_size: Option<u64>,
    pub no_clobber_old_sections: bool,
    pub output: Option<PathBuf>,
    pub debug: bool,
}

/// `@file` indirection: read the argument value verbatim from the named file.
fn resolve(arg: &str) -> Result<String> {
    if let Some(path) = arg.strip_prefix('@') {
        let bytes = fs::read(path).map_err(|source| Error::Io {
            path: PathBuf::from(path),
            source,
        })?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    } else {
        Ok(arg.to_string())
    }
}

fn val(p: &mut lexopt::Parser) -> Result<String> {
    let v = p
        .value()
        .map_err(|e| Error::Cli(format!("missing argument: {e}")))?;
    let s = v
        .into_string()
        .map_err(|_| Error::Cli("argument is not valid UTF-8".into()))?;
    resolve(&s)
}

/// A path-valued option. Unlike `val` it keeps the raw OS string, since
/// filesystem paths need not be valid UTF-8.
fn path_val(p: &mut lexopt::Parser) -> Result<PathBuf> {
    let v = p
        .value()
        .map_err(|e| Error::Cli(format!("missing argument: {e}")))?;
    Ok(PathBuf::from(v))
}

pub enum Parsed {
    Run(Args),
    Help,
    Version,
}

/// # Errors
/// Returns an error on an unknown flag or a missing or malformed argument.
pub fn parse<I>(raw: I) -> Result<Parsed>
where
    I: IntoIterator<Item = std::ffi::OsString>,
{
    let mut args = Args::default();
    let mut p = lexopt::Parser::from_iter(raw);

    while let Some(arg) = p
        .next()
        .map_err(|e| Error::Cli(format!("bad arguments: {e}")))?
    {
        match arg {
            Long("set-interpreter" | "interpreter") => {
                args.ops.push(Operation::SetInterpreter(val(&mut p)?));
            }
            Long("page-size") => {
                let s = val(&mut p)?;
                let n: u64 = s
                    .parse()
                    .map_err(|_| Error::Cli("invalid argument to --page-size".into()))?;
                if n == 0 {
                    return Err(Error::Cli("invalid argument to --page-size".into()));
                }
                args.page_size = Some(n);
            }
            Long("print-interpreter") => args.ops.push(Operation::PrintInterpreter),
            Long("print-os-abi") => args.ops.push(Operation::PrintOsAbi),
            Long("set-os-abi") => args.ops.push(Operation::SetOsAbi(val(&mut p)?)),
            Long("print-soname") => args.ops.push(Operation::PrintSoname),
            Long("set-soname") => args.ops.push(Operation::SetSoname(val(&mut p)?)),
            Long("remove-rpath") => args.ops.push(Operation::RemoveRpath),
            Long("shrink-rpath") => args.ops.push(Operation::ShrinkRpath),
            Long("allowed-rpath-prefixes") => {
                args.ops.push(Operation::AllowedRpathPrefixes(val(&mut p)?));
            }
            Long("set-rpath") => args.ops.push(Operation::SetRpath(val(&mut p)?)),
            Long("add-rpath") => args.ops.push(Operation::AddRpath(val(&mut p)?)),
            Long("print-rpath") => args.ops.push(Operation::PrintRpath),
            Long("force-rpath") => args.ops.push(Operation::ForceRpath),
            Long("add-needed") => args.ops.push(Operation::AddNeeded(val(&mut p)?)),
            Long("remove-needed") => args.ops.push(Operation::RemoveNeeded(val(&mut p)?)),
            Long("replace-needed") => {
                let old = val(&mut p)?;
                let new = val(&mut p)?;
                args.ops.push(Operation::ReplaceNeeded { old, new });
            }
            Long("print-needed") => args.ops.push(Operation::PrintNeeded),
            Long("no-default-lib") => args.ops.push(Operation::NoDefaultLib),
            Long("clear-symbol-version") => {
                args.ops.push(Operation::ClearSymbolVersion(val(&mut p)?));
            }
            Long("add-debug-tag") => args.ops.push(Operation::AddDebugTag),
            Long("build-resolution-cache") => args.ops.push(Operation::BuildResolutionCache),
            Long("print-execstack") => args.ops.push(Operation::PrintExecstack),
            Long("clear-execstack") => args.ops.push(Operation::ClearExecstack),
            Long("set-execstack") => args.ops.push(Operation::SetExecstack),
            Long("rename-dynamic-symbols") => args
                .ops
                .push(Operation::RenameDynamicSymbols(path_val(&mut p)?)),
            // We never reorder program or section headers (new entries are
            // appended in place), which is exactly what --no-sort requests, so
            // it is accepted as a no-op.
            Long("no-sort") => {}
            Long("no-clobber-old-sections") => args.no_clobber_old_sections = true,
            Long("output") => args.output = Some(path_val(&mut p)?),
            Long("debug") => args.debug = true,
            Long("help") | Short('h') => return Ok(Parsed::Help),
            Long("version") => return Ok(Parsed::Version),
            Value(v) => args.files.push(PathBuf::from(v)),
            other => return Err(Error::Cli(format!("unexpected argument: {other:?}"))),
        }
    }

    Ok(Parsed::Run(args))
}

pub const HELP: &str = "\
syntax: formatelf
  [--set-interpreter FILENAME]
  [--page-size SIZE]
  [--print-interpreter]
  [--print-os-abi]
  [--set-os-abi ABI]
  [--print-soname]
  [--set-soname SONAME]
  [--set-rpath RPATH]
  [--add-rpath RPATH]
  [--remove-rpath]
  [--shrink-rpath]
  [--allowed-rpath-prefixes PREFIXES]
  [--print-rpath]
  [--force-rpath]
  [--add-needed LIBRARY]
  [--remove-needed LIBRARY]
  [--replace-needed LIBRARY NEW_LIBRARY]
  [--print-needed]
  [--no-default-lib]
  [--no-sort]
  [--clear-symbol-version SYMBOL]
  [--add-debug-tag]
  [--build-resolution-cache]
  [--print-execstack]
  [--clear-execstack]
  [--set-execstack]
  [--rename-dynamic-symbols NAME_MAP_FILE]
  [--no-clobber-old-sections]
  [--output FILE]
  [--debug]
  [--version]
  FILENAME...
";

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<std::ffi::OsString> {
        std::iter::once(std::ffi::OsString::from("formatelf"))
            .chain(args.iter().map(std::ffi::OsString::from))
            .collect()
    }

    fn run(args: &[&str]) -> Args {
        match parse(os(args)).unwrap() {
            Parsed::Run(a) => a,
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn preserves_operation_order() {
        let a = run(&["--add-rpath", "/a", "--set-soname", "libx.so", "main"]);
        assert_eq!(
            a.ops,
            vec![
                Operation::AddRpath("/a".into()),
                Operation::SetSoname("libx.so".into()),
            ]
        );
        assert_eq!(a.files, vec![PathBuf::from("main")]);
    }

    #[test]
    fn replace_needed_takes_two_values() {
        let a = run(&["--replace-needed", "old.so", "new.so", "main"]);
        assert_eq!(
            a.ops,
            vec![Operation::ReplaceNeeded {
                old: "old.so".into(),
                new: "new.so".into()
            }]
        );
    }

    #[test]
    fn at_file_indirection() {
        let dir = std::env::temp_dir().join("formatelf_cli_test");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("rpath");
        std::fs::write(&f, "/from/file").unwrap();
        let a = run(&["--set-rpath", &format!("@{}", f.display()), "main"]);
        assert_eq!(a.ops, vec![Operation::SetRpath("/from/file".into())]);
    }

    #[test]
    fn page_size_rejects_zero() {
        assert!(parse(os(&["--page-size", "0", "main"])).is_err());
    }

    #[test]
    fn help_and_version() {
        assert!(matches!(parse(os(&["--help"])).unwrap(), Parsed::Help));
        assert!(matches!(
            parse(os(&["--version"])).unwrap(),
            Parsed::Version
        ));
    }
}
