//! Any text run as a script, with the instruction and memory limits a sandbox
//! would set. It can fail however it likes, but it has to stop and it can't
//! take the host down.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|source: &str| {
    let mut engine = rhue::Engine::new();
    engine.set_instruction_limit(Some(200_000));
    engine.set_memory_limit(Some(16 << 20));
    let _ = engine.eval(source);
});
