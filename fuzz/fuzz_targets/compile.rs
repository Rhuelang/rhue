//! Any text through the lexer, parser, checker and compiler. Errors are fine,
//! panics aren't.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|source: &str| {
    let mut engine = rhue::Engine::new();
    let _ = engine.check("fuzz", source);
    let _ = engine.compile("fuzz", source);
});
