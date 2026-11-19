# Contributing

Issues and pull requests are welcome. Rhue is mostly just me, so keep a PR to one thing and it'll get looked at faster.

## Before you open a PR

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

CI runs the same three, plus the docs build, the MSRV (1.88) and `cargo package`, on Linux, macOS and Windows.

A few things CI can't check for you:

- Anything users would notice gets a line in `CHANGELOG.md`, under `## [Unreleased]` at the top (add the heading if it's not there).
- Performance work comes with before and after numbers from `bench/`.
- New API is documented, and the README still matches what it does.

## Commits

Conventional commits, the history already looks like this:

```
feat(vm): charge natives to the instruction budget
fix(lsp): hover on a method call shows its signature
docs: say what Chunk::to_bytes leaves out
```

Types are `feat`, `fix`, `perf`, `docs`, `test`, `chore`, `ci` and `cleanup`. The scope is the crate without the `rhue-` (`vm`, `lsp`, `types`...), or whatever area it touches. Anything that breaks the public API of `rhue` gets a `!`.

## Where things live

`rhue` is the crate people embed and the only one with a stable API. Everything else (`rhue-vm`, `rhue-compiler`, `rhue-types`...) is its insides and can change whenever, they're pinned to the exact same version as `rhue`. `docs/DESIGN.md` has the reasoning behind most decisions, so check there before reworking something that looks odd.

## Security

If you can get a script out of the sandbox, past a limit, or into a panic or hang the host can't stop, please don't open an issue. See [SECURITY.md](SECURITY.md).
