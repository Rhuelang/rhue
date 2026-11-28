# Changelog

Everything that changed in Rhue. It got built in milestones before the first release, so 1.0.0 is all of them. [`docs/DESIGN.md`](docs/DESIGN.md) has the reasoning behind each one, and the README has the numbers.

This file is generated from [`changelog/`](changelog/) by `cargo xtask gen`, edit those instead.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow [Semantic Versioning](https://semver.org/). Only the `rhue` crate's API is meant to be stable, the other crates are its insides.

## [Unreleased]

- Feat: Hosts can hang their own data on threads (`Context::set_thread_data`) and environments (`Environment::set_data`), and a native can find the environment its caller runs in with `Context::caller_environment`. `Thread` works as a map key now.
- Feat: `Engine::spawn_chunk` and `spawn_chunk_in` start an already compiled chunk as a task, so one script can run as many tasks (in as many environments) without compiling it each time.
- Feat: `Engine::set_max_call_depth` lowers how deep script calls can nest before `stack overflow`. It's 10,000 by default, which is also the most.
- Feat: `pcall` and `xpcall` work directly around a native that yields, so `pcall(require, m)` can wait on the host. `pcall(coroutine.yield, x)` yields now too, instead of erroring.
- Feat: `Engine::resume_error` and `Context::resume_error` resume a waiting script so the call it waited in raises instead. Under `pcall` that gives `false, err`, for a native that waited on the host and then failed.
- Fix: Async natives are plain natives now, without the script wrapper they used to sit in. Tracebacks lose the `[async name]` frame, and an error from one is raised at the script's own call.
- Feat: `Context::kill_thread` and `cancel_thread` stop a thread in any state, and scripts can't catch it. `coroutine.resume` sees a dead coroutine, the host gets an error with `is_killed()`, and a killed task isn't an error `step()` reports. `Context::thread_end` says how a thread ended.
- Feat: `Engine::freeze_libraries` makes `math`, `string`, `table` and the other library tables read-only, along with the metatable strings share. Without it a script in one environment could replace `math.floor` or `string.format` for every other environment and the host. It's opt-in until 3.0.
- Fix: The "attempt to yield across a C-call boundary" error says R-call now, since Rhue is Rust and not C, and it names the native in the way (`table.sort`'s comparator says 'sort') and says to wait before or after calling it. Yielding outside a coroutine says what to use instead.
- Feat: `Engine::define_class` makes classes whose objects are all one host handle type, with parents: a `Box` that's a `Shape` that's a `Node`. Members get `&H` and no `RefCell`, a child has its parent's members, and the checker takes a child wherever its parent goes. `extend_class` adds members later.
- Feat: `Context::object(&class, id, make)` gives back the script object for a host object id if scripts already have one, so the same object reached twice is `==` and the same table key.
- Feat: Weak tables. `__mode` with `"k"`, `"v"` or `"kv"` on the metatable, as in Luau.
- Feat: `ClassBuilder::index` gives a class a hook for names that aren't members, for children by name (`world.Box`). Members come first, and `Engine::declare_index` tells the checker what the hook returns.
- Feat: Handle class getters and setters can take the `Context` and fail (`getter_with`, `setter_with`), so a setter can refuse a value or fire a changed signal. `getter_dyn`, `setter_dyn`, `property_dyn` and `method_dyn` take script types as strings, for hosts that register classes from their own reflection data.
- Feat: Definitions files (`.d.rhue`), Luau's declaration syntax for what a host has. `Engine::definitions` writes one, `Engine::load_definitions` reads one, and `Engine::check_definitions` lists what a file has that the engine doesn't, for a test that keeps a checked-in one honest.
- Feat: `rhue --check --definitions FILE` checks against a host's definitions file. `rhue-lsp` reads the `rhue.definitions` setting, or every `*.d.rhue` at the top of the workspace, and shows a definitions file's own errors.
- Feat: Class names as strings. `@class(param)` in a definitions file, or `Engine::declare_class_param` and `declare_global_class_param`, says an argument names a class, so `FindFirstChildOfClass("Box")` is a `Box?`, `Instance.new("Box")` is a `Box`, and `if n:IsA("Box") then` makes `n` a `Box`. A literal that isn't a class is an error in strict scripts.
- Feat: `docs/migrating.md` says what a 2.x host has to check for 3.0, and walks a host with one handle type over to class trees.
- Feat: Method calls on host objects (`obj:Method()`) are cached per call site, about 1.4% fewer instructions on `bench/instances.rhue`.
- Feat: `Engine::vector` gives the built-in `vector` host properties and methods (`v.Magnitude`, `v:Dot(w)`) and a constructor table under a name the host picks (`Vector3.new`). The checker knows them too, and `v.x` costs the same.
- Feat: Operators on host classes. `TypeBuilder::operator` and `ClassBuilder::operator` take an `Operator` (arithmetic, `Unm`, `Concat`, `Len`, `Eq`, `Lt`, `Le`, `ToString`, `Call`, `Iter`), and giving one more than once overloads it by operand type, so `Vec2 * Vec2`, `Vec2 * number` and `number * Vec2` all work. The checker picks the same overload, and errors when none takes the operands.
- Feat: Definitions files can have operators, `function __add(self, arg1: Vec2): Vec2`, and one with the object second (`2 * v`) can leave out `self`.
- Changed: `rhue_types::ClassType` has an `operators` field, and `Env::declare_operator` adds to it.
- Changed: `@` is a token, so a stray one in a script is a parse error now instead of a lex error.
- Changed: `rhue_types::ClassType` has a `class_params` field, the methods that take a class name.
- Changed: `rhue_types::Env::declare_class` takes the class's parent, `None` for a class without one.
- Changed: `rhue_types::ClassType` has an `index` field, the type an index hook gives.

## [2.2.0] - 2026-11-20

The rest of the tech debt audit. It's a minor since `Engine::set_clock` and the two new libraries are new API.

- Feat: `Engine::set_clock`. `os.time`, `os.date`, `os.clock` and the `task` timers read the engine's clock now, so Rhue runs on `wasm32-unknown-unknown` once the host hands it one, and anywhere else scripts can be put on a fake clock. With no clock to read they're an error instead of a panic.
- Feat: `rhue-dap` is a library too. `rhue_dap::serve` runs the adapter over any pair of streams and `rhue_dap::run` is the session fed from a channel.
- Feat: So is `rhue-cli`, `rhue_cli::main(args)` is the whole command.
- Feat: `cargo xtask gen` writes `CHANGELOG.md` from `changelog/` and the README's benchmark table from `bench/results.json`, and `bench/compare --write` fills in `results.json`.
- Feat: Fuzz targets in `fuzz/` for compiling, loading chunks, patterns and running scripts.
- Fix: Every narrowing cast is checked or says why it can't overflow, and clippy's `cast_possible_truncation` is on for the whole workspace.
- Fix: syn 3, rustyline 17, lsp-server 0.10 and lsp-types 0.97.
- Fix: The README's language, embedding and tooling sections moved to `docs/`, with a list of where Rhue differs from Luau and a short page on how it works now.

## [2.1.0] - 2026-11-19

Fixes from reviewing 2.0.0. It's a minor and not a patch since `is_module_name` is new API.

- Fix: An interrupt reaches a loop that keeps calling pattern functions now. Each call used to hand the VM a fresh slice, so it never ran dry and never looked at the flag.
- Fix: `string.rep("", 1e12)` returns `""` instead of taking the host down trying to build it.
- Fix: `table.concat` with a huge `j` is an error `pcall` catches again, instead of using up the instruction limit before it even looks at the table.
- Feat: `rhue::is_module_name`. `module_paths` doesn't check names, so `require("/etc/passwd")` could reach outside the scripts folder. The CLI, test runner, language server, debugger and hot reload example all check first now.
- Fix: The README says which parts of the standard library count against the instruction limit, instead of all of it.
- Fix: CI tests `rhue` with its default features too.

## [2.0.0] - 2026-11-19

1.0.0 promised more than I meant it to, so 2.0.0 cleans up the API a day later instead of living with it for years. Nobody should have to wait for 3.0 just because a value kind got added.

### Breaking

- Feat: The public enums and data structs are `#[non_exhaustive]`, so new value kinds or error kinds can come in minor releases. A `match` on `Value` needs a `_` arm now.
- Feat: `HookMask` and `CompileOptions` can't be struct literals anymore, use `HookMask::new().line(true)` and `CompileOptions::new().fold_constants(false)`.
- Feat: `Engine::compile`, `load` and `compile_in` hand out a `Chunk` instead of an `Rc<Proto>`. It can be saved, disassembled and run, and what's inside is free to change between releases.
- Feat: `Engine::analyze`, `outline` and inlay hints are behind a `tooling` feature that isn't covered by semver. They're for the language server and change with it.
- Fix: Only chunk format 4 loads. Formats 1 to 3 never shipped.
- Fix: The internal crates are pinned to the exact same version as `rhue`, so `cargo update` can't mix and match them.

### Sandbox

- Fix: Natives count against the instruction limit and interrupts reach them. A `string.find` with a few `.-` in a row could hang a host that set every limit before.
- Feat: `Context::charge`, for host natives that do a lot of work in one call.

### Other

- Feat: `rhue::module_paths`, the one place `require("a.b")` turns into a path. The CLI, language server, debugger and test runner all use it.
- Feat: `bench/counts.sh` checks callgrind instruction counts against `bench/counts.json`. The budget change costs `patterns` about 2%.
- Feat: `CONTRIBUTING.md` and `SECURITY.md`.
- Fix: The tests are grouped by feature into two binaries instead of 32.

## [1.0.0] - 2026-11-18

### Release

- Feat: First release on crates.io, every crate at 1.0.0.
- Feat: The repository moved to [rhuelang/rhue](https://github.com/rhuelang/rhue), and the site's up at [rhue.dev](https://rhue.dev).
- Feat: `changelog/` and `bench/results.json`, what rhue.dev reads for its changelog, "What's next" and benchmark table.

### Hot reload (milestone 26)

- Feat: `Engine::reload_module` swaps a module over to new source while scripts keep running. Functions change in place, so every handle to them runs the new code.
- Feat: State carries over. The module's tables stay the same tables and keep their values, and locals its functions capture keep theirs.
- Feat: A broken edit (syntax, types, or an error while it runs) changes nothing and comes back as an `Err`.
- Feat: `Engine::reload` asks the loader again, `Engine::loaded_modules` lists what's been `require`d, and `Context::reload_module` reloads from inside a native or a debug hook.
- Feat: Checked code compiled after a reload sees the module's new type.
- Feat: `rhue-dap` reloads a module when you save it mid-session.
- Feat: The `hot_reload` example, a game loop that reloads modules when their files change.

### Async hosts (milestone 25)

- Feat: Async Rust functions scripts can call and wait on (`register_async`). An `Err` gets raised where the script called it.
- Feat: `eval_async`, `call_async` and `run_until_idle` to run those scripts. Dropping the future cancels the script.
- Feat: `task.wait` and `task.delay` run on real time under the async drivers, through whatever sleep function the host passes to `set_sleep`.
- Feat: `interrupt_handle()` stops a running or waiting script from any thread. Doesn't cost anything until the instruction budget's slice runs out.
- Feat: `Error::is_interrupted`, and the `async_host` example on tokio.

### Release readiness (milestone 24)

- Continuous integration on GitHub Actions: build and tests on Linux, macOS
  and Windows, the examples, the benchmark scripts' expected results,
  `rustfmt`, clippy with warnings denied, docs with warnings denied, a build
  on the minimum Rust version, and `cargo package` for every crate.
- Crate metadata for crates.io (descriptions, keywords, categories, versioned
  internal dependencies) and the `LICENSE-MIT` and `LICENSE-APACHE` files the
  license field already named.
- Every public item of the `rhue` crate is documented (`missing_docs` is now
  a warning there), with runnable examples for sandboxing, modules and
  compiled chunks, and tasks.
- Examples: `hello`, `sandbox`, `modules` and `tasks`, alongside `embed`.
- `.gitattributes` keeps line endings LF on every platform.

### Changed

- The minimum supported Rust version is 1.88, not 1.85: the code uses let
  chains, which became stable in 1.88. CI now checks it.

### Performance IV: calls (milestone 23)

- Cheaper Lua calls and returns.
- Fast paths for common builtins (`type`, `setmetatable`, `rawget`, `math.*`,
  `string.len`/`byte`/`sub`, the `pairs` and `ipairs` iterators). They belong
  to the function value, so a script that shadows a builtin calls its own.
- Coroutine switches that allocate nothing and swap state in place.
- `rhue_vm::Native` has a new public field, `fast`.

### Performance III (milestone 22)

- Table templates (`DUPTABLE`): constructors with constant keys make their
  tables with the keys in place.
- `table.move`, and a `table.create` that fills in one step.
- The `arrays` benchmark. Saved chunks are format 4 (older ones still load).

### Tooling III (milestone 21)

- An interactive REPL: multi-line input, history, readable tables, `:type`.
- `rhue test`, a test runner with Jest-style `expect` matchers.

### Vectors (milestone 20)

- Luau's `vector` value, its arithmetic, the `vector` library and a `vector`
  type, with `[f32; 3]` conversions for natives.

### Embedding II (milestone 19)

- Natives that suspend the script (`Engine::spawn`, `resume`), a host-driven
  `task` library, environments with their own globals, and `serde`
  conversions behind a feature flag.

### Standard library II (milestone 18)

- `bit32`, `buffer` (a new value type), `table.freeze`/`isfrozen`/`clone`,
  and `debug.traceback`/`debug.info`.

### Editor II (milestone 17)

- Inlay hints, document and workspace symbols, semantic tokens, quick fixes,
  and navigation across modules in `rhue-lsp`.

### Types II (milestone 16)

- Fields that methods add to `self`, read-only and write-only properties,
  flow typing through loops, and exact recursive types in saved chunks.

### Performance II (milestone 15)

- Constant tables cached per prototype, cheaper upvalues, a linear-time
  checker on long functions, and comparisons with Luau and Rhai.

### Types that travel (milestone 14)

- Generic type packs and defaults, flow typing of `local x` declared without
  a value, and types saved with chunks and module exports.

### Saved chunks and incremental tooling (milestone 13)

- Serialized bytecode (loaded without parsing, gated by the verifier),
  incremental re-analysis in the language server, conditional breakpoints,
  logpoints and expression evaluation in the debugger.

### Debugging (milestone 12)

- A host debug API (hooks, breakpoints, inspecting suspended frames) and a
  Debug Adapter Protocol server; rename and signature help in the language
  server.

### Tooling (milestone 11)

- `rhue-lsp` (diagnostics, hover, definition, references, completion,
  formatting) and `rhue fmt`.

### Patterns and modules (milestone 10)

- Lua patterns, `utf8`, a sandbox-safe `os`, and `require` with
  host-provided, typed modules.

### Coroutines (milestone 9)

- Coroutines (yielding across `pcall` and metamethods, host resume),
  metamethods in the dispatch loop, typed `self` in class methods.

### Language completeness (milestone 8)

- String interpolation, metatables and typed `setmetatable` classes, tail
  calls, and a memory limit.

### Performance and the verifier (milestone 7)

- A faster interpreter (inline caches, fused comparisons, small hash parts)
  and a bytecode verifier every chunk passes before it runs.

### Type checker (milestone 6)

- Luau type syntax, `--!strict`/`--!nonstrict`, narrowing, generics, and
  typed host declarations.

### Embedding (milestone 5)

- Typed `register`, calling scripts from Rust, re-entrant natives, and
  `#[derive(UserData)]`.

### Tables and the collector (milestone 4)

- Tables, methods, generic `for`, `pcall`, a mark-and-sweep collector, and
  the `table`, `string` and `math` libraries.

### Functions (milestone 3)

- Functions, multiple returns, varargs, closures and upvalues, the standard
  library's core, and tracebacks.

### Statements (milestone 2)

- Locals, assignment, comparisons, `and`/`or`, control flow, strings,
  globals, and the resolver.

### Expressions (milestone 1)

- Arithmetic expressions compiled to bytecode and run by the VM.
