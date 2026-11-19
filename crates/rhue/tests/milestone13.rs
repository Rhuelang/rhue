//! Milestone 13: compiled chunks saved as bytes and loaded without parsing.

use std::rc::Rc;

use rhue::{Engine, ErrorKind, Proto, Value};

fn programs() -> Vec<(String, String)> {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let mut out = Vec::new();
    for dir in ["examples", "bench"] {
        let mut files: Vec<_> = std::fs::read_dir(format!("{root}/{dir}"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "rhue"))
            .collect();
        files.sort();
        for f in files {
            out.push((
                f.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read_to_string(&f).unwrap(),
            ));
        }
    }
    assert!(out.len() > 10);
    out
}

#[test]
fn every_example_round_trips_exactly() {
    for (name, src) in programs() {
        let mut e = Engine::new();
        let proto = e.compile(&name, &src).unwrap();
        let bytes = proto.to_bytes();
        let back = Proto::from_bytes(&bytes).unwrap();
        assert_eq!(*back, *proto, "{name}");
        // smaller than the source plus its code would be in memory
        assert!(bytes.len() < src.len() * 6, "{name}: {} bytes", bytes.len());
        assert_eq!(back.to_bytes(), bytes, "{name}");
    }
}

#[test]
fn loaded_chunks_run_like_compiled_ones() {
    let src = r#"
        local Counter = {}
        Counter.__index = Counter
        function Counter.new(start) return setmetatable({ n = start }, Counter) end
        function Counter:bump(by) self.n += by or 1 return self end
        local c = Counter.new(10)
        for i = 1, 5 do c:bump(i) end
        local words = {}
        for w in string.gmatch("the quick brown fox", "%a+") do table.insert(words, w:upper()) end
        local gen = coroutine.wrap(function() for i = 1, 3 do coroutine.yield(i * i) end end)
        total = c.n
        return c.n, table.concat(words, ","), gen() + gen() + gen(), `{#words} words`, 0.1 + 0.2
    "#;
    let mut compiler = Engine::new();
    let bytes = compiler.compile_to_bytes("counter.rhue", src).unwrap();
    let expected = compiler.eval_named("counter.rhue", src).unwrap();

    // a fresh engine runs the bytes, as often as asked
    let mut e = Engine::new();
    let proto = e.load(&bytes).unwrap();
    for _ in 0..3 {
        let got = e.run(&proto).unwrap();
        assert_eq!(format!("{got:?}"), format!("{expected:?}"));
    }
    assert_eq!(e.get::<f64>("total").unwrap(), 25.0);
}

#[test]
fn errors_in_loaded_chunks_point_at_the_source() {
    let mut e = Engine::new();
    let bytes = e
        .compile_to_bytes(
            "boom.rhue",
            "local t = {}\nlocal function f()\n  return t.x.y\nend\nreturn f()\n",
        )
        .unwrap();
    let mut fresh = Engine::new();
    let proto = fresh.load(&bytes).unwrap();
    let err = fresh.run(&proto).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Runtime);
    assert_eq!(err.source_name(), "boom.rhue");
    let shown = err.render(false);
    assert!(shown.contains("boom.rhue:3"), "{shown}");
    assert!(shown.contains("return t.x.y"), "{shown}");
}

#[test]
fn compiling_still_checks_types() {
    let mut e = Engine::new();
    let err = e
        .compile_to_bytes("t.rhue", "--!strict\nlocal n: number = 'x'\n")
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Type);
}

#[test]
fn bad_bytes_are_refused() {
    let mut e = Engine::new();
    let bytes = e
        .compile_to_bytes("t.rhue", "local x = 1 return function() return x end")
        .unwrap();

    let err = e.load(b"print('hi')").unwrap_err();
    assert_eq!(err.kind, ErrorKind::Load);
    assert!(
        err.to_string().contains("not a compiled Rhue chunk"),
        "{err}"
    );

    let mut other = bytes.clone();
    other[rhue_bytecode::MAGIC.len()] = 99;
    let err = e.load(&other).unwrap_err();
    assert!(err.to_string().contains("format 99"), "{err}");

    // every truncation is an error, never a panic
    for n in 0..bytes.len() {
        assert_eq!(
            e.load(&bytes[..n]).unwrap_err().kind,
            ErrorKind::Load,
            "{n}"
        );
    }
    let mut longer = bytes.clone();
    longer.push(0);
    assert!(
        e.load(&longer)
            .unwrap_err()
            .to_string()
            .contains("trailing")
    );
    assert!(e.load(&bytes).is_ok());
}

#[test]
fn the_verifier_guards_loading() {
    let mut e = Engine::new();
    let proto = e
        .compile("t.rhue", "local a, b = 1, 2 return a + b")
        .unwrap();
    let edit = |f: &dyn Fn(&mut Proto)| {
        let mut p = (*proto).clone();
        f(&mut p);
        p.to_bytes()
    };
    let mut e = Engine::new();
    // a register beyond the frame
    let bad = edit(&|p| p.max_stack = 1);
    let err = e.load(&bad).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Load);
    assert!(err.to_string().contains("compiled chunk rejected"), "{err}");
    // debug info the debugger would trust
    let bad = edit(&|p| p.locals[0].reg = 200);
    assert!(
        e.load(&bad)
            .unwrap_err()
            .to_string()
            .contains("debug info for local `a`")
    );
    let bad = edit(&|p| p.locals[1].end_pc = 999);
    assert!(e.load(&bad).is_err());
    // spans beyond the text are refused while reading
    let bad = edit(&|p| p.source = rhue::SourceFile::new("t.rhue", "x"));
    assert!(
        e.load(&bad)
            .unwrap_err()
            .to_string()
            .contains("span outside")
    );
    // and `run` verifies hand-made prototypes the same way
    let mut p = (*proto).clone();
    p.locals[0].reg = 200;
    assert_eq!(e.run(&Rc::new(p)).unwrap_err().kind, ErrorKind::Compile);
}

#[test]
fn loaded_chunks_work_with_the_rest_of_the_engine() {
    let mut e = Engine::new();
    e.add_module("greet", "return function(n) return 'hi ' .. n end");
    let bytes = e
        .compile_to_bytes(
            "main.rhue",
            "local greet = require('greet') return greet(name)",
        )
        .unwrap();
    let mut fresh = Engine::new();
    fresh.add_module("greet", "return function(n) return 'hello ' .. n end");
    fresh.set_global("name", "ada");
    let proto = fresh.load(&bytes).unwrap();
    assert_eq!(fresh.run(&proto).unwrap(), vec![Value::from("hello ada")]);
}

/// `examples/tables.rhue` (the README's first example) stopped passing its own
/// `--!strict` once class methods got a typed `self` (M9). In a plain table's
/// method `self` is usually an object the methods got copied into, so it's open
/// (unknown fields are `any`), but known fields keep their types.
#[test]
fn self_in_a_plain_tables_methods_is_open() {
    let mut e = Engine::new();
    let ok = "--!strict
local Inventory = {}
function Inventory.new() return { items = {}, count = 0, add = Inventory.add } end
function Inventory:add(name: string) self.items[name] = true self.count += 1 return self end
local M = { n = 1 }
function M:double(): number return self.n * 2 end
return M:double()";
    e.check("ok.rhue", ok).unwrap();
    let bad = "--!strict\nlocal M = { n = 1 }\nfunction M:f() return self.n .. {} end\n";
    assert_eq!(e.check("bad.rhue", bad).unwrap_err().kind, ErrorKind::Type);
}

#[test]
fn every_strict_example_type_checks() {
    for (name, src) in programs().into_iter().filter(|(n, _)| !n.contains("bench")) {
        if std::path::Path::new(&name).extension().is_some() && src.starts_with("--!strict") {
            let mut e = Engine::new();
            e.on_print(|_| {});
            e.check(&name, &src)
                .unwrap_or_else(|err| panic!("{}", err.render(false)));
        }
    }
}

#[test]
fn hooks_can_evaluate_expressions_in_a_frame() {
    use rhue::{DebugEvent, HookMask};
    use std::cell::RefCell;
    let seen = Rc::new(RefCell::new(Vec::<String>::new()));
    let log = seen.clone();
    let mut e = Engine::new();
    e.set_global("scale", 10.0);
    e.set_debug_hook(
        HookMask {
            line: true,
            ..HookMask::default()
        },
        move |dbg, event| {
            if event != DebugEvent::Line(5) {
                return Ok(());
            }
            let mut show = |level: usize, src: &str| {
                let shown = match dbg.eval(level, src) {
                    Ok(vs) => vs
                        .iter()
                        .map(|v| v.to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                    Err(e) => format!("error: {e}"),
                };
                log.borrow_mut().push(format!("{src} => {shown}"));
            };
            show(0, "x * scale + offset"); // a local, a global, an upvalue
            show(0, "#items, items[2]");
            show(0, "string.format('%s=%d', name, x)");
            show(1, "x"); // the caller's `x`
            show(0, "items[1] = 99"); // a statement: tables are shared
            show(0, "x = 1000"); // locals are copies
            show(0, "nope.field");
            show(0, "1 +");
            Ok(())
        },
    );
    let out = e
        .eval(
            "local offset = 0.5
local function f(x, name)
  local items = { 'a', 'b', offset }
  local x = x + 1
  return items[1], x
end
local x = 'outer'
local a, b = f(2, 'n') -- not a tail call: this frame stays
return a, b",
        )
        .unwrap();
    assert_eq!(out, vec![Value::Number(99.0), Value::Number(3.0)]);
    let seen = seen.borrow();
    assert_eq!(seen[0], "x * scale + offset => 30.5");
    assert_eq!(seen[1], "#items, items[2] => 3, b");
    assert_eq!(seen[2], "string.format('%s=%d', name, x) => n=3");
    assert_eq!(seen[3], "x => outer");
    assert_eq!(seen[4], "items[1] = 99 => ");
    assert_eq!(seen[5], "x = 1000 => ");
    assert!(
        seen[6].starts_with("nope.field => error: ") && seen[6].contains("attempt to index nil"),
        "{}",
        seen[6]
    );
    assert!(seen[7].starts_with("1 + => error: "), "{}", seen[7]);
}
