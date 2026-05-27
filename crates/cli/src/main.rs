//! nes-to-sms — NES ROM → SMS project translator.
//!
//! Usage:
//!     nes-to-sms <rom.nes> <profile.toml> <out_dir> [--runtime <runtime_dir>]

use std::path::PathBuf;
use std::process::ExitCode;

mod pipeline;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let parsed = match parse_args(&args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}\n");
            print_usage();
            return ExitCode::from(2);
        }
    };

    match pipeline::run(&parsed) {
        Ok(report) => {
            println!("{}", report);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    eprintln!("usage: nes-to-sms <rom.nes> <profile.toml> <out_dir>");
    eprintln!("                  [--runtime <runtime_dir>]");
    eprintln!("                  [--validate] [--validate-vectors N]");
    eprintln!("                  [--debug-unresolved-stubs]");
}

#[derive(Debug, Clone)]
pub struct Args {
    pub rom: PathBuf,
    pub profile: PathBuf,
    pub out: PathBuf,
    pub runtime: Option<PathBuf>,
    /// Run the differential validation harness over every lifted routine.
    /// Off by default because it adds noticeable time on a large ROM;
    /// on by `--validate`.
    pub validate: bool,
    /// Number of random initial-state vectors per routine. Defaults to 32.
    pub validate_vectors: usize,
    /// Let unresolved translated labels return through a permissive debug stub.
    /// Off by default: unresolved labels trap at runtime via rt_unresolved_jsr.
    pub debug_unresolved_stubs: bool,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut positional: Vec<&str> = Vec::new();
    let mut runtime: Option<PathBuf> = None;
    let mut validate = false;
    let mut validate_vectors: usize = 32;
    let mut debug_unresolved_stubs = false;
    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--runtime" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| "--runtime needs a path".to_string())?;
                runtime = Some(PathBuf::from(value));
            }
            "--validate" => {
                validate = true;
            }
            "--debug-unresolved-stubs" => {
                debug_unresolved_stubs = true;
            }
            "--validate-vectors" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| "--validate-vectors needs an integer".to_string())?;
                validate_vectors = value
                    .parse()
                    .map_err(|_| format!("--validate-vectors: not an integer: {value}"))?;
            }
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            other => positional.push(other),
        }
        i += 1;
    }
    if positional.len() != 3 {
        return Err(format!(
            "expected 3 positional arguments, got {}",
            positional.len()
        ));
    }
    Ok(Args {
        rom: PathBuf::from(positional[0]),
        profile: PathBuf::from(positional[1]),
        out: PathBuf::from(positional[2]),
        runtime,
        validate,
        validate_vectors,
        debug_unresolved_stubs,
    })
}
