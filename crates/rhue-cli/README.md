<div align="center">
  <img src="https://raw.githubusercontent.com/rhuelang/rhue/main/assets/Logo.svg" width="140" alt="Rhue logo">
  <h1>rhue-cli</h1>
  <p>The <code>rhue</code> command: run scripts, a REPL, a test runner, a formatter, and compiling to bytecode.</p>
  <p>
    <a href="https://crates.io/crates/rhue-cli"><img src="https://img.shields.io/crates/v/rhue-cli?style=for-the-badge&amp;color=e07b4f" alt="rhue-cli on crates.io"></a>
    <a href="https://github.com/rhuelang/rhue/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/rhuelang/rhue/ci.yml?branch=main&amp;style=for-the-badge&amp;label=ci" alt="CI status"></a>
    <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.88%2B-e07b4f?style=for-the-badge&amp;logo=rust" alt="Rust 1.88 or newer"></a>
    <a href="https://github.com/rhuelang/rhue#-license"><img src="https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue?style=for-the-badge" alt="MIT or Apache-2.0"></a>
  </p>
</div>

```bash
cargo install rhue-cli
```

```bash
rhue                          # REPL
rhue main.rhue                # run a script, print what it returns
rhue -e "return 1 + 2"        # run a snippet
rhue --check main.rhue        # type-check only
rhue --strict main.rhue       # check everything strictly, then run
rhue compile main.rhue        # writes main.rhuec, `rhue main.rhuec` runs it without parsing
rhue test                     # every *_test.rhue under the current folder
rhue fmt src/*.rhue           # format in place (--check for CI, --indent 4 for spaces)
```

`require("a.b")` loads `a/b.rhue` (or `a/b/init.rhue`) from next to the script, or from the current folder for `-e` and the REPL.

## The REPL

```bash
$ rhue
Rhue 2.0.0. Globals persist between entries; :help for commands, Ctrl-D to exit.
> local function fib(n)
    if n < 2 then return n end
    return fib(n - 1) + fib(n - 2)
  end
> fib(20)
6765
> :type fib
(number) -> number
```

Unfinished input carries onto the next line, and unlike Luau's REPL a `local` sticks around between entries. History goes in `~/.rhue_history`. `:help` has the rest.

## Tests

```lua
--!strict
local cart = require("lib.cart")

describe("cart", function()
    test("totals prices", function()
        expect(cart.total({ { price = 2 }, { price = 3 } })).toBe(5)
    end)
end)
```

`expect` has the usual matchers (`toBe`, `toEqual`, `toThrow`, `toBeCloseTo`, `.never` and friends), all typed. Each file gets its own engine, failures point at the `expect` line, `--filter TEXT` runs only the tests that match, and the exit code fails CI if anything did.

## License

MIT or Apache-2.0, same as the rest of Rhue.
