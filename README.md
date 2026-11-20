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
rhue = "2"
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

2.1.0 is out, 26 milestones in (2.0 was a quick API cleanup right after 1.0). What's in it:

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

## 📚 Docs

- [The language](docs/language.md): types, classes and metatables, coroutines, patterns, buffers and vectors, modules, and where Rhue isn't quite Luau
- [Embedding](docs/embedding.md): registering functions and types, limits, tasks, async hosts, WebAssembly, hot reload, environments, serde, saved chunks
- [Tooling](docs/tooling.md): the `rhue` CLI, the language server and the debugger
- [How it works](docs/ARCHITECTURE.md): the pipeline and the VM as they are now, and [`docs/DESIGN.md`](docs/DESIGN.md) for how they got that way

## 🚀 Performance

`bench/` has small programs that each poke at one thing. The runner checks every result and prints times:

```bash
$ cargo run --release -p rhue --example bench                # all of them
$ cargo run --release -p rhue --example bench -- --save before.txt
$ cargo run --release -p rhue --example bench -- --baseline before.txt fib
```

Wall times drift too much to trust a few percent, so for that there's `bench/counts.sh`. It runs each script under callgrind and compares the instruction counts with `bench/counts.json` (Linux, needs valgrind):

```bash
$ bench/counts.sh            # print them
$ bench/counts.sh --check    # fail if any got more than 2% worse
$ bench/counts.sh --save     # make these the new baseline
```

### Compared with Luau and Rhai

`bench/compare` runs the same scripts on Rhue, Luau 0.709 (the interpreter, through `mlua`) and Rhai 1.27 (hand ports of six of them), fastest of five runs:

```bash
$ cargo run --release --manifest-path bench/compare/Cargo.toml
```

Add `--write` and the times go in `bench/results.json`, then `cargo xtask gen` puts them in this table.

<!-- bench-table:start (generated from bench/results.json by `cargo xtask gen`) -->
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
<!-- bench-table:end -->

Measured at M23. Luau's had years of tuning and it shows, though plain arithmetic loops are about even. Rhai walks a syntax tree, so it's not close. Progress gets tracked with the instruction counts in `bench/counts.json`, older milestones' numbers are in [`docs/DESIGN.md`](docs/DESIGN.md). Also these are my own benchmarks, so they lean a bit toward what I tuned for.

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
| `rhue-bytecode` | opcodes, `Proto` (what a `Chunk` wraps), the verifier, the chunk format |
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
cargo test             # 582 tests, 587 with --all-features
cargo clippy --all-targets
cargo run --release -p rhue --example bench
cargo run -p rhue-cli  # REPL
```

A lot of those tests are property and fuzz tests: random programs, damaged chunks, patterns checked against the `regex` crate, and suites re-run with the GC collecting at every safe point.

Needs Rust 1.88 or newer (edition 2024), and there's no `unsafe` in Rhue's own crates. CI runs everything on Linux, macOS and Windows, plus clippy, rustfmt, rustdoc, an MSRV build and `cargo package`.

## 📄 License

[MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), your pick. Unless you say otherwise, anything you contribute is dual licensed the same way.
