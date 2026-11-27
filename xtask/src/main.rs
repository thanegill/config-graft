use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage: cargo xtask man <OUT_DIR>";

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let (Some(task), Some(out_dir), None) = (args.next(), args.next(), args.next()) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    if task != "man" {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let out_dir = PathBuf::from(out_dir);
    match generate_man_pages(&out_dir) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {}: {e}", out_dir.display());
            ExitCode::FAILURE
        }
    }
}

/// Writes config-graft(1) and one config-graft-<subcommand>(1) page per subcommand.
fn generate_man_pages(out_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(out_dir)?;
    clap_mangen::generate_to(config_graft::command(), out_dir)
}
