# Rhue

[![CI](https://github.com/gyfdb/rhue/actions/workflows/ci.yml/badge.svg)](https://github.com/gyfdb/rhue/actions/workflows/ci.yml)

An embeddable scripting language for Rust. It looks like Luau (syntax, gradual typing with `--!strict`), embeds as easily as Rhai, and compiles to register-based bytecode run by a VM. Nothing is tree-walked.

> **Status: milestone 25.** Async hosts: async Rust functions scripts can wait on, `eval_async`, the `task` clock on real time, and interrupting scripts from anywhere. That's on top of release prep (24), faster calls, builtin fast paths and allocation-free coroutine switches (23), table templates, `table.move` and a faster `table.create` (22), the REPL and `rhue test` (21), vectors (20), embedding II (19: natives that suspend scripts, tasks and the `task` library, environments, `serde`), the standard library II of 18 (`bit32`, buffers, frozen tables, `debug`), the editor features of 17 (inlay hints, quick fixes, symbols, semantic highlighting, navigation across modules), types II (16: read-only and write-only properties, loops, fields methods add, exact types in saved chunks), the performance round of 15 (1–10% fewer instructions, a linear-time checker, measurements against Luau and Rhai), types that travel (14: type packs, defaults, flow typing, typed saved chunks), saved chunks, the incremental language server and smarter breakpoints (13), the debugger and debug hooks (12), the tooling of 11 (`rhue-lsp`, `rhue fmt`), patterns, `utf8`/`os` and typed modules (10), coroutines and in-loop metamethods (9), metatables, tail calls, interpolation and the memory limit (8), the performance pass and bytecode verifier (7), Luau-style gradual typing (6), and the embedding API, tables, garbage collector, closures and standard library of milestones 1–5. See [`docs/DESIGN.md`](docs/DESIGN.md).

```bash
$ cargo run -q -p rhue-cli -- examples/tables.rhue
3 kinds, total $5.00
brown, fox, jumps, quick, the
longest:	5
42	->	42
abc	->	error: examples/tables.rhue:49: not a number: abc
heap after churn: 82 KB
```

## Types

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

Checking happens when a chunk is compiled. A chunk with type errors fails with `ErrorKind::Type` and doesn't run. Without a directive nothing is checked (`Engine::set_default_type_mode` changes that), and types never affect how code runs. Anything cast with `:: any` is unchecked; everything else the checker accepts can't fail with a type error at runtime, which a property test checks on tens of thousands of random programs.

Generic functions can take **type packs**, so wrappers keep the signature of what they wrap, and aliases can have **defaults**:

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

In `--!strict` mode, a **local declared without a value** gets its type from what's assigned to it, following the code: after `if c then x = 1 else x = 2 end`, `x` is a `number`; with only the `then` branch it's `number?` until something rules `nil` out. Loops are followed too: the checker runs over a loop body until what it assigns settles, so `local best; for _, v in xs do if v > 0 then best = v end end` leaves `best` a `number?`, and inside the loop it's whatever it can be on any pass. In a closure that reads or assigns it, or once it holds a table, it's `any`. (`local x = nil` is still the gradual "fill it in later": `any`.)

Properties can be **read-only** or **write-only**, as in Luau. Reading a `write` property or assigning a `read` one is an error, and the variance follows: `{ read x: number }` accepts any table whose `x` is readable as a number (`{ x: 1 }` included), while a plain `{ x: number }` property has to match exactly because it can be written through.

```lua
type Config = { read name: string, write log: (string) -> () }
local function setup(c: Config)
    print(c.name)
    c.name = "x"             -- error: Property 'name' is read-only
end
```

Registered Rust functions are declared to the checker with Luau signatures derived from their Rust types: `|a: f64, b: Option<String>| -> Vec<String>` becomes `(number, string?) -> {string}`. `#[derive(UserData)]` types become classes with typed fields and methods, `set_global` declares the value's type, and `engine.declare("name", "type")` covers anything else.

## Classes and metatables

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

Metatables work as in Lua 5.4 / Luau: `__index` and `__newindex` (tables or functions, chained), every arithmetic event plus `__unm`, `__concat`, `__len`, `__eq`, `__lt`, `__le`, `__call`, `__tostring` (used by `tostring`, `print` and interpolation), `__metatable` protection and Luau's `__iter` for generalized `for`. Strings share a metatable whose `__index` is `string`, so `s:upper()` works. The checker follows `setmetatable` too: it knows `T.__index = T` classes, inheritance through a metatable's `__index`, operator metamethods and `__call`. Inside a class's `:` methods, `self` has the fields the class's constructors give instances plus the class's methods. A field that only a method adds (`function Account:freeze() self.frozen = true end`) is part of the class too, typed as optional with the union of what's assigned to it, so `if acct.frozen then` checks and `acct.frozen = 1` doesn't.

Backtick strings interpolate any expression in `{}` (`` `hp {p.hp}/{max}` ``), going through `tostring` and `__tostring`. `return f(x)` is a proper tail call, so tail recursion runs in constant stack (tracebacks keep the last two tail-called frames and count the rest). `Engine::set_memory_limit` caps the heap: going over it raises an uncatchable `not enough memory` error that `pcall` can't swallow, and natives can check a big allocation up front with `Context::reserve`.

## Coroutines

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

The whole `coroutine` library is there: `create`, `resume`, `yield`, `wrap`, `status`, `running`, `isyieldable` and `close`. A coroutine can yield from inside `pcall` and from inside metamethods (an `__index` that waits for data, say), but not from inside a callback that a native is running (a `table.sort` comparator): that raises "attempt to yield across a C-call boundary". Errors kill the coroutine and come back from `resume` as `false, err`; the instruction and memory limits still stop everything. Hosts drive coroutines too:

```rust
let worker: Function = engine.get("worker")?;
let mut cx = engine.context();
let co = cx.create_thread(&worker);
let first: String = cx.resume(&co, "job-1")?;   // runs until the first yield
let done: f64 = cx.resume(&co, 42)?;             // 42 is what `yield` returns inside
assert_eq!(cx.thread_status(&co), "dead");
```

## Patterns and text

```lua
local k, v = ("retries = 3"):match("^(%w+)%s*=%s*(%d+)$")      -- "retries", "3"
for word in ("the quick brown fox"):gmatch("%a+") do print(word) end
print(("$name is $age"):gsub("%$(%w+)", { name = "Aalto", age = 3 }))   -- Aalto is 3   2
print(("hello world"):gsub("%w+", string.upper))                       -- HELLO WORLD  2
print(utf8.len("héllo"), os.date("!%Y-%m-%d", 0))                      -- 5   1970-01-01
```

Patterns are Lua 5.4's, ported from `lstrlib.c`: classes, sets, the four quantifiers, captures (including `()` positions), back-references, `%b()` and `%f[...]`. They match bytes, like Lua's, so `.` takes one byte of a multi-byte character (a lone half comes out as U+FFFD, as with `string.sub`). A property test checks `string.find` against the `regex` crate on thousands of random patterns. `utf8` has `char`, `codepoint`, `len`, `offset` and `codes`; `os` has only `time`, `clock`, `date` (UTC) and `difftime`, so nothing reaches outside the sandbox.

## Bits, buffers, vectors and frozen tables

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

These follow Luau. `bit32` treats numbers as unsigned 32-bit integers (`band`, `bor`, `bxor`, `bnot`, `btest`, shifts and rotations, `extract`/`replace`, `countlz`/`countrz`, `byteswap`). A **buffer** is a fixed-size array of bytes, a value of its own (`type(b) == "buffer"`, and the checker narrows on that), with reads and writes of every integer width and `f32`/`f64`, strings, bit ranges, `copy` and `fill`; every access is bounds-checked, buffers count against the memory limit, and hosts make and read them with `Context::create_buffer` and `buffer_bytes`. **Frozen tables** refuse every write, including `rawset`, `table.insert` and `setmetatable` (`__newindex` still runs, since that isn't a write to the table); `table.clone` copies a table and its metatable. `debug.info(level or function, "slnaf")` reports source, line, name, arity and the function itself, and `debug.traceback` gives the stack as text, for a suspended coroutine too.

**Vectors** are Luau's: three 32-bit floats stored in the value itself, so making one allocates nothing. `vector.create(x, y, z)`, components `v.x`/`v.y`/`v.z` (and Roblox's `v.X`...), arithmetic (`a + b`, `a - b`, `a * b`, `a / b`, `a // b` componentwise; `v * n`, `n * v`, `v / n`, `n / v`, `v // n` scaling; `-v`), equality and use as table keys by value, and the `vector` library: `magnitude`, `normalize`, `dot`, `cross`, `angle`, `floor`, `ceil`, `abs`, `sign`, `clamp`, `min`, `max`, `zero`, `one`. The checker knows `vector` as a type, with the same arithmetic rules (`vector + number` is an error). Natives take and return them as `[f32; 3]`.

```lua
--!strict
local function step(position: vector, velocity: vector, dt: number): vector
    return position + velocity * dt
end
local p = step(vector.zero, vector.create(3, 0, 4), 0.5)   -- 1.5, 0, 2
print(p, vector.magnitude(p), vector.normalize(p))          -- 2.5   0.6, 0, 0.8
```

One deliberate difference: Rhue strings are UTF-8, so there's no `string.pack`/`string.unpack` (their output is arbitrary bytes). Binary data lives in buffers, and `buffer.tostring` and `readstring` refuse bytes that aren't UTF-8 instead of making a string that isn't.

## Modules

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

`require(name)` runs a module once, with its name as `...`, and caches what it returned (`true` if nothing). Rhue never touches the file system on its own: modules come from `Engine::add_module` or the host's loader (the `rhue` CLI loads `a.b` from `a/b.rhue` or `a/b/init.rhue` next to the script). A cycle is an error naming it (`cyclic module dependency: a -> b -> a`), and a module that fails to load isn't cached. When a checked chunk calls `require` with a literal name, the engine checks that module first, so its exports (functions, records, even metatable classes with their methods and operators) keep their types in the requiring file.

## The command line

`rhue FILE` runs a script, `rhue -e '...'` a snippet, and `rhue compile`, `rhue fmt` and `rhue --check` do what they say. `rhue` on its own is a REPL:

```bash
$ rhue
Rhue 0.1.0. Globals persist between entries; :help for commands, Ctrl-D to exit.
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

Unfinished input (an open `function`, `if` or `{`, a long string) continues on the next line, and the whole entry is recalled from history (`~/.rhue_history`) as one. An expression's value is printed, tables spelled out. A `local` that starts an entry stays for later ones, unlike in Luau's REPL (it's kept as a global). `:type EXPR` shows what the checker thinks of an expression, `:load FILE` runs a file in the session, `:help` lists the rest.

`rhue test` runs tests: every `*_test.rhue` (or `*.test.rhue`) under the current folder, or the paths given, each in a fresh engine.

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

`expect(x)` has `toBe` (same value), `toEqual` (deeply equal), `toBeNil`, `toBeTruthy`, `toBeFalsy`, `toBeCloseTo(n, digits?)`, `toBeGreaterThan`, `toBeLessThan`, `toHaveLength`, `toContain` (an item, or a substring), and `toThrow(text?)`; `expect(x).never` negates each. They're typed, so strict test files are checked like any other. A failure points at the line of the `expect` that failed. Test files can `require` modules next to them or from the folder `rhue test` runs in. `--filter TEXT` runs only tests whose names contain it; the exit code is non-zero if anything failed or couldn't run, for CI.

## Editor support

```bash
$ cargo install --path crates/rhue-lsp      # the language server, `rhue-lsp`
$ rhue fmt src/*.rhue                        # format in place (tabs; --indent 4 for spaces)
$ rhue fmt --check src/*.rhue                # CI: list files that need formatting
```

`rhue-lsp` speaks the Language Server Protocol over stdio, so any LSP client can use it. It reports syntax, name and type errors as you type (files without a directive are checked non-strictly, so annotations count), shows types on hover (inferred as `--!strict` would, even in unchecked files), jumps to a local's definition or to the file behind `require("a.b")`, finds references, renames locals (refusing a new name that would shadow or be shadowed by another variable), shows the signature of the call you're typing, completes members after `.` and `:` (fields, class methods through metatables, the string library) and names in scope, and formats documents. Modules resolve next to the document, preferring unsaved editor buffers. Edits arrive incrementally, each document keeps the types of the modules it imports while you type in it, and the requests after a keystroke share one analysis; editing a module re-checks (and republishes diagnostics for) only the open documents that require it. In Neovim:

```lua
vim.filetype.add({ extension = { rhue = "luau" } })
vim.lsp.config("rhue", { cmd = { "rhue-lsp" }, filetypes = { "luau" }, root_markers = { ".git" } })
vim.lsp.enable("rhue")
```

It also shows **inlay hints** for the types the checker inferred where the code doesn't say (locals, loop variables, callback parameters, function results); double-clicking one writes it in, and so does the matching code action. Errors come with **quick fixes** where there's an obvious one: a misspelled field, method or global becomes the closest name (`p.nmae` → `p.name`, `pirnt` → `print`; the CLI prints these as `help: did you mean 'name'?`), and a value that could be nil gets wrapped in `assert(...)` (which, as in Luau, returns its argument without the nil). The outline lists a file's types, locals and functions, with `function M.new()` nested under `M`, and workspace symbol search fuzzy-matches names in every `.rhue` file under the workspace folders. **Semantic tokens** tell the editor what each name is: a parameter, a local function, a module, a field or method, a type or type parameter, a standard-library global. **Definitions and references work across modules**: on `shapes.area` where `local shapes = require("geo.shapes")`, go-to-definition opens `geo/shapes.rhue` at `function M.area`, and find-references lists every use of that field in the workspace (through any local the module is bound to, or `require("geo.shapes").area` directly), plus the module's own uses of it.

The formatter keeps your line breaks, blank lines (at most one in a row) and comments, and fixes indentation and spacing: one space around operators and after commas, `{ a, b }` braces, tight calls, `obj:method()` and generics, `x: T` annotations. Its output always lexes to exactly the input's tokens and comments, or it refuses with an internal error instead of changing your program.

## Debugging

```bash
$ cargo install --path crates/rhue-dap      # the debug adapter, `rhue-dap`
```

`rhue-dap` speaks the Debug Adapter Protocol over stdio. Launch it with `{ "program": "main.rhue", "stopOnEntry": false }` and you get line breakpoints (in the program and in the modules it `require`s, which load from next to it) with conditions (`i % 4 == 0`), hit counts (`5`, `>= 5`, `% 5`) and log messages (`i={i} total={total}`, printed instead of stopping), continue/step over/into/out, pause, the call stack, Locals/Upvalues/Globals scopes with expandable tables, watch and debug-console evaluation of any expression in the selected frame (hovers only read names and field paths like `player.stats.hp`, so they can't run code), and `print` output in the debug console. In Neovim with nvim-dap:

```lua
local dap = require("dap")
dap.adapters.rhue = { type = "executable", command = "rhue-dap" }
dap.configurations.luau = {
  { type = "rhue", request = "launch", name = "Run file", program = "${file}" },
}
```

VS Code needs a small extension that declares a `rhue` debugger whose adapter is the `rhue-dap` executable; the launch configuration is the same.

Hosts can do the same thing directly. A debug hook sees each new line, call and return, and can walk the stack, read locals and upvalues of any frame, and evaluate code in one (`dbg.eval(0, "#items > 3")`; locals are passed in as copies, tables are shared):

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

Without a hook the interpreter pays one predictable branch per instruction (about 3% on the loop benchmarks). The hook doesn't fire for scripts it runs itself.

## Embedding

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

// Typed signatures: arguments are checked and converted, results converted back.
engine.register("add", |a: f64, b: f64| a + b);
engine.register("sum", |xs: Variadic<f64>| xs.iter().sum::<f64>());
engine.register("half", |n: i64| -> Result<i64, String> {
    if n % 2 == 0 { Ok(n / 2) } else { Err(format!("{n} is odd")) } // Err = script error
});
// `&mut Context` first to call back into scripts; their errors pass through `?` unchanged.
engine.register("twice", |cx: &mut Context, f: Function, x: f64| -> Result<f64, ScriptError> {
    let once: f64 = cx.call(&f, x)?;
    cx.call(&f, once)
});

// Rust types as script objects: fields become properties, plus methods and a constructor.
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

A wrong argument type is a script error such as `invalid argument #1 to 'add' (number expected, got string)`. `cargo run -p rhue --example embed` runs a fuller example.

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

Globals (including functions and tables) persist across `eval` calls on the same `Engine`; locals don't. Runtime errors carry a stack traceback and point into whichever source defined the failing function. Tables, functions and userdata returned to Rust are pinned, so the garbage collector won't free them while you hold them; read and write tables through `engine.context()`.

### Tasks: scripts the host drives

A script can wait for the host. `Engine::spawn` runs it as a coroutine until it first yields, and a native can suspend it by returning `ScriptError::yield_with(values)`: the script stops in the middle of the call, the host gets the values, and whatever the host passes to `Engine::resume` becomes the call's result. Scripts stay straight-line code; the host decides when they continue (a dialog waiting for the player, a request waiting on the network).

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

`Engine::open_tasks` adds Luau's (and Roblox's) `task` library on top: `task.wait(seconds)`, `task.delay(seconds, f, ...)`, `task.spawn(f, ...)` (runs now), `task.defer(f, ...)` (runs when the current resumption ends) and `task.cancel(thread)`. Time passes only when the host calls `engine.step(dt)`, say once a frame; `step` resumes everything that's due and returns the errors tasks raised, without one failing task stopping the others.

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

If your host is async, register async functions with `register_async` and run scripts with `eval_async`. The script just calls the function like normal and sits there until the future's done, without blocking anything.

```rust
engine.register_async("fetch", |url: String| async move {
    let res = reqwest::get(url).await.map_err(|e| e.to_string())?;
    res.text().await.map_err(|e| e.to_string())
});
engine.set_sleep(tokio::time::sleep); // lets task.wait run on real time

let page = engine.eval_async(r#"return fetch("https://example.com")"#).await?;
```

An `Err` from the future gets raised where the script called it, so `pcall` works on it like anything else. `task.spawn` a few calls and they all wait at once. `run_until_idle` finishes off anything still waiting after the main script's done.

Rhue doesn't care which runtime you use, but its futures aren't `Send`, so on tokio use a current-thread runtime or a `LocalSet`. Drop the `eval_async` future (a `tokio::time::timeout`, say) and the script's cancelled along with it.

To stop a script from somewhere else, grab `engine.interrupt_handle()`. It's `Send`, and `interrupt()` stops whatever's running with an error `pcall` can't catch. A script stuck in a loop is holding its thread, so on a single-threaded runtime that call has to come from another thread. Checking for it is free until the script's instruction budget slice runs out, so it doesn't slow anything down.

### Environments

`Engine::create_environment` makes a separate set of globals, for running several scripts (plugins, players' mods) in one engine without them seeing each other's. A script run with `eval_in`, `run_in` or `spawn_in` reads globals from its environment first, then from the engine's (the standard library and everything the host registered), and every global it assigns stays in its environment; functions it defines keep using it wherever they're called from. The type checker follows: a chunk is checked against the engine's globals plus its own environment's. `env.globals()` is a table the host can seed, read or freeze.

```rust
let plugin_a = engine.create_environment();
let plugin_b = engine.create_environment();
engine.eval_in(&plugin_a, "name = 'A'; function greet() print('hi from ' .. name) end")?;
engine.eval_in(&plugin_b, "name = 'B'")?;     // its own `name`; `greet` is nil here
engine.eval_in(&plugin_a, "greet()")?;         // hi from A
```

### serde

With the `serde` feature, `rhue::serde::to_value` and `from_value` convert between Rhue values and any type that implements `Serialize` or `Deserialize`, mapped the way JSON maps them: structs and maps become tables, sequences become array tables, `None` is `nil`, unit enum variants are their name and others `{ Variant = contents }`, and bytes become buffers. Config structs, save files and network messages can go straight into scripts and back.

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

Compile once, ship or cache the bytes, and load them without parsing, resolving or type-checking (about 9× faster than compiling for a typical script):

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

A chunk keeps its source text, so runtime errors and the debugger still show lines. Loading checks the bytes' shape (lengths, UTF-8, nesting, spans) and then runs the bytecode verifier, so a damaged or hand-made chunk is refused instead of run; a fuzz test damages chunks byte by byte and runs whatever still loads. The format has a version number: chunks from older versions still load, and newer ones are refused.

Chunks carry **types** too: what they return and the globals they define, saved as Luau type syntax. `Engine::load` declares the globals to the checker, and `engine.add_module_bytes("counter", &bytes)` makes a compiled module `require`-able with its type, so checked code that uses it is checked as if its source were there:

```rust
let bytes = Engine::new().compile_to_bytes("counter", &source)?;
engine.add_module_bytes("counter", &bytes)?;
engine.check("main.rhue", "--!strict\nlocal c = require('counter').new(1)\nc:bump('x')")?; // error
```

Saved types come back exactly. Each named or recursive type is saved as a type alias, and classes as `setmetatable<Fields, Class>` (a type function you can use in your own annotations), so a linked list stays a linked list however deep you follow it, and a class instance keeps its methods and operators:

```lua
-- what `rhue compile` saves for a vector class and a list
type V = setmetatable<{ x: number, y: number }, VClass>
type VClass = { __index: VClass, new: (number, number) -> V, __add: (any, any) -> V, len: (V) -> number }
type List<T> = { head: T, tail: List<T>? }
```

The aliases resolve among themselves wherever the module is used, without clashing with the user's own names. A type that doesn't resolve in the loading engine (a host type it lacks) is `any`.

## Performance

`bench/` holds small programs that each stress one thing; the runner checks every result and reports times:

```console
$ cargo run --release -p rhue --example bench                # all of them
$ cargo run --release -p rhue --example bench -- --save now.txt
$ cargo run --release -p rhue --example bench -- --baseline bench/m10-baseline.txt fib
```

| Script | Stresses | M6 ms | M7 ms | Speedup |
| --- | --- | ---: | ---: | ---: |
| `arrays` | `table.create`, `table.move`, `table.insert`, shifting a window | – | 7.2 (M22) | – |
| `classes` | `setmetatable` classes, inherited methods, operator metamethods | – | 340 (M8), 315 (M9) | – |
| `closures` | upvalues, closure creation, small calls | 104 | 80 | 1.30× |
| `coroutines` | a `wrap` generator, `resume`/`yield` ping-pong | – | 95 (M9) | – |
| `patterns` | `match`, `gmatch` and `gsub` over 40,000 lines | – | 170 (M10) | – |
| `fib` | recursive calls and returns | 64 | 40 | 1.59× |
| `host` | calling typed Rust functions | 334 | 203 | 1.65× |
| `loops` | numeric `for`, arithmetic, comparisons | 201 | 152 | 1.32× |
| `methods` | `obj:method()`, field reads and writes, small tables | 355 | 195 | 1.82× |
| `natives` | `math.*` calls, `table.sort` with a Lua comparator | 730 | 469 | 1.56× |
| `strings` | concatenation, interning, `string.*` | 109 | 80 | 1.37× |
| `tables` | arrays, records, `pairs`/`ipairs` | 120 | 91 | 1.32× |

(Fastest of 15 runs, both versions measured back to back on the same machine.) [`docs/DESIGN.md`](docs/DESIGN.md) lists what changed; the short version: inline caches for field and global lookups, comparisons against constants fused with their jump, a register window that needs no bounds checks, lighter call frames, natives that don't allocate, small tables without hashing, and faster string interning.

Milestones 8 and 9 left the other benchmarks unchanged: instruction counts (callgrind) stay within 3% of M7's. Running metamethods in the interpreter loop (M9) cut `classes` by about 4% of its instructions, or 7–10% in wall time.

Milestone 15 cut instructions on every benchmark (callgrind, whole `rhue` runs, against M14): `callbacks` (closures made in a hot loop) −9.8%, `loops` −6.3%, `closures` −4.0%, `fib` −2.8%, `natives` and `tables` −2.4%, the rest −0.4% to −1.8%. Upvalues are shared `Rc<Cell>` cells instead of slab entries, `CLOSURE` reuses constants converted once per chunk, and closure creation moved out of the dispatch loop, which shrank the loop enough to improve code generation for every instruction. `bench/m15-baseline.txt` has this machine's current times (wall times here vary by ±40% between runs, so compare instruction counts).

Milestone 18's new value type and frozen tables cost at most 1.5% of instructions on table-heavy benchmarks (`classes` +1.5%, `tables`, `methods` and `strings` +1.1%; the rest within ±0.5%), mostly from hashing and comparing table keys with one more kind of value. The collector now reuses its mark vectors between collections instead of allocating them each time, which cut `callbacks` by 2.9% and `patterns` by 1.1%. Milestone 19's environments added a check to global reads and writes, which costs at most 0.4% (`natives`), every other benchmark within 0.3%. Milestone 20's vector value made the derived comparison of table keys slower (a variant now holds an array); comparing keys by hand, string keys first, more than made up for it: against M19, `methods` is 2.1% faster, `tables` 1.6% and `classes` 1.3%, while `loops` (+1.3%) and `fib` (+1.0%) moved with the dispatch loop's code layout.

Milestone 22 gives table constructors **templates**. `{ x = x, y = y }` used to make an empty table and add each key as the constructor stored it, growing the hash part as it went. Now the compiler records the constructor's constant keys once per function, and a new `DUPTABLE` instruction makes each table with those keys already in place (holding nil, which tables treat as removed entries), so every field store finds its slot through its inline cache. Against M21: `methods` −8.6% of instructions, `tables` −4.1%, `classes` −3.4%, every other benchmark within ±0.03%. Templates stop at the first computed key and skip fields written as `nil`, so `pairs` still sees keys in the order they were first assigned. The same milestone added `table.move` (Luau's, raw like the rest of the table library, copying array runs in one go) and made `table.create` fill its array in one step; `bench/arrays.rhue` measures both.

Milestone 23 went after **calls**, the widest gap to Luau. A Lua-to-Lua call used to cost about 775 instructions (`fib`), with nearly half in bookkeeping: the return moved one result through a `memmove` call, every return called a function to find no upvalues to close, the callee was looked up twice, and the three special natives (`pcall`, `yield`, `resume`) were checked before the common case. Those are fixed. Builtins get **fast paths**, Luau's `FASTCALL` done at run time: when `CALL` finds it's calling `math.floor`, `type`, `setmetatable`, `string.sub` or one of a dozen others with a few plain arguments, it runs a direct function and writes the result into the caller's registers, with no native context, argument buffer or result vector, and falls back to the full native for anything unusual (including every error, so messages come from one place). The check belongs to the function value being called, so a script that shadows `math.floor` simply calls its own. The iterators behind `pairs` and `ipairs` have fast paths too. **Coroutines** were allocating four times per `yield`, three of them for an error-shaped signal that's never shown, and moved each thread's state four times per switch; yields now allocate nothing and threads swap state in place. Against M22, in instructions: `coroutines` −33.8%, `natives` −8.5%, `fib` −5.2%, `strings` −5.0%, `classes` −4.9%, `tables` −4.8%, `closures` −4.2%, `callbacks` −3.5%, `methods` −2.5%, `arrays` −0.8%; `loops` unchanged and `patterns` +0.2%. (Adding the fast-path module shifted the compiler's inlining so `Table::set` stopped being inlined into `t[k] = v`, costing `tables` 3.6% until it was marked `#[inline]`.)

### Compared with Luau and Rhai

`bench/compare` runs the same programs under Rhue, Luau (0.709, through `mlua`, interpreter at `-O2`) and Rhai (1.27, hand ports of six of them, using integers), checks every result, and reports the fastest of five runs:

```console
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

Measured at M23 (the best of two runs of five). Luau's interpreter is about two times faster on methods, classes, tables and coroutines, two and a half to three and a half on plain calls and closures (it has years of tuning: NaN-boxed values, a hand-scheduled dispatch loop), and about even on arithmetic loops. Luau's own `fib` time moves between 13 and 21 ms from session to session here, which alone swings that ratio between 1.7× and 2.6×. Wall times on this machine drift by tens of percent between sessions for both languages, so compare ratios from one run rather than times across milestones; instruction counts (above) are the reliable measure of Rhue's own progress. Rhue is three to fourteen times faster than Rhai, which walks a syntax tree instead of running bytecode. The scripts are Rhue's own benchmarks, so they lean towards what Rhue was tuned on.

Every compiled chunk passes a **bytecode verifier** (registers, constants, jump targets, the `top` protocol, closure captures) before it runs, and `Engine::run` verifies any prototype that didn't come from the compiler. A fuzz test damages compiled programs at random and runs whatever still verifies: none may panic the VM, and none may escape the instruction limit.

## Examples

`crates/rhue/examples` has small hosts to start from; run any of them with `cargo run -p rhue --example NAME`:

| Example | Shows |
| --- | --- |
| `hello` | registering a Rust function, setting a global, reading a result, rendering an error |
| `embed` | a game host: a `#[derive(UserData)]` type with methods, callbacks, Rust calling script functions each frame |
| `sandbox` | untrusted plugins, each in its own environment, with instruction and memory limits |
| `modules` | `require` from source, from a chunk compiled ahead of time, and from a loader; types across modules |
| `tasks` | the `task` library driven by a game loop: scripts that wait, delay and spawn |
| `async_host` | tokio: async functions, lookups that overlap, stopping a runaway script |
| `bench` | the benchmark runner (`--release`) |

`examples/*.rhue` are scripts for the command line (`rhue examples/tour.rhue`).

## Layout

| Crate | Role |
| --- | --- |
| `rhue-common` | `Span`, `Diagnostic` (rendered with ariadne) |
| `rhue-lexer` | logos-based lexer covering every Luau token |
| `rhue-parser` | recursive descent + Pratt expression parser, AST |
| `rhue-bytecode` | 32-bit instruction encoding, opcodes, `Proto`, shared arithmetic semantics, the verifier, the saved-chunk format |
| `rhue-compiler` | resolver (names → locals/globals), constant folding, single-pass register compiler |
| `rhue-types` | the type checker: Luau-style gradual types, narrowing, generics, host declarations |
| `rhue-vm` | `Value`, tables, the handle heap and garbage collector, call frames and `pcall`, coroutines, the interpreter, and the standard library |
| `rhue` | the embedding facade: `Engine`, `Context`, value conversions, typed natives, userdata; end-to-end tests |
| `rhue-derive` | `#[derive(UserData)]` |
| `rhue-fmt` | the formatter (token-based, comment-preserving, verified) |
| `rhue-lsp` | the `rhue-lsp` language server: analysis (on top of `Engine::analyze`) and the stdio protocol loop |
| `rhue-dap` | the `rhue-dap` debug adapter, on top of the engine's debug hook |
| `rhue-cli` | the `rhue` binary: REPL, `-e`, files and compiled chunks, `--dump`, `compile`, `fmt`, `test` |

## Development

```sh
cargo test             # 556 tests (561 with --all-features, which adds serde): property tests over ~38,000 generated or damaged programs (including a type-soundness property, verifier and chunk-loading fuzz tests, metamethod-vs-plain arithmetic, the same arithmetic with a coroutine yield at every step, Lua patterns against the regex crate, and formatting that ignores how the input was spaced), a table-vs-HashMap model test, and suites re-run with the GC collecting at every safe point
cargo run --release -p rhue --example bench
cargo clippy --all-targets
cargo run -p rhue-cli  # REPL
```

Rhue needs Rust 1.88 or newer (edition 2024) and forbids `unsafe` in its own crates. CI (`.github/workflows/ci.yml`) runs the build and tests on Linux, macOS and Windows, then the examples and the benchmark scripts' expected results; it also runs `rustfmt`, clippy and rustdoc with warnings denied, a build on Rust 1.88, and `cargo package` for every crate. [`CHANGELOG.md`](CHANGELOG.md) lists what each milestone added.

## License

Rhue is licensed under either of the [Apache License, Version 2.0](LICENSE-APACHE) or the [MIT license](LICENSE-MIT), at your option. Unless you explicitly state otherwise, any contribution you intentionally submit for inclusion in Rhue, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
