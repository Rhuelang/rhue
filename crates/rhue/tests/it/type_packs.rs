//! Milestone 14: generic type packs and default type parameters, flow typing
//! for locals declared without a value, and types that survive saving (compiled
//! chunks and modules).

use rhue::{Engine, ErrorKind};

fn check(src: &str) -> Result<(), Vec<String>> {
    let mut e = Engine::new();
    e.check("t.rhue", &format!("--!strict\n{src}"))
        .map_err(|err| {
            let shown = err.render(false);
            shown
                .lines()
                .filter_map(|l| l.strip_prefix("Error: ").map(String::from))
                .collect()
        })
}

#[track_caller]
fn ok(src: &str) {
    if let Err(errs) = check(src) {
        panic!("expected no errors, got {errs:?}\nin:\n{src}");
    }
}

#[track_caller]
fn fails(src: &str, needle: &str) {
    match check(src) {
        Ok(()) => panic!("expected an error containing {needle:?}\nin:\n{src}"),
        Err(errs) => assert!(
            errs.iter().any(|e| e.contains(needle)),
            "expected an error containing {needle:?}, got {errs:?}"
        ),
    }
}

fn type_of(src: &str, name: &str) -> String {
    let mut e = Engine::new();
    let a = e.analyze(src, None);
    let at = src.find(name).unwrap() as u32;
    a.types
        .iter()
        .find(|(s, _)| s.start == at && s.end == at + name.len() as u32)
        .map(|(_, t)| t.clone())
        .unwrap_or_else(|| panic!("no type for {name}"))
}

const PASS: &str = "local function pass<T...>(...: T...): T... return ... end\n";

#[test]
fn packs_carry_argument_types_through() {
    ok(&format!("{PASS}local a: number, b: string = pass(1, 'x')"));
    ok(&format!("{PASS}local n: number = select('#', pass())"));
    fails(
        &format!("{PASS}local c: string = pass(1)"),
        "could not be converted",
    );
    fails(
        &format!("{PASS}local a: number, b: number = pass(1, 'x')"),
        "could not be converted into 'number'",
    );
    assert_eq!(type_of(PASS, "pass"), "<T...>(T...) -> T...");
}

#[test]
fn packs_link_a_callback_to_its_arguments() {
    let call = "local function call<A..., R...>(f: (A...) -> R..., ...: A...): R...\n  return f(...)\nend\nlocal function add(a: number, b: number): number return a + b end\n";
    ok(&format!("{call}local n: number = call(add, 1, 2)"));
    fails(
        &format!("{call}local n = call(add, 1, 'x')"),
        "could not be converted into 'number'",
    );
    fails(
        &format!("{call}local s: string = call(add, 1, 2)"),
        "'number' could not be converted into 'string'",
    );
    // wrapping keeps the whole signature
    let wrap = "local function wrap<A..., R...>(f: (A...) -> R...): (A...) -> R... return f end\nlocal g = wrap(function(x: number): string return tostring(x) end)\n";
    ok(&format!("{wrap}local s: string = g(1)"));
    fails(
        &format!("{wrap}local s = g('x')"),
        "could not be converted into 'number'",
    );
    assert_eq!(type_of(wrap, "g"), "(number) -> string");
}

#[test]
fn packs_in_type_aliases() {
    let cb = "type Callback<A...> = (A...) -> ()\n";
    ok(&format!(
        "{cb}local f: Callback<number, string> = function(a: number, b: string) end"
    ));
    ok(&format!(
        "{cb}local f: Callback<(number, string)> = function(a: number, b: string) end"
    ));
    ok(&format!("{cb}local f: Callback<()> = function() end\nf()"));
    ok(&format!(
        "{cb}local f: Callback<...number> = function(...: number) end\nf(1, 2, 3)"
    ));
    fails(
        &format!("{cb}local f: Callback<number> = function(a: string) end"),
        "could not be converted",
    );
    fails(
        &format!("{cb}local f: Callback<number> = function(a: number) end\nf('x')"),
        "could not be converted into 'number'",
    );
    let mixed = "type Fn<T, R...> = (T) -> R...\nlocal f: Fn<number, (string, boolean)> = function(n: number) return 'x', true end\n";
    ok(&format!("{mixed}local s: string, b: boolean = f(1)"));
    fails(
        &format!("{mixed}local n: number = f(1)"),
        "could not be converted into 'number'",
    );
    // inside a generic function, an alias can be given its packs
    ok(
        "type Callback<A...> = (A...) -> ()\nlocal function each<A...>(cb: Callback<A...>, ...: A...) cb(...) end\neach(function(a: number, b: string) end, 1, 'x')",
    );
}

#[test]
fn default_type_parameters() {
    let bx = "type Box<T = number> = { value: T }\n";
    ok(&format!("{bx}local b: Box = {{ value = 1 }}"));
    ok(&format!("{bx}local b: Box<string> = {{ value = 'x' }}"));
    fails(
        &format!("{bx}local b: Box = {{ value = 'x' }}"),
        "could not be converted",
    );
    // a default can name an earlier parameter
    let pair = "type Pair<A, B = A> = { a: A, b: B }\n";
    ok(&format!(
        "{pair}local p: Pair<string> = {{ a = 'x', b = 'y' }}"
    ));
    ok(&format!(
        "{pair}local p: Pair<string, number> = {{ a = 'x', b = 1 }}"
    ));
    fails(
        &format!("{pair}local p: Pair<string> = {{ a = 'x', b = 1 }}"),
        "could not be converted",
    );
    fails(
        &format!("{pair}local p: Pair = {{ a = 1, b = 1 }}"),
        "expects at least 1 type argument",
    );
    fails(
        &format!("{pair}local p: Pair<number, number, number> = {{ a = 1, b = 1 }}"),
        "expects at most 2 type arguments",
    );
    // packs can have defaults too
    let h = "type Handler<A... = (string)> = (A...) -> ()\n";
    ok(&format!(
        "{h}local f: Handler = function(s: string) end\nf('x')"
    ));
    fails(
        &format!("{h}local f: Handler = function(s: string) end\nf(1)"),
        "could not be converted",
    );
    ok(&format!(
        "{h}local f: Handler<number> = function(n: number) end\nf(1)"
    ));
    // a bad default is reported even if no use needs it
    fails("type Bad<T = Nope> = { value: T }", "Unknown type 'Nope'");
}

#[test]
fn packs_and_defaults_are_used_correctly() {
    fails(
        "local function f<T>(...: T...) end",
        "'T' is a type, not a type pack",
    );
    fails(
        "local function f<T...>(x: T) end",
        "Type pack 'T' used where a type is expected",
    );
    fails(
        "local function f(...: U...) end",
        "Unknown type pack 'U...'",
    );
    fails(
        "type F<T...> = { value: T }",
        "Type pack 'T' used where a type is expected",
    );
    fails(
        "type F<T> = (T) -> ()\nlocal f: F<(number, string)>",
        "is a type, not a type pack",
    );
    let parse = |src: &str| Engine::new().check("t.rhue", src).unwrap_err().to_string();
    assert!(parse("local function f<T = number>() end").contains("only allowed in type aliases"));
    assert!(parse("type F<T = number, U> = T").contains("need defaults too"));
    assert!(parse("type F<T..., U> = T").contains("must come before type packs"));
}

#[test]
fn generic_code_still_runs() {
    let mut e = Engine::new();
    let out = e
        .eval_as::<(f64, String, f64)>(
            "--!strict
local function pass<T...>(...: T...): T... return ... end
local function call<A..., R...>(f: (A...) -> R..., ...: A...): R... return f(...) end
type Box<T = number> = { value: T }
local b: Box = { value = 41 }
local a, s = pass(1, 'two')
return a + 0, s, call(function(x: number, y: number) return x + y end, b.value, 1)",
        )
        .unwrap();
    assert_eq!(out, (1.0, "two".to_string(), 42.0));
}

#[test]
fn the_formatter_keeps_packs_tight() {
    let src = "type Callback<A..., R... = ()> = (A...) -> R...\nlocal function pass<T...>(...: T...): T...\n\treturn ...\nend\ntype Box<T = number> = { value: T }\n";
    let out = rhue_fmt::format(src, &rhue_fmt::FormatOptions::default()).unwrap();
    assert_eq!(out, src);
}

#[test]
fn compile_errors_are_type_errors() {
    let mut e = Engine::new();
    let err = e
        .check("t.rhue", "--!strict\nlocal function f<T...>(x: T) end")
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Type);
}

#[test]
fn locals_without_a_value_follow_their_assignments() {
    ok("local x\nx = 1\nlocal n: number = x + 1");
    ok("local x\nif math.random() > 0.5 then x = 1 else x = 2 end\nlocal n: number = x");
    ok(
        "local x\nif math.random() > 0.5 then x = 'a' else x = 'b' end\nlocal s: string = x:upper()",
    );
    // not assigned on every path: could still be nil
    fails(
        "local x\nif math.random() > 0.5 then x = 1 end\nlocal n: number = x",
        "could not be converted",
    );
    // ...until something rules nil out
    ok(
        "local x\nif math.random() > 0.5 then x = 1 end\nif x == nil then x = 0 end\nlocal n: number = x",
    );
    ok(
        "local x\nif math.random() > 0.5 then x = 1 end\nif not x then return end\nlocal n: number = x",
    );
    // a union of everything assigned
    fails(
        "local x\nif math.random() > 0.5 then x = 1 else x = 'a' end\nlocal n: number = x",
        "could not be converted",
    );
    ok("local x\nif math.random() > 0.5 then x = 1 else x = 'a' end\nlocal v: number | string = x");
    // branches that leave don't count
    ok("local x\nif math.random() > 0.5 then x = 1 else error('no') end\nlocal n: number = x");
    ok("local x\ndo x = 'inner' end\nlocal s: string = x");
    // read before any assignment: it's nil
    fails("local x\nlocal n: number = x + 1", "nil");
    // assigning different things is allowed (it's how the type is built)
    ok("local x\nx = 1\nx = 'two'\nlocal s: string = x");
}

#[test]
fn flow_locals_stay_safe_where_one_pass_cant_see() {
    // loops: after one, it may not have run
    fails(
        "local last\nfor i = 1, 3 do last = i end\nlocal n: number = last",
        "could not be converted",
    );
    ok("local last\nfor i = 1, 3 do last = i end\nlocal n: number = last or 0");
    // inside the loop, an earlier iteration may have set it: `any`
    ok(
        "local prev\nfor i = 1, 3 do\n  if prev then local s: string = tostring(prev + i) end\n  prev = i\nend",
    );
    // closures: reading it inside one is `any` (it may run later)...
    ok("local cb\nlocal function run() cb() end\ncb = function() end\nrun()");
    // ...and a local a closure assigns is plain `any` from then on
    ok("local x\nlocal function set() x = 'late' end\nx = 1\nset()\nlocal s: string = x");
    // tables grow fields after they're assigned: `any`
    ok("local t\nt = {}\nt.count = 1\nlocal n: number = t.count");
    // `local x = nil` is still the gradual "fill it in later"
    ok("local x = nil\nx = 1\nx = 'a'");
}

#[test]
fn flow_locals_in_non_strict_files_are_any() {
    let mut e = Engine::new();
    e.check(
        "t.rhue",
        "--!nonstrict\nlocal x\nx = 1\nlocal s: string = x",
    )
    .unwrap();
}

#[test]
fn hovering_a_flow_local_shows_what_it_can_hold() {
    let src =
        "--!strict\nlocal value\nif math.random() > 0.5 then value = 1 else value = 'a' end\n";
    assert_eq!(type_of(src, "value"), "nil | number | string");
}

#[test]
fn narrowings_now_survive_if_statements() {
    // declared locals benefit too: both branches assign a number
    ok(
        "local x: number? = nil\nif math.random() > 0.5 then x = 1 else x = 2 end\nlocal n: number = x",
    );
    ok("local x: number? = nil\nif x == nil then x = 5 end\nlocal n: number = x");
    fails(
        "local x: number? = nil\nif math.random() > 0.5 then x = 1 end\nlocal n: number = x",
        "could not be converted",
    );
}

const COUNTER: &str = "--!strict
local Counter = {}
Counter.__index = Counter
function Counter.new(start: number)
  return setmetatable({ n = start }, Counter)
end
function Counter:bump(by: number?)
  self.n += by or 1
  return self
end
function Counter:get(): number return self.n end
local function pass<T...>(...: T...): T... return ... end
return { new = Counter.new, pass = pass, version = '1.0' }
";

#[test]
fn compiled_modules_keep_their_types() {
    let bytes = Engine::new().compile_to_bytes("counter", COUNTER).unwrap();
    let mut e = Engine::new();
    e.add_module_bytes("counter", &bytes).unwrap();
    let use_it = "--!strict
local counter = require('counter')
local c = counter.new(10)
local n: number = c:bump(5):get()
local a: number, b: string = counter.pass(1, 'x')
local v: string = counter.version
return n, a, b, v";
    e.check("main.rhue", use_it)
        .unwrap_or_else(|err| panic!("{}", err.render(false)));
    // and it runs, without the module's source anywhere
    let out = e.eval_named("main.rhue", use_it).unwrap();
    assert_eq!(
        format!("{out:?}"),
        r#"[Number(15.0), Number(1.0), String("x"), String("1.0")]"#
    );
    // misuse is caught as if the source were there
    for (bad, needle) in [
        (
            "local counter = require('counter')\ncounter.new('ten')",
            "could not be converted into 'number'",
        ),
        (
            "local counter = require('counter')\nlocal s: string = counter.new(1):get()",
            "could not be converted into 'string'",
        ),
        ("local counter = require('counter')\ncounter.nope()", "nope"),
    ] {
        let err = e
            .check("bad.rhue", &format!("--!strict\n{bad}"))
            .unwrap_err();
        assert!(
            err.render(false).contains(needle),
            "{bad}: {}",
            err.render(false)
        );
    }
}

#[test]
fn loaded_chunks_declare_their_globals() {
    let lib =
        "--!strict\nfunction greet(name: string): string return 'hi ' .. name end\nVERSION = 3\n";
    let bytes = Engine::new().compile_to_bytes("lib.rhue", lib).unwrap();
    let mut e = Engine::new();
    let proto = e.load(&bytes).unwrap();
    // known to the checker as soon as it's loaded...
    e.check(
        "a.rhue",
        "--!strict\nlocal s: string = greet('ada')\nlocal n: number = VERSION",
    )
    .unwrap();
    let err = e.check("b.rhue", "--!strict\ngreet(1)").unwrap_err();
    assert!(
        err.render(false)
            .contains("could not be converted into 'string'")
    );
    // ...and defined once it runs
    e.run(&proto).unwrap();
    assert_eq!(e.eval_as::<String>("return greet('bo')").unwrap(), "hi bo");
}

#[test]
fn untyped_chunks_load() {
    let mut e = Engine::new();
    let chunk = e.compile("old.rhue", "return 1 + 1").unwrap();
    let mut fresh = Engine::new();
    let p = fresh.load(&chunk.to_bytes()).unwrap();
    assert_eq!(fresh.run(&p).unwrap(), vec![rhue::Value::Number(2.0)]);
    // a compiled module without types is `any` to the checker
    let untyped = Engine::new()
        .compile("m", "return { f = function(x: number) return x end }")
        .unwrap()
        .to_bytes();
    e.add_module_bytes("m", &untyped).unwrap();
    e.check(
        "t.rhue",
        "--!strict\nlocal m = require('m')\nm.f('anything')",
    )
    .unwrap();
}

#[test]
fn saved_types_that_dont_resolve_are_any() {
    // a host type the loading engine doesn't have
    let mut host = Engine::new();
    host.declare("spawn", "(Vec3) -> ()").unwrap_or(());
    let lib = "--!strict\nfunction place(p: Vec3) spawn(p) end\n";
    let bytes = host.compile_to_bytes("lib", lib);
    if let Ok(bytes) = bytes {
        let mut e = Engine::new();
        e.load(&bytes).unwrap();
        e.check("t.rhue", "--!strict\nplace(5)").unwrap();
    }
}
