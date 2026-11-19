//! Loading is the door bytes from outside come through, so damaged chunks have
//! to get refused, or if they still verify, run without panicking.

use std::panic::{AssertUnwindSafe, catch_unwind};

use proptest::prelude::*;
use rhue::Engine;

const PROGRAMS: &[&str] = &[
    "local t = {} for i = 1, 10 do t[i] = i * 2 end local s = 0 for _, v in ipairs(t) do s += v end return s",
    "local function f(a, b, ...) local n = select('#', ...) return a + b + n, ... end return f(1, 2, 3, 4)",
    "local c = 0 local function inc() c += 1 return c end for i = 1, 5 do inc() end return c, inc()",
    "local p = {x = 1, y = 2} function p:sum() return self.x + self.y end return p:sum(), #'abc' .. 'd'",
    "local ok, e = pcall(function() error('x') end) return ok, e, tostring(nil)",
    "local fs = {} for i = 1, 3 do fs[i] = function() return i end end return fs[1]() + fs[3](), {fs[2]()}",
    "local co = coroutine.create(function(a) local b = coroutine.yield(a + 1) return b * 2 end) return coroutine.resume(co, 1)",
];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    #[test]
    fn damaged_chunks_never_panic(
        program in 0..PROGRAMS.len(),
        edits in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..5),
    ) {
        let mut e = Engine::new();
        let mut bytes = e.compile_to_bytes("damaged", PROGRAMS[program]).unwrap();
        for (at, byte) in &edits {
            let i = at.index(bytes.len());
            bytes[i] = *byte;
        }
        let ran = catch_unwind(AssertUnwindSafe(|| {
            let mut e = Engine::new();
            e.set_instruction_limit(Some(20_000));
            e.set_memory_limit(Some(16 << 20));
            if let Ok(p) = e.load(&bytes)
                && let Err(err) = e.run(&p) {
                    let _ = err.render(false);
                }
        }));
        prop_assert!(ran.is_ok(), "loading damaged bytes panicked");
    }
}

/// Not vacuous. Plenty of single-byte damage still loads and verifies, so the
/// property above really does run damaged code.
#[test]
fn damage_often_still_loads() {
    let mut e = Engine::new();
    let (mut loaded, mut total) = (0, 0);
    for src in PROGRAMS {
        let bytes = e.compile_to_bytes("damaged", src).unwrap();
        for i in (0..bytes.len()).step_by(3) {
            for byte in [0u8, 1, 7, 0x41, 0xff] {
                let mut b = bytes.clone();
                b[i] = byte;
                total += 1;
                loaded += e.load(&b).is_ok() as usize;
            }
        }
    }
    assert!(
        loaded * 4 >= total,
        "only {loaded} of {total} damaged chunks loaded"
    );
}
