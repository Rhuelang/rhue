# fuzz

Fuzz targets for [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz). There's no `unsafe` in Rhue, so all a fuzzer can find is panics and hangs, but for a host running scripts it didn't write either one is an outage.

| Target | Feeds |
| --- | --- |
| `compile` | any text to the lexer, parser, checker and compiler |
| `load` | any bytes to `Engine::load`, and runs what the verifier lets through |
| `patterns` | a pattern and a subject to `find`, `match`, `gmatch` and `gsub` |
| `run` | any text as a script, under an instruction and memory limit |

```bash
cargo install cargo-fuzz
cargo +nightly fuzz run compile -- -max_total_time=60
```

Seed the text targets from the scripts already in the repo, the first folder is where new finds go:

```bash
cargo +nightly fuzz run run fuzz/corpus/run examples bench -- -max_total_time=300
```

`load` gets further from real chunks than from random bytes, so seed it with a few made by `rhue compile`:

```bash
mkdir -p fuzz/corpus/load
for f in examples/*.rhue; do rhue compile "$f" -o "fuzz/corpus/load/$(basename "$f" .rhue).rhuec"; done
```

A crash lands in `fuzz/artifacts/<target>/`, rerun it with `cargo +nightly fuzz run <target> <file>`. Turn it into a test before fixing it.
