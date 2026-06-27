use std::process::ExitCode;

use formatelf::cli::{self, Parsed};
use formatelf::error::{self, Error};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    let raw: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if raw.len() <= 1 {
        eprint!("{}", cli::HELP);
        return ExitCode::FAILURE;
    }

    match cli::parse(raw) {
        Ok(Parsed::Help) => {
            eprint!("{}", cli::HELP);
            ExitCode::SUCCESS
        }
        Ok(Parsed::Version) => {
            println!("formatelf {VERSION}");
            ExitCode::SUCCESS
        }
        Ok(Parsed::Run(args)) => match run(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("formatelf: {e}");
                ExitCode::FAILURE
            }
        },
        Err(e) => {
            eprintln!("formatelf: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &cli::Args) -> error::Result<()> {
    if args.files.is_empty() {
        return Err(Error::Cli("no input files".into()));
    }

    let mutating = args.ops.iter().any(|o| !is_read_only(o));
    let mut mods = formatelf::ops::Modifiers {
        debug: args.debug,
        ..Default::default()
    };
    for op in &args.ops {
        match op {
            cli::Operation::ForceRpath => mods.force_rpath = true,
            cli::Operation::AllowedRpathPrefixes(s) => {
                mods.allowed_rpath_prefixes = s
                    .split(':')
                    .map(str::to_owned)
                    .filter(|p| !p.is_empty())
                    .collect();
            }
            _ => {}
        }
    }

    for file in &args.files {
        let fd = std::fs::File::open(file).map_err(|source| Error::Io {
            path: file.clone(),
            source,
        })?;
        // Borrow the input from the page cache; unchanged sections never copy.
        let data = unsafe { memmap2::Mmap::map(&fd) }.map_err(|source| Error::Io {
            path: file.clone(),
            source,
        })?;
        let mut image = formatelf::parser::parse(&data)?;
        let t = image.ehdr.e_type;
        if t != formatelf::ir::et::EXEC && t != formatelf::ir::et::DYN {
            return Err(Error::Unsupported("wrong ELF type".into()));
        }
        let mut report = formatelf::ops::Report::default();
        for op in &args.ops {
            formatelf::ops::apply(&mut image, op, &mods, &mut report)?;
        }
        for line in report.lines {
            println!("{line}");
        }
        if mutating {
            let dest = args.output.as_ref().unwrap_or(file);
            write_output(&mut image, &data, args, &fd, dest)?;
        }
    }
    Ok(())
}

/// Stream the patched image to a temp file beside `dest`, then atomically
/// rename it into place: the input mmap stays valid (the target is never
/// truncated under us) and a crash can never leave a half-written binary.
fn write_output(
    image: &mut formatelf::ir::ElfImage<'_>,
    data: &[u8],
    args: &cli::Args,
    src: &std::fs::File,
    dest: &std::path::Path,
) -> error::Result<()> {
    let io = |source| Error::Io {
        path: dest.to_path_buf(),
        source,
    };
    let dir = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(io)?;
    {
        let mut out = std::io::BufWriter::new(tmp.as_file_mut());
        formatelf::layout::finalize(
            image,
            data,
            args.page_size,
            args.debug,
            args.no_clobber_old_sections,
            &mut out,
        )?;
        std::io::Write::flush(&mut out).map_err(io)?;
    }
    // Carry over the source mode; the temp file is created 0600 otherwise.
    if let Ok(meta) = src.metadata() {
        let _ = tmp.as_file().set_permissions(meta.permissions());
    }
    tmp.persist(dest).map_err(|e| Error::Io {
        path: dest.to_path_buf(),
        source: e.error,
    })?;
    Ok(())
}

fn is_read_only(op: &cli::Operation) -> bool {
    use cli::Operation::{
        PrintExecstack, PrintInterpreter, PrintNeeded, PrintOsAbi, PrintRpath, PrintSoname,
    };
    matches!(
        op,
        PrintInterpreter | PrintOsAbi | PrintSoname | PrintRpath | PrintNeeded | PrintExecstack
    )
}
