use std::path::{Path, PathBuf};
use std::process::Command as Process;

use clap::{Parser, Subcommand};
use forgust_assay::{Certainty, RepoMap};

/// Map a repository into blocks: what can be built, run or deployed apart, the links
/// between them, and the people who know them.
#[derive(Parser)]
#[command(name = "assay", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the map of a repository: JSON (schema 1), or a summary to read
    Map {
        /// The repository's folder
        #[arg(default_value = ".")]
        path: PathBuf,
        /// A summary to read rather than JSON
        #[arg(long, conflicts_with = "pretty")]
        text: bool,
        /// Indented JSON
        #[arg(long)]
        pretty: bool,
        /// Leave the people out (the git history is not read)
        #[arg(long)]
        no_people: bool,
        /// How many days of history tell who knows a block (default: assay.toml's, else 365)
        #[arg(long)]
        days: Option<u32>,
    },
    /// Write the report a forge reads from a CI job: the map of the clone's commit, and what
    /// shows it (Forgust's Smart View runs this)
    Report {
        /// The repository's folder, a git clone
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Where to write it
        #[arg(long, short, default_value = forgust_assay::report::FILE)]
        out: PathBuf,
        /// The repository's name, which the block of its top folder takes (default: the folder's)
        #[arg(long)]
        name: Option<String>,
    },
    /// Check the rules of assay.toml (`[[rules]]`): print the links that break one, and fail
    /// if any does (a CI job's step)
    Check {
        /// The repository's folder
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Write the map of a repository as a page to look at: one HTML file, which opens offline
    View {
        /// The repository's folder
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Where to write it (default: `<repository>.assay.html`, here)
        #[arg(long, short)]
        out: Option<PathBuf>,
        /// Open it in the browser once written
        #[arg(long)]
        open: bool,
        /// Leave the people out (the git history is not read)
        #[arg(long)]
        no_people: bool,
        /// How many days of history tell who knows a block (default: assay.toml's, else 365)
        #[arg(long)]
        days: Option<u32>,
    },
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Map { path, text, pretty, no_people, days } => {
            let map = forgust_assay::map(&path, &forgust_assay::Options { people: !no_people, days, name: None })?;
            if text {
                print!("{}", summary(&map));
            } else if pretty {
                println!("{}", serde_json::to_string_pretty(&map)?);
            } else {
                println!("{}", serde_json::to_string(&map)?);
            }
        }
        Command::Report { path, out, name } => {
            let report = forgust_assay::report::report(&path, &forgust_assay::Options { name, ..Default::default() })?;
            std::fs::write(&out, serde_json::to_vec(&report)?)?;
            println!("{}", out.display());
        }
        Command::Check { path } => {
            let map = forgust_assay::map(&path, &forgust_assay::Options { people: false, days: None, name: None })?;
            let rules = forgust_assay::Config::load(&path)?.rules.len();
            for w in &map.warnings {
                eprintln!("warning: {w}");
            }
            if map.violations.is_empty() {
                println!("{} kept", plural(rules, "rule"));
                return Ok(());
            }
            print!("{}", violations(&map));
            std::process::exit(1);
        }
        Command::View { path, out, open, no_people, days } => {
            let map = forgust_assay::map(&path, &forgust_assay::Options { people: !no_people, days, name: None })?;
            let name =
                path.canonicalize()?.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "repository".to_string());
            let out = out.unwrap_or_else(|| PathBuf::from(format!("{name}.assay.html")));
            std::fs::write(&out, forgust_assay::page(&map, &name))?;
            println!("{}", out.display());
            if open {
                open_in_browser(&out);
            }
        }
    }
    Ok(())
}

/// Opens a file with the system's browser; under WSL, Windows' one, by the file's Windows path.
fn open_in_browser(path: &Path) {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let wsl = std::fs::read_to_string("/proc/version").is_ok_and(|v| v.to_lowercase().contains("microsoft"));
    let started = if cfg!(target_os = "macos") {
        Process::new("open").arg(&path).spawn()
    } else if cfg!(windows) {
        Process::new("cmd").args(["/C", "start", ""]).arg(&path).spawn()
    } else if wsl {
        let windows_path =
            Process::new("wslpath").arg("-w").arg(&path).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        match windows_path {
            Ok(p) if !p.is_empty() => Process::new("explorer.exe").arg(p).spawn(),
            _ => Process::new("xdg-open").arg(&path).spawn(),
        }
    } else {
        Process::new("xdg-open").arg(&path).spawn()
    };
    if started.is_err() {
        eprintln!("no browser could be started: open {} yourself", path.display());
    }
}

/// The map as a few lines per block, then its links: to check what the detection found.
fn summary(map: &RepoMap) -> String {
    let mut out = String::new();
    let width = map.blocks.iter().map(|b| b.id.len()).max().unwrap_or(0);
    out.push_str(&format!("{} blocks\n", map.blocks.len()));
    for b in &map.blocks {
        let kind = match b.role {
            Some(r) => format!("{}/{}", json_name(&b.kind), json_name(&r)),
            None => json_name(&b.kind),
        };
        let place = if b.roots.is_empty() { b.image.clone().map(|i| format!("image {i}")).unwrap_or_default() } else { b.roots.join(", ") };
        out.push_str(&format!("  {:width$}  {kind:18}  {place}\n", b.id));
        let mut facts = vec![format!("{} files", b.files)];
        if let Some(l) = b.languages.first() {
            facts.push(format!("{} {}%", l.name, l.files * 100 / b.files.max(1)));
        }
        if let Some(p) = &b.parent {
            facts.push(format!("in {p}"));
        }
        if !b.jobs.is_empty() {
            facts.push(format!("jobs: {}", b.jobs.join(", ")));
        }
        if !b.owners.is_empty() {
            facts.push(format!("owners: {}", b.owners.join(", ")));
        }
        if !b.people.is_empty() {
            let names: Vec<&str> = b.people.iter().map(|p| p.name.as_str()).collect();
            facts.push(format!("people: {}", names.join(", ")));
        }
        if b.bus_factor == Some(1) {
            facts.push("one person knows it".to_string());
        }
        out.push_str(&format!("  {:width$}  {}\n", "", facts.join(" · ")));
    }
    out.push_str(&format!("\n{} links (~ a guess)\n", map.links.len()));
    for l in &map.links {
        let arrow = if l.certainty == Certainty::Inferred { "~>" } else { "->" };
        out.push_str(&format!("  {:width$} {arrow} {:width$}  {:10}  {}\n", l.from, l.to, json_name(&l.kind), l.via));
    }
    out.push_str(&format!("\n{} files in no block", map.unassigned.files));
    if !map.unassigned.sample.is_empty() {
        out.push_str(&format!(
            ": {}{}",
            map.unassigned.sample.join(", "),
            if map.unassigned.files > map.unassigned.sample.len() { ", …" } else { "" }
        ));
    }
    out.push('\n');
    if !map.violations.is_empty() {
        out.push('\n');
        out.push_str(&violations(map));
    }
    if !map.warnings.is_empty() {
        out.push_str(&format!("\n{} warnings\n", map.warnings.len()));
        for w in &map.warnings {
            out.push_str(&format!("  {w}\n"));
        }
    }
    out
}

/// The links that break a rule, one a line, under how many.
fn violations(map: &RepoMap) -> String {
    let mut out = format!("{} breaking a rule\n", plural(map.violations.len(), "link"));
    for v in &map.violations {
        let guess = if v.certainty == Certainty::Inferred { " (a guess)" } else { "" };
        let why = v.why.as_deref().map(|w| format!(": {w}")).unwrap_or_default();
        out.push_str(&format!("  {} {} {}{guess}, in {}: rule {}{why}\n", v.from, json_name(&v.kind), v.to, v.via, v.rule));
    }
    out
}

fn plural(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

/// An enum's name as the JSON gives it.
fn json_name(v: &impl serde::Serialize) -> String {
    serde_json::to_value(v).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}
