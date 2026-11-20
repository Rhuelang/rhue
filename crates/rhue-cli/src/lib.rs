//! `rhue`, runs Rhue scripts and expressions. The binary only calls [`main`]
//! with the process arguments.
//!
//! ```text
//! rhue                     start a REPL (`:help` there for its commands)
//! rhue FILE                run a script, print what it returns
//! rhue -e SOURCE           evaluate SOURCE
//! rhue compile [--strict] FILE [-o OUT]
//!                          compile (and type-check) FILE to bytecode,
//!                          saved as OUT (default: FILE with `.rhuec`);
//!                          `rhue OUT` runs it without parsing
//! rhue test [--filter TEXT] [PATH...]
//!                          run `*_test.rhue` files (under PATH, default
//!                          `.`) and summarize; exits non-zero on failure
//! rhue fmt [--check] [--indent N] FILE...
//!                          format files in place (`-` formats stdin to
//!                          stdout); `--check` only reports which would
//!                          change; `--indent N` uses N spaces, not tabs
//! options:
//!   --check                type-check only (strict unless the file says
//!                          otherwise); don't run
//!   --strict               type-check everything strictly before running
//!                          (files can still opt out with `--!nocheck`)
//!   --dump                 print the bytecode before running
//!   --no-fold              disable constant folding
//! ```
//!
//! `require("a.b")` loads `a/b.rhue` (or `a/b/init.rhue`) from the
//! script's folder, or from the current folder for `-e` and the REPL.

use std::io::{self, IsTerminal};
use std::process::ExitCode;

mod pretty;
mod repl;
mod testing;

use rhue::{Chunk, CompileOptions, Engine, Error, ErrorKind, TypeMode, Value};

struct Args {
    dump: bool,
    fold: bool,
    check_only: bool,
    strict: bool,
    input: Input,
}

enum Input {
    Repl,
    File(String),
    Inline(String),
}

fn usage() -> &'static str {
    "usage: rhue [--check] [--strict] [--dump] [--no-fold] [FILE | -e SOURCE]\n       rhue compile [--strict] FILE [-o OUT]\n       rhue test [--filter TEXT] [PATH...]\n       rhue fmt [--check] [--indent N] FILE..."
}

fn parse_args(raw: &[String]) -> Result<Args, String> {
    let mut args = Args {
        dump: false,
        fold: true,
        check_only: false,
        strict: false,
        input: Input::Repl,
    };
    let mut it = raw.iter().cloned();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--dump" => args.dump = true,
            "--check" => args.check_only = true,
            "--strict" => args.strict = true,
            "--no-fold" => args.fold = false,
            "-e" => args.input = Input::Inline(it.next().ok_or("-e needs an argument")?),
            "-h" | "--help" => return Err(usage().into()),
            s if s.starts_with('-') => return Err(format!("unknown option `{s}`\n{}", usage())),
            file => args.input = Input::File(file.into()),
        }
    }
    Ok(args)
}

/// `rhue fmt ...`
fn fmt_main(args: &[String]) -> ExitCode {
    let mut check = false;
    let mut options = rhue_fmt::FormatOptions::default();
    let mut files = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--check" => check = true,
            "--indent" => match it.next().and_then(|n| n.parse::<usize>().ok()) {
                Some(n) => options.indent = " ".repeat(n),
                None => {
                    eprintln!("--indent needs a number of spaces");
                    return ExitCode::from(2);
                }
            },
            s if s.starts_with("--") => {
                eprintln!("unknown option `{s}`\nusage: rhue fmt [--check] [--indent N] FILE...");
                return ExitCode::from(2);
            }
            file => files.push(file.to_string()),
        }
    }
    if files.is_empty() {
        eprintln!("usage: rhue fmt [--check] [--indent N] FILE...");
        return ExitCode::from(2);
    }
    let color = io::stderr().is_terminal();
    let mut status = ExitCode::SUCCESS;
    for path in files {
        let src = if path == "-" {
            let mut s = String::new();
            if io::Read::read_to_string(&mut io::stdin(), &mut s).is_err() {
                eprintln!("rhue: cannot read stdin");
                return ExitCode::FAILURE;
            }
            s
        } else {
            match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("rhue: cannot read {path}: {e}");
                    status = ExitCode::FAILURE;
                    continue;
                }
            }
        };
        match rhue_fmt::format(&src, &options) {
            Ok(out) if path == "-" => print!("{out}"),
            Ok(out) if out == src => {}
            Ok(_) if check => {
                println!("{path} would be reformatted");
                status = ExitCode::FAILURE;
            }
            Ok(out) => {
                if let Err(e) = std::fs::write(&path, out) {
                    eprintln!("rhue: cannot write {path}: {e}");
                    status = ExitCode::FAILURE;
                }
            }
            Err(rhue_fmt::FormatError::Syntax(diags)) => {
                for d in diags {
                    eprint!("{}", d.render(&path, &src, color));
                }
                status = ExitCode::FAILURE;
            }
            Err(rhue_fmt::FormatError::Internal(m)) => {
                eprintln!("rhue: internal formatter error in {path}: {m}");
                status = ExitCode::FAILURE;
            }
        }
    }
    status
}

/// `rhue compile`: source to a `.rhuec` chunk.
fn compile_main(args: &[String]) -> ExitCode {
    let usage = "usage: rhue compile [--strict] FILE [-o OUT]";
    let (mut input, mut output, mut strict) = (None, None, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--strict" => strict = true,
            "-o" => match it.next() {
                Some(o) => output = Some(o.clone()),
                None => {
                    eprintln!("-o needs a file\n{usage}");
                    return ExitCode::from(2);
                }
            },
            s if s.starts_with('-') => {
                eprintln!("unknown option `{s}`\n{usage}");
                return ExitCode::from(2);
            }
            file if input.is_none() => input = Some(file.to_string()),
            _ => {
                eprintln!("{usage}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(input) = input else {
        eprintln!("{usage}");
        return ExitCode::from(2);
    };
    let output = output.unwrap_or_else(|| {
        std::path::Path::new(&input)
            .with_extension("rhuec")
            .to_string_lossy()
            .into_owned()
    });
    let src = match std::fs::read_to_string(&input) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("rhue: cannot read {input}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut engine = Engine::new();
    if strict {
        engine.set_default_type_mode(TypeMode::Strict);
    }
    match engine.compile_to_bytes(&input, &src) {
        Ok(bytes) => match std::fs::write(&output, bytes) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("rhue: cannot write {output}: {e}");
                ExitCode::FAILURE
            }
        },
        Err(e) => {
            eprint!("{}", e.render(io::stderr().is_terminal()));
            ExitCode::FAILURE
        }
    }
}

/// Runs `rhue` with `args` (without the program name) and returns its exit code.
pub fn main(args: impl IntoIterator<Item = String>) -> ExitCode {
    let raw: Vec<String> = args.into_iter().collect();
    match raw.first().map(String::as_str) {
        Some("fmt") => return fmt_main(&raw[1..]),
        Some("compile") => return compile_main(&raw[1..]),
        Some("test") => return testing::main(&raw[1..]),
        _ => {}
    }
    let args = match parse_args(&raw) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };
    let mut engine = Engine::with_options(CompileOptions::new().fold_constants(args.fold));
    if args.strict {
        engine.set_default_type_mode(TypeMode::Strict);
    }
    let base = match &args.input {
        Input::File(path) => std::path::Path::new(path)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default(),
        _ => std::path::PathBuf::from("."),
    };
    engine.set_module_loader(move |name| {
        if !rhue::is_module_name(name) {
            return None;
        }
        rhue::module_paths(&base, name)
            .iter()
            .find_map(|p| std::fs::read_to_string(p).ok())
    });
    let color = io::stderr().is_terminal();

    if args.check_only {
        let (name, src) = match &args.input {
            Input::File(path) => match std::fs::read_to_string(path) {
                Ok(src) => (path.clone(), src),
                Err(e) => {
                    eprintln!("rhue: cannot read {path}: {e}");
                    return ExitCode::FAILURE;
                }
            },
            Input::Inline(src) => ("<command line>".to_string(), src.clone()),
            Input::Repl => {
                eprintln!("rhue: --check needs a FILE or -e SOURCE");
                return ExitCode::from(2);
            }
        };
        return match engine.check(&name, &src) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprint!("{}", e.render(color));
                ExitCode::FAILURE
            }
        };
    }

    match &args.input {
        Input::Repl => repl::run(&mut engine, args.dump, color),
        Input::File(path) => match std::fs::read(path) {
            Ok(bytes) if rhue::is_chunk(&bytes) => {
                let result = engine.load(&bytes).and_then(|proto| {
                    if args.dump {
                        eprint!("{}", proto.disassemble());
                    }
                    engine.run(&proto)
                });
                report(result, color)
            }
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(src) => report(run(&mut engine, path, &src, args.dump, false), color),
                Err(_) => {
                    eprintln!("rhue: {path} is neither UTF-8 source nor a compiled chunk");
                    ExitCode::FAILURE
                }
            },
            Err(e) => {
                eprintln!("rhue: cannot read {path}: {e}");
                ExitCode::FAILURE
            }
        },
        Input::Inline(src) => report(
            run(&mut engine, "<command line>", src, args.dump, true),
            color,
        ),
    }
}

/// Compiles and runs `src`. In expression mode (REPL, `-e`) a lone expression
/// works as if it had `return` in front, same as the Luau REPL.
pub(crate) fn run(
    engine: &mut Engine,
    name: &str,
    src: &str,
    dump: bool,
    expr_mode: bool,
) -> Result<Vec<Value>, Error> {
    let proto: Chunk = if expr_mode {
        match engine.compile_expr(name, src) {
            Ok(p) => p,
            // report whichever reading the user most likely meant
            Err(expr_err) => match engine.compile(name, src) {
                Ok(p) => p,
                // an expression followed by more (`x = )`), they meant a
                // statement, so show that error
                Err(chunk_err)
                    if expr_err
                        .diagnostic()
                        .message
                        .starts_with("expected end of input after expression") =>
                {
                    return Err(chunk_err);
                }
                // it parsed as a chunk, so its error's the real one
                Err(chunk_err) if chunk_err.kind != ErrorKind::Syntax => return Err(chunk_err),
                Err(chunk_err) if starts_with_statement(src) => return Err(chunk_err),
                Err(_) => return Err(expr_err),
            },
        }
    } else {
        engine.compile(name, src)?
    };
    if dump {
        eprint!("{}", proto.disassemble());
    }
    engine.run(&proto)
}

fn starts_with_statement(src: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "return", "local", "function", "if", "while", "for", "repeat", "do", "break", "continue",
        "export", "type",
    ];
    let word: String = src
        .trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    KEYWORDS.contains(&word.as_str())
}

fn print_values(values: &[Value]) {
    if values.is_empty() {
        return;
    }
    let line: Vec<String> = values.iter().map(Value::to_string).collect();
    println!("{}", line.join("\t"));
}

fn report(result: Result<Vec<Value>, Error>, color: bool) -> ExitCode {
    match result {
        Ok(values) => {
            print_values(&values);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprint!("{}", e.render(color));
            ExitCode::FAILURE
        }
    }
}
