# Embedding

Everything about running Rhue inside a Rust program: registering functions and types, limits, pausing scripts, async hosts, hot reload, environments, serde and saved chunks. The API docs on [docs.rs](https://docs.rs/rhue) have the rest.

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
engine.set_max_call_depth(200);                // and runaway recursion (10,000 by default)
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

## Tasks

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

If whatever it was waiting on fails, `resume_error` makes the call raise instead. A `pcall` around it gets `false, err` and the script carries on, otherwise the task dies and you get the error back. A string gets the position of the call, like `error("msg")` there would.

```rust
let mut load = engine.spawn("load.rhue", r#"
    local ok, err = pcall(fetch, "/save")
    if not ok then print("couldn't load: " .. err) end
"#)?;
engine.resume_error(&mut load, "not found")?; // couldn't load: load.rhue:2: not found
```

To stop a thread for good, `cx.kill_thread(&thread)` works on it in any state, and scripts can't `pcall` their way out of it. A waiting one dies on the spot. One that's waiting on a coroutine it resumed dies when that hands back, and a native killing its own thread should return `ScriptError::killed()` so nothing else runs. Whoever resumed it sees it as dead: `coroutine.resume` gives `false, "thread killed"`, and the host gets an error where `is_killed()` is `true`. `cx.cancel_thread` is the polite one, the thread keeps going and dies the next time it waits. `cx.thread_end(&thread)` says how a dead one went (finished, failed, killed or cancelled).

```rust
// the script's been disabled, stop whatever it was doing
engine.context().kill_thread(task.thread());
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

## Async hosts

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

The heavy parts of the standard library count against the instruction limit too (the pattern functions, `table.move`, `table.concat` and `string.rep`), so a pattern that backtracks forever or a huge `table.move` gets stopped the same way a loop does. Your own natives can do the same with `cx.charge(n)` when they do a lot of work in one call.

## WebAssembly

Rhue builds for `wasm32-unknown-unknown`, there's just no clock there. `os.time`, `os.clock` and the `task` timers error until you give the engine one:

```rust
engine.set_clock(|| Duration::from_secs_f64(js_sys::Date::now() / 1000.0));
```

It takes the time since the Unix epoch, and works anywhere else too if you want scripts on a fake clock.

## Hot reload

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

## Environments

`create_environment` gives scripts their own globals, so plugins or mods can share one engine without seeing each other's stuff. Reads fall back to the engine's globals (the stdlib and whatever you registered), writes stay in the environment, and the checker knows about it.

```rust
let plugin_a = engine.create_environment();
let plugin_b = engine.create_environment();
engine.eval_in(&plugin_a, "name = 'A'; function greet() print('hi from ' .. name) end")?;
engine.eval_in(&plugin_b, "name = 'B'")?;     // its own `name`; `greet` is nil here
engine.eval_in(&plugin_a, "greet()")?;         // hi from A
```

A native can tell who's calling it. `env.set_data(...)` hangs anything on an environment and `cx.caller_environment()` finds the one the calling function runs in (the one it was defined in, wherever it's called from). Threads get the same with `cx.set_thread_data` and `cx.thread_data`, and a `Thread` works as a map key if you'd rather keep your own table.

```rust
plugin_a.set_data(Permissions::Trusted);
engine.register("delete_save", |cx: &mut Context<'_>| -> Result<(), ScriptError> {
    let trusted = cx
        .caller_environment()
        .and_then(|env| env.data::<Permissions>())
        .is_some_and(|p| *p == Permissions::Trusted);
    if !trusted {
        return Err(ScriptError::new("not allowed"));
    }
    Ok(())
});
```

One script that runs once per player (or per anything) only needs compiling once. `spawn_chunk_in` starts it as a task in an environment, each on its own thread, and the chunk isn't tied to the environment it was compiled for. `spawn_chunk` does the same on the engine's globals.

```rust
let chunk = engine.compile("character.rhue", &source)?;
for player in players {
    let env = engine.create_environment();
    engine.context().set(env.globals(), "player", player)?;
    engine.spawn_chunk_in(&env, &chunk)?;
}
```

The libraries are shared though. `math`, `string` and the rest are one table each, so a plugin doing `math.floor = nil` breaks it for every other plugin and for the host too. `freeze_libraries` stops that:

```rust
engine.freeze_libraries();
assert!(engine.eval_in(&plugin_a, "math.floor = nil").is_err());  // attempt to modify a readonly table
engine.eval_in(&plugin_a, "math = {}")?;  // fine, that's plugin_a's own `math` now
```

Every library table and the metatable strings share go read-only. Libraries opened after it (`open_tasks`) get frozen too, tables you added yourself don't (`table.freeze` those if you want). It's opt-in for now and turns on by default in 3.0, since plenty of Lua code likes adding its own helpers to `string`.

## serde

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

## Compiled chunks

Compile once, load later without parsing or type-checking (about 9× faster than compiling):

```rust
let bytes = engine.compile_to_bytes("main.rhue", &source)?; // or chunk.to_bytes(), without the types
std::fs::write("main.rhuec", &bytes)?;

let chunk = other_engine.load(&std::fs::read("main.rhuec")?)?; // ErrorKind::Load if it's not valid
other_engine.run(&chunk)?;
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
