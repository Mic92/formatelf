use std::process::ExitCode;

use patchelf_rs::cli::{self, Parsed};
use patchelf_rs::error::{self, Error};

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
            println!("patchelf-rs {VERSION}");
            ExitCode::SUCCESS
        }
        Ok(Parsed::Run(args)) => match run(args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("patchelf: {e}");
                ExitCode::FAILURE
            }
        },
        Err(e) => {
            eprintln!("patchelf: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: cli::Args) -> error::Result<()> {
    if args.files.is_empty() {
        return Err(Error::Cli("no input files".into()));
    }

    let read_only = args.ops.iter().all(is_read_only);
    if !read_only {
        todo!("mutating ops: wire ops -> layout -> constraints -> serialize");
    }

    for file in &args.files {
        let data = std::fs::read(file).map_err(|source| Error::Io {
            path: file.clone(),
            source,
        })?;
        let mut image = patchelf_rs::parser::parse(&data)?;
        let t = image.ehdr.e_type;
        if t != patchelf_rs::ir::et::EXEC && t != patchelf_rs::ir::et::DYN {
            return Err(Error::Unsupported("wrong ELF type".into()));
        }
        let mut report = patchelf_rs::ops::Report::default();
        for op in &args.ops {
            patchelf_rs::ops::apply(&mut image, op, &mut report)?;
        }
        for line in report.lines {
            println!("{line}");
        }
    }
    Ok(())
}

fn is_read_only(op: &cli::Operation) -> bool {
    use cli::Operation::*;
    matches!(
        op,
        PrintInterpreter | PrintOsAbi | PrintSoname | PrintRpath | PrintNeeded | PrintExecstack
    )
}
