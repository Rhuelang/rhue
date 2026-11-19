//! Milestone 22: performance III. Table constructors with constant keys build
//! their tables from a template (`DUPTABLE`), `table.move` copies array runs in
//! one go, and so does `table.create` when filling. Programs should never be
//! able to tell templates exist, so these tests pin that down, plus the new
//! library function.

use std::rc::Rc;

use rhue::{Engine, Value};
use rhue_bytecode::{Constant, Op, Proto, TableTemplate, read_chunk, write_chunk};

fn run(src: &str) -> String {
    let mut e = Engine::new();
    let vals: Vec<Value> = e
        .eval_as::<rhue::Variadic<Value>>(src)
        .unwrap_or_else(|err| panic!("{src}\n{}", err.render(false)))
        .0;
    vals.iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(" | ")
}

fn error(src: &str) -> String {
    Engine::new()
        .eval(src)
        .unwrap_err()
        .diagnostic()
        .message
        .clone()
}

/// Keys in `pairs` order, as one string.
const KEYS: &str = "local function keys(t) local o = {} for k in pairs(t) do table.insert(o, tostring(k)) end return table.concat(o, ',') end\n";

fn ops(p: &Proto) -> Vec<Op> {
    p.code.iter().filter_map(|i| i.op()).collect()
}

#[test]
fn constructors_with_constant_keys_use_templates() {
    let mut e = Engine::new();
    let p = e
        .compile(
            "t",
            "local a = { x = 1, y = 2 }\nlocal b = { 1, 2, 3 }\nreturn a, b",
        )
        .unwrap()
        .proto()
        .clone();
    assert_eq!(p.templates.len(), 1);
    assert_eq!(p.templates[0].array, 0);
    assert_eq!(p.templates[0].hash, 2);
    let names: Vec<String> = p.templates[0]
        .keys
        .iter()
        .map(|&k| match &p.constants[k as usize] {
            Constant::String(s) => s.to_string(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(names, ["x", "y"]);
    let ops = ops(&p);
    assert_eq!(ops.iter().filter(|&&o| o == Op::DupTable).count(), 1);
    assert_eq!(ops.iter().filter(|&&o| o == Op::NewTable).count(), 1);
    assert!(
        p.disassemble()
            .contains("DUPTABLE  R0 T0           ; {x, y}")
    );
}

#[test]
fn templates_are_invisible() {
    // values, `#`, `next` and `pairs` order are what the plain path gives
    assert_eq!(
        run(&format!(
            "{KEYS}local p = {{ x = 1, y = 2, [1] = 'a', z = 3 }}\nreturn keys(p), p.x, p.y, p.z, #p"
        )),
        "1,x,y,z | 1 | 2 | 3 | 1"
    );
    // a field set to nil isn't stored, so it goes in where it's first assigned,
    // as without a template
    assert_eq!(
        run(&format!(
            "{KEYS}local p = {{ x = 1, y = nil, z = 3 }}\np.w = 0\np.y = 2\nreturn keys(p), next({{ a = nil }})"
        )),
        "x,z,w,y | nil"
    );
    // a computed key ends the template, so later constant keys still go in
    // after it
    assert_eq!(
        run(&format!(
            "{KEYS}local k = 'mid'\nlocal p = {{ a = 1, [k] = 2, b = 3 }}\nreturn keys(p)"
        )),
        "a,mid,b"
    );
    // a key given twice keeps its first place and its last value
    assert_eq!(
        run(&format!(
            "{KEYS}local p = {{ x = 1, y = 2, x = 3 }}\nreturn keys(p), p.x"
        )),
        "x,y | 3"
    );
    // many keys (past the small linear form), nested constructors, and values
    // that come from calls and branches
    let mut many = String::from("local p = {");
    for i in 0..20 {
        many.push_str(&format!(" f{i} = {i},"));
    }
    many.push_str(" }\nlocal s = 0 for _, v in pairs(p) do s += v end\n");
    assert_eq!(
        run(&format!("{KEYS}{many}return s, keys(p):sub(1, 11), p.f19")),
        "190 | f0,f1,f2,f3 | 19"
    );
    assert_eq!(
        run(
            "local function f() return 5 end\nlocal c = true\nlocal p = { a = { b = { c = f() } }, d = c and 'yes' or 'no', e = f() + 1 }\nreturn p.a.b.c, p.d, p.e"
        ),
        "5 | yes | 6"
    );
    // each constructor call makes a new table
    assert_eq!(
        run(
            "local function new(v) return { v = v } end\nlocal a, b = new(1), new(2)\na.v = 10\nreturn a.v, b.v, a ~= b"
        ),
        "10 | 2 | true"
    );
    // a metatable set afterwards sees nothing odd: the template's nil slots
    // don't stop __newindex from firing for fields not given a value
    assert_eq!(
        run(
            "local log = {}\nlocal p = setmetatable({ x = 1, y = nil }, { __newindex = function(t, k, v) table.insert(log, k) rawset(t, k, v) end })\np.y = 2\np.x = 3\nreturn table.concat(log, ','), p.x, p.y"
        ),
        "y | 3 | 2"
    );
}

#[test]
fn templated_tables_can_be_frozen_cloned_and_cleared() {
    assert_eq!(
        run(
            "local p = table.freeze({ a = 1, b = 2 })\nlocal ok = pcall(function() p.a = 5 end)\nlocal c = table.clone(p)\nc.a = 7\nlocal d = { a = 1, b = 2 }\ntable.clear(d)\nreturn ok, p.a, c.a, c.b, next(d)"
        ),
        "false | 1 | 7 | 2 | nil"
    );
}

#[test]
fn templates_survive_saved_chunks() {
    let mut e = Engine::new();
    let bytes = e
        .compile_to_bytes(
            "m",
            "return { point = { x = 1, y = 2 }, sum = function(p) return p.x + p.y end }",
        )
        .unwrap();
    let (proto, _) = read_chunk(&bytes).unwrap();
    assert_eq!(proto.templates.len(), 2);
    assert_eq!(
        write_chunk(&proto, None),
        write_chunk(&read_chunk(&write_chunk(&proto, None)).unwrap().0, None)
    );
    e.add_module_bytes("m", &bytes).unwrap();
    assert_eq!(
        e.eval_as::<f64>("local m = require('m') return m.sum(m.point)")
            .unwrap(),
        3.0
    );
}

#[test]
fn the_verifier_checks_templates() {
    let mut e = Engine::new();
    let good = e
        .compile("t", "return { x = 1, y = 2 }")
        .unwrap()
        .proto()
        .clone();
    let broken = |change: &dyn Fn(&mut Proto)| {
        let mut p = (*good).clone();
        change(&mut p);
        rhue_bytecode::verify(&p).unwrap_err().to_string()
    };
    let msg = broken(&|p| p.templates.clear());
    assert!(msg.contains("no table template T0"), "{msg}");
    let msg = broken(&|p| p.templates[0].keys[1] = p.templates[0].keys[0]);
    assert!(msg.contains("a key appears twice"), "{msg}");
    let msg = broken(&|p| {
        p.constants.push(Constant::Number(1.0));
        p.templates[0].keys[0] = p.constants.len() as u32 - 1;
    });
    assert!(msg.contains("isn't a string constant"), "{msg}");
    let msg = broken(&|p| p.templates[0].keys[1] = 999);
    assert!(msg.contains("isn't a string constant"), "{msg}");
    let msg = broken(&|p| p.templates[0].hash = 1);
    assert!(msg.contains("fewer hash slots than keys"), "{msg}");
    let msg = broken(&|p| p.templates[0].array = u32::MAX);
    assert!(msg.contains("too large"), "{msg}");
    // a hand-made template the compiler wouldn't write still runs
    let mut p = (*good).clone();
    p.templates.push(TableTemplate {
        array: 4,
        hash: 9,
        keys: vec![],
    });
    rhue_bytecode::verify(&p).unwrap();
    assert_eq!(
        e.run(&rhue::Chunk::from_proto(Rc::new(p))).unwrap().len(),
        1
    );
}

#[test]
fn table_move() {
    let show = "local function show(t, n) local o = {} for i = 1, n do o[i] = tostring(t[i]) end return table.concat(o, ' ') end\n";
    let cases = [
        // overlapping moves inside one table, both directions
        (
            "local a = {1,2,3,4,5} table.move(a, 1, 3, 3) return show(a, 5), #a",
            "1 2 1 2 3 | 5",
        ),
        (
            "local a = {1,2,3,4,5} table.move(a, 2, 5, 1) return show(a, 5), #a",
            "2 3 4 5 5 | 5",
        ),
        // into another table: appending, overwriting, extending
        (
            "local b = table.move({1,2,3}, 1, 3, 1, {}) return show(b, 3), #b",
            "1 2 3 | 3",
        ),
        (
            "local b = table.move({1,2,3}, 1, 3, 4, {9,9,9}) return show(b, 6), #b",
            "9 9 9 1 2 3 | 6",
        ),
        (
            "local b = table.move({1,2,3}, 2, 3, 2, {7,7,7,7}) return show(b, 4)",
            "7 2 3 7",
        ),
        // holes are copied as holes, nil at the end shortens the array
        (
            "local b = table.move({1,nil,3}, 1, 3, 1, {7,7,7}) return show(b, 3)",
            "1 nil 3",
        ),
        (
            "local a = {1,2,3} table.move({}, 1, 2, 2, a) return show(a, 3), #a",
            "1 nil nil | 1",
        ),
        // keys outside the array part: zero, negatives, the hash part
        (
            "local c = table.move({1,2,3}, 1, 3, 0, {}) return c[0], c[1], c[2], #c",
            "1 | 2 | 3 | 2",
        ),
        (
            "local s = { [-1] = 'a', [0] = 'b' } local c = table.move(s, -1, 0, 1, {}) return c[1], c[2]",
            "a | b",
        ),
        (
            "local s = { [10] = 'x', [11] = 'y' } local c = table.move(s, 10, 11, 1, {}) return show(c, 2), #c",
            "x y | 2",
        ),
        // an empty range moves nothing, the destination comes back
        (
            "local d = {5} return table.move({1,2}, 1, 0, 1, d) == d, show(d, 1)",
            "true | 5",
        ),
        (
            "local a = {1} return table.move(a, 1, 1, 2) == a, #a",
            "true | 2",
        ),
        // raw: metamethods aren't consulted
        (
            "local log = 0 local d = setmetatable({}, { __newindex = function() log += 1 end }) table.move({1,2}, 1, 2, 1, d) return log, d[2]",
            "0 | 2",
        ),
    ];
    for (src, want) in cases {
        assert_eq!(run(&format!("{show}{src}")), want, "{src}");
    }
    assert!(error("table.move(table.freeze({1}), 1, 1, 2)").contains("readonly"));
    assert!(error("table.move({1}, 1, 1, 1, table.freeze({}))").contains("readonly"));
    assert!(error("table.move({}, 1, 2^40, 1)").contains("too many elements"));
    assert!(error("table.move({}, 1, 3, 2^53)").contains("wrap around"));
    assert!(error("table.move({}, 1, 1.5, 1)").contains("integer representation"));
    assert!(error("table.move(1, 1, 1, 1)").contains("table expected"));
    let mut e = Engine::new();
    e.check(
        "t.rhue",
        "--!strict\nlocal a: {number} = table.move({1, 2}, 1, 2, 1, {})\nlocal b: {string} = table.move({'x'}, 1, 1, 2)",
    )
    .unwrap();
    assert!(
        e.check(
            "t.rhue",
            "--!strict\nlocal a: {string} = table.move({1}, 1, 1, 1)"
        )
        .is_err()
    );
}

#[test]
fn table_create_fills() {
    assert_eq!(
        run(
            "local t = table.create(3, 'x') local u = table.create(4) t[4] = 'y' return table.concat(t, ','), #u, next(u), #table.create(0, 1)"
        ),
        "x,x,x,y | 0 | nil | 0"
    );
    assert!(error("table.create(-1)").contains("size out of range"));
}
