//! Milestone 15: performance. The VM changes (shared upvalue cells, a constants
//! tree per chunk, `CLOSURE` out of line) can't change behaviour, the checker's
//! join merge has to stay linear, plus the checker fixes that turned up while
//! profiling it.

use rhue::Engine;

fn check(src: &str) -> Result<(), String> {
    Engine::new()
        .check("t.rhue", &format!("--!strict\n{src}"))
        .map_err(|e| e.render(false))
}

#[test]
fn closures_share_upvalues_and_constants() {
    let mut e = Engine::new();
    let out = e
        .eval_as::<(f64, f64, String, f64)>(
            "
local function counter()
  local n = 0
  return function() n += 1 return n end, function() return n end
end
local inc, get = counter()
inc() inc()
local makers = {}
for i = 1, 3 do
  -- each closure has its own `i`, and the same string constants
  makers[i] = function(s) return s .. ':' .. i end
end
local co = coroutine.wrap(function()
  local x = 10
  local function bump() x += 1 return x end
  coroutine.yield(bump())
  return bump() -- the upvalue was open in the coroutine, now closed
end)
co()
return get(), inc(), makers[2]('k') .. makers[3]('k'), co()",
        )
        .unwrap();
    assert_eq!(out, (2.0, 3.0, "k:2k:3".to_string(), 12.0));
}

#[test]
fn closures_survive_collections() {
    let mut e = Engine::new();
    e.set_gc_stress(true);
    let out = e
        .eval_as::<String>(
            "
local fs = {}
for i = 1, 50 do
  local tag = 'item' .. i
  fs[i] = function(suffix) return tag .. suffix .. '!' end
  collectgarbage()
end
local parts = {}
for i = 1, 50, 7 do table.insert(parts, fs[i]('-x')) end
return table.concat(parts, ',')",
        )
        .unwrap();
    assert_eq!(
        out,
        "item1-x!,item8-x!,item15-x!,item22-x!,item29-x!,item36-x!,item43-x!,item50-x!"
    );
}

#[test]
fn a_branchs_condition_still_counts_at_the_join() {
    // `x` isn't changed in the first branch, but its condition says it's a
    // number there, the second branch assigns one
    check("local x: number? = nil\nif x ~= nil then print(x) else x = 0 end\nlocal n: number = x")
        .unwrap();
    check("local x: number? = nil\nif x == nil then x = 0 end\nlocal n: number = x").unwrap();
    assert!(check("local x: number? = nil\nif math.random() > 0.5 then print(x) else x = 0 end\nlocal n: number = x").is_err());
    // nested ifs report what they changed to the outer one
    check("local x: number? = nil\nif math.random() > 0.5 then\n  if math.random() > 0.5 then x = 1 else x = 2 end\nelse\n  x = 3\nend\nlocal n: number = x").unwrap();
}

#[test]
fn falling_back_to_an_empty_table() {
    check(
        "local function f(tags: {string}?)
  for _, t in tags or {} do local s: string = t end
end",
    )
    .unwrap();
    check(
        "local function f(m: {[string]: number}?)
  for k, v in m or {} do local n: number = v end
end",
    )
    .unwrap();
    check(
        "local function f(a: {number}, b: {string})
  for _, v in if math.random() > 0.5 then a else b do local s: number | string = v end
end",
    )
    .unwrap();
    let err = check(
        "local function f(tags: {string}?)
  for _, t in tags or {} do local n: number = t end
end",
    )
    .unwrap_err();
    assert!(
        err.contains("'string' could not be converted into 'number'"),
        "{err}"
    );
    let err = check(
        "local function f(a: {number}, b: {string})
  for _, v in if math.random() > 0.5 then a else b do local n: number = v end
end",
    )
    .unwrap_err();
    assert!(
        err.contains("could not be converted into 'number'"),
        "{err}"
    );
}

#[test]
fn long_functions_check_in_linear_time() {
    // 300 narrowed locals and 3,000 `if`s in one function: merging every live
    // narrowing at every join (as M14 did) took over 30 times longer
    let mut src = String::from("--!strict\nlocal function f(xs: {number?})\n");
    for i in 0..300 {
        src += &format!("  local v{i}: number? = xs[{}]\n", i + 1);
    }
    for i in 0..3000 {
        let j = i % 300;
        src += &format!("  if v{j} == nil then v{j} = {i} end\n  local s{i}: number = v{j} + 1\n");
    }
    src += "end\nreturn f\n";
    let start = std::time::Instant::now();
    Engine::new().check("long.rhue", &src).unwrap();
    let took = start.elapsed();
    // release: ~40 ms (it was ~1.5 s), debug builds are ~10x slower
    assert!(took.as_secs_f64() < 5.0, "took {took:?}");
}
