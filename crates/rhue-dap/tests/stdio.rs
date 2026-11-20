//! The binary itself: framing on stdin and stdout, and exiting cleanly.

use std::io::{BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::json;

#[test]
fn talks_the_protocol_on_stdio() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rhue-dap"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let init = json!({ "seq": 1, "type": "request", "command": "initialize", "arguments": { "adapterID": "rhue" } });
    rhue_dap::write_message(&mut stdin, &init).unwrap();
    let reply = rhue_dap::read_message(&mut stdout).unwrap();
    assert_eq!(reply["command"], "initialize");
    assert_eq!(reply["success"], true);
    let bye = json!({ "seq": 2, "type": "request", "command": "disconnect", "arguments": {} });
    rhue_dap::write_message(&mut stdin, &bye).unwrap();
    stdin.flush().unwrap();
    drop(stdin);
    assert!(child.wait().unwrap().success());
}
