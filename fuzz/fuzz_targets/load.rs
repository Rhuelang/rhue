//! Any bytes as a saved chunk. The loader and verifier have to refuse what's
//! broken, and whatever they accept has to run without panicking.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    let mut engine = rhue::Engine::new();
    engine.set_instruction_limit(Some(100_000));
    engine.set_memory_limit(Some(16 << 20));
    if let Ok(chunk) = engine.load(bytes) {
        let _ = engine.run(&chunk);
    }
});
