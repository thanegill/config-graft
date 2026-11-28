use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

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

/// A section 7 guide built from `docs/<stem>.md`: its NAME line and SEE ALSO.
struct Guide {
    stem: &'static str,
    description: &'static str,
    see_also: &'static [&'static str],
}

const GUIDES: &[Guide] = &[
    Guide {
        stem: "formats",
        description: "how config-graft reads and writes JSON, plist, YAML and TOML",
        see_also: &[
            "config-graft(1)",
            "config-graft-json(1)",
            "config-graft-yaml(1)",
            "config-graft-toml(1)",
            "config-graft-plist(1)",
            "config-graft-arrays(7)",
            "config-graft-directory(7)",
        ],
    },
    Guide {
        stem: "arrays",
        description: "how config-graft combines arrays, and --merge-key for lists of records",
        see_also: &[
            "config-graft(1)",
            "config-graft-json(1)",
            "config-graft-yaml(1)",
            "config-graft-toml(1)",
            "config-graft-plist(1)",
            "config-graft-formats(7)",
        ],
    },
    Guide {
        stem: "directory",
        description: "reconciling a whole directory tree",
        see_also: &[
            "config-graft(1)",
            "config-graft-directory(1)",
            "config-graft-formats(7)",
        ],
    },
];

/// The guides a subcommand's page points to.
fn guides_for(subcommand: &str) -> &'static [&'static str] {
    match subcommand {
        "directory" => &["config-graft-directory(7)"],
        _ => &["config-graft-formats(7)", "config-graft-arrays(7)"],
    }
}

/// Writes config-graft(1), one config-graft-<subcommand>(1) page per subcommand,
/// and the section 7 guides. The section 1 pages are rendered here rather than by
/// `clap_mangen::generate_to`, so each carries the package version and a SEE ALSO
/// section.
fn generate_man_pages(out_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(out_dir)?;
    let mut root = config_graft::command().disable_help_subcommand(true);
    root.build();
    // clap takes a version only as `&'static str`.
    let version: &'static str = root
        .get_version()
        .expect("config-graft sets a version")
        .to_owned()
        .leak();
    let subcommands: Vec<clap::Command> = root
        .get_subcommands()
        .filter(|sub| !sub.is_hide_set())
        .map(|sub| sub.clone().version(version))
        .collect();
    if let Some(nested) = subcommands.iter().find(|sub| sub.has_subcommands()) {
        return Err(std::io::Error::other(format!(
            "{} has subcommands, which get no man page",
            page_name(nested)
        )));
    }

    let mut root_see_also: Vec<String> = subcommands
        .iter()
        .map(|sub| format!("{}(1)", page_name(sub)))
        .collect();
    root_see_also.extend(
        GUIDES
            .iter()
            .map(|guide| format!("config-graft-{}(7)", guide.stem)),
    );
    write_section_1(&root, version, &root_see_also, out_dir)?;
    for sub in &subcommands {
        let mut see_also = vec![format!("{}(1)", root.get_name())];
        see_also.extend(
            guides_for(sub.get_name())
                .iter()
                .map(|page| page.to_string()),
        );
        write_section_1(sub, version, &see_also, out_dir)?;
    }

    for guide in GUIDES {
        write_section_7(guide, version, out_dir)?;
    }
    Ok(())
}

/// Converts `docs/<stem>.md` to config-graft-<stem>(7) with pandoc and the Lua
/// filter beside this crate, which turns the H1 into NAME, links to the other
/// guides into page references, other links into their text plus URL, and appends
/// SEE ALSO. A link to any other `.md` page fails the conversion.
fn write_section_7(guide: &Guide, version: &str, out_dir: &Path) -> std::io::Result<()> {
    let xtask_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let name = format!("config-graft-{}", guide.stem);
    let mut pandoc = Command::new("pandoc");
    pandoc
        .args([
            "--standalone",
            "--from",
            "markdown-smart",
            "--wrap=none",
            "--to",
            "man",
            "--lua-filter",
        ])
        .arg(xtask_dir.join("docs-to-man.lua"))
        .args(["-M", &format!("title={name}"), "-M", "section=7"])
        .args(["-M", &format!("name={name}")])
        .args(["-M", &format!("description={}", guide.description)])
        .args(["-M", &format!("footer=config-graft {version}")]);
    for page in guide.see_also {
        pandoc.args(["-M", &format!("see_also={page}")]);
    }
    for other in GUIDES {
        pandoc.args(["-M", &format!("guides={}", other.stem)]);
    }
    pandoc
        .arg(xtask_dir.join("../docs").join(format!("{}.md", guide.stem)))
        .arg("-o")
        .arg(out_dir.join(format!("{name}.7")));
    let status = pandoc.status().map_err(|e| {
        std::io::Error::new(e.kind(), format!("running pandoc (is it on PATH?): {e}"))
    })?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "pandoc failed on {name}: {status}"
        )));
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
