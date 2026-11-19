//! Milestone 16: types II. Read-only and write-only properties, flow typing
//! through loops, the fields methods add to `self`, and recursive types that
//! survive exporting and saving.

use rhue::Engine;

fn check(src: &str) -> Result<(), String> {
    Engine::new()
        .check("t.rhue", &format!("--!strict\n{src}"))
        .map_err(|e| e.render(false))
}

#[track_caller]
fn ok(src: &str) {
    if let Err(e) = check(src) {
        panic!("expected no errors:\n{e}\nin:\n{src}");
    }
}

#[track_caller]
fn fails(src: &str, needle: &str) {
    match check(src) {
        Ok(()) => panic!("expected an error containing {needle:?}\nin:\n{src}"),
        Err(e) => assert!(e.contains(needle), "expected {needle:?} in:\n{e}"),
    }
}

#[test]
fn read_only_and_write_only_properties() {
    let decl = "type Account = { read id: number, write audit: string, balance: number }\n\
                local function f(a: Account)\n";
    ok(&format!(
        "{decl}  local n: number = a.id + a.balance\n  a.audit = 'seen'\n  a.balance = 1\nend"
    ));
    fails(
        &format!("{decl}  a.id = 2\nend"),
        "Property 'id' is read-only",
    );
    fails(
        &format!("{decl}  print(a.audit)\nend"),
        "Property 'audit' is write-only",
    );
    fails(&format!("{decl}  a.id += 1\nend"), "read-only");
    // `read`/`write` are only keywords before a property name
    ok("type T = { read: number, write: string }\nlocal function f(t: T) t.read = 1 end");
    let err = Engine::new()
        .check("t.rhue", "type T = { read [string]: number }")
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("apply to properties, not indexers")
    );
}

#[test]
fn access_follows_variance() {
    // read-only is covariant: a narrower type can be read where a wider one is
    // expected
    ok(
        "type Shape = { read kind: string }\nlocal function show(s: Shape) end\nshow({ kind = 'circle' })\nlocal c: { kind: \"circle\" } = { kind = 'circle' }\nshow(c)",
    );
    // write-only is contravariant: a sink for `string?` takes a string
    ok(
        "type Sink = { write value: string }\nlocal function fill(s: Sink) s.value = 'x' end\nlocal t: { value: string? } = { value = nil }\nfill(t)",
    );
    fails(
        "type Sink = { write value: string? }\nlocal function fill(s: Sink) s.value = nil end\nlocal t: { value: string } = { value = 'x' }\nfill(t)",
        "could not be converted",
    );
    // a read-only value can't be passed where it would be written
    fails(
        "type Ro = { read n: number }\nlocal function bump(t: { n: number }) t.n += 1 end\nlocal function f(r: Ro) bump(r) end",
        "field 'n' is read-only",
    );
    fails(
        "type Wo = { write n: number }\nlocal function show(t: { read n: number }) end\nlocal function f(w: Wo) show(w) end",
        "field 'n' is write-only",
    );
    // read-write can be narrowed to either
    ok(
        "local function f(t: { n: number })\n  local r: { read n: number } = t\n  local w: { write n: number } = t\nend",
    );
}

#[test]
fn access_shows_in_types() {
    let mut e = Engine::new();
    let src = "--!strict\nlocal acct: { read id: number, write audit: string } = { id = 1, audit = '' }\n";
    let a = e.analyze(src, None);
    let at = src.find("acct").unwrap() as u32;
    let t = a
        .types
        .iter()
        .find(|(s, _)| s.start == at)
        .unwrap()
        .1
        .clone();
    assert_eq!(t, "{ read id: number, write audit: string }");
    let fmt = rhue_fmt::format(
        "type T = {read   id:number,write  x: string}\n",
        &rhue_fmt::FormatOptions::default(),
    )
    .unwrap();
    assert_eq!(fmt, "type T = { read id: number, write x: string }\n");
}

#[test]
fn flow_locals_are_typed_inside_loops() {
    // an earlier iteration assigned a number: inside the loop it's `number?`,
    // not `any`
    fails(
        "local prev\nfor i = 1, 3 do\n  if prev then local s: string = prev end\n  prev = i\nend",
        "could not be converted into 'string'",
    );
    ok(
        "local prev\nfor i = 1, 3 do\n  if prev then local n: number = prev + i end\n  prev = i\nend",
    );
    fails(
        "local prev\nfor i = 1, 3 do\n  local n: number = prev + i\n  prev = i\nend",
        "'number?' could not be converted",
    );
    // a value built from itself settles
    ok("local acc\nfor i = 1, 10 do acc = (acc or 0) + i end\nlocal n: number = acc or 0");
    // types an iteration can leave, whatever order they're assigned in
    ok(
        "local v\nwhile math.random() > 0.1 do\n  if v == nil then v = 0 else v = tostring(v) end\nend\nlocal w: nil | number | string = v",
    );
    fails(
        "local v\nwhile math.random() > 0.1 do\n  if v == nil then v = 0 else v = tostring(v) end\nend\nlocal w: number? = v",
        "could not be converted",
    );
    // repeat-until and generic for, and loops inside loops
    ok("local last\nrepeat last = 'x' until last ~= nil\nlocal s: string = last");
    ok(
        "local found\nfor _, v in {1, 2, 3} do\n  if v > 1 then found = v break end\nend\nif found then local n: number = found end",
    );
    fails(
        "local best\nfor i = 1, 3 do\n  for j = 1, 3 do\n    if best then local s: string = best end\n    best = i * j\n  end\nend",
        "could not be converted into 'string'",
    );
    // after the loop, it may never have run
    fails(
        "local last\nfor i = 1, 3 do last = i end\nlocal n: number = last",
        "could not be converted",
    );
}

#[test]
fn silent_passes_leave_no_trace() {
    // one error, reported once (not once per pass), and hover types aren't
    // duplicated
    let src = "--!strict\nlocal prev\nfor i = 1, 3 do\n  local bad: string = 1\n  prev = i\nend\n";
    let err = Engine::new()
        .check("t.rhue", src)
        .unwrap_err()
        .render(false);
    assert_eq!(err.matches("could not be converted").count(), 1, "{err}");
    let a = Engine::new().analyze(src, None);
    let at = src.find("bad").unwrap() as u32;
    assert_eq!(a.types.iter().filter(|(s, _)| s.start == at).count(), 1);
    let decl = src.find("prev").unwrap() as u32;
    let t = &a.types.iter().find(|(s, _)| s.start == decl).unwrap().1;
    assert_eq!(t, "number?");
}

const CACHE: &str = "local Cache = {}
Cache.__index = Cache
function Cache.new(size: number)
  return setmetatable({ size = size }, Cache)
end
function Cache:fill()
  self.items = {}
  self.label = 'cache of ' .. self.size
end
";

#[test]
fn methods_add_fields_to_self() {
    // later methods (and code using instances) see them, as optional: something
    // may read them before `fill` has run
    ok(&format!(
        "{CACHE}function Cache:describe(): string\n  return self.label or 'empty'\nend"
    ));
    fails(
        &format!("{CACHE}function Cache:describe(): string\n  return self.label\nend"),
        "could not be converted",
    );
    fails(
        &format!("{CACHE}function Cache:count(): number\n  return self.label + 1\nend"),
        "string",
    );
    ok(&format!(
        "{CACHE}local c = Cache.new(3)\nc:fill()\nif c.label then local s: string = c.label end"
    ));
    // within the method that assigns it, it's known
    ok(
        "local K = {}\nK.__index = K\nfunction K.new() return setmetatable({}, K) end\nfunction K:init()\n  self.n = 1\n  local m: number = self.n + 1\nend",
    );
    // constructor fields keep their exact, checked types
    fails(
        &format!("{CACHE}function Cache:shrink() self.size = 'small' end"),
        "could not be converted into 'number'",
    );
    // only the class's methods widen it, other code has to fit
    fails(
        &format!("{CACHE}local c = Cache.new(3)\nc:fill()\nc.label = 5"),
        "could not be converted into 'string?'",
    );
    ok(&format!(
        "{CACHE}local c = Cache.new(3)\nc.label = 'x'\nc.label = nil"
    ));
    // a field assigned different things holds any of them
    ok(
        "local K = {}\nK.__index = K\nfunction K.new() return setmetatable({}, K) end\nfunction K:a() self.v = 1 end\nfunction K:b() self.v = 'x' end\nfunction K:c(): number | string | nil return self.v end",
    );
}

const SHAPES: &str = "--!strict
type Ring = { value: number, next: () -> Ring }
type List<T> = { head: T, tail: List<T>? }
local V = {}
V.__index = V
function V.new(x: number, y: number)
  return setmetatable({ x = x, y = y }, V)
end
function V.__add(a, b) return V.new(a.x + b.x, a.y + b.y) end
function V:scale(k: number) return V.new(self.x * k, self.y * k) end
function V:len(): number return math.sqrt(self.x * self.x + self.y * self.y) end
local function ring(): Ring
  local r: Ring
  r = { value = 1, next = function() return r end }
  return r
end
local function list<T>(x: T): List<T> return { head = x, tail = nil } end
return { V = V, ring = ring, list = list }
";

/// Same checks, against the module as source and as saved bytes.
fn engines() -> Vec<(&'static str, Engine)> {
    let mut source = Engine::new();
    source.add_module("shapes", SHAPES);
    let bytes = Engine::new().compile_to_bytes("shapes", SHAPES).unwrap();
    let mut saved = Engine::new();
    saved.add_module_bytes("shapes", &bytes).unwrap();
    vec![("source", source), ("saved", saved)]
}

#[test]
fn recursive_types_survive_requiring() {
    for (how, mut e) in engines() {
        let check = |e: &mut Engine, body: &str| {
            e.check(
                "main.rhue",
                &format!("--!strict\nlocal shapes = require('shapes')\n{body}"),
            )
            .map_err(|err| err.render(false))
        };
        // deep enough that unrolling a few levels (as before) gave `any`
        check(&mut e, "local r = shapes.ring()\nlocal n: number = r.next().next().next().next().next().next().next().value")
            .unwrap_or_else(|err| panic!("{how}: {err}"));
        let err = check(&mut e, "local r = shapes.ring()\nlocal s: string = r.next().next().next().next().next().next().next().value")
            .unwrap_err();
        assert!(
            err.contains("could not be converted into 'string'"),
            "{how}: {err}"
        );
        // generic recursive aliases
        check(&mut e, "local l = shapes.list('a')\nif l.tail and l.tail.tail then local s: string = l.tail.tail.head end")
            .unwrap_or_else(|err| panic!("{how}: {err}"));
        assert!(
            check(
                &mut e,
                "local l = shapes.list('a')\nlocal n: number = l.head"
            )
            .is_err(),
            "{how}"
        );
    }
}

#[test]
fn classes_keep_methods_and_operators() {
    for (how, mut e) in engines() {
        let check = |e: &mut Engine, body: &str| {
            e.check(
                "main.rhue",
                &format!("--!strict\nlocal shapes = require('shapes')\nlocal V = shapes.V\n{body}"),
            )
            .map_err(|err| err.render(false))
        };
        check(&mut e, "local a = V.new(1, 2) + V.new(3, 4)\nlocal n: number = a:scale(2):scale(2):scale(2):scale(2):scale(2):scale(2):len()")
            .unwrap_or_else(|err| panic!("{how}: {err}"));
        let err = check(
            &mut e,
            "local s: string = (V.new(1, 2) + V.new(1, 1)):len()",
        )
        .unwrap_err();
        assert!(
            err.contains("could not be converted into 'string'"),
            "{how}: {err}"
        );
        let err = check(&mut e, "V.new(1, 2):scale('x')").unwrap_err();
        assert!(
            err.contains("could not be converted into 'number'"),
            "{how}: {err}"
        );
        let err = check(&mut e, "V.new(1, 2):nope()").unwrap_err();
        assert!(err.contains("nope"), "{how}: {err}");
        // and it all runs
        let out = e
            .eval_as::<f64>(
                "local V = require('shapes').V\nreturn (V.new(3, 0) + V.new(0, 4)):scale(2):len()",
            )
            .unwrap();
        assert_eq!(out, 10.0, "{how}");
    }
}

#[test]
fn exported_types_read_as_written() {
    let bytes = Engine::new().compile_to_bytes("shapes", SHAPES).unwrap();
    let (_, types) = rhue_bytecode::read_chunk(&bytes).unwrap();
    let types = types.unwrap();
    let all = types.aliases.join("\n");
    assert!(
        all.contains("type Ring = { value: number, next: () -> Ring }"),
        "{all}"
    );
    assert!(
        all.contains("type List<T> = { head: T, tail: List<T>? }"),
        "{all}"
    );
    assert!(
        all.contains("type V = setmetatable<{ x: number, y: number }, VClass>"),
        "{all}"
    );
    assert!(all.contains("__add: (any, any) -> V"), "{all}");
    assert!(all.contains("scale: (V, number) -> V"), "{all}");
    assert_eq!(
        types.returns,
        "{ V: VClass, ring: () -> Ring, list: <T>(T) -> List<T> }"
    );
}

#[test]
fn exported_names_dont_collide() {
    // a user alias already called `CounterClass`, and two classes named
    // `Counter` in different scopes
    let src = "--!strict
type CounterClass = { label: string }
local function make()
  local Counter = {}
  Counter.__index = Counter
  function Counter.new() return setmetatable({ n = 1 }, Counter) end
  return Counter
end
local Counter = {}
Counter.__index = Counter
function Counter.new() return setmetatable({ s = 'x' }, Counter) end
local tag: CounterClass = { label = 'a' }
return { a = make().new(), b = Counter.new(), tag = tag }
";
    let mut e = Engine::new();
    e.add_module("m", src);
    e.check("main.rhue", "--!strict\nlocal m = require('m')\nlocal n: number = m.a.n\nlocal s: string = m.b.s\nlocal l: string = m.tag.label")
        .unwrap_or_else(|err| panic!("{}", err.render(false)));
    let err = e
        .check(
            "bad.rhue",
            "--!strict\nlocal m = require('m')\nlocal n: number = m.b.s",
        )
        .unwrap_err();
    assert!(err.render(false).contains("could not be converted"));
}

#[test]
fn setmetatable_type_function() {
    ok(
        "type P = { x: number }\ntype PClass = { __index: PClass, norm: (P) -> number }\ntype Pt = setmetatable<P, PClass>\nlocal function f(p: Pt): number return p:norm() + p.x end",
    );
    fails("type Pt = setmetatable<number, {}>", "needs a table type");
    fails("type Pt = setmetatable<{}>", "takes two type arguments");
}

#[test]
fn older_chunk_formats_still_load() {
    use rhue_bytecode::{MAGIC, read_chunk, write_chunk};
    let src =
        "--!strict\nlocal M = {}\nfunction M.twice(n: number): number return n * 2 end\nreturn M";
    let bytes = Engine::new().compile_to_bytes("m", src).unwrap();
    let (proto, types) = read_chunk(&bytes).unwrap();
    let mut types = types.unwrap();
    types.aliases.clear();
    // format 2 is format 3 without the alias list: drop its (zero) count
    let with = write_chunk(&proto, Some(&types));
    let without = write_chunk(&proto, None);
    let header = with
        .iter()
        .zip(&without)
        .take_while(|(a, b)| a == b)
        .count();
    let proto_len = without.len() - header - 1;
    let mut v2 = with.clone();
    assert_eq!(v2.remove(with.len() - proto_len - 1), 0);
    v2[MAGIC.len()] = 2;
    let mut v1 = without;
    v1[MAGIC.len()] = 1;
    v1.remove(header); // format 1 has no types flag at all

    for (version, bytes) in [(2, v2), (1, v1)] {
        let mut e = Engine::new();
        e.add_module_bytes("m", &bytes)
            .unwrap_or_else(|err| panic!("v{version}: {err}"));
        assert_eq!(
            e.eval_as::<f64>("return require('m').twice(21)").unwrap(),
            42.0
        );
        let typed = e.check(
            "main.rhue",
            "--!strict\nlocal s: string = require('m').twice(1)",
        );
        assert_eq!(typed.is_err(), version == 2, "v{version}");
    }
    let mut v9 = bytes.clone();
    v9[MAGIC.len()] = 9;
    let err = Engine::new().add_module_bytes("m", &v9).unwrap_err();
    assert!(err.to_string().contains("formats 1 to 4"), "{err}");
}

#[test]
fn import_falls_back_when_a_type_is_missing() {
    // types naming something the loading engine lacks (a host type, say) don't
    // stop the module loading, they fall back to `any`
    use rhue_bytecode::{read_chunk, write_chunk};
    let src = "--!strict\nlocal M = {}\nfunction M.len(n: number): number return n end\nreturn M";
    let bytes = Engine::new().compile_to_bytes("m", src).unwrap();
    let (proto, types) = read_chunk(&bytes).unwrap();
    let mut types = types.unwrap();
    types.aliases = vec!["type P = { at: HostVec3 }".into()];
    types.returns = "{ len: (P) -> number }".into();
    let mut e = Engine::new();
    e.add_module_bytes("m", &write_chunk(&proto, Some(&types)))
        .unwrap();
    assert_eq!(e.eval_as::<f64>("return require('m').len(3)").unwrap(), 3.0);
    e.check("main.rhue", "--!strict\nlocal n = require('m').len(3)")
        .unwrap_or_else(|err| panic!("{}", err.render(false)));
}
