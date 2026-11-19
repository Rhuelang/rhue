<div align="center">
  <img src="https://raw.githubusercontent.com/rhuelang/rhue/main/assets/Logo.svg" width="180" alt="Rhue logo">
  <h1>Rhue</h1>
  <p>Luau-style scripting for Rust, compiled to <b>register bytecode</b>.</p>
  <p>
    <a href="https://crates.io/crates/rhue"><img src="https://img.shields.io/crates/v/rhue?style=for-the-badge&amp;color=e07b4f" alt="rhue on crates.io"></a>
    <a href="https://docs.rs/rhue"><img src="https://img.shields.io/docsrs/rhue?style=for-the-badge" alt="docs.rs"></a>
    <a href="https://github.com/rhuelang/rhue/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/rhuelang/rhue/ci.yml?branch=main&amp;style=for-the-badge&amp;label=ci" alt="CI status"></a>
    <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.88%2B-e07b4f?style=for-the-badge&amp;logo=rust" alt="Rust 1.88 or newer"></a>
    <a href="https://luau.org"><img src="https://img.shields.io/badge/syntax-luau-e07b4f?style=for-the-badge" alt="Luau syntax"></a>
    <a href="LICENSE-MIT"><img src="https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue?style=for-the-badge" alt="MIT or Apache-2.0"></a>
  </p>
</div>

I write a lot of Luau and a lot of Rust, and wanted the first one inside the second, so here we are.

Rhue looks like Luau (same syntax, same `--!strict` gradual typing), embeds about as easily as Rhai, and compiles to register bytecode for its own VM. Nothing gets tree-walked.

The site's at [rhue.dev](https://rhue.dev) and the API docs are on [docs.rs](https://docs.rs/rhue).

## ⚡️ Quick start

```toml
[dependencies]
rhue = "1"
```

```rust
use rhue::Engine;

fn main() -> Result<(), rhue::Error> {
    let mut engine = Engine::new();
    engine.register("shout", |s: String| s.to_uppercase() + "!");

    let out: String = engine.eval_as(r#"return shout("hello from rhue")"#)?;
    println!("{out}"); // HELLO FROM RHUE!
    Ok(())
}
```

The CLI comes with a REPL:

```bash
cargo install rhue-cli
rhue
```

Or from a clone, run one of the example scripts:

```bash
$ cargo run -q -p rhue-cli -- examples/tables.rhue
3 kinds, total $5.00
brown, fox, jumps, quick, the
longest:	5
42	->	42
abc	->	error: examples/tables.rhue:49: not a number: abc
heap after churn: 82 KB
```

## ✨ Status

1.0.0 is out, 26 milestones in. What's in it:

- **Language** — Luau syntax, string interpolation, metatables and `setmetatable` classes, tail calls, coroutines, generic `for` with `__iter`, vectors
- **Types** — gradual typing with `--!strict` and `--!nonstrict`, generics and type packs, narrowing and flow typing, read-only and write-only properties
- **Standard library** — `table`, `string` with Lua patterns, `math`, `utf8`, a sandbox-safe `os`, `bit32`, `buffer`, `vector`, `debug`, frozen tables
- **Embedding** — typed `register`, `#[derive(UserData)]`, calling scripts from Rust, natives that pause scripts, the `task` library, separate environments for plugins, `serde`, instruction and memory limits
- **Async** — async Rust functions scripts can wait on, `eval_async`, `task.wait` on real time, stopping scripts from any thread
- **Hot reload** — swap a module for new code while scripts run, functions change in place and state carries over
- **Modules and chunks** — `require` with typed modules, saved bytecode that goes through a verifier before it runs
- **Tooling** — the `rhue` CLI (REPL, `rhue test`, `rhue fmt`, compiling to bytecode), a language server, a debug adapter
- **Speed** — inline caches, table templates, fast paths for builtins. Luau's interpreter is still 1.1–3.3× faster, Rhai is 4–15× slower

Full history's in [`CHANGELOG.md`](CHANGELOG.md), and the why behind things is in [`docs/DESIGN.md`](docs/DESIGN.md).

### 🗺️ Planned

1. **Faster calls, again** — still where Luau's furthest ahead

## 🔤 Types

```lua
--!strict
type Item = { name: string, price: number, tags: {string}? }

local function total(items: {Item}): number
    local sum = 0
    for _, item in items do
        sum += item.price
    end
    return sum
end

local function firstTag(item: Item): string
    if not item.tags then        -- without this check: "Value of type '{string}?' could be nil"
        return "untagged"
    end
    return item.tags[1]
end
```

```bash
$ rhue --check examples/typed.rhue     # type-check only
$ rhue --strict script.rhue            # check every file strictly, then run
```

Types get checked when a chunk compiles, and a chunk with type errors doesn't run. No directive means no checking (`Engine::set_default_type_mode` changes that). Types never change how code runs, and anything the checker accepts without an `:: any` can't hit a type error at runtime. There's a property test throwing tens of thousands of random programs at that.

Type packs and defaults work too:

```lua
--!strict
local function call<A..., R...>(f: (A...) -> R..., ...: A...): R...
    return f(...)
end
local n: number = call(math.max, 1, 5)      -- call(math.max, "x") is an error

type Callback<A...> = (A...) -> ()
type Box<T = number> = { value: T }
local onHit: Callback<string, number> = function(who, damage) end
local b: Box = { value = 1 }                -- Box<number>
```

In strict mode, `local x` with no value gets its type from whatever's assigned to it later, through branches and loops. `local x = nil` is still `any`, for when you just want to fill it in later.

Read-only and write-only properties work like Luau's:

```lua
type Config = { read name: string, write log: (string) -> () }
local function setup(c: Config)
    print(c.name)
    c.name = "x"             -- error: Property 'name' is read-only
end
```

Rust functions you register show up to the checker with types from their signatures, so `|a: f64, b: Option<String>| -> Vec<String>` is `(number, string?) -> {string}`. `#[derive(UserData)]` types become typed classes, and `engine.declare("name", "type")` covers anything else.

## 🧩 Classes and metatables

```lua
--!strict
local Account = {}
Account.__index = Account

function Account.new(owner: string, balance: number)
    return setmetatable({ owner = owner, balance = balance }, Account)
end

function Account:deposit(n: number)
    self.balance += n                 -- `self` is typed: `self.balance = "x"` is an error
end

function Account.__add(a, b)          -- operators, `__eq`/`__lt`/`__le`, `__len`, `__concat`, `__call`, `__iter`...
    return Account.new(`{a.owner}&{b.owner}`, a.balance + b.balance)
end

Account.__tostring = function(a) return `{a.owner}: ${a.balance}` end

local joint = Account.new("Aalto", 10) + Account.new("Fern", 5)
joint:deposit(1)
print(joint)                          -- Aalto&Fern: $16
-- joint:withdraw(1)  -- type error: Key 'withdraw' not found in table '{ owner: string, balance: number } with metatable Account'
```

Metatables work like Lua 5.4 and Luau, every event plus `__iter`, `__tostring` and `__metatable`. Strings share a metatable, so `s:upper()` works. The checker gets `setmetatable` classes, inheritance through `__index`, and fields that only a method adds (those come out optional).

Tail calls are real tail calls, and `Engine::set_memory_limit` caps the heap with an out-of-memory error `pcall` can't swallow.

## 🔁 Coroutines

```lua
local function range(n)
    return coroutine.wrap(function()
        for i = 1, n do coroutine.yield(i) end
    end)
end
for i in range(3) do print(i) end      -- 1 2 3

local co = coroutine.create(function(a)
    local b = coroutine.yield(a * 2)     -- suspends; resume's arguments come back here
    return a + b
end)
print(coroutine.resume(co, 5))         -- true  10
print(coroutine.resume(co, 1))         -- true  6
print(coroutine.status(co))            -- dead
```

The whole `coroutine` library's there. You can yield inside `pcall` and metamethods, just not inside a callback a native is running (a `table.sort` comparator, for example). Hosts can drive them too:

```rust
let worker: Function = engine.get("worker")?;
let mut cx = engine.context();
let co = cx.create_thread(&worker);
let first: String = cx.resume(&co, "job-1")?;   // runs until the first yield
let done: f64 = cx.resume(&co, 42)?;             // 42 is what `yield` returns inside
assert_eq!(cx.thread_status(&co), "dead");
```

## 🧵 Patterns and text

```lua
local k, v = ("retries = 3"):match("^(%w+)%s*=%s*(%d+)$")      -- "retries", "3"
for word in ("the quick brown fox"):gmatch("%a+") do print(word) end
print(("$name is $age"):gsub("%$(%w+)", { name = "Aalto", age = 3 }))   -- Aalto is 3   2
print(("hello world"):gsub("%w+", string.upper))                       -- HELLO WORLD  2
print(utf8.len("héllo"), os.date("!%Y-%m-%d", 0))                      -- 5   1970-01-01
```

Patterns are Lua 5.4's, ported from `lstrlib.c`, and match bytes the same way Lua's do. `os` only has `time`, `clock`, `date` and `difftime`, so nothing gets out of the sandbox.

## 🧮 Bits, buffers, vectors and frozen tables

```lua
local header = buffer.create(8)
buffer.writeu32(header, 0, 0x52485545)             -- little-endian, offsets from 0
buffer.writeu16(header, 4, 18)
buffer.writebits(header, 48, 3, 5)                  -- any bit range
print(buffer.readu16(header, 4), bit32.extract(buffer.readu32(header, 0), 24, 8))  -- 18  82

local Config = table.freeze({ retries = 3, hosts = { "a", "b" } })
Config.retries = 4                                  -- error: attempt to modify a readonly table
local mine = table.clone(Config)                    -- a shallow, writable copy
print(debug.info(1, "sl"), debug.traceback("here"))
```

All of this follows Luau. Buffers are bounds-checked and count against the memory limit.

Vectors are three f32s stored right in the value, so making one doesn't allocate. The `vector` library and the checker both know about them, and natives get them as `[f32; 3]`.

```lua
--!strict
local function step(position: vector, velocity: vector, dt: number): vector
    return position + velocity * dt
end
local p = step(vector.zero, vector.create(3, 0, 4), 0.5)   -- 1.5, 0, 2
print(p, vector.magnitude(p), vector.normalize(p))          -- 2.5   0.6, 0, 0.8
```

One difference on purpose: strings are always UTF-8, so there's no `string.pack`/`unpack`. Binary stuff goes in buffers.

## 📦 Modules

```rust
engine.add_module("vec", include_str!("scripts/vec.rhue"));            // a module from source
engine.set_module_loader(|name| std::fs::read_to_string(               // or look them up on demand
    format!("scripts/{}.rhue", name.replace('.', "/"))).ok());
engine.eval(r#"--!strict
    local Vec = require("vec")       -- typed: the checker checked vec.rhue first
    local v = Vec.new(3, 4)
    print(v:len())
"#)?;
```

`require` runs a module once and caches it. Rhue never touches the file system by itself, modules come from `add_module` or your loader (the CLI looks for `a/b.rhue` or `a/b/init.rhue` next to the script). Cycles are an error, and a required module keeps its types in checked code.

## 💻 The command line

`rhue FILE` runs a script, `rhue -e '...'` runs a snippet, and `compile`, `fmt` and `--check` do what they sound like. On its own it's a REPL:

```bash
$ rhue
Rhue 1.0.0. Globals persist between entries; :help for commands, Ctrl-D to exit.
> local function fib(n)
    if n < 2 then return n end
    return fib(n - 1) + fib(n - 2)
  end
> fib(20)
6765
> { name = "Aalto", tags = { "fox", "dev" } }
{ name = "Aalto", tags = { "fox", "dev" } }
> :type fib
(number) -> number
```

Unfinished input carries onto the next line. Unlike Luau's REPL, a `local` sticks around between entries. `:type EXPR` shows what the checker thinks of something, `:help` has the rest.

`rhue test` runs every `*_test.rhue` (or `*.test.rhue`) under the current folder, each in its own engine:

```lua
--!strict
local cart = require("lib.cart")

describe("cart", function()
    test("totals prices", function()
        expect(cart.total({ { price = 2 }, { price = 3 } })).toBe(5)
    end)
    test("rejects negative prices", function()
        expect(function() cart.total({ { price = -1 } }) end).toThrow("negative")
    end)
end)
```

```bash
$ rhue test
tests/cart_test.rhue
  ✓ cart > totals prices (0.1 ms)
  ✗ cart > rejects negative prices (0.1 ms)
      Error: expected an error containing "negative", but it returned
         ,-[ tests/cart_test.rhue:9:9 ]
      ...
1 passed, 1 failed in 1 file (3.2 ms)
```

`expect` has the usual Jest-ish matchers (`toBe`, `toEqual`, `toThrow`, `.never` and friends), all typed. `--filter TEXT` runs only the tests that match, and a failure gives a non-zero exit code for CI.

## ✏️ Editor support

```bash
$ cargo install rhue-lsp                    # the language server, `rhue-lsp`
$ rhue fmt src/*.rhue                        # format in place (tabs; --indent 4 for spaces)
$ rhue fmt --check src/*.rhue                # CI: list files that need formatting
```

`rhue-lsp` is a normal LSP server over stdio. Errors as you type, hover types, inlay hints, go to definition and find references (across modules too), rename, signature help, completion, semantic tokens, typo quick fixes (`pirnt` → `print`) and formatting. In Neovim:

```lua
vim.filetype.add({ extension = { rhue = "luau" } })
vim.lsp.config("rhue", { cmd = { "rhue-lsp" }, filetypes = { "luau" }, root_markers = { ".git" } })
vim.lsp.enable("rhue")
```

The formatter keeps your line breaks and comments and only fixes spacing and indentation. If its output wouldn't lex back to the exact same tokens, it gives up instead of touching your code.

## 🐞 Debugging

```bash
$ cargo install rhue-dap                    # the debug adapter, `rhue-dap`
```

`rhue-dap` speaks the Debug Adapter Protocol over stdio. Breakpoints (with conditions, hit counts and logpoints), stepping, the call stack, scopes, watches and console eval all work, `require`d modules included. With nvim-dap:

```lua
local dap = require("dap")
dap.adapters.rhue = { type = "executable", command = "rhue-dap" }
dap.configurations.luau = {
  { type = "rhue", request = "launch", name = "Run file", program = "${file}" },
}
```

VS Code just needs a tiny extension pointing a `rhue` debugger at `rhue-dap`.

Hosts can hook in directly too:

```rust
use rhue::{DebugEvent, Engine, HookMask};

let mut engine = Engine::new();
engine.set_debug_hook(HookMask { line: true, ..HookMask::default() }, |dbg, event| {
    if let DebugEvent::Line(12) = event {
        let frame = dbg.frame(0).unwrap();
        println!("{}:{} in {}", frame.source, frame.line, frame.function);
        for (name, value) in dbg.locals(0) {
            println!("  {name} = {value:?}");
        }
    }
    Ok(()) // Err(message) stops the script with an error it can't catch
});
```

With no hook set it costs about 3% on the loop benchmarks.

## 🦀 Embedding

```rust
use rhue::{Context, Engine, Function, ScriptError, UserData, Variadic};

#[derive(UserData, Clone)]
struct Player {
    name: String,
    hp: f64,
    #[rhue(readonly)]
    level: u32,
}

let mut engine = Engine::new();

// typed signatures, arguments get checked and converted
engine.register("add", |a: f64, b: f64| a + b);
engine.register("sum", |xs: Variadic<f64>| xs.iter().sum::<f64>());
engine.register("half", |n: i64| -> Result<i64, String> {
    if n % 2 == 0 { Ok(n / 2) } else { Err(format!("{n} is odd")) } // Err = script error
});
// take `&mut Context` first to call back into scripts
engine.register("twice", |cx: &mut Context, f: Function, x: f64| -> Result<f64, ScriptError> {
    let once: f64 = cx.call(&f, x)?;
    cx.call(&f, once)
});

// Rust types as script objects
engine
    .register_type::<Player>()
    .function("new", |name: String| Player { name, hp: 100.0, level: 1 })
    .method("heal", |p: &mut Player, amount: f64| p.hp += amount);

engine.eval(r#"
    hero = Player.new("Aalto")
    hero:heal(25)
    hero.name ..= "!"
    function on_tick(dt) return twice(function(x) return x * 2 end, dt) end
"#)?;

let quadrupled: f64 = engine.call_global("on_tick", 1.5)?; // 6
let hero: Player = engine.get("hero")?;                     // a copy; Ud<Player> for the live object
```

Wrong argument types turn into script errors like `invalid argument #1 to 'add' (number expected, got string)`. `cargo run -p rhue --example embed` has a fuller version.

```rust
use rhue::{Engine, Value};

let mut engine = Engine::new();
engine.set_global("name", "Aalto");
engine.set_instruction_limit(Some(1_000_000)); // stop runaway scripts
engine.set_memory_limit(Some(64 << 20));       // and runaway allocation (64 MB)
engine.on_print(|line| eprintln!("[script] {line}")); // route `print`

let values = engine.eval(r#"
    local function counter()
        local n = 0
        return function() n += 1 return n end
    end
    local next = counter()
    next() next()
    print("about to return")
    return next(), "hello, " .. name
"#)?;
assert_eq!(values, [Value::Number(3.0), Value::from("hello, Aalto")]);

match engine.eval_expr("1 + true") {
    Err(e) => eprint!("{}", e.render(true)), // ariadne-rendered, with source and traceback
    Ok(v) => println!("{v}"),
}
```

Globals stick around between `eval`s on the same engine, locals don't. Anything handed back to Rust is pinned, so the GC leaves it alone while you're holding it.

### Tasks

A native can pause a script by returning `ScriptError::yield_with(values)`. The host gets the values, and whatever it passes to `resume` becomes the call's result. Scripts stay straight-line code and the host decides when they move on (a dialog waiting on the player, a request waiting on the network).

```rust
use rhue::{Engine, ScriptError, TaskState, Value};

engine.register("ask", |question: String| -> Result<String, ScriptError> {
    Err(ScriptError::yield_with(vec![Value::from(question)])) // stop here
});
let mut dialog = engine.spawn("npc.rhue", r#"
    local name = ask("What's your name?")
    return "Nice to meet you, " .. name
"#)?;
// dialog.state() == Suspended(["What's your name?"])
engine.resume(&mut dialog, vec![Value::from("Aalto")])?;
// dialog.state() == Finished(["Nice to meet you, Aalto"])
```

`engine.open_tasks()` adds Roblox's `task` library (`wait`, `delay`, `spawn`, `defer`, `cancel`). Time only moves when you call `engine.step(dt)`, and one task erroring doesn't take the rest down with it.

```rust
engine.open_tasks();
engine.spawn("blink.rhue", r#"
    while true do
        light.on = not light.on
        task.wait(0.5)
    end
"#)?;
loop {
    for error in engine.step(frame_time) {
        eprint!("{}", error.render(true));
    }
}
```

### Async hosts

If your host is async, register async functions with `register_async` and run scripts with `eval_async`. The script calls the function like normal and just sits there until the future's done, without blocking anything.

```rust
engine.register_async("fetch", |url: String| async move {
    let res = reqwest::get(url).await.map_err(|e| e.to_string())?;
    res.text().await.map_err(|e| e.to_string())
});
engine.set_sleep(tokio::time::sleep); // lets task.wait run on real time

let page = engine.eval_async(r#"return fetch("https://example.com")"#).await?;
```

An `Err` from the future gets raised where the script called it, so `pcall` works on it. `task.spawn` a few calls and they all wait at once, and `run_until_idle` finishes off whatever's left after the main script's done.

Any runtime works, but the futures aren't `Send`, so on tokio use a current-thread runtime or a `LocalSet`. Dropping the `eval_async` future (like with a tokio timeout) cancels the script too.

`engine.interrupt_handle()` stops a script from somewhere else with an error `pcall` can't catch. A script stuck in a loop is hogging its thread, so on a single-threaded runtime the interrupt has to come from another thread. It only gets checked when the instruction budget refills, so it costs basically nothing.

### Hot reload

`reload_module` swaps a module over to new source while everything keeps running. Functions in it change in place, so anything already holding one (a callback you stored, a field another module copied) runs the new code from its next call. The module's tables stay the same tables and keep their values, and locals its functions capture keep theirs, so state carries over. A broken edit comes back as an `Err` and the old code keeps going.

```rust
engine.set_module_loader(|name| std::fs::read_to_string(format!("scripts/{name}.rhue")).ok());
engine.eval("npc = require('npc')")?;

// later, when scripts/npc.rhue changes
for name in engine.loaded_modules() {
    if changed(&name) {
        engine.reload(&name)?; // asks the loader again
    }
}
```

A call that's already running finishes on the old code, and closures the old code built at runtime keep theirs until something builds them again. `rhue-dap` does this for you, save a module mid-session and it reloads. `cargo run -p rhue --example hot_reload` has the whole loop.

### Environments

`create_environment` gives scripts their own globals, so plugins or mods can share one engine without seeing each other's stuff. Reads fall back to the engine's globals (the stdlib and whatever you registered), writes stay in the environment, and the checker knows about it.

```rust
let plugin_a = engine.create_environment();
let plugin_b = engine.create_environment();
engine.eval_in(&plugin_a, "name = 'A'; function greet() print('hi from ' .. name) end")?;
engine.eval_in(&plugin_b, "name = 'B'")?;     // its own `name`; `greet` is nil here
engine.eval_in(&plugin_a, "greet()")?;         // hi from A
```

### serde

With the `serde` feature on, `rhue::serde::to_value` and `from_value` convert anything `Serialize`/`Deserialize`, mapped about the way JSON would.

```rust
#[derive(Serialize, Deserialize)]
struct Config { retries: u32, hosts: Vec<String> }

let value = rhue::serde::to_value(&mut engine.context(), &config)?;
engine.set_global("config", value);
engine.eval("config.retries += 1; table.insert(config.hosts, 'backup')")?;
let value = engine.global("config");
let config: Config = rhue::serde::from_value(&mut engine.context(), value)?;
```

### Compiled chunks

Compile once, load later without parsing or type-checking (about 9× faster than compiling):

```rust
let bytes = engine.compile_to_bytes("main.rhue", &source)?; // or proto.to_bytes()
std::fs::write("main.rhuec", &bytes)?;

let proto = other_engine.load(&std::fs::read("main.rhuec")?)?; // ErrorKind::Load if it's not valid
other_engine.run(&proto)?;
```

```bash
$ rhue compile main.rhue          # writes main.rhuec (-o to choose; --strict to check strictly)
$ rhue main.rhuec                 # runs it; the source file isn't needed
```

Loading runs the bytecode verifier first, so a broken or hand-made chunk gets refused instead of run. Chunks keep their source for error lines and keep their types, so a compiled module is still checked like the source was there:

```rust
let bytes = Engine::new().compile_to_bytes("counter", &source)?;
engine.add_module_bytes("counter", &bytes)?;
engine.check("main.rhue", "--!strict\nlocal c = require('counter').new(1)\nc:bump('x')")?; // error
```

## 🚀 Performance

`bench/` has small programs that each poke at one thing. The runner checks every result and prints times:

```bash
$ cargo run --release -p rhue --example bench                # all of them
$ cargo run --release -p rhue --example bench -- --save now.txt
$ cargo run --release -p rhue --example bench -- --baseline bench/m10-baseline.txt fib
```

### Compared with Luau and Rhai

`bench/compare` runs the same scripts on Rhue, Luau 0.709 (the interpreter, through `mlua`) and Rhai 1.27 (hand ports of six of them), fastest of five runs:

```bash
$ cargo run --release --manifest-path bench/compare/Cargo.toml
```

| Script | Rhue ms | Luau ms | Rhai ms | Luau vs Rhue | Rhai vs Rhue |
| --- | ---: | ---: | ---: | ---: | ---: |
| `arrays` | 7.1 | 3.2 | – | 2.2× faster | – |
| `callbacks` | 15.1 | 4.6 | – | 3.3× faster | – |
| `classes` | 302 | 136 | – | 2.2× faster | – |
| `closures` | 61.8 | 22.4 | 407 | 2.8× faster | 6.6× slower |
| `coroutines` | 77.9 | 33.9 | – | 2.3× faster | – |
| `fib` | 34.0 | 13.3 | 258 | 2.6× faster | 7.6× slower |
| `loops` | 89.4 | 78.3 | 1346 | 1.1× faster | 15× slower |
| `methods` | 154 | 70.1 | 611 | 2.2× faster | 4.0× slower |
| `patterns` | 206 | 105 | – | 2.0× faster | – |
| `strings` | 74.8 | 53.6 | 395 | 1.4× faster | 5.3× slower |
| `tables` | 66.6 | 37.4 | 349 | 1.8× faster | 5.2× slower |

Measured at M23. Luau's had years of tuning and it shows, though plain arithmetic loops are about even. Rhai walks a syntax tree, so it's not close. Wall times on my machine drift a lot between sessions, so I track progress with callgrind instruction counts instead, and those numbers live in [`docs/DESIGN.md`](docs/DESIGN.md). Also these are my own benchmarks, so they lean a bit toward what I tuned for.

## 🧪 Examples

`crates/rhue/examples` has small hosts to start from, run any of them with `cargo run -p rhue --example NAME`:

| Example | Shows |
| --- | --- |
| `hello` | registering a function, setting a global, reading a result, printing an error |
| `embed` | a little game host with a `#[derive(UserData)]` type and per-frame script calls |
| `sandbox` | untrusted plugins in their own environments, with limits |
| `modules` | `require` from source, from compiled chunks and from a loader |
| `tasks` | the `task` library driven by a game loop |
| `async_host` | tokio, overlapping async calls, killing a runaway script |
| `hot_reload` | a game loop that reloads modules when their files change |
| `bench` | the benchmark runner (`--release`) |

`examples/*.rhue` are scripts for the CLI (`rhue examples/tour.rhue`).

## 🗂️ Layout

| Crate | What's in it |
| --- | --- |
| `rhue-common` | spans and diagnostics |
| `rhue-lexer` | the lexer (logos) |
| `rhue-parser` | the parser and AST |
| `rhue-bytecode` | opcodes, `Proto`, the verifier, the chunk format |
| `rhue-compiler` | name resolution, constant folding, the register compiler |
| `rhue-types` | the type checker |
| `rhue-vm` | values, the GC, the interpreter, coroutines, the stdlib |
| `rhue` | the embedding API (`Engine`, `Context`, conversions, userdata) |
| `rhue-derive` | `#[derive(UserData)]` |
| `rhue-fmt` | the formatter |
| `rhue-lsp` | the language server |
| `rhue-dap` | the debug adapter |
| `rhue-cli` | the `rhue` binary |

## 🛠️ Development

```bash
cargo test             # 571 tests, 576 with --all-features
cargo clippy --all-targets
cargo run --release -p rhue --example bench
cargo run -p rhue-cli  # REPL
```

A lot of those tests are property and fuzz tests: random programs, damaged chunks, patterns checked against the `regex` crate, and suites re-run with the GC collecting at every safe point.

Needs Rust 1.88 or newer (edition 2024), and there's no `unsafe` in Rhue's own crates. CI runs everything on Linux, macOS and Windows, plus clippy, rustfmt, rustdoc, an MSRV build and `cargo package`.

## 📄 License

[MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), your pick. Unless you say otherwise, anything you contribute is dual licensed the same way.
