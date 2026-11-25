//! Runs the benchmark scripts in `bench/` under Rhue, Luau (through mlua,
//! interpreter only, optimization level 2) and Rhai (ports in
//! `bench/compare/rhai/`, where one exists), checks every result against
//! the script's `-- expect:` line, and reports the fastest of N runs.
//!
//! Each program is compiled once outside the timing; each timed run starts
//! from a fresh engine state where the engine allows (Rhue and Rhai), and
//! a fresh Lua state for Luau. Rhai's ports use integers where Rhue and
//! Luau use floats (Rhai's natural number type). Every engine gets the host
//! type from `host.rs` for `instances`.

mod host;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Scripts that call natives registered by Rhue's own bench runner.
const RHUE_ONLY: &[&str] = &["host", "natives"];

fn main() {
    let mut runs = 5usize;
    let mut filter: Option<String> = None;
    let mut write = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--runs" => runs = args.next().and_then(|n| n.parse().ok()).expect("--runs N"),
            "--write" => write = true,
            other => filter = Some(other.to_string()),
        }
    }
    let mut measured = Vec::new();
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bench = here.join("..");
    let mut scripts: Vec<PathBuf> = std::fs::read_dir(&bench)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "rhue"))
        .filter(|p| {
            filter
                .as_ref()
                .is_none_or(|f| p.to_string_lossy().contains(f.as_str()))
        })
        .collect();
    scripts.sort();

    println!(
        "{:<11} {:>9} {:>9} {:>9}   {:>10} {:>10}",
        "ms (min)", "Rhue", "Luau", "Rhai", "Luau/Rhue", "Rhai/Rhue"
    );
    let mut failed = false;
    for path in scripts {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        if RHUE_ONLY.contains(&name.as_str()) {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        let expect: Vec<String> = source
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("-- expect:"))
            .unwrap_or("")
            .split_whitespace()
            .map(String::from)
            .collect();

        let mut check = |engine: &str, got: Vec<String>| {
            if got != expect {
                eprintln!("{name} ({engine}): expected {expect:?}, got {got:?}");
                failed = true;
            }
        };

        let rhue = time(runs, || {
            let mut e = rhue::Engine::new();
            host::rhue(&mut e);
            let proto = e.compile(&name, &source).unwrap();
            let start = Instant::now();
            let out = e.run(&proto).unwrap();
            (start.elapsed(), out.iter().map(|v| v.to_string()).collect())
        });
        check("rhue", rhue.1);

        let luau = time(runs, || {
            let lua = mlua::Lua::new();
            host::luau(&lua).unwrap();
            lua.set_compiler(mlua::Compiler::new().set_optimization_level(2));
            let f = lua.load(&source).set_name(&name).into_function().unwrap();
            let start = Instant::now();
            let out: mlua::MultiValue = f.call(()).unwrap();
            let took = start.elapsed();
            let shown = out
                .iter()
                .map(|v| v.to_string().unwrap_or_default())
                .collect();
            (took, shown)
        });
        check("luau", luau.1);

        let rhai_src = here.join("rhai").join(format!("{name}.rhai"));
        let rhai = std::fs::read_to_string(&rhai_src).ok().map(|src| {
            let r = time(runs, || {
                let mut engine = rhai::Engine::new();
                host::rhai(&mut engine);
                engine.set_max_expr_depths(0, 0);
                let ast = engine.compile(&src).unwrap();
                let start = Instant::now();
                let out: rhai::Dynamic = engine.eval_ast(&ast).unwrap();
                let took = start.elapsed();
                let shown = match out.clone().try_cast::<rhai::Array>() {
                    Some(a) => a.iter().map(|v| v.to_string()).collect(),
                    None => vec![out.to_string()],
                };
                (took, shown)
            });
            check("rhai", r.1.clone());
            r.0
        });

        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        measured.push((name.clone(), ms(rhue.0), ms(luau.0), rhai.map(ms)));
        let ratio = |d: Duration| format!("{:.2}x", ms(d) / ms(rhue.0));
        println!(
            "{:<11} {:>9.1} {:>9.1} {:>9}   {:>10} {:>10}",
            name,
            ms(rhue.0),
            ms(luau.0),
            rhai.map_or("-".to_string(), |d| format!("{:.1}", ms(d))),
            ratio(luau.0),
            rhai.map_or("-".to_string(), ratio),
        );
    }
    if failed {
        std::process::exit(1);
    }
    if write {
        let path = bench.join("results.json");
        write_results(&path, &measured);
        println!(
            "wrote {}, run `cargo xtask gen` to update the README",
            path.display()
        );
    }
}

/// Puts new times in `bench/results.json`, keeping everything else in it (the
/// notes, what each script stresses). A script it doesn't know yet gets a row
/// with an empty `stresses` to fill in.
fn write_results(path: &Path, measured: &[(String, f64, f64, Option<f64>)]) {
    use serde_json::{Map, Value, json};
    let text = std::fs::read_to_string(path).unwrap_or_else(|_| "{}".into());
    let mut results: Map<String, Value> =
        serde_json::from_str(&text).expect("results.json is JSON");
    let mut rows: Vec<Value> = results
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for (name, rhue, luau, rhai) in measured {
        let row = match rows.iter_mut().find(|r| r["name"] == name.as_str()) {
            Some(r) => r,
            None => {
                rows.push(json!({ "name": name, "stresses": "" }));
                rows.last_mut().unwrap()
            }
        };
        row["rhue"] = rounded(*rhue);
        row["luau"] = rounded(*luau);
        row["rhai"] = rhai.map_or(Value::Null, rounded);
    }
    rows.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    results.remove("rows");
    // one row per line, the way it's been written by hand
    let mut out = String::from("{\n");
    for (k, v) in &results {
        out.push_str(&format!("  {}: {},\n", Value::from(k.as_str()), v));
    }
    out.push_str("  \"rows\": [\n");
    let shown: Vec<String> = rows
        .iter()
        .map(|r| {
            let fields: Vec<String> = r
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| format!("{}: {v}", Value::from(k.as_str())))
                .collect();
            format!("    {{ {} }}", fields.join(", "))
        })
        .collect();
    out.push_str(&shown.join(",\n"));
    out.push_str("\n  ]\n}\n");
    std::fs::write(path, out).expect("writing results.json");
}

/// Three-ish significant figures: whole from 100 ms up, one decimal under.
fn rounded(ms: f64) -> serde_json::Value {
    if ms >= 100.0 {
        serde_json::Value::from(ms.round() as i64)
    } else {
        serde_json::Value::from((ms * 10.0).round() / 10.0)
    }
}

/// The fastest of `runs` runs, and the last run's results.
fn time(runs: usize, mut run: impl FnMut() -> (Duration, Vec<String>)) -> (Duration, Vec<String>) {
    let mut best = Duration::MAX;
    let mut out = Vec::new();
    for _ in 0..runs {
        let (d, o) = run();
        best = best.min(d);
        out = o;
    }
    (best, out)
}
