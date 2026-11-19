//! `rhue test`, runs `*_test.rhue` (and `*.test.rhue`) files.
//!
//! A test file calls `test(name, fn)` (inside `describe(name, fn)` blocks if it
//! wants) and checks stuff with `expect(value)`. Each file gets its own engine
//! and each test is a function call. A test fails when it raises an error, from
//! a failed expectation or anything else.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use rhue::{Engine, Function, Table, Value};

const PRELUDE: &str = include_str!("test_prelude.rhue");

const MATCHERS: &str = "toBe: (any) -> (), toEqual: (any) -> (), toBeNil: () -> (),
    toBeTruthy: () -> (), toBeFalsy: () -> (), toBeCloseTo: (number, number?) -> (),
    toBeGreaterThan: (number) -> (), toBeLessThan: (number) -> (),
    toHaveLength: (number) -> (), toContain: (any) -> (), toThrow: (string?) -> ()";

/// What the prelude's frames are called in tracebacks (and they're left out of
/// failure reports).
const PRELUDE_NAME: &str = "<rhue test>";

/// An error the way a failure report shows it. The prelude's own frames (an
/// `expect` matcher's insides) get cut from its traceback, and the traceback's
/// dropped completely when all that's left is the test itself.
fn failure(e: &rhue::Error, color: bool) -> String {
    let text = e.render(color);
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| !l.contains(&format!("({PRELUDE_NAME}:")))
        .collect();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        if l.contains("Note: stack traceback:") {
            let frames = lines[i + 1..]
                .iter()
                .take_while(|f| f.trim_start().starts_with("in ") || f.contains("called from"))
                .count();
            if frames <= 1 {
                // drop the note, its frame, and the spacer line before it
                if out.last().is_some_and(|p| p.trim() == "|") {
                    out.pop();
                }
                i += 1 + frames;
                continue;
            }
        }
        out.push(l);
        i += 1;
    }
    out.join("\n")
}

const USAGE: &str = "usage: rhue test [--filter TEXT] [PATH...]";

/// Test files under `path` (or `path` itself if it's a file).
fn find(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_file() {
        out.push(path.to_path_buf());
        return;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if !name.starts_with('.') && name != "target" && name != "node_modules" {
                find(&p, out);
            }
        } else if name.ends_with("_test.rhue") || name.ends_with(".test.rhue") {
            out.push(p);
        }
    }
}

struct Paint(bool);

impl Paint {
    fn wrap(&self, code: &str, s: &str) -> String {
        if self.0 {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
    fn green(&self, s: &str) -> String {
        self.wrap("32", s)
    }
    fn red(&self, s: &str) -> String {
        self.wrap("31", s)
    }
    fn dim(&self, s: &str) -> String {
        self.wrap("2", s)
    }
}

fn ms(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1000.0)
}

fn indent(text: &str, by: &str) -> String {
    text.lines()
        .map(|l| {
            if l.is_empty() {
                String::new()
            } else {
                format!("{by}{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A fresh engine with `test`, `describe` and `expect`, loading modules next to
/// `file` or from the current folder (so `tests/x_test.rhue` can
/// `require("lib.x")`), plus the list tests get collected into.
fn engine_for(file: &Path) -> Result<(Engine, Table), rhue::Error> {
    let mut engine = Engine::new();
    let bases = [
        file.parent().map(Path::to_path_buf).unwrap_or_default(),
        PathBuf::from("."),
    ];
    engine.set_module_loader(move |name| {
        if !rhue::is_module_name(name) {
            return None;
        }
        bases.iter().find_map(|base| {
            rhue::module_paths(base, name)
                .iter()
                .find_map(|p| std::fs::read_to_string(p).ok())
        })
    });
    let expectation = format!("{{ {MATCHERS}, never: {{ {MATCHERS} }} }}");
    engine.declare("test", "(string, () -> ()) -> ()")?;
    engine.declare("describe", "(string, () -> ()) -> ()")?;
    engine.declare("expect", &format!("(any) -> {expectation}"))?;
    let prelude = engine.compile(PRELUDE_NAME, PRELUDE)?;
    match engine.run(&prelude)?.into_iter().next() {
        Some(Value::Table(tests)) => Ok((engine, tests)),
        _ => unreachable!("the prelude returns its list of tests"),
    }
}

#[derive(Default)]
struct Tally {
    passed: usize,
    failed: usize,
    files_failed: usize,
}

/// Runs one file's tests (the ones whose names contain `filter`).
fn run_file(path: &Path, filter: Option<&str>, paint: &Paint, color: bool, tally: &mut Tally) {
    println!("{}", path.display());
    let fail_file = |tally: &mut Tally, e: &rhue::Error| {
        println!("  {} could not run", paint.red("✗"));
        println!("{}", indent(&failure(e, color), "      "));
        tally.files_failed += 1;
    };
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            println!("  {} cannot read it: {e}", paint.red("✗"));
            tally.files_failed += 1;
            return;
        }
    };
    let (mut engine, tests) = match engine_for(path) {
        Ok(x) => x,
        Err(e) => return fail_file(tally, &e),
    };
    let name = path.display().to_string();
    if let Err(e) = engine.compile(&name, &src).and_then(|p| engine.run(&p)) {
        return fail_file(tally, &e);
    }
    let collected: Vec<(String, Function)> = {
        let mut cx = engine.context();
        (1..=cx.len(&tests))
            .filter_map(|i| {
                let entry: Table = cx.get(&tests, i as f64).ok()?;
                let name: String = cx.get(&entry, "name").ok()?;
                let f: Function = cx.get(&entry, "fn").ok()?;
                Some((name, f))
            })
            .collect()
    };
    let mut ran = 0;
    for (name, f) in collected {
        if filter.is_some_and(|text| !name.contains(text)) {
            continue;
        }
        ran += 1;
        let start = Instant::now();
        let result = engine.call::<Value>(&f, ());
        let took = paint.dim(&format!("({})", ms(start.elapsed())));
        match result {
            Ok(_) => {
                tally.passed += 1;
                println!("  {} {name} {took}", paint.green("✓"));
            }
            Err(e) => {
                tally.failed += 1;
                println!("  {} {name} {took}", paint.red("✗"));
                println!("{}", indent(&failure(&e, color), "      "));
            }
        }
    }
    if ran == 0 {
        println!("  {}", paint.dim("(no tests)"));
    }
}

pub fn main(args: &[String]) -> ExitCode {
    let mut filter = None;
    let mut paths = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--filter" => match it.next() {
                Some(f) => filter = Some(f.clone()),
                None => {
                    eprintln!("--filter needs some text\n{USAGE}");
                    return ExitCode::from(2);
                }
            },
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            s if s.starts_with('-') => {
                eprintln!("unknown option `{s}`\n{USAGE}");
                return ExitCode::from(2);
            }
            p => paths.push(PathBuf::from(p)),
        }
    }
    if paths.is_empty() {
        paths.push(PathBuf::from("."));
    }
    let mut files = Vec::new();
    for p in &paths {
        if !p.exists() {
            eprintln!("rhue test: {} doesn't exist", p.display());
            return ExitCode::from(2);
        }
        find(p, &mut files);
    }
    if files.is_empty() {
        eprintln!("rhue test: no test files (`*_test.rhue` or `*.test.rhue`) found");
        return ExitCode::FAILURE;
    }
    let color = std::io::stdout().is_terminal();
    let paint = Paint(color);
    let start = Instant::now();
    let mut tally = Tally::default();
    for file in &files {
        run_file(file, filter.as_deref(), &paint, color, &mut tally);
    }
    let mut summary = vec![paint.green(&format!("{} passed", tally.passed))];
    if tally.failed > 0 {
        summary.push(paint.red(&format!("{} failed", tally.failed)));
    }
    if tally.files_failed > 0 {
        let s = if tally.files_failed == 1 { "" } else { "s" };
        summary.push(paint.red(&format!("{} file{s} couldn't run", tally.files_failed)));
    }
    let s = if files.len() == 1 { "" } else { "s" };
    println!(
        "\n{} in {} file{s} ({})",
        summary.join(", "),
        files.len(),
        ms(start.elapsed())
    );
    if tally.failed > 0 || tally.files_failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
