//! Runs the scripts in `bench/` and reports how long each one takes.
//!
//! ```text
//! cargo run --release -p rhue --example bench -- [--runs N] [--save FILE] [--baseline FILE] [FILTER]
//! ```
//!
//! Every script starts with `-- expect: <results>`, and a run whose results
//! don't match fails, so an optimisation can't quietly change behaviour.
//! Each script gets compiled once and run `N` times (default 5) in a fresh
//! engine, and the report shows the fastest and the median run. `--save`
//! writes the medians to a file, `--baseline` compares against one of
//! those. Every engine has two natives registered from Rust (`add` and
//! `count`) for `host.rhue` to call.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rhue::Engine;

fn main() {
    let mut runs = 5usize;
    let mut save: Option<PathBuf> = None;
    let mut baseline: Option<PathBuf> = None;
    let mut filter: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--runs" => runs = args.next().and_then(|n| n.parse().ok()).expect("--runs N"),
            "--save" => save = Some(args.next().expect("--save FILE").into()),
            "--baseline" => baseline = Some(args.next().expect("--baseline FILE").into()),
            other => filter = Some(other.to_string()),
        }
    }

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench");
    let mut scripts: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("bench/ directory")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "rhue"))
        .filter(|p| {
            filter
                .as_ref()
                .is_none_or(|f| p.to_string_lossy().contains(f.as_str()))
        })
        .collect();
    scripts.sort();

    let base = baseline.map(|p| read_results(&p)).unwrap_or_default();
    let mut medians = BTreeMap::new();
    let mut failed = false;
    println!(
        "{:<10} {:>10} {:>10} {:>10}",
        "script", "min ms", "median ms", "vs base"
    );
    for path in &scripts {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let source = std::fs::read_to_string(path).expect("readable script");
        let expect = source
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("-- expect: "))
            .map(str::trim)
            .unwrap_or_else(|| panic!("{name}: first line must be `-- expect: ...`"))
            .to_string();
        let mut times = Vec::with_capacity(runs);
        for _ in 0..runs {
            let mut engine = Engine::new();
            // for `host.rhue`, natives registered through the typed API
            engine.register("add", |a: f64, b: f64| a + b);
            engine.register("count", |n: i64| n + 1);
            let proto = match engine.compile(&name, &source) {
                Ok(p) => p,
                Err(e) => panic!("{}", e.render(false)),
            };
            let start = Instant::now();
            let result = engine.run(&proto);
            times.push(start.elapsed());
            let got = match result {
                Ok(values) => values
                    .iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" "),
                Err(e) => format!("error: {}", e.render(false)),
            };
            if got != expect {
                eprintln!("{name}: expected `{expect}`, got `{got}`");
                failed = true;
                break;
            }
        }
        times.sort();
        let min = ms(times[0]);
        let median = ms(times[times.len() / 2]);
        let vs = base
            .get(&name)
            .map(|&b| format!("{:>+9.1}%", (median / b - 1.0) * 100.0))
            .unwrap_or_default();
        println!("{name:<10} {min:>10.1} {median:>10.1} {vs:>10}");
        medians.insert(name, median);
    }
    if let Some(path) = save {
        let text: String = medians
            .iter()
            .map(|(n, m)| format!("{n} {m:.2}\n"))
            .collect();
        std::fs::write(&path, text).expect("writable --save file");
    }
    if failed {
        std::process::exit(1);
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn read_results(path: &Path) -> BTreeMap<String, f64> {
    std::fs::read_to_string(path)
        .expect("readable --baseline file")
        .lines()
        .filter_map(|l| {
            let (name, ms) = l.split_once(' ')?;
            Some((name.to_string(), ms.trim().parse().ok()?))
        })
        .collect()
}
