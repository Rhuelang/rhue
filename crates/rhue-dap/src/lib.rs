//! `rhue-dap`, a debugger for Rhue scripts that speaks the Debug Adapter
//! Protocol over stdio (what VS Code, nvim-dap and friends use).
//!
//! It runs whatever the `launch` request names in an [`Engine`] with a debug
//! hook. You get breakpoints (by line, with conditions, hit counts and log
//! messages), stepping (over, into, out), pausing, the call stack, scopes
//! (locals, upvalues, globals) with expandable tables, evaluating expressions
//! in a frame, and the script's `print` output. Hovers only read names and
//! field paths like `player.stats.hp`, so they never run code.
//!
//! Save a module the script `require`d and it gets hot reloaded, checked about
//! every 300ms while running and right before resuming from a stop.
//!
//! [`serve`] is the whole adapter over any pair of streams, and [`run`] is
//! the session on its own, fed from a channel, which is what the tests use.
//! One thread reads the protocol and hands each message to the session
//! thread, which owns the engine (engines aren't `Send`). While the
//! script runs the hook checks for new messages every line, and while it's
//! stopped the hook waits on them.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rhue::{DebugEvent, Debugger, Engine, HookMask, Table, Value};
use serde_json::json;

/// A protocol message.
pub use serde_json::Value as Json;

/// Where outgoing messages go.
type Sink = Box<dyn FnMut(Json) + Send>;

/// Sends protocol messages (the session and the print hook share it).
#[derive(Clone)]
struct Out {
    inner: Arc<Mutex<(Sink, i64)>>,
}

impl Out {
    fn send(&self, mut msg: Json) {
        let mut guard = self.inner.lock().expect("output lock");
        guard.1 += 1;
        msg["seq"] = json!(guard.1);
        (guard.0)(msg);
    }

    fn respond(&self, req: &Json, body: Json) {
        self.send(json!({
            "type": "response",
            "request_seq": req["seq"],
            "success": true,
            "command": req["command"],
            "body": body,
        }));
    }

    fn fail(&self, req: &Json, message: &str) {
        self.send(json!({
            "type": "response",
            "request_seq": req["seq"],
            "success": false,
            "command": req["command"],
            "message": message,
        }));
    }

    fn event(&self, event: &str, body: Json) {
        self.send(json!({ "type": "event", "event": event, "body": body }));
    }
}

/// Reads one `Content-Length`-framed message, `None` at the end of the input
/// or on anything that isn't one.
pub fn read_message(input: &mut impl BufRead) -> Option<Json> {
    let mut len = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(n) = line.strip_prefix("Content-Length:") {
            len = n.trim().parse().ok();
        }
    }
    let mut body = vec![0; len?];
    input.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

/// Writes one message with its `Content-Length` header.
pub fn write_message(out: &mut impl Write, msg: &Json) -> std::io::Result<()> {
    let body = msg.to_string();
    write!(out, "Content-Length: {}\r\n\r\n{body}", body.len())?;
    out.flush()
}

/// Runs a debugging session: requests come in on `messages`, and every
/// response and event goes to `send` (already numbered). Returns when the
/// client disconnects or `messages` closes. The engine lives on the calling
/// thread, since engines aren't `Send`.
pub fn run(messages: Receiver<Json>, send: impl FnMut(Json) + Send + 'static) {
    let out = Out {
        inner: Arc::new(Mutex::new((Box::new(send), 0))),
    };
    Session::new(out, messages).run();
}

/// The whole adapter over a pair of streams, what the binary runs on stdio.
/// A thread reads `input` and the session runs on this one.
pub fn serve(input: impl BufRead + Send + 'static, mut output: impl Write + Send + 'static) {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut input = input;
        while let Some(msg) = read_message(&mut input) {
            if tx.send(msg).is_err() {
                break;
            }
        }
    });
    run(rx, move |msg| {
        let _ = write_message(&mut output, &msg);
    });
}

/// How to carry on after a stop.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Run,
    /// Stop at the next line at this depth or shallower (`next`).
    Over(usize),
    /// Stop at the next line anywhere (`stepIn`).
    Into,
    /// Stop at the next line shallower than this (`stepOut`).
    Out(usize),
}

/// What a `variablesReference` stands for (good until the script runs again).
#[derive(Clone)]
enum VarRef {
    Locals(usize),
    Upvalues(usize),
    Globals,
    Table(Table),
}

/// A line breakpoint, maybe with a condition, a hit count or a log message (a
/// logpoint, which prints instead of stopping).
struct Breakpoint {
    line: u32,
    condition: Option<String>,
    hit_condition: Option<HitCondition>,
    log_message: Option<String>,
    hits: u32,
}

/// When a breakpoint's hit count lets it stop: `N` or `== N` (only the Nth
/// hit), `> N`, `>= N`, `< N`, `<= N`, `% N` (every Nth).
#[derive(Clone, Copy, Debug, PartialEq)]
enum HitCondition {
    Eq(u32),
    Gt(u32),
    Ge(u32),
    Lt(u32),
    Le(u32),
    Every(u32),
}

impl HitCondition {
    fn parse(s: &str) -> Option<HitCondition> {
        let s = s.trim();
        let (op, rest) = ["==", ">=", "<=", ">", "<", "%"]
            .iter()
            .find_map(|op| s.strip_prefix(op).map(|r| (*op, r)))
            .unwrap_or(("==", s));
        let n: u32 = rest.trim().parse().ok()?;
        Some(match op {
            "==" => HitCondition::Eq(n),
            ">=" => HitCondition::Ge(n),
            "<=" => HitCondition::Le(n),
            ">" => HitCondition::Gt(n),
            "<" => HitCondition::Lt(n),
            _ if n == 0 => return None,
            _ => HitCondition::Every(n),
        })
    }

    fn allows(self, hits: u32) -> bool {
        match self {
            HitCondition::Eq(n) => hits == n,
            HitCondition::Gt(n) => hits > n,
            HitCondition::Ge(n) => hits >= n,
            HitCondition::Lt(n) => hits < n,
            HitCondition::Le(n) => hits <= n,
            HitCondition::Every(n) => hits.is_multiple_of(n),
        }
    }
}

struct State {
    out: Out,
    rx: Receiver<Json>,
    /// Breakpoints by canonical source path.
    breakpoints: HashMap<PathBuf, Vec<Breakpoint>>,
    program: Option<PathBuf>,
    stop_on_entry: bool,
    mode: Mode,
    pause: bool,
    /// Module name → file, for sources `require` loaded.
    module_files: Rc<RefCell<HashMap<String, PathBuf>>>,
    /// What each module's file said when it was last loaded, to spot edits.
    module_texts: Rc<RefCell<HashMap<String, String>>>,
    /// When the files were last checked for edits.
    last_check: Instant,
    refs: Vec<VarRef>,
    started: bool,
    terminate: bool,
}

struct Session {
    state: Rc<RefCell<State>>,
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

impl Session {
    fn new(out: Out, rx: Receiver<Json>) -> Self {
        Session {
            state: Rc::new(RefCell::new(State {
                out,
                rx,
                breakpoints: HashMap::new(),
                program: None,
                stop_on_entry: false,
                mode: Mode::Run,
                pause: false,
                module_files: Rc::new(RefCell::new(HashMap::new())),
                module_texts: Rc::new(RefCell::new(HashMap::new())),
                last_check: Instant::now(),
                refs: Vec::new(),
                started: false,
                terminate: false,
            })),
        }
    }

    /// Handles messages until the client disconnects.
    fn run(&self) {
        loop {
            let msg = match self.state.borrow().rx.recv() {
                Ok(m) => m,
                Err(_) => return,
            };
            let command = msg["command"].as_str().unwrap_or("").to_string();
            let out = self.state.borrow().out.clone();
            match command.as_str() {
                "configurationDone" => {
                    out.respond(&msg, json!({}));
                    self.launch_program();
                }
                "disconnect" | "terminate" => {
                    out.respond(&msg, json!({}));
                    return;
                }
                _ => {
                    if !self.state.borrow_mut().common(&msg, None) {
                        out.fail(&msg, &format!("`{command}` isn't available now"));
                    }
                }
            }
        }
    }

    fn launch_program(&self) {
        let (program, out, module_files, module_texts) = {
            let mut st = self.state.borrow_mut();
            if st.started {
                return;
            }
            st.started = true;
            (
                st.program.clone(),
                st.out.clone(),
                st.module_files.clone(),
                st.module_texts.clone(),
            )
        };
        let Some(program) = program else {
            out.event(
                "output",
                json!({ "category": "stderr", "output": "no program to launch\n" }),
            );
            out.event("terminated", json!({}));
            return;
        };
        let source = match std::fs::read_to_string(&program) {
            Ok(s) => s,
            Err(e) => {
                out.event(
                    "output",
                    json!({ "category": "stderr", "output": format!("cannot read {}: {e}\n", program.display()) }),
                );
                out.event("exited", json!({ "exitCode": 1 }));
                out.event("terminated", json!({}));
                return;
            }
        };
        let mut engine = Engine::new();
        let print_out = out.clone();
        engine.on_print(move |line| {
            print_out.event(
                "output",
                json!({ "category": "stdout", "output": format!("{line}\n") }),
            );
        });
        let dir = program.parent().map(Path::to_path_buf).unwrap_or_default();
        engine.set_module_loader(move |name| {
            if !rhue::is_module_name(name) {
                return None;
            }
            rhue::module_paths(&dir, name).into_iter().find_map(|p| {
                let text = std::fs::read_to_string(&p).ok()?;
                module_files
                    .borrow_mut()
                    .insert(name.to_string(), canonical(&p));
                module_texts
                    .borrow_mut()
                    .insert(name.to_string(), text.clone());
                Some(text)
            })
        });
        let state = self.state.clone();
        let mask = HookMask::new().line(true);
        engine.set_debug_hook(mask, move |dbg, event| {
            let DebugEvent::Line(line) = event else {
                return Ok(());
            };
            on_line(&state, dbg, line)
        });
        let name = program.to_string_lossy().to_string();
        let result = engine.eval_named(&name, &source);
        let code = match result {
            Ok(_) => 0,
            Err(e) if self.state.borrow().terminate => {
                let _ = e;
                0
            }
            Err(e) => {
                out.event(
                    "output",
                    json!({ "category": "stderr", "output": e.render(false) }),
                );
                1
            }
        };
        out.event("exited", json!({ "exitCode": code }));
        out.event("terminated", json!({}));
    }
}

impl State {
    /// Requests that make sense whether the script's stopped or not. Returns
    /// `false` for ones it doesn't handle.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "frame and variable ids come from us and are small"
    )]
    fn common(&mut self, msg: &Json, dbg: Option<&mut Debugger<'_, '_>>) -> bool {
        let out = self.out.clone();
        let args = &msg["arguments"];
        match msg["command"].as_str().unwrap_or("") {
            "initialize" => {
                out.respond(
                    msg,
                    json!({
                        "supportsConfigurationDoneRequest": true,
                        "supportsEvaluateForHovers": true,
                        "supportsTerminateRequest": true,
                        "supportsConditionalBreakpoints": true,
                        "supportsHitConditionalBreakpoints": true,
                        "supportsLogPoints": true,
                    }),
                );
                out.event("initialized", json!({}));
            }
            "launch" => {
                self.program = args["program"].as_str().map(PathBuf::from);
                self.stop_on_entry = args["stopOnEntry"].as_bool().unwrap_or(false);
                out.respond(msg, json!({}));
            }
            "setBreakpoints" => {
                let path = canonical(Path::new(args["source"]["path"].as_str().unwrap_or("")));
                let mut reply = Vec::new();
                let mut list = Vec::new();
                for b in args["breakpoints"].as_array().into_iter().flatten() {
                    let Some(line) = b["line"].as_u64() else {
                        continue;
                    };
                    let text = |k: &str| {
                        b[k].as_str()
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(String::from)
                    };
                    let hit_text = text("hitCondition");
                    let hit_condition = hit_text.as_deref().map(HitCondition::parse);
                    if let Some(None) = hit_condition {
                        reply.push(json!({
                            "verified": false,
                            "line": line,
                            "message": "hit counts are N, == N, > N, >= N, < N, <= N or % N",
                        }));
                        continue;
                    }
                    reply.push(json!({ "verified": true, "line": line }));
                    list.push(Breakpoint {
                        line: line as u32,
                        condition: text("condition"),
                        hit_condition: hit_condition.flatten(),
                        log_message: text("logMessage"),
                        hits: 0,
                    });
                }
                self.breakpoints.insert(path, list);
                out.respond(msg, json!({ "breakpoints": reply }));
            }
            "threads" => out.respond(msg, json!({ "threads": [{ "id": 1, "name": "main" }] })),
            "pause" => {
                self.pause = true;
                out.respond(msg, json!({}));
            }
            "stackTrace" | "scopes" | "variables" | "evaluate" => match dbg {
                Some(dbg) => self.inspect(msg, dbg),
                None => out.fail(msg, "the program isn't stopped"),
            },
            _ => return false,
        }
        true
    }

    fn source_path(&self, name: &str) -> PathBuf {
        match self.module_files.borrow().get(name) {
            Some(p) => p.clone(),
            None => canonical(Path::new(name)),
        }
    }

    fn add_ref(&mut self, r: VarRef) -> usize {
        self.refs.push(r);
        self.refs.len()
    }

    fn variable(&mut self, name: String, v: &Value) -> Json {
        let (value, reference) = match v {
            Value::String(s) => (format!("{s:?}"), 0),
            Value::Table(t) => (v.to_string(), self.add_ref(VarRef::Table(t.clone()))),
            other => (other.to_string(), 0),
        };
        json!({ "name": name, "value": value, "type": v.type_of(), "variablesReference": reference })
    }

    /// The requests that poke at a stopped script.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "frame and variable ids come from us and are small"
    )]
    fn inspect(&mut self, msg: &Json, dbg: &mut Debugger<'_, '_>) {
        let out = self.out.clone();
        let args = &msg["arguments"];
        match msg["command"].as_str().unwrap_or("") {
            "stackTrace" => {
                let frames: Vec<Json> = dbg
                    .stack()
                    .iter()
                    .enumerate()
                    .map(|(i, f)| {
                        let path = self.source_path(&f.source);
                        json!({
                            "id": i + 1,
                            "name": f.function,
                            "line": f.line,
                            "column": 1,
                            "source": {
                                "name": path.file_name().map(|n| n.to_string_lossy().to_string()),
                                "path": path.to_string_lossy(),
                            },
                        })
                    })
                    .collect();
                let total = frames.len();
                out.respond(msg, json!({ "stackFrames": frames, "totalFrames": total }));
            }
            "scopes" => {
                let level = args["frameId"].as_u64().unwrap_or(1).saturating_sub(1) as usize;
                let locals = self.add_ref(VarRef::Locals(level));
                let upvalues = self.add_ref(VarRef::Upvalues(level));
                let globals = self.add_ref(VarRef::Globals);
                out.respond(
                    msg,
                    json!({ "scopes": [
                        { "name": "Locals", "variablesReference": locals, "expensive": false },
                        { "name": "Upvalues", "variablesReference": upvalues, "expensive": false },
                        { "name": "Globals", "variablesReference": globals, "expensive": true },
                    ]}),
                );
            }
            "variables" => {
                let r = args["variablesReference"].as_u64().unwrap_or(0) as usize;
                let Some(which) = r.checked_sub(1).and_then(|i| self.refs.get(i)).cloned() else {
                    out.fail(msg, "unknown variablesReference");
                    return;
                };
                let entries: Vec<(String, Value)> = match which {
                    VarRef::Locals(l) => dbg.locals(l),
                    VarRef::Upvalues(l) => dbg.upvalues(l),
                    VarRef::Globals => {
                        let mut cx = dbg.context();
                        let g = cx.globals_table();
                        let mut v: Vec<(String, Value)> = g
                            .into_iter()
                            .filter(|(_, v)| !matches!(v, Value::Function(_)))
                            .collect();
                        v.sort_by(|a, b| a.0.cmp(&b.0));
                        v
                    }
                    VarRef::Table(t) => {
                        let mut cx = dbg.context();
                        let mut v: Vec<(String, Value)> = cx
                            .pairs(&t)
                            .into_iter()
                            .map(|(k, v)| {
                                let key = match &k {
                                    Value::String(s) => s.to_string(),
                                    other => format!("[{other}]"),
                                };
                                (key, v)
                            })
                            .collect();
                        v.sort_by(|a, b| a.0.cmp(&b.0));
                        v
                    }
                };
                let vars: Vec<Json> = entries
                    .into_iter()
                    .map(|(n, v)| self.variable(n, &v))
                    .collect();
                out.respond(msg, json!({ "variables": vars }));
            }
            "evaluate" => {
                let level = args["frameId"].as_u64().unwrap_or(1).saturating_sub(1) as usize;
                let expr = args["expression"].as_str().unwrap_or("").trim();
                // hovering can't run code (a call could do anything), so it
                // only reads names and field paths
                let result = if args["context"] == "hover" {
                    evaluate_path(dbg, level, expr)
                        .map(|v| vec![v])
                        .ok_or_else(|| format!("`{expr}` isn't a name or field path"))
                } else {
                    dbg.eval(level, expr)
                };
                match result {
                    Ok(values) => {
                        let (result, ty, reference) = match values.as_slice() {
                            [] => ("(no value)".to_string(), "nil".to_string(), json!(0)),
                            [v] => {
                                let var = self.variable(expr.to_string(), v);
                                (
                                    var["value"].as_str().unwrap_or("").to_string(),
                                    var["type"].as_str().unwrap_or("").to_string(),
                                    var["variablesReference"].clone(),
                                )
                            }
                            many => (
                                many.iter().map(shown).collect::<Vec<_>>().join(", "),
                                "values".to_string(),
                                json!(0),
                            ),
                        };
                        out.respond(
                            msg,
                            json!({ "result": result, "type": ty, "variablesReference": reference }),
                        );
                    }
                    Err(e) => out.fail(msg, &e),
                }
            }
            _ => {}
        }
    }

    /// Whether the breakpoints on `line` of `path` stop here, after counting
    /// the hit, checking conditions and printing log messages.
    fn breakpoint_stops(&mut self, path: &Path, line: u32, dbg: &mut Debugger<'_, '_>) -> bool {
        let out = self.out.clone();
        let Some(list) = self.breakpoints.get_mut(path) else {
            return false;
        };
        let mut stop = false;
        for b in list.iter_mut().filter(|b| b.line == line) {
            if let Some(cond) = &b.condition {
                match dbg.eval(0, cond) {
                    Ok(v) if v.first().is_some_and(Value::is_truthy) => {}
                    Ok(_) => continue,
                    Err(e) => {
                        // stop, so the broken condition gets noticed
                        out.event(
                            "output",
                            json!({ "category": "stderr", "output": format!("breakpoint condition `{cond}` failed: {e}\n") }),
                        );
                        stop = true;
                        continue;
                    }
                }
            }
            b.hits += 1;
            if b.hit_condition.is_some_and(|h| !h.allows(b.hits)) {
                continue;
            }
            match &b.log_message {
                Some(message) => {
                    let text = interpolate(dbg, message);
                    out.event(
                        "output",
                        json!({ "category": "console", "output": format!("{text}\n") }),
                    );
                }
                None => stop = true,
            }
        }
        stop
    }
}

/// How a value reads in the debugger, strings quoted and the rest as
/// `tostring`.
fn shown(v: &Value) -> String {
    match v {
        Value::String(s) => format!("{s:?}"),
        other => other.to_string(),
    }
}

/// A logpoint's message with each `{expression}` swapped for its value (`{{`
/// and `}}` for literal braces).
fn interpolate(dbg: &mut Debugger<'_, '_>, message: &str) -> String {
    let mut out = String::new();
    let mut rest = message;
    while let Some(i) = rest.find(['{', '}']) {
        out.push_str(&rest[..i]);
        let brace = rest.as_bytes()[i];
        if rest[i + 1..].starts_with(brace as char) {
            out.push(brace as char);
            rest = &rest[i + 2..];
            continue;
        }
        if brace == b'}' {
            out.push('}');
            rest = &rest[i + 1..];
            continue;
        }
        let Some(end) = rest[i..].find('}') else {
            out.push_str(&rest[i..]);
            return out;
        };
        let expr = &rest[i + 1..i + end];
        match dbg.eval(0, expr) {
            Ok(values) => {
                let parts: Vec<String> = values
                    .iter()
                    .map(|v| match v {
                        Value::String(s) => s.to_string(),
                        other => other.to_string(),
                    })
                    .collect();
                out.push_str(&parts.join(", "));
            }
            Err(e) => out.push_str(&format!("<error: {e}>")),
        }
        rest = &rest[i + end + 1..];
    }
    out.push_str(rest);
    out
}

/// A name (local, upvalue or global) followed by `.field`s in frame `level`,
/// read without running any code.
fn evaluate_path(dbg: &mut Debugger<'_, '_>, level: usize, expr: &str) -> Option<Value> {
    let mut parts = expr.split('.');
    let first = parts.next()?.trim();
    let mut v = dbg
        .locals(level)
        .into_iter()
        .rev()
        .chain(dbg.upvalues(level))
        .find(|(n, _)| n == first)
        .map(|(_, v)| v)
        .or_else(|| {
            let mut cx = dbg.context();
            cx.global::<Value>(first).ok().filter(|v| !v.is_nil())
        })?;
    for field in parts {
        let Value::Table(t) = &v else {
            return None;
        };
        let mut cx = dbg.context();
        v = cx.get::<Value>(t, field.trim()).ok()?;
    }
    Some(v)
}

/// The hook. Decides whether to stop at `line`, and serves requests while
/// stopped.
/// How often a running script's modules get checked for edits.
const RELOAD_EVERY: Duration = Duration::from_millis(300);

/// Reloads modules whose files changed since they were loaded, so an edit
/// shows up without restarting the session. A broken edit gets reported and
/// the old code keeps running.
fn reload_changed(st: &mut State, dbg: &mut Debugger<'_, '_>) {
    st.last_check = Instant::now();
    let files: Vec<(String, PathBuf)> = st
        .module_files
        .borrow()
        .iter()
        .map(|(n, p)| (n.clone(), p.clone()))
        .collect();
    for (name, path) in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if st.module_texts.borrow().get(&name) == Some(&text) {
            continue;
        }
        st.module_texts
            .borrow_mut()
            .insert(name.clone(), text.clone());
        let (category, output) = match dbg.context().reload_module(&name, &text) {
            Ok(_) => ("console", format!("reloaded {name}\n")),
            Err(e) => ("stderr", e.render(false)),
        };
        st.out
            .event("output", json!({ "category": category, "output": output }));
    }
}

fn on_line(
    state: &Rc<RefCell<State>>,
    dbg: &mut Debugger<'_, '_>,
    line: u32,
) -> Result<(), String> {
    let mut st = state.borrow_mut();
    // requests that came in while running
    loop {
        match st.rx.try_recv() {
            Ok(msg) => {
                if handle_while_running(&mut st, &msg)? {
                    continue;
                }
                if !st.common(&msg, Some(dbg)) {
                    let c = msg["command"].as_str().unwrap_or("").to_string();
                    st.out
                        .fail(&msg, &format!("`{c}` isn't available while running"));
                }
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => return Err("the debugger went away".into()),
        }
    }
    if st.last_check.elapsed() >= RELOAD_EVERY {
        reload_changed(&mut st, dbg);
    }
    let depth = dbg.depth();
    let source = dbg.frame(0).map(|f| st.source_path(&f.source));
    let at_breakpoint = match &source {
        Some(p) => st.breakpoint_stops(p, line, dbg),
        None => false,
    };
    let reason = if std::mem::take(&mut st.stop_on_entry) {
        Some("entry")
    } else if std::mem::take(&mut st.pause) {
        Some("pause")
    } else if at_breakpoint {
        Some("breakpoint")
    } else {
        match st.mode {
            Mode::Into => Some("step"),
            Mode::Over(d) if depth <= d => Some("step"),
            Mode::Out(d) if depth < d => Some("step"),
            _ => None,
        }
    };
    let Some(reason) = reason else {
        return Ok(());
    };
    st.out.event(
        "stopped",
        json!({ "reason": reason, "threadId": 1, "allThreadsStopped": true }),
    );
    // stopped, serve requests until one resumes
    loop {
        let Ok(msg) = st.rx.recv() else {
            return Err("the debugger went away".into());
        };
        let next = match msg["command"].as_str().unwrap_or("") {
            "continue" => Some(Mode::Run),
            "next" => Some(Mode::Over(depth)),
            "stepIn" => Some(Mode::Into),
            "stepOut" => Some(Mode::Out(depth)),
            "disconnect" | "terminate" => {
                st.out.respond(&msg, json!({}));
                st.terminate = true;
                return Err("terminated by the debugger".into());
            }
            _ => None,
        };
        match next {
            Some(mode) => {
                st.mode = mode;
                st.refs.clear();
                let body = if mode == Mode::Run {
                    json!({ "allThreadsContinued": true })
                } else {
                    json!({})
                };
                st.out.respond(&msg, body);
                // edits made while it sat here apply before it carries on
                reload_changed(&mut st, dbg);
                return Ok(());
            }
            None => {
                if !st.common(&msg, Some(dbg)) {
                    let c = msg["command"].as_str().unwrap_or("").to_string();
                    st.out.fail(&msg, &format!("unsupported request `{c}`"));
                }
            }
        }
    }
}

/// `disconnect` while running stops the script, resuming requests just get
/// answered (there's nothing to resume). `Ok(true)` means it was handled.
fn handle_while_running(st: &mut State, msg: &Json) -> Result<bool, String> {
    match msg["command"].as_str().unwrap_or("") {
        "disconnect" | "terminate" => {
            st.out.respond(msg, json!({}));
            st.terminate = true;
            Err("terminated by the debugger".into())
        }
        "continue" | "next" | "stepIn" | "stepOut" => {
            st.out.respond(msg, json!({}));
            Ok(true)
        }
        _ => Ok(false),
    }
}
