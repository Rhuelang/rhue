//! Lua patterns against a subject, through find, match, gmatch and gsub.
//! Bad patterns are errors and the budget stops the slow ones, nothing should
//! panic or hang.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: (&str, &str)| {
    let (pattern, subject) = input;
    let mut engine = rhue::Engine::new();
    engine.set_instruction_limit(Some(200_000));
    engine.set_global("p", pattern);
    engine.set_global("s", subject);
    let _ = engine.eval(
        "pcall(string.find, s, p) pcall(string.match, s, p) \
         pcall(function() for _ in string.gmatch(s, p) do end end) \
         pcall(string.gsub, s, p, '%0')",
    );
});
