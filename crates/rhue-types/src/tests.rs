use rhue_parser::ast::TypeMode;
use rhue_parser::parse_chunk;

use crate::{Env, check_chunk};

fn errors_in(mode: TypeMode, env: &Env, src: &str) -> Vec<String> {
    let chunk = parse_chunk(src).unwrap_or_else(|e| panic!("parse: {}", e[0].message));
    let res = rhue_compiler::resolve::resolve(&chunk).unwrap();
    check_chunk(&chunk, &res, mode, env)
        .errors
        .into_iter()
        .map(|d| d.message)
        .collect()
}

fn strict(src: &str) -> Vec<String> {
    errors_in(TypeMode::Strict, &Env::new(), src)
}

fn nonstrict(src: &str) -> Vec<String> {
    errors_in(TypeMode::NonStrict, &Env::new(), src)
}

#[track_caller]
fn ok(src: &str) {
    let e = strict(src);
    assert!(e.is_empty(), "expected no errors in:\n{src}\ngot: {e:#?}");
}

#[track_caller]
fn err(src: &str, want: &[&str]) {
    assert_eq!(strict(src), want, "in:\n{src}");
}

// ─ Annotations and Inference ─────────────────────────────────────────────────

#[test]
fn annotations_are_checked_in_both_modes() {
    for check in [strict, nonstrict] {
        assert_eq!(
            check("local x: number = 'one'"),
            ["Type '\"one\"' could not be converted into 'number'"]
        );
        assert_eq!(
            check("local a: 'on' | 'off' = 'on' a = 'maybe'"),
            ["Type '\"maybe\"' could not be converted into '\"on\" | \"off\"'"]
        );
        assert!(check("local x: number? = nil local y: string | number = 1").is_empty());
    }
}

#[test]
fn strict_infers_local_types_and_nonstrict_does_not() {
    let src = "local x = 1 x = 'a'";
    assert_eq!(
        strict(src),
        ["Type '\"a\"' could not be converted into 'number'"]
    );
    assert!(nonstrict(src).is_empty());
    // literals widen, `local s = "a"` is a string and not `"a"`
    ok("local s = 'a' s = 'b' local f = true f = false");
    // `local x` / `local x = nil` get filled in later, so `any`
    ok("local x x = 5 x = 'now a string' local y = nil y = {}");
}

#[test]
fn unknown_globals_only_in_strict() {
    assert_eq!(
        strict("print(undefined_thing)"),
        ["Unknown global 'undefined_thing'"]
    );
    assert!(nonstrict("print(undefined_thing)").is_empty());
    // a global the chunk assigns anywhere is known, even before the assignment
    ok("local function f() return later end later = 1");
}

#[test]
fn operators() {
    err(
        "local x = 1 + true",
        &["Type 'true' could not be converted into 'number'"],
    );
    err(
        "local s = 'a' .. {}",
        &["Type '{}' could not be converted into 'string | number'"],
    );
    ok("local s = 'a' .. 1 .. 'b'");
    err(
        "local b = 1 < 'a'",
        &["Types 'number' and '\"a\"' cannot be compared with '<'"],
    );
    ok("local b = 'a' < 'b' local c = 1 <= 2 local d = 1 == 'x'");
    err(
        "local n = #5",
        &["Cannot get the length of a value of type 'number'"],
    );
    ok("local n = #'abc' + #{1, 2}");
    err(
        "local n = -'x'",
        &["Type '\"x\"' could not be converted into 'number'"],
    );
}

// ─ Functions ─────────────────────────────────────────────────────────────────

#[test]
fn parameters_results_and_arity() {
    let src = "local function f(a: number, b: string?): string return (b or '') .. a end
               local s: string = f(1, 'x')
               f(1)
               f('a')
               f(1, 'x', true)
               local n: number = f(1)";
    err(
        src,
        &[
            "Type '\"a\"' could not be converted into 'number'",
            "Argument count mismatch. Function 'f' expects 2 arguments, but 3 are specified",
            "Type 'string' could not be converted into 'number'",
        ],
    );
    err(
        "local function g(a: number, b: number) end g(1)",
        &["Argument count mismatch. Function 'g' expects 2 arguments, but 1 is specified"],
    );
}

#[test]
fn results_are_inferred_in_strict() {
    err(
        "local function half(n: number) return n / 2 end local s: string = half(4)",
        &["Type 'number' could not be converted into 'string'"],
    );
    // a function that can fall off the end might return nil
    err(
        "local function f(x: boolean) if x then return 1 end end local n: number = f(true)",
        &["Type 'number?' could not be converted into 'number'"],
    );
    // non-strict, unannotated results are `any`
    assert!(
        nonstrict("local function half(n: number) return n / 2 end local s: string = half(4)")
            .is_empty()
    );
}

#[test]
fn declared_results_are_enforced() {
    err(
        "local function f(): number if math.random() > 0.5 then return 1 end end",
        &["Not all codepaths in this function return 'number'"],
    );
    ok("local function f(x: number): number if x > 0 then return 1 else return 2 end end");
    ok("local function f(): number error('never returns normally') end");
    err(
        "local function f(): (number, string) return 1 end",
        &["Type pack 'number' could not be converted into '(number, string)'"],
    );
    err(
        "local function f(): number return 'x' end",
        &["Type pack '\"x\"' could not be converted into 'number'"],
    );
    ok("local function f(): () return end local function g(): number? return nil end");
}

#[test]
fn varargs_and_multiple_results() {
    err(
        "local function sum(...: number): number local s = 0 for _, v in {...} do s += v end return s end sum(1, 2, 'x')",
        &["Type '\"x\"' could not be converted into 'number'"],
    );
    ok(
        "local function two(): (number, string) return 1, 'a' end local n: number, s: string = two()",
    );
    err(
        "local function two(): (number, string) return 1, 'a' end local a: string, b: number = two()",
        &[
            "Type 'number' could not be converted into 'string'",
            "Type 'string' could not be converted into 'number'",
        ],
    );
    // results expand into a call's last argument
    ok(
        "local function two(): (number, number) return 1, 2 end local function add(a: number, b: number) return a + b end local n: number = add(two())",
    );
}

#[test]
fn recursion_and_mutual_recursion() {
    err(
        "local function fact(n: number): number if n <= 1 then return 1 end return n * fact(n - 1) end local s: string = fact(5)",
        &["Type 'number' could not be converted into 'string'"],
    );
    err(
        "function isEven(n: number): boolean if n == 0 then return true end return isOdd(n - 1) end
         function isOdd(n: number): boolean if n == 0 then return false end return isEven(n - 1) end
         local s: string = isEven(4)",
        &["Type 'boolean' could not be converted into 'string'"],
    );
}

#[test]
fn generics() {
    err(
        "local function id<T>(x: T): T return x end local n: number = id(5) local s: string = id(5)",
        &["Type 'number' could not be converted into 'string'"],
    );
    err(
        "local function first<T>(xs: {T}): T return xs[1] end local s: string = first({1, 2})",
        &["Type 'number' could not be converted into 'string'"],
    );
    // inside the function T is opaque
    err(
        "local function bad<T>(x: T): T return x + 1 end",
        &[
            "Type 'T' could not be converted into 'number'",
            "Type pack 'number' could not be converted into 'T'",
        ],
    );
    // generic function types and aliases
    ok(
        "type Mapper<A, B> = (A) -> B local m: Mapper<number, string> = function(n) return tostring(n) end local s: string = m(1)",
    );
}

#[test]
fn callbacks_are_typed_by_the_parameter_they_fill() {
    // `a` and `b` are strings here, from `{string}`
    err(
        "local words: {string} = {'b', 'a'} table.sort(words, function(a, b) return a + b end)",
        &[
            "Type 'string' could not be converted into 'number'",
            "Type 'string' could not be converted into 'number'",
            "Type '(string, string) -> number' could not be converted into '((string, string) -> boolean)?'",
        ],
    );
    ok("local words: {string} = {'b', 'a'} table.sort(words, function(a, b) return #a < #b end)");
    err(
        "local f: (number) -> string = function(n: string) return n end",
        &["Type '(string) -> string' could not be converted into '(number) -> string'"],
    );
}

// ─ Tables ────────────────────────────────────────────────────────────────────

#[test]
fn records() {
    err(
        "type Point = {x: number, y: number} local q: Point = {x = 1}",
        &["Type '{ x: number }' could not be converted into 'Point': missing field 'y'"],
    );
    err(
        "type Point = {x: number, y: number} local q: Point = {x = 1, y = 'a'}",
        &[
            "Type '{ x: number, y: \"a\" }' could not be converted into 'Point': field 'y' is '\"a\"', expected 'number'",
        ],
    );
    ok("type P = {x: number, label: string?} local p: P = {x = 1}");
    err(
        "local t = {x = 1, y = 'a'} local z = t.z",
        &["Key 'z' not found in table '{ x: number, y: string }'"],
    );
    err(
        "local p: {x: number} = {x = 1} p.x = 'a' p.w = 1",
        &[
            "Type '\"a\"' could not be converted into 'number'",
            "Cannot add property 'w' to table '{ x: number }'",
        ],
    );
}

#[test]
fn table_literals_grow_through_their_local() {
    err(
        "local M = {} M.x = 1 function M.add(a: number, b: number) return a + b end
         local n: number = M.x local s: string = M.add(1, 2)",
        &["Type 'number' could not be converted into 'string'"],
    );
    // an empty literal fits any table type
    ok("local list: {number} = {} table.insert(list, 5)");
}

#[test]
fn arrays_and_indexers() {
    err(
        "local t: {number} = {} t[1] = 'x' t.x = 1",
        &[
            "Type '\"x\"' could not be converted into 'number'",
            "Cannot add property 'x' to table '{number}'",
        ],
    );
    err(
        "local m: {[string]: number} = {a = 1} local n: number = m.b m['c'] = 'x'",
        &["Type '\"x\"' could not be converted into 'number'"],
    );
    err(
        "local t: {number} = {1, 2} for i, v in ipairs(t) do local s: string = v end",
        &["Type 'number' could not be converted into 'string'"],
    );
    err(
        "local m: {[string]: number} = {} for k, v in pairs(m) do local a: number = k local b: number = v end",
        &["Type 'string' could not be converted into 'number'"],
    );
    ok("local t: {number} = {3, 1} for i, v in t do local n: number = i + v end");
}

// ─ Unions, Nil and Narrowing ─────────────────────────────────────────────────

#[test]
fn optional_values_must_be_checked_in_strict() {
    err(
        "local function f(x: number?) return x + 1 end",
        &["Type 'number?' could not be converted into 'number'"],
    );
    err(
        "local function f(p: {name: string}?) return p.name end",
        &["Value of type '{ name: string }?' could be nil"],
    );
    assert!(nonstrict("local function f(p: {name: string}?) return p.name end").is_empty());
}

#[test]
fn narrowing_in_conditions() {
    ok("local function f(x: number?) if x then return x + 1 end return 0 end");
    ok("local function f(x: number?) if x ~= nil then return x + 1 end return 0 end");
    ok("local function f(x: number?) if x == nil then return 0 else return x + 1 end end");
    ok("local function f(x: number?, y: number?) if x and y then return x + y end return 0 end");
    ok("local function f(x: number?) return x and x + 1 end");
    ok("local function f(x: number?) return (x or 0) + 1 end");
    ok("local function f(x: number?) local n = if x then x else 0 return n + 1 end");
    err(
        "local function f(x: number?) if x then return 0 end return x + 1 end",
        &["Type 'nil' could not be converted into 'number'"],
    );
}

#[test]
fn narrowing_by_type_tests() {
    ok(
        "local function f(x: string | number) if type(x) == 'string' then return #x else return x + 1 end end",
    );
    err(
        "local function f(x: string | number) if type(x) == 'number' then return #x end return 0 end",
        &["Cannot get the length of a value of type 'number'"],
    );
    ok("local function f(v: unknown) if type(v) == 'number' then return v + 1 end return 0 end");
    err(
        "local function f(v: unknown) return v + 1 end",
        &["Type 'unknown' could not be converted into 'number'"],
    );
}

#[test]
fn early_exits_narrow_the_rest_of_the_block() {
    ok("local function f(x: number?) if not x then return 0 end return x + 1 end");
    ok("local function f(x: number?) if x == nil then error('need x') end return x + 1 end");
    ok(
        "local function f(xs: {number?}) for _, x in xs do if not x then continue end local n: number = x end end",
    );
    ok(
        "local function f(x: number?, y: string?) if not x or not y then return end local n = x + #y end",
    );
    ok("local function f(x: number?) assert(x) return x + 1 end");
}

#[test]
fn narrowing_field_paths() {
    ok(
        "local function f(n: {next: {value: number}?}) if n.next then return n.next.value end return 0 end",
    );
    err(
        "local function f(n: {next: {value: number}?}) return n.next.value end",
        &["Value of type '{ value: number }?' could be nil"],
    );
    // assigning the field forgets what was known about it
    err(
        "local function f(n: {v: number?}) if n.v then n.v = nil return n.v + 1 end return 0 end",
        &["Type 'nil' could not be converted into 'number'"],
    );
}

#[test]
fn narrowing_is_forgotten_where_it_could_be_stale() {
    // the loop body runs again after assigning nil
    err(
        "local function f(x: number?) if not x then return end while true do local y = x + 1 x = nil end end",
        &["Type 'number?' could not be converted into 'number'"],
    );
    // a closure could run after `x` changes
    err(
        "local function f(x: number?) if not x then return end return function() return x + 1 end end",
        &["Type 'number?' could not be converted into 'number'"],
    );
}

// ─ Aliases and Casts ─────────────────────────────────────────────────────────

#[test]
fn type_aliases() {
    ok(
        "type List = {value: number, next: List?} local l: List = {value = 1, next = {value = 2}} local n: number = l.value",
    );
    err(
        "type Pair<A, B> = {first: A, second: B} local p: Pair<number> = {first = 1}",
        &["Generic type 'Pair' expects 2 type arguments, but 1 is specified"],
    );
    err("local x: Nope = 1", &["Unknown type 'Nope'"]);
    err(
        "type A = B type B = A",
        &[
            "Type alias 'A' refers to itself without a table or function in between",
            "Type alias 'B' refers to itself without a table or function in between",
        ],
    );
    err(
        "type T = number type T = string",
        &["Redefinition of type 'T'"],
    );
    // aliases are block-scoped, and visible before their declaration
    ok("local p: Later = 1 type Later = number");
    err(
        "do type Inner = number end local x: Inner = 1",
        &["Unknown type 'Inner'"],
    );
}

#[test]
fn recursive_types_compare_structurally() {
    ok("type A = {next: A?} type B = {next: B?} local a: A = {} local b: B = a");
    err(
        "type A = {next: A?, v: number} type B = {next: B?, v: string} local a: A = {v = 1} local b: B = a",
        &["Type 'A' could not be converted into 'B': field 'v' is 'number', expected 'string'"],
    );
}

#[test]
fn casts() {
    ok("local x = (5 :: any) :: string local n = 1 :: number");
    err(
        "local y = ('a' :: number)",
        &["Cannot cast '\"a\"' into 'number' because the types are unrelated"],
    );
    ok("local function f(x: number?) local n = x :: number return n + 1 end");
}

// ─ The Standard Library ──────────────────────────────────────────────────────

#[test]
fn standard_library_types() {
    err(
        "local s = 'abc' local u: string = s:upper() local n: number = s:len() local bad = s:nope()",
        &["Key 'nope' not found in string"],
    );
    err(
        "table.insert({1}, 'x') table.insert({1}, 2, 3)",
        &["No overload of 'table.insert' accepts arguments of type ({number}, \"x\")"],
    );
    err(
        "local n: number = math.floor('x')",
        &["Type '\"x\"' could not be converted into 'number'"],
    );
    ok(
        "local ok, e = pcall(function() error('x') end) local s: string = string.format('%d', 1) local c: number = select('#', 1, 2)",
    );
    ok("local n: number? = tonumber('12') local parts: {string} = string.split('a,b', ',')");
}

// ─ The Environment ───────────────────────────────────────────────────────────

#[test]
fn host_declarations() {
    let mut env = Env::new();
    env.declare_global("add", "(number, number) -> number")
        .unwrap();
    env.declare_property("Player", "name", "string", true)
        .unwrap();
    env.declare_property("Player", "id", "number", false)
        .unwrap();
    env.declare_method("Player", "heal", "(Player, number) -> number")
        .unwrap();
    env.declare_global("hero", "Player").unwrap();
    let errors = errors_in(
        TypeMode::Strict,
        &env,
        "local n: number = add(1, 2) add('a', 2)
         local h: number = hero:heal(5) hero.name = 'x' hero.id = 3 local q = hero.nope
         hero:heal('lots')",
    );
    assert_eq!(
        errors,
        [
            "Type '\"a\"' could not be converted into 'number'",
            "Property 'id' of 'Player' is read-only",
            "Key 'nope' not found in class 'Player'",
            "Type '\"lots\"' could not be converted into 'number'",
        ]
    );
    // host globals keep their declared types
    assert_eq!(
        errors_in(TypeMode::Strict, &env, "add = 5"),
        ["Type 'number' could not be converted into '(number, number) -> number'"]
    );
    // a declaration can mention a host type before it's declared
    env.declare_global("spawn", "(string) -> Enemy").unwrap();
    assert!(env.class("Enemy").is_some());
    assert!(env.declare_global("bad", "(number) ->").is_err());
}

#[test]
fn chunks_export_their_globals() {
    let mut env = Env::new();
    let src = "function greet(n: string): string return 'hi ' .. n end counter = 0";
    let chunk = parse_chunk(src).unwrap();
    let res = rhue_compiler::resolve::resolve(&chunk).unwrap();
    let out = check_chunk(&chunk, &res, TypeMode::Strict, &env);
    assert!(out.errors.is_empty());
    for (name, ty) in out.globals {
        env.define_script_global(&name, ty);
    }
    assert_eq!(
        env.global("greet").unwrap().to_string(),
        "(string) -> string"
    );
    assert_eq!(env.global("counter").unwrap().to_string(), "number");
    assert_eq!(
        errors_in(TypeMode::Strict, &env, "local n: number = greet('x')"),
        ["Type 'string' could not be converted into 'number'"]
    );
    // script globals can be redefined by a later chunk
    assert!(
        errors_in(
            TypeMode::Strict,
            &env,
            "function greet(n: number): number return n end"
        )
        .is_empty()
    );
}

#[test]
fn display_of_types() {
    let mut env = Env::new();
    for (src, shown) in [
        ("number?", "number?"),
        ("((number) -> string)?", "((number) -> string)?"),
        ("{number}", "{number}"),
        (
            "{x: number, [string]: boolean}",
            "{ x: number, [string]: boolean }",
        ),
        (
            "(number, ...string) -> (boolean, string)",
            "(number, ...string) -> (boolean, string)",
        ),
        ("() -> ()", "() -> ()"),
        ("<T>(T) -> T", "<T>(T) -> T"),
        ("\"a\" | \"b\" | nil", "\"a\" | \"b\" | nil"),
        ("true | false", "boolean"),
        ("string | \"a\"", "string"),
    ] {
        env.declare_global("x", src).unwrap();
        assert_eq!(env.global("x").unwrap().to_string(), shown, "{src}");
    }
}

#[test]
fn errors_are_capped_and_nothing_cascades() {
    let src = (0..100)
        .map(|i| format!("local x{i}: number = 'a'\n"))
        .collect::<String>();
    assert_eq!(strict(&src).len(), crate::MAX_TYPE_ERRORS);
    // an unknown type gives one error, its uses don't pile on more
    err(
        "local x: Nope = 1 local y = x + 1 local z = x.field",
        &["Unknown type 'Nope'"],
    );
}

#[test]
fn nocheck_checks_nothing() {
    assert!(
        errors_in(
            TypeMode::NoCheck,
            &Env::new(),
            "local x: number = 'a' print(nope)"
        )
        .is_empty()
    );
}

#[test]
fn pathological_types_terminate() {
    // arguments that grow every level
    strict("type T<A> = {x: T<{A}>?} local a: T<number> = {} local b: T<string> = a");
    // deeply nested types and long unions
    let nested = format!("local x: {}number{} = 1", "{".repeat(200), "}".repeat(200));
    strict(&nested);
    let wide = (0..300)
        .map(|i| format!("\"k{i}\""))
        .collect::<Vec<_>>()
        .join(" | ");
    strict(&format!("local x: {wide} = 'k5' local y: {wide} = x"));
    // long `and` chains are linear, not exponential
    let chain = (0..200)
        .map(|i| format!("x{i}"))
        .collect::<Vec<_>>()
        .join(" and ");
    let decls: String = (0..200)
        .map(|i| format!("local x{i}: number? = 1\n"))
        .collect();
    strict(&format!("{decls}local r = {chain}"));
}
