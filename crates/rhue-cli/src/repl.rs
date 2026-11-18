//! The interactive prompt: `rhue` with no file.
//!
//! Lines run as they're entered; a statement that isn't finished yet (an
//! open `function`, `if` or `{`, a long string) continues on the next line,
//! edited and recalled from history as one entry. A lone expression prints its value, tables spelled
//! out. Globals persist between entries, and so does a `local` that starts
//! an entry (it's kept as a global: `local x = 1`, `local function f()`);
//! other locals last for one entry. History is kept in `~/.rhue_history`.

use std::io::{self, BufRead, IsTerminal, Write};
use std::process::ExitCode;

use rhue::{Engine, ErrorKind, Value};
use rustyline::error::ReadlineError;
use rustyline::validate::{ValidationContext, ValidationResult, Validator};
use rustyline::{Completer, Helper, Highlighter, Hinter};

use crate::pretty;

const HELP: &str = "\
Enter statements or expressions; an expression's value is printed.
Unfinished input (an open `function`, `if`, `{`...) continues on the next line.
A `local` that starts an entry is kept for later entries (as a global).
  :type EXPR    the type the checker gives EXPR
  :load FILE    run a file here (its globals stay)
  :help         this
  :quit         leave (or Ctrl-D)";

/// Whether `src` is a start that more lines could finish: it fails to
/// parse only because it ends too soon.
pub fn is_incomplete(engine: &mut Engine, src: &str) -> bool {
    if src.trim().is_empty() || src.trim_start().starts_with(':') {
        return false;
    }
    match engine.compile("stdin", src) {
        Err(e) if e.kind == ErrorKind::Syntax => {
            let m = &e.diagnostic().message;
            m.contains("found end of input")
                || m.starts_with("unterminated long string")
                || m.starts_with("unterminated block comment")
        }
        _ => false,
    }
}

/// An entry starting with a plain `local` declaration, rewritten to declare
/// globals so that later entries see them: `local x, y = 1, 2` and
/// `local function f()` lose their `local`. Declarations with annotations
/// or attributes are left alone.
pub fn promote_locals(entry: &str) -> Option<String> {
    let start = entry.len() - entry.trim_start().len();
    let rest = entry[start..].strip_prefix("local")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let body = rest.trim_start();
    let names_end = body.find(['=', '\n']).unwrap_or(body.len());
    let names = &body[..names_end];
    let plain = names.starts_with("function ")
        || (!names.trim().is_empty()
            && names
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ',' || c.is_whitespace())
            && !names.split(',').any(|n| n.trim().is_empty()));
    plain.then(|| format!("{}{}", &entry[..start], body))
}

/// What one entry did, for the caller to print.
pub enum Outcome {
    Values(Vec<Value>),
    Text(String),
    Error(rhue::Error),
    Quit,
}

/// Runs one complete entry: a command, or code.
pub fn eval(engine: &mut Engine, entry: &str, dump: bool) -> Outcome {
    let trimmed = entry.trim();
    if let Some(command) = trimmed.strip_prefix(':') {
        let (name, rest) = command
            .split_once(char::is_whitespace)
            .unwrap_or((command, ""));
        let rest = rest.trim();
        return match name {
            "q" | "quit" | "exit" => Outcome::Quit,
            "h" | "help" | "?" => Outcome::Text(HELP.into()),
            "t" | "type" if !rest.is_empty() => match engine.expression_type(rest) {
                Ok(t) => Outcome::Text(t),
                Err(e) => Outcome::Error(e),
            },
            "l" | "load" if !rest.is_empty() => match std::fs::read_to_string(rest) {
                Ok(src) => match crate::run(engine, rest, &src, dump, false) {
                    Ok(v) => Outcome::Values(v),
                    Err(e) => Outcome::Error(e),
                },
                Err(e) => Outcome::Text(format!("cannot read {rest}: {e}")),
            },
            "t" | "type" | "l" | "load" => {
                Outcome::Text(format!(":{name} needs an argument (see :help)"))
            }
            _ => Outcome::Text(format!("unknown command :{name} (see :help)")),
        };
    }
    let promoted = promote_locals(entry);
    match crate::run(
        engine,
        "stdin",
        promoted.as_deref().unwrap_or(entry),
        dump,
        true,
    ) {
        Ok(v) => Outcome::Values(v),
        Err(e) => Outcome::Error(e),
    }
}

fn print(engine: &mut Engine, outcome: Outcome, color: bool) -> bool {
    match outcome {
        Outcome::Values(values) if !values.is_empty() => {
            let mut cx = engine.context();
            let shown: Vec<String> = values.iter().map(|v| pretty::show(&mut cx, v)).collect();
            println!("{}", shown.join("\t"));
        }
        Outcome::Values(_) => {}
        Outcome::Text(t) => println!("{t}"),
        Outcome::Error(e) => eprint!("{}", e.render(color)),
        Outcome::Quit => return false,
    }
    true
}

/// Line editing's view of the engine: whether input is complete.
#[derive(Completer, Helper, Highlighter, Hinter)]
struct Input {
    /// A separate engine, so checking a half-typed entry can't define
    /// anything.
    probe: std::cell::RefCell<Engine>,
}

impl Validator for Input {
    fn validate(&self, cx: &mut ValidationContext<'_>) -> rustyline::Result<ValidationResult> {
        Ok(if is_incomplete(&mut self.probe.borrow_mut(), cx.input()) {
            ValidationResult::Incomplete
        } else {
            ValidationResult::Valid(None)
        })
    }
}

fn history_path() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(std::path::PathBuf::from(home).join(".rhue_history"))
}

pub fn run(engine: &mut Engine, dump: bool, color: bool) -> ExitCode {
    if !io::stdin().is_terminal() {
        return run_piped(engine, dump, color);
    }
    let mut editor = match rustyline::Editor::<Input, rustyline::history::DefaultHistory>::new() {
        Ok(e) => e,
        Err(_) => return run_piped(engine, dump, color),
    };
    editor.set_helper(Some(Input {
        probe: std::cell::RefCell::new(Engine::new()),
    }));
    let history = history_path();
    if let Some(h) = &history {
        let _ = editor.load_history(h);
    }
    println!(
        "Rhue {}. Globals persist between entries; :help for commands, Ctrl-D to exit.",
        env!("CARGO_PKG_VERSION")
    );
    loop {
        match editor.readline("> ") {
            Ok(entry) => {
                if entry.trim().is_empty() {
                    continue;
                }
                let _ = editor.add_history_entry(entry.as_str());
                let outcome = eval(engine, &entry, dump);
                if !print(engine, outcome, color) {
                    break;
                }
            }
            // Ctrl-C drops the entry being typed.
            Err(ReadlineError::Interrupted) => continue,
            Err(_) => break,
        }
    }
    if let Some(h) = &history {
        let _ = editor.save_history(h);
    }
    ExitCode::SUCCESS
}

/// Piped input: the same, line by line, without line editing or prompts.
fn run_piped(engine: &mut Engine, dump: bool, color: bool) -> ExitCode {
    let mut probe = Engine::new();
    let mut pending = String::new();
    let mut failed = false;
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        pending.push_str(&line);
        pending.push('\n');
        if is_incomplete(&mut probe, &pending) {
            continue;
        }
        let entry = std::mem::take(&mut pending);
        if entry.trim().is_empty() {
            continue;
        }
        let outcome = eval(engine, &entry, dump);
        failed |= matches!(outcome, Outcome::Error(_));
        if !print(engine, outcome, color) {
            break;
        }
        let _ = io::stdout().flush();
    }
    if !pending.trim().is_empty() {
        // Input ended in the middle of something: report it.
        let outcome = eval(engine, &pending, dump);
        failed |= matches!(outcome, Outcome::Error(_));
        print(engine, outcome, color);
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
