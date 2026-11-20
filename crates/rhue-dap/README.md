<div align="center">
  <img src="https://raw.githubusercontent.com/rhuelang/rhue/main/assets/Logo.svg" width="140" alt="Rhue logo">
  <h1>rhue-dap</h1>
  <p>A debugger for Rhue scripts. It speaks the Debug Adapter Protocol over stdio, so VS Code, nvim-dap and anything else that talks DAP can use it.</p>
  <p>
    <a href="https://crates.io/crates/rhue-dap"><img src="https://img.shields.io/crates/v/rhue-dap?style=for-the-badge&amp;color=e07b4f" alt="rhue-dap on crates.io"></a>
    <a href="https://github.com/rhuelang/rhue/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/rhuelang/rhue/ci.yml?branch=main&amp;style=for-the-badge&amp;label=ci" alt="CI status"></a>
    <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.88%2B-e07b4f?style=for-the-badge&amp;logo=rust" alt="Rust 1.88 or newer"></a>
    <a href="https://github.com/rhuelang/rhue#-license"><img src="https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue?style=for-the-badge" alt="MIT or Apache-2.0"></a>
  </p>
</div>

```bash
cargo install rhue-dap
```

Launch it with `{ "program": "main.rhue", "stopOnEntry": false }`. With nvim-dap:

```lua
local dap = require("dap")
dap.adapters.rhue = { type = "executable", command = "rhue-dap" }
dap.configurations.luau = {
  { type = "rhue", request = "launch", name = "Run file", program = "${file}" },
}
```

VS Code needs a tiny extension that declares a `rhue` debugger pointing at the `rhue-dap` executable, the launch config's the same.

What works:

- line breakpoints, in the program and in modules it `require`s, with conditions (`i % 4 == 0`), hit counts (`5`, `>= 5`, `% 5`) and logpoints (`i={i} total={total}`)
- continue, step over, into and out, and pause
- the call stack, with locals, upvalues and globals, and tables you can expand
- watches and the debug console, evaluated in the selected frame
- `print` output in the debug console
- hot reload: save a module the script loaded and it gets swapped in without restarting

Hovers only read names and field paths (`player.stats.hp`), so hovering over something can never run code.

It's a library too, if you'd rather run it inside something else than as its own process. `rhue_dap::serve(input, output)` is the whole adapter over any pair of streams, and `rhue_dap::run(messages, send)` is just the session, fed from a channel.

## License

MIT or Apache-2.0, same as the rest of Rhue.
