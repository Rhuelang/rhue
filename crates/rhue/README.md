<div align="center">
  <img src="https://raw.githubusercontent.com/rhuelang/rhue/main/assets/Logo.svg" width="140" alt="Rhue logo">
  <h1>rhue</h1>
  <p>Luau-style scripting for Rust, compiled to register bytecode.</p>
  <p>
    <a href="https://crates.io/crates/rhue"><img src="https://img.shields.io/crates/v/rhue?style=for-the-badge&amp;color=e07b4f" alt="rhue on crates.io"></a>
    <a href="https://docs.rs/rhue"><img src="https://img.shields.io/docsrs/rhue?style=for-the-badge" alt="docs.rs"></a>
    <a href="https://github.com/rhuelang/rhue/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/rhuelang/rhue/ci.yml?branch=main&amp;style=for-the-badge&amp;label=ci" alt="CI status"></a>
    <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.88%2B-e07b4f?style=for-the-badge&amp;logo=rust" alt="Rust 1.88 or newer"></a>
    <a href="https://github.com/rhuelang/rhue#-license"><img src="https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue?style=for-the-badge" alt="MIT or Apache-2.0"></a>
  </p>
</div>

I write a lot of Luau and a lot of Rust, and wanted the first one inside the second, so here we are. Rhue looks like Luau (same syntax, same `--!strict` gradual typing), embeds about as easily as Rhai, and runs on its own register VM. This is the crate you want, the rest are its insides.

```toml
[dependencies]
rhue = "2"
```

```rust
use rhue::Engine;

let mut engine = Engine::new();
engine.register("shout", |s: String| s.to_uppercase() + "!");

let out: String = engine.eval_as(r#"return shout("hello from rhue")"#).unwrap();
assert_eq!(out, "HELLO FROM RHUE!");
```

What you get:

- **Typed `register`** — parameters and results convert on their own, and the type checker learns each function's signature from its Rust types
- **`#[derive(UserData)]`** — Rust structs as script objects, with methods and a constructor
- **Calling back** — take `&mut Context` first and a native can call script functions, errors pass through `?` unchanged
- **Sandboxing** — instruction and memory limits, separate environments so plugins can't see each other's globals, a sandbox-safe `os`
- **Modules** — `require` from source, a loader, or compiled bytecode, with types that travel with them
- **Tasks** — natives that pause a script until the host resumes it, and Roblox's `task` library on a clock you drive
- **Async** — async Rust functions scripts can wait on, `eval_async`, and interrupts from any thread
- **Hot reload** — swap a module for new code while scripts run, state carries over
- **Debug hooks** — line, call and return events, locals, upvalues, evaluating in a frame
- **`serde`** — behind the `serde` feature, convert any `Serialize`/`Deserialize` type to script values and back
- **`tooling`** — what the language server's built on (`Engine::analyze`, outlines, inlay hints). It's not covered by semver, it changes whenever the editor features need it to

```rust
use rhue::Engine;

let mut engine = Engine::new();
engine.set_instruction_limit(Some(100_000));
engine.set_memory_limit(Some(1 << 20));

let err = engine.eval("while true do end").unwrap_err();
assert!(err.to_string().contains("instruction limit"));
```

The [repo's README](https://github.com/rhuelang/rhue) has the full tour of the language, the tooling (CLI, language server, debugger) and the benchmarks. The API docs are on [docs.rs](https://docs.rs/rhue), and the site's at [rhue.dev](https://rhue.dev).

Needs Rust 1.88 or newer.

## License

MIT or Apache-2.0, at your option.
