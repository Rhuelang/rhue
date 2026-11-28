use std::io::Write;
use std::process::{Command, Stdio};

fn rhue() -> Command {
    Command::new(env!("CARGO_BIN_EXE_rhue"))
}

fn run(args: &[&str]) -> (bool, String, String) {
    let out = rhue().args(args).output().unwrap();
    (
        out.status.success(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

#[test]
fn evaluates_inline_expression() {
    let (ok, out, _) = run(&["-e", "1 + 2 * 3"]);
    assert!(ok);
    assert_eq!(out, "7\n");
}

#[test]
fn inline_chunk_with_multiple_returns() {
    let (ok, out, _) = run(&["-e", "return 1, 2 ^ 0.5, nil"]);
    assert!(ok);
    assert_eq!(out, "1\t1.4142135623730951\tnil\n");
}

#[test]
fn dump_prints_bytecode_to_stderr() {
    let (ok, out, err) = run(&["--dump", "--no-fold", "-e", "(1 + 2) * 3"]);
    assert!(ok);
    assert_eq!(out, "9\n");
    assert!(err.contains("ADDK") && err.contains("MULK"), "{err}");
}

#[test]
fn runtime_error_exits_nonzero_with_rendered_diagnostic() {
    let (ok, out, err) = run(&["-e", "1 + true"]);
    assert!(!ok);
    assert!(out.is_empty());
    assert!(
        err.contains("attempt to perform arithmetic (add) on boolean"),
        "{err}"
    );
    assert!(err.contains("<command line>"), "{err}");
}

#[test]
fn runs_a_file() {
    let dir = std::env::temp_dir().join(format!("rhue-cli-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("calc.rhue");
    std::fs::write(
        &path,
        "--!strict\n-- area of a 3x4 rectangle, then a power\nreturn 3 * 4, 2 ^ 16\n",
    )
    .unwrap();
    let (ok, out, err) = run(&[path.to_str().unwrap()]);
    assert!(ok, "{err}");
    assert_eq!(out, "12\t65536\n");
}

#[test]
fn repl_reads_lines_from_stdin() {
    let mut child = rhue()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"1 + 1\n\n10 // 3\nreturn 2, 3\n1 +\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "2\n3\n2\t3\n");
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .contains("expected an expression")
    );
}

#[test]
fn repl_keeps_globals_between_lines() {
    let mut child = rhue()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            b"total = 0\nfor i = 1, 4 do total += i end\ntotal\n'sum: ' .. total\nlocal x = 1\nx\ndo local y = 2 end\ny\n",
        )
        .unwrap();
    let out = child.wait_with_output().unwrap();
    // a `local` starting an entry lasts, one inside a block doesn't
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "10\nsum: 10\n1\nnil\n"
    );
}

#[test]
fn reports_every_syntax_error_in_a_file() {
    let dir = std::env::temp_dir().join(format!("rhue-cli-errs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bad.rhue");
    std::fs::write(&path, "local a = \nlocal b = )\n").unwrap();
    let (ok, _, err) = run(&[path.to_str().unwrap()]);
    assert!(!ok);
    assert_eq!(err.matches("Error:").count(), 2, "{err}");
}

#[test]
fn check_reports_type_errors_without_running() {
    let dir = std::env::temp_dir().join(format!("rhue-cli-check-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("typed.rhue");
    std::fs::write(
        &path,
        "local function half(n: number): number\n  return n / 2\nend\nprint('ran')\nlocal s: string = half(4)\nlocal t: number = 'x'\n",
    )
    .unwrap();
    let (ok, out, err) = run(&["--check", path.to_str().unwrap()]);
    assert!(!ok);
    assert_eq!(out, "", "nothing runs");
    assert!(
        err.contains("Type 'number' could not be converted into 'string'"),
        "{err}"
    );
    assert!(
        err.contains("Type '\"x\"' could not be converted into 'number'"),
        "{err}"
    );

    std::fs::write(&path, "local n: number = 1\nprint(n)\n").unwrap();
    let (ok, out, _) = run(&["--check", path.to_str().unwrap()]);
    assert!(ok);
    assert_eq!(out, "");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn check_reads_a_hosts_definitions() {
    let dir = std::env::temp_dir().join(format!("rhue-cli-defs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let defs = dir.join("host.d.rhue");
    std::fs::write(
        &defs,
        "declare class Node\n    Name: string\nend\ndeclare world: Node\n",
    )
    .unwrap();
    let (defs, script) = (defs.to_str().unwrap(), "local n: string = world.Name");
    let (ok, _, err) = run(&["--check", "-e", script]);
    assert!(!ok, "no world without the file");
    assert!(err.contains("Unknown global 'world'"), "{err}");
    let (ok, _, err) = run(&["--check", "--definitions", defs, "-e", script]);
    assert!(ok, "{err}");
    let (ok, _, err) = run(&["--check", "--definitions", defs, "-e", "world.Nope = 1"]);
    assert!(!ok);
    assert!(
        err.contains("Key 'Nope' not found in class 'Node'"),
        "{err}"
    );

    // a bad file says where, and running with one is refused
    let bad = dir.join("bad.d.rhue");
    std::fs::write(&bad, "declare class Node\n    Name: Nope\nend\n").unwrap();
    let (ok, _, err) = run(&["--check", "--definitions", bad.to_str().unwrap(), "-e", "1"]);
    assert!(!ok);
    assert!(
        err.contains("Unknown type 'Nope'") && err.contains("bad.d.rhue:2"),
        "{err}"
    );
    let (ok, _, err) = run(&["--definitions", defs, "-e", "1"]);
    assert!(!ok);
    assert!(
        err.contains("--definitions only goes with --check"),
        "{err}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn strict_flag_checks_before_running() {
    let (ok, _, err) = run(&["-e", "local x: number = 'one'"]);
    assert!(ok, "unchecked by default: {err}");
    let (ok, out, err) = run(&["--strict", "-e", "print('ran') local x: number = 'one'"]);
    assert!(!ok);
    assert_eq!(out, "");
    assert!(err.contains("could not be converted"), "{err}");
    // a file can still opt out
    let (ok, _, _) = run(&["--strict", "-e", "--!nocheck\nlocal x: number = 'one'"]);
    assert!(ok);
}

#[test]
fn require_loads_modules_next_to_the_script() {
    let dir = std::env::temp_dir().join(format!("rhue-cli-require-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("util/shapes")).unwrap();
    std::fs::write(
        dir.join("util/greet.rhue"),
        "--!strict\nlocal M = {}\nfunction M.greet(n: string): string return `hi {n}` end\nreturn M\n",
    )
    .unwrap();
    std::fs::write(dir.join("util/shapes/init.rhue"), "return { sides = 4 }\n").unwrap();
    let main = dir.join("main.rhue");
    std::fs::write(
        &main,
        "--!strict\nlocal g = require('util.greet')\nreturn g.greet('Aalto'), require('util.shapes').sides\n",
    )
    .unwrap();
    let (ok, out, err) = run(&[main.to_str().unwrap()]);
    assert!(ok, "{err}");
    assert_eq!(out, "hi Aalto\t4\n");
    std::fs::write(
        &main,
        "--!strict\nlocal n: number = require('util.greet').greet('x')\n",
    )
    .unwrap();
    let (ok, _, err) = run(&["--check", main.to_str().unwrap()]);
    assert!(!ok);
    assert!(
        err.contains("Type 'string' could not be converted into 'number'"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fmt_formats_in_place_checks_and_reads_stdin() {
    let dir = std::env::temp_dir().join(format!("rhue-cli-fmt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("a.rhue");
    std::fs::write(&file, "local x=1\nif x then\nprint(x)\nend\n").unwrap();
    let path = file.to_str().unwrap();
    let (ok, out, _) = run(&["fmt", "--check", path]);
    assert!(!ok);
    assert!(out.contains("would be reformatted"), "{out}");
    let (ok, _, err) = run(&["fmt", path]);
    assert!(ok, "{err}");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "local x = 1\nif x then\n\tprint(x)\nend\n"
    );
    let (ok, _, _) = run(&["fmt", "--check", path]);
    assert!(ok);
    let (ok, _, _) = run(&["fmt", "--indent", "2", path]);
    assert!(ok);
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("\n  print(x)\n")
    );
    std::fs::write(&file, "local = 1\n").unwrap();
    let (ok, _, err) = run(&["fmt", path]);
    assert!(!ok);
    assert!(err.contains("expected"), "{err}");
    let mut child = rhue()
        .args(["fmt", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"f(a,b)").unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "f(a, b)\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn compile_then_run_the_chunk() {
    let dir = std::env::temp_dir().join(format!("rhue-cli-compile-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("hello.rhue");
    std::fs::write(
        &src,
        "local n = 0\nfor i = 1, 4 do n += i end\nprint('sum', n)\nreturn n * 2\n",
    )
    .unwrap();
    let (ok, _, err) = run(&["compile", src.to_str().unwrap()]);
    assert!(ok, "{err}");
    let chunk = dir.join("hello.rhuec");
    assert!(std::fs::read(&chunk).unwrap().starts_with(b"\x1bRhue"));
    // running the chunk doesn't need the source any more
    std::fs::remove_file(&src).unwrap();
    let (ok, out, err) = run(&[chunk.to_str().unwrap()]);
    assert!(ok, "{err}");
    assert_eq!(out, "sum\t10\n20\n");

    // -o, type errors, and damaged chunks
    let typed = dir.join("typed.rhue");
    std::fs::write(&typed, "--!strict\nlocal n: number = 'x'\n").unwrap();
    let out_path = dir.join("custom.bin");
    let (ok, _, err) = run(&[
        "compile",
        typed.to_str().unwrap(),
        "-o",
        out_path.to_str().unwrap(),
    ]);
    assert!(!ok, "{err}");
    assert!(!out_path.exists());
    let mut bytes = std::fs::read(&chunk).unwrap();
    bytes.truncate(bytes.len() / 2);
    std::fs::write(&chunk, bytes).unwrap();
    let (ok, _, err) = run(&[chunk.to_str().unwrap()]);
    assert!(!ok);
    assert!(
        err.starts_with("error: ") && err.contains("end of the chunk"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Runs the REPL on `input`, returning (success, stdout, stderr).
fn repl(input: &str) -> (bool, String, String) {
    let mut child = rhue()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.success(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

#[test]
fn repl_continues_unfinished_input() {
    let (ok, out, err) = repl(
        "local function fib(n)\n  if n < 2 then return n end\n  return fib(n - 1) + fib(n - 2)\nend\nfib(10)\n\
         local s = [[two\nlines]]\ns\nlocal t = {\n  a = 1,\n}\nt.a\n",
    );
    assert!(ok, "{err}");
    assert_eq!(out, "55\ntwo\nlines\n1\n");
    // a finished entry with an error isn't held open, one that never finishes
    // is reported when the input ends
    let (ok, out, err) = repl("x = )\n1 + 1\nfunction f()\n");
    assert!(!ok);
    assert_eq!(out, "2\n");
    assert!(err.contains("expected an expression, found `)`"), "{err}");
    assert!(err.contains("expected `end` to close `function`"), "{err}");
}

#[test]
fn repl_shows_tables_and_types() {
    let (ok, out, err) = repl(
        "{ 1, 2, name = 'aalto', nested = { ok = true } }\n\
         :type { x = 1, y = 'a' }\n:type vector.create(1, 2)\n:type 1 +\n:help\n:nope\n",
    );
    assert!(!ok, "a :type with an error fails the run");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines[0],
        r#"{ 1, 2, name = "aalto", nested = { ok = true } }"#
    );
    assert_eq!(lines[1], "{ x: number, y: string }");
    assert_eq!(lines[2], "vector");
    assert!(out.contains(":type EXPR"), "{out}");
    assert!(out.contains("unknown command :nope"), "{out}");
    assert!(err.contains("expected an expression"), "{err}");
}

#[test]
fn repl_loads_files_and_quits() {
    let dir = std::env::temp_dir().join(format!("rhue-cli-repl-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("defs.rhue");
    std::fs::write(&file, "greeting = 'hi'\nreturn 'loaded'").unwrap();
    let (ok, out, err) = repl(&format!(
        ":load {}\ngreeting\n:quit\nprint('after')\n",
        file.display()
    ));
    assert!(ok, "{err}");
    assert_eq!(out, "loaded\nhi\n");
    let _ = std::fs::remove_dir_all(dir);
}

/// A folder of test files, and `rhue test` run in it with `args`.
fn rhue_test(name: &str, files: &[(&str, &str)], args: &[&str]) -> (Option<i32>, String, String) {
    let dir = std::env::temp_dir().join(format!("rhue-cli-tests-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (path, text) in files {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    let out = rhue()
        .current_dir(&dir)
        .arg("test")
        .args(args)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.code(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

const MATH: &str = "local M = {}\nfunction M.add(a: number, b: number): number return a + b end\n\
function M.div(a: number, b: number): number\n  if b == 0 then error('division by zero') end\n  return a / b\nend\nreturn M\n";

#[test]
fn test_runner_passes_and_fails() {
    let passing = "--!strict
local mathx = require('lib.mathx')
describe('mathx', function()
  test('adds', function()
    expect(mathx.add(2, 3)).toBe(5)
    expect(mathx.add(0.1, 0.2)).toBeCloseTo(0.3)
    expect(mathx.add(1, 1)).never.toBe(3)
  end)
  test('divides', function()
    expect(function() mathx.div(1, 0) end).toThrow('division by zero')
    expect(function() mathx.div(1, 1) end).never.toThrow()
  end)
end)
test('matchers', function()
  expect({ 1, { a = 2 } }).toEqual({ 1, { a = 2 } })
  expect({ 1, 2 }).never.toBe({ 1, 2 })
  expect('hello world').toContain('wor')
  expect({ 'a', 'b' }).toContain('b')
  expect({ 1, 2, 3 }).toHaveLength(3)
  expect(nil).toBeNil()
  expect(0).toBeTruthy()
  expect(false).toBeFalsy()
  expect(3).toBeGreaterThan(2)
  expect(2).toBeLessThan(3)
  expect(0 / 0).toEqual(0 / 0)
end)
";
    let (code, out, err) = rhue_test(
        "pass",
        &[
            ("lib/mathx.rhue", MATH),
            ("tests/math_test.rhue", passing),
            ("tests/helper.rhue", "error('not a test file')"),
        ],
        &[],
    );
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(out.contains("✓ mathx > adds"), "{out}");
    assert!(out.contains("✓ mathx > divides"), "{out}");
    assert!(out.contains("✓ matchers"), "{out}");
    assert!(out.contains("3 passed in 1 file"), "{out}");

    let failing = "test('ok', function() expect(1).toBe(1) end)
test('wrong sum', function()
  expect(2 + 2).toBe(5)
end)
test('wrong table', function() expect({ x = 1 }).toEqual({ x = 2 }) end)
test('negated', function() expect('abc').never.toContain('b') end)
test('no throw', function() expect(function() end).toThrow() end)
test('plain error', function() local t = nil; return t.x end)
";
    let (code, out, _) = rhue_test("fail", &[("sums_test.rhue", failing)], &[]);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("1 passed, 5 failed in 1 file"), "{out}");
    for message in [
        "expected 4 to be 5",
        "expected { x = 1 } to equal { x = 2 }",
        "expected not \"abc\" to contain \"b\"",
        "expected an error, but it returned",
        "attempt to index nil with 'x'",
    ] {
        assert!(out.contains(message), "{message} in\n{out}");
    }
    // failures point at the test's line, not the matcher's insides
    assert!(out.contains("sums_test.rhue:3:3"), "{out}");
    assert!(!out.contains("<rhue test>"), "{out}");
}

#[test]
fn test_runner_options_and_problems() {
    let tests = "test('alpha', function() end)\ntest('beta', function() error('no') end)\n";
    let (code, out, _) = rhue_test("filter", &[("a_test.rhue", tests)], &["--filter", "alph"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("✓ alpha") && !out.contains("beta"), "{out}");
    // `.test.rhue` counts too, named files run whatever they're called
    let (code, out, _) = rhue_test(
        "names",
        &[("x.test.rhue", tests), ("other.rhue", tests)],
        &["x.test.rhue", "other.rhue"],
    );
    assert_eq!(code, Some(1));
    assert!(out.contains("2 passed, 2 failed in 2 files"), "{out}");
    // a file that can't run fails the run, so does a strict type error
    let (code, out, _) = rhue_test(
        "broken",
        &[
            ("bad_test.rhue", "local x = = 1"),
            ("typed_test.rhue", "--!strict\nexpect(1).toBeCloseTo('x')\n"),
        ],
        &[],
    );
    assert_eq!(code, Some(1));
    assert!(out.contains("2 files couldn't run"), "{out}");
    assert!(out.contains("expected an expression"), "{out}");
    assert!(
        out.contains("could not be converted into 'number'"),
        "{out}"
    );
    // nothing to run is a failure too
    let (code, _, err) = rhue_test("empty", &[("readme.md", "hi")], &[]);
    assert_eq!(code, Some(1));
    assert!(err.contains("no test files"), "{err}");
    let (code, _, err) = rhue_test("missing", &[], &["nope"]);
    assert_eq!(code, Some(2));
    assert!(err.contains("doesn't exist"), "{err}");
}
