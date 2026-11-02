//! Deterministic Penroam sync protocol fixture generator.
//!
//! ```text
//! penroam-sync-fixtures <dir>           write fixtures into <dir>
//! penroam-sync-fixtures --check <dir>   exit 1 if <dir> differs from generated output
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!("usage: penroam-sync-fixtures <dir> | --check <dir>");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag, dir] if flag == "--check" => {
            match penroam_sync_testkit::fixtures::check(&PathBuf::from(dir)) {
                Ok(diffs) if diffs.is_empty() => {
                    println!("fixtures in {dir} are up to date");
                    ExitCode::SUCCESS
                }
                Ok(diffs) => {
                    eprintln!("{} fixture difference(s) in {dir}:", diffs.len());
                    for d in &diffs {
                        eprintln!("  {d}");
                    }
                    eprintln!("regenerate with: cargo run -p penroam-sync-testkit --bin penroam-sync-fixtures -- {dir}");
                    ExitCode::FAILURE
                }
                Err(e) => {
                    eprintln!("error reading {dir}: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        [dir] if !dir.starts_with("--") => {
            match penroam_sync_testkit::fixtures::write(&PathBuf::from(dir)) {
                Ok(()) => {
                    println!("wrote fixtures to {dir}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error writing {dir}: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => usage(),
    }
}
