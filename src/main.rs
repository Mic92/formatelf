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
    let _ = args;
    todo!("wire parser -> ops -> layout -> constraints -> serialize")
}
