# The language

Rhue is Luau, mostly. Same syntax, same gradual types, same standard library where it makes sense in a sandbox. This page walks through what's there, and the end has the places it doesn't match Luau.

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

Metatables work like Lua 5.4 and Luau, every event plus `__iter`, `__tostring` and `__metatable`. Strings share a metatable, so `s:upper()` works. The checker gets `setmetatable` classes, inheritance through `__index`, and fields that only a method adds (those come out optional).

Tail calls are real tail calls, and `Engine::set_memory_limit` caps the heap with an out-of-memory error `pcall` can't swallow.

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

The whole `coroutine` library's there. You can yield inside `pcall` and metamethods, just not inside a callback a native is running (a `table.sort` comparator, for example). A native that yields right inside `pcall` or `xpcall` is fine too, and so is `pcall(coroutine.yield, x)`. Hosts can drive them too:

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

Patterns are Lua 5.4's, ported from `lstrlib.c`, and match bytes the same way Lua's do. `os` only has `time`, `clock`, `date` and `difftime`, so nothing gets out of the sandbox.

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

`require` runs a module once and caches it. Rhue never touches the file system by itself, modules come from `add_module` or your loader (the CLI looks for `a/b.rhue` or `a/b/init.rhue` next to the script). Cycles are an error, and a required module keeps its types in checked code.

## Differences from Luau

Small list so far. If you hit something else that behaves differently, it's either a bug or it belongs here, so open an issue.

- Strings are UTF-8. An escape that makes invalid UTF-8 (`"\xff"`) is a compile error instead of a byte string.
- `os` only has `time`, `clock`, `date` and `difftime`, and dates are UTC since there's no time zone database. Nothing touches files, processes or the environment.
- There's no `io`, and `require` never reads files by itself, the host decides where modules come from.
- `--!strict` code with a type error doesn't run, the error comes back from `compile` or `eval` like a syntax error would.
- The REPL keeps `local`s between entries.
