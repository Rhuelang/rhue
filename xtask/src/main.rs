//! Repo chores, run with `cargo xtask`.
//!
//! `gen` writes the files that come from other files, so there's one place to
//! edit each thing:
//!
//! - `CHANGELOG.md` from `changelog/` (`index.json`, the `.md` per entry, and
//!   `unreleased.md` for what's merged but not out yet)
//! - the benchmark table in `README.md` from `bench/results.json`, between
//!   the `bench-table` markers
//!
//! `gen --check` changes nothing and fails if either is out of date, CI runs it.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::Value;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let check = args.iter().any(|a| a == "--check");
    match args.first().map(String::as_str) {
        Some("gen") => gen_all(check),
        _ => {
            eprintln!("usage: cargo xtask gen [--check]");
            ExitCode::FAILURE
        }
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn gen_all(check: bool) -> ExitCode {
    let root = root();
    let mut stale = Vec::new();
    let changelog = root.join("CHANGELOG.md");
    let readme = root.join("README.md");
    let outputs = [
        (changelog, changelog_md(&root)),
        (readme.clone(), readme_with_table(&root, &readme)),
    ];
    for (path, want) in outputs {
        let want = match want {
            Ok(w) => w,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::FAILURE;
            }
        };
        let have = std::fs::read_to_string(&path).unwrap_or_default();
        if have.replace("\r\n", "\n") == want {
            continue;
        }
        let name = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string();
        if check {
            stale.push(name);
        } else {
            std::fs::write(&path, want).expect("writing a generated file");
            println!("wrote {name}");
        }
    }
    if stale.is_empty() {
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "out of date: {}. run `cargo xtask gen` and commit the result",
            stale.join(", ")
        );
        ExitCode::FAILURE
    }
}

// ─ Changelog ─────────────────────────────────────────────────────────────────

const CHANGELOG_TOP: &str = "# Changelog

Everything that changed in Rhue. It got built in milestones before the first release, so 1.0.0 is all of them. [`docs/DESIGN.md`](docs/DESIGN.md) has the reasoning behind each one, and the README has the numbers.

This file is generated from [`changelog/`](changelog/) by `cargo xtask gen`, edit those instead.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow [Semantic Versioning](https://semver.org/). Only the `rhue` crate's API is meant to be stable, the other crates are its insides.
";

/// `CHANGELOG.md`: a section per release, newest first. Milestones come after
/// the release they shipped in (1.0.0 for all of them so far), each under its
/// own heading, with the release's own notes under "Release".
fn changelog_md(root: &Path) -> Result<String, String> {
    let dir = root.join("changelog");
    let index: Value = read_json(&dir.join("index.json"))?;
    let entries = index["entries"]
        .as_array()
        .ok_or("changelog/index.json has no entries")?;
    let mut out = String::from(CHANGELOG_TOP);
    // what's merged but not released yet, if anything
    if let Ok(next) = std::fs::read_to_string(dir.join("unreleased.md")) {
        let next = next.replace("\r\n", "\n");
        if !next.trim().is_empty() {
            out.push_str(&format!("\n## [Unreleased]\n\n{}\n", next.trim()));
        }
    }
    let mut i = 0;
    while i < entries.len() {
        let release = &entries[i];
        let tag = text(release, "tag")?;
        if milestone_number(tag).is_some() {
            return Err(format!(
                "milestone {tag} comes before any release in changelog/index.json"
            ));
        }
        let mut milestones = Vec::new();
        i += 1;
        while let Some(m) = entries
            .get(i)
            .filter(|e| e["tag"].as_str().and_then(milestone_number).is_some())
        {
            milestones.push(m);
            i += 1;
        }
        out.push_str(&format!("\n## [{tag}] - {}\n\n", text(release, "date")?));
        let body = entry_body(&dir, release)?;
        if milestones.is_empty() {
            out.push_str(&body);
            continue;
        }
        out.push_str("### Release\n\n");
        out.push_str(&body);
        for m in milestones {
            let n = milestone_number(text(m, "tag")?).expect("filtered above");
            out.push_str(&format!("\n### {} (milestone {n})\n\n", text(m, "title")?));
            out.push_str(&entry_body(&dir, m)?);
        }
    }
    Ok(out)
}

fn milestone_number(tag: &str) -> Option<u32> {
    tag.strip_prefix('M')?.parse().ok()
}

fn entry_body(dir: &Path, entry: &Value) -> Result<String, String> {
    let file = text(entry, "file")?;
    let body = std::fs::read_to_string(dir.join(file))
        .map_err(|e| format!("changelog/{file}: {e}"))?
        .replace("\r\n", "\n");
    Ok(format!("{}\n", body.trim()))
}

// ─ Benchmark table ───────────────────────────────────────────────────────────

const TABLE_START: &str =
    "<!-- bench-table:start (generated from bench/results.json by `cargo xtask gen`) -->";
const TABLE_END: &str = "<!-- bench-table:end -->";

fn readme_with_table(root: &Path, readme: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(readme)
        .map_err(|e| format!("README.md: {e}"))?
        .replace("\r\n", "\n");
    let (Some(start), Some(end)) = (text.find(TABLE_START), text.find(TABLE_END)) else {
        return Err("README.md is missing the bench-table markers".into());
    };
    let results = read_json(&root.join("bench/results.json"))?;
    let table = bench_table(&results)?;
    Ok(format!(
        "{}{TABLE_START}\n{table}{}",
        &text[..start],
        &text[end..]
    ))
}

fn bench_table(results: &Value) -> Result<String, String> {
    let rows = results["rows"]
        .as_array()
        .ok_or("bench/results.json has no rows")?;
    let mut out = String::from(
        "| Script | Rhue ms | Luau ms | Rhai ms | Luau vs Rhue | Rhai vs Rhue |\n| --- | ---: | ---: | ---: | ---: | ---: |\n",
    );
    for row in rows {
        let name = text(row, "name")?;
        let rhue = row["rhue"]
            .as_f64()
            .ok_or(format!("{name} has no Rhue time"))?;
        let luau = row["luau"].as_f64();
        let rhai = row["rhai"].as_f64();
        let ms = |v: Option<f64>| v.map_or("–".to_string(), shown_ms);
        let luau_vs = luau.map_or("–".to_string(), |l| format!("{} faster", times(rhue / l)));
        let rhai_vs = rhai.map_or("–".to_string(), |r| format!("{} slower", times(r / rhue)));
        out.push_str(&format!(
            "| `{name}` | {} | {} | {} | {luau_vs} | {rhai_vs} |\n",
            shown_ms(rhue),
            ms(luau),
            ms(rhai)
        ));
    }
    Ok(out)
}

/// A time in ms the way the table shows it: whole from 100 up, one decimal
/// under that.
fn shown_ms(ms: f64) -> String {
    if ms >= 100.0 {
        format!("{ms:.0}")
    } else {
        format!("{ms:.1}")
    }
}

/// A ratio like `2.2×`, or `15×` from 10 up.
fn times(r: f64) -> String {
    if r >= 10.0 {
        format!("{r:.0}×")
    } else {
        format!("{r:.1}×")
    }
}

// ─ Helpers ───────────────────────────────────────────────────────────────────

fn read_json(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v[key]
        .as_str()
        .ok_or(format!("an entry is missing `{key}`: {v}"))
}
