//! A debugging session against the real `rhue-dap` binary, as an editor
//! drives it: breakpoints, the stack, scopes and variables, stepping,
//! evaluating, output, and the end of the program.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    seq: i64,
    /// Events received while waiting for something else.
    events: Vec<Value>,
}

impl Client {
    fn start() -> Client {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rhue-dap"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        Client {
            stdin: child.stdin.take().unwrap(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
            seq: 0,
            events: Vec::new(),
        }
    }

    fn recv(&mut self) -> Value {
        let mut len = 0;
        loop {
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "rhue-dap closed stdout"
            );
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(n) = line.strip_prefix("Content-Length: ") {
                len = n.parse().unwrap();
            }
        }
        let mut body = vec![0; len];
        self.stdout.read_exact(&mut body).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn request(&mut self, command: &str, arguments: Value) -> Value {
        self.seq += 1;
        let msg = json!({ "seq": self.seq, "type": "request", "command": command, "arguments": arguments });
        let body = msg.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        self.stdin.flush().unwrap();
        loop {
            let m = self.recv();
            if m["type"] == "response" && m["request_seq"] == json!(self.seq) {
                return m;
            }
            self.events.push(m);
        }
    }

    /// The next event named `name` (possibly one already received).
    fn event(&mut self, name: &str) -> Value {
        if let Some(i) = self.events.iter().position(|e| e["event"] == name) {
            return self.events.remove(i);
        }
        loop {
            let m = self.recv();
            if m["event"] == name {
                return m;
            }
            self.events.push(m);
        }
    }

    fn variables(&mut self, reference: &Value) -> Vec<(String, String)> {
        let r = self.request("variables", json!({ "variablesReference": reference }));
        r["body"]["variables"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                (
                    v["name"].as_str().unwrap().to_string(),
                    v["value"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }
}

fn script(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rhue-dap-{name}-{}", std::process::id()));
    for (path, text) in files {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    dir
}

const MAIN: &str = "local util = require('util')
local function total(items)
  local sum = 0
  for i, v in items do
    sum += util.double(v)
  end
  return sum
end
local data = { 1, 2, 3 }
print('start')
local t = total(data)
print('total', t)
";

const UTIL: &str = "local M = {}
function M.double(x)
  return x * 2
end
return M
";

#[test]
fn breakpoints_stack_variables_and_stepping() {
    let dir = script("main", &[("main.rhue", MAIN), ("util.rhue", UTIL)]);
    let main = dir.join("main.rhue");
    let mut c = Client::start();
    let init = c.request("initialize", json!({ "adapterID": "rhue" }));
    assert_eq!(
        init["body"]["supportsConfigurationDoneRequest"],
        json!(true)
    );
    c.event("initialized");
    c.request("launch", json!({ "program": main }));
    let bp = c.request(
        "setBreakpoints",
        json!({ "source": { "path": main }, "breakpoints": [{ "line": 5 }] }),
    );
    assert_eq!(bp["body"]["breakpoints"][0]["verified"], json!(true));
    c.request("configurationDone", json!({}));

    // `print` output arrives as output events
    let out = c.event("output");
    assert_eq!(out["body"]["output"], "start\n");

    let stopped = c.event("stopped");
    assert_eq!(stopped["body"]["reason"], "breakpoint");
    let threads = c.request("threads", json!({}));
    assert_eq!(threads["body"]["threads"][0]["id"], json!(1));
    let st = c.request("stackTrace", json!({ "threadId": 1 }));
    let frames = st["body"]["stackFrames"].as_array().unwrap().clone();
    let names: Vec<&str> = frames.iter().map(|f| f["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["total", "<main>"]);
    assert_eq!(frames[0]["line"], json!(5));
    assert_eq!(frames[1]["line"], json!(11));
    assert!(
        frames[0]["source"]["path"]
            .as_str()
            .unwrap()
            .ends_with("main.rhue")
    );

    let scopes = c.request("scopes", json!({ "frameId": frames[0]["id"] }));
    let scopes = scopes["body"]["scopes"].as_array().unwrap().clone();
    assert_eq!(scopes[0]["name"], "Locals");
    let locals = c.variables(&scopes[0]["variablesReference"]);
    let shown: Vec<String> = locals.iter().map(|(n, v)| format!("{n}={v}")).collect();
    assert!(shown[0].starts_with("items=table: 0x"), "{}", shown[0]);
    assert_eq!(&shown[1..], ["sum=0", "i=1", "v=1"]);
    let upvalues = c.variables(&scopes[1]["variablesReference"]);
    assert_eq!(upvalues[0].0, "util");

    // expand the `items` table
    let vars = c.request(
        "variables",
        json!({ "variablesReference": scopes[0]["variablesReference"] }),
    );
    let items_ref = vars["body"]["variables"][0]["variablesReference"].clone();
    assert_ne!(items_ref, json!(0));
    assert_eq!(
        c.variables(&items_ref),
        [
            ("[1]".into(), "1".into()),
            ("[2]".into(), "2".into()),
            ("[3]".into(), "3".into())
        ]
    );

    // evaluate names and fields
    let ev = c.request("evaluate", json!({ "expression": "sum", "frameId": 1 }));
    assert_eq!(ev["body"]["result"], "0");
    let ev = c.request(
        "evaluate",
        json!({ "expression": "util.double", "frameId": 1 }),
    );
    assert!(
        ev["body"]["result"]
            .as_str()
            .unwrap()
            .starts_with("function")
    );
    // expressions run in the frame (watch and console), hovers only read
    let ev = c.request(
        "evaluate",
        json!({ "expression": "sum + v * 10, #items", "frameId": 1, "context": "watch" }),
    );
    assert_eq!(ev["body"]["result"], "10, 3");
    let hover = c.request(
        "evaluate",
        json!({ "expression": "sum + 1", "frameId": 1, "context": "hover" }),
    );
    assert_eq!(hover["success"], json!(false));
    let bad = c.request(
        "evaluate",
        json!({ "expression": "nope.x", "frameId": 1, "context": "repl" }),
    );
    assert_eq!(bad["success"], json!(false));
    assert!(
        bad["message"]
            .as_str()
            .unwrap()
            .contains("attempt to index nil"),
        "{bad}"
    );

    // step into `util.double` (in another file)
    c.request("stepIn", json!({ "threadId": 1 }));
    assert_eq!(c.event("stopped")["body"]["reason"], "step");
    let st = c.request("stackTrace", json!({ "threadId": 1 }));
    let top = &st["body"]["stackFrames"][0];
    assert_eq!(top["name"], "M.double");
    assert!(
        top["source"]["path"]
            .as_str()
            .unwrap()
            .ends_with("util.rhue"),
        "{top}"
    );
    assert_eq!(top["line"], json!(3));

    // step out, back into `total`
    c.request("stepOut", json!({ "threadId": 1 }));
    c.event("stopped");
    let st = c.request("stackTrace", json!({ "threadId": 1 }));
    assert_eq!(st["body"]["stackFrames"][0]["name"], "total");

    // next stays in `total`: the loop comes back to line 5
    c.request("next", json!({ "threadId": 1 }));
    c.event("stopped");
    let st = c.request("stackTrace", json!({ "threadId": 1 }));
    assert_eq!(st["body"]["stackFrames"][0]["name"], "total");

    // clear the breakpoint and run to the end
    c.request(
        "setBreakpoints",
        json!({ "source": { "path": main }, "breakpoints": [] }),
    );
    c.request("continue", json!({ "threadId": 1 }));
    let out = c.event("output");
    assert_eq!(out["body"]["output"], "total\t12\n");
    assert_eq!(c.event("exited")["body"]["exitCode"], json!(0));
    c.event("terminated");
    c.request("disconnect", json!({}));
    assert!(c.child.wait().unwrap().success());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stop_on_entry_errors_and_terminate() {
    let dir = script(
        "entry",
        &[("main.rhue", "local a = 1\nlocal b = nil\nprint(a + b)\n")],
    );
    let main = dir.join("main.rhue");
    let mut c = Client::start();
    c.request("initialize", json!({}));
    c.request("launch", json!({ "program": main, "stopOnEntry": true }));
    c.request("configurationDone", json!({}));
    let stopped = c.event("stopped");
    assert_eq!(stopped["body"]["reason"], "entry");
    c.request("continue", json!({ "threadId": 1 }));
    // the runtime error is reported, and the program ends
    let out = c.event("output");
    assert_eq!(out["body"]["category"], "stderr");
    assert!(
        out["body"]["output"]
            .as_str()
            .unwrap()
            .contains("attempt to perform arithmetic")
    );
    assert_eq!(c.event("exited")["body"]["exitCode"], json!(1));
    c.request("disconnect", json!({}));
    assert!(c.child.wait().unwrap().success());

    // disconnecting while stopped ends the script
    let mut c = Client::start();
    c.request("initialize", json!({}));
    c.request("launch", json!({ "program": main, "stopOnEntry": true }));
    c.request("configurationDone", json!({}));
    c.event("stopped");
    c.request("disconnect", json!({}));
    assert_eq!(c.event("exited")["body"]["exitCode"], json!(0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pause_a_running_script() {
    let dir = script(
        "pause",
        &[("main.rhue", "local n = 0\nwhile true do\n  n += 1\nend\n")],
    );
    let main = dir.join("main.rhue");
    let mut c = Client::start();
    c.request("initialize", json!({}));
    c.request("launch", json!({ "program": main }));
    c.request("configurationDone", json!({}));
    std::thread::sleep(std::time::Duration::from_millis(50));
    c.request("pause", json!({ "threadId": 1 }));
    assert_eq!(c.event("stopped")["body"]["reason"], "pause");
    let ev = c.request("evaluate", json!({ "expression": "n", "frameId": 1 }));
    assert!(
        ev["body"]["result"]
            .as_str()
            .unwrap()
            .parse::<f64>()
            .unwrap()
            > 0.0
    );
    c.request("disconnect", json!({}));
    c.event("exited");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn conditions_hit_counts_and_logpoints() {
    let src = "local total = 0
for i = 1, 10 do
  total += i
end
print('done', total)
";
    let dir = script("cond", &[("main.rhue", src)]);
    let main = dir.join("main.rhue");
    let mut c = Client::start();
    let init = c.request("initialize", json!({}));
    for cap in [
        "supportsConditionalBreakpoints",
        "supportsHitConditionalBreakpoints",
        "supportsLogPoints",
    ] {
        assert_eq!(init["body"][cap], json!(true), "{cap}");
    }
    c.request("launch", json!({ "program": main }));
    let bp = c.request(
        "setBreakpoints",
        json!({ "source": { "path": main }, "breakpoints": [
            { "line": 3, "condition": "i % 4 == 0" },
            { "line": 3, "logMessage": "i={i} total={total} {{literal}}", "hitCondition": "% 5" },
            { "line": 5, "hitCondition": "bogus" },
        ] }),
    );
    let verified: Vec<bool> = bp["body"]["breakpoints"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["verified"].as_bool().unwrap())
        .collect();
    assert_eq!(verified, [true, true, false]);
    c.request("configurationDone", json!({}));

    // the condition stops at i = 4 and 8, the logpoint prints at its 5th and
    // 10th hits (i = 5, 10) without stopping
    let mut stops = Vec::new();
    let mut logs = Vec::new();
    loop {
        let m = c.recv();
        match m["event"].as_str() {
            Some("stopped") => {
                let ev = c.request(
                    "evaluate",
                    json!({ "expression": "i", "frameId": 1, "context": "watch" }),
                );
                stops.push(ev["body"]["result"].as_str().unwrap().to_string());
                c.request("continue", json!({ "threadId": 1 }));
            }
            Some("output") if m["body"]["category"] == "console" => {
                logs.push(m["body"]["output"].as_str().unwrap().to_string());
            }
            Some("exited") => break,
            _ => {}
        }
    }
    // events queued while waiting for responses count too
    for m in c.events.drain(..) {
        if m["event"] == "output" && m["body"]["category"] == "console" {
            logs.push(m["body"]["output"].as_str().unwrap().to_string());
        }
    }
    logs.sort();
    assert_eq!(stops, ["4", "8"]);
    assert_eq!(
        logs,
        ["i=10 total=45 {literal}\n", "i=5 total=10 {literal}\n"]
    );
    c.request("disconnect", json!({}));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hit_counts_and_broken_conditions() {
    let src = "for i = 1, 6 do\n  local sq = i * i\nend\n";
    let dir = script("hits", &[("main.rhue", src)]);
    let main = dir.join("main.rhue");
    let mut c = Client::start();
    c.request("initialize", json!({}));
    c.request("launch", json!({ "program": main }));
    c.request(
        "setBreakpoints",
        json!({ "source": { "path": main }, "breakpoints": [{ "line": 2, "hitCondition": ">= 5" }] }),
    );
    c.request("configurationDone", json!({}));
    let mut seen = Vec::new();
    for _ in 0..2 {
        c.event("stopped");
        let ev = c.request(
            "evaluate",
            json!({ "expression": "i", "frameId": 1, "context": "repl" }),
        );
        seen.push(ev["body"]["result"].as_str().unwrap().to_string());
        if seen.len() == 2 {
            // a condition that errors stops (and says why)
            c.request(
                "setBreakpoints",
                json!({ "source": { "path": main }, "breakpoints": [{ "line": 2, "condition": "i.x > 1" }] }),
            );
        }
        c.request("continue", json!({ "threadId": 1 }));
    }
    assert_eq!(seen, ["5", "6"]);
    c.event("exited");
    c.request("disconnect", json!({}));

    // a broken condition from the start
    let mut c = Client::start();
    c.request("initialize", json!({}));
    c.request("launch", json!({ "program": main }));
    c.request(
        "setBreakpoints",
        json!({ "source": { "path": main }, "breakpoints": [{ "line": 2, "condition": "i.x > 1" }] }),
    );
    c.request("configurationDone", json!({}));
    let out = c.event("output");
    assert!(
        out["body"]["output"]
            .as_str()
            .unwrap()
            .contains("breakpoint condition `i.x > 1` failed"),
        "{out}"
    );
    c.event("stopped");
    c.request("disconnect", json!({}));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn edited_modules_reload_mid_session() {
    let dir = script("reload", &[("main.rhue", MAIN), ("util.rhue", UTIL)]);
    let main = dir.join("main.rhue");
    let mut c = Client::start();
    c.request("initialize", json!({ "adapterID": "rhue" }));
    c.event("initialized");
    c.request("launch", json!({ "program": main }));
    c.request(
        "setBreakpoints",
        json!({ "source": { "path": main }, "breakpoints": [{ "line": 5 }] }),
    );
    c.request("configurationDone", json!({}));
    assert_eq!(c.event("stopped")["body"]["reason"], "breakpoint");

    // edit util while stopped, before it's ever called
    std::fs::write(dir.join("util.rhue"), UTIL.replace("x * 2", "x * 3")).unwrap();
    c.request(
        "setBreakpoints",
        json!({ "source": { "path": main }, "breakpoints": [] }),
    );
    c.request("continue", json!({ "threadId": 1 }));

    let mut outputs = Vec::new();
    loop {
        let e = c.event("output");
        let text = e["body"]["output"].as_str().unwrap().to_string();
        let done = text.starts_with("total");
        outputs.push(text);
        if done {
            break;
        }
    }
    assert!(
        outputs.contains(&"reloaded util\n".to_string()),
        "{outputs:?}"
    );
    // all three went through the new double: (1 + 2 + 3) * 3
    assert_eq!(outputs.last().unwrap(), "total\t18\n");
    c.event("terminated");
}
