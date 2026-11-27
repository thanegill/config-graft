use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap_mangen::roff::{bold, roman, Inline, Roff};
use clap_mangen::Man;

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
/// Each page is rendered here rather than by `clap_mangen::generate_to`, so every
/// one carries the package version and a SEE ALSO section.
fn generate_man_pages(out_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(out_dir)?;
    let mut root = config_graft::command().disable_help_subcommand(true);
    root.build();
    // clap takes a version only as `&'static str`.
    let version: &'static str = root.get_version().unwrap_or_default().to_owned().leak();
    let subcommands: Vec<clap::Command> = root
        .get_subcommands()
        .filter(|sub| !sub.is_hide_set())
        .map(|sub| sub.clone().version(version))
        .collect();

    let root_see_also: Vec<String> = subcommands
        .iter()
        .map(|sub| format!("{}(1)", page_name(sub)))
        .collect();
    write_section_1(&root, version, &root_see_also, out_dir)?;
    for sub in &subcommands {
        write_section_1(sub, version, &[format!("{}(1)", root.get_name())], out_dir)?;
    }
    Ok(())
}

/// A subcommand's man page name: `config-graft-json`, as clap sets it on build.
fn page_name(cmd: &clap::Command) -> &str {
    cmd.get_display_name().unwrap_or_else(|| cmd.get_name())
}

/// Renders one section 1 page: clap_mangen's sections, footed by the package
/// version and closed by a SEE ALSO section.
fn write_section_1(
    cmd: &clap::Command,
    version: &str,
    see_also: &[String],
    out_dir: &Path,
) -> std::io::Result<()> {
    let man = Man::new(cmd.clone());
    let mut rendered = Vec::new();
    man.render(&mut rendered)?;
    let rendered = String::from_utf8(rendered).expect("clap_mangen renders UTF-8");
    let mut page = String::new();
    for line in rendered.lines() {
        if line.starts_with(".TH ") {
            page.push_str(&title_line(cmd, version));
        } else {
            page.push_str(line);
        }
        page.push('\n');
    }
    page.push_str(&see_also_section(see_also));
    std::fs::write(out_dir.join(man.get_filename()), page)
}

/// The `.TH` line with an explicit empty date, so the package version lands in
/// the source (left footer) slot. clap_mangen drops an empty date argument, which
/// shifts its source string into the date slot.
fn title_line(cmd: &clap::Command, version: &str) -> String {
    format!(".TH {} 1 \"\" \"config-graft {version}\"", page_name(cmd))
}

/// A SEE ALSO section listing `pages`, each written `name(section)`, with
/// hyphenation and justification off so page names are neither split nor spread.
fn see_also_section(pages: &[String]) -> String {
    let mut inlines: Vec<Inline> = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        let (name, section) = page.split_at(page.find('(').expect("page reference has a section"));
        if i > 0 {
            inlines.push(roman(", "));
        }
        inlines.push(bold(name));
        inlines.push(roman(section));
    }
    let mut roff = Roff::new();
    roff.control("SH", ["SEE ALSO"])
        .control("nh", [])
        .control("ad", ["l"])
        .text(inlines);
    roff.to_roff()
}
