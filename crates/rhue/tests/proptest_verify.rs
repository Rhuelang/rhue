//! The verifier is the VM's contract: bytecode it accepts has to run without
//! panicking (a script error is fine). Real programs get compiled, their
//! bytecode gets damaged at random, and whatever still verifies gets run.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use proptest::prelude::*;
use proptest::strategy::ValueTree;
use rhue::{Engine, Proto};
use rhue_bytecode::{Instr, Op, verify};

const PROGRAMS: &[&str] = &[
    "local t = {} for i = 1, 10 do t[i] = i * 2 end local s = 0 for _, v in ipairs(t) do s += v end return s",
    "local function f(a, b, ...) local n = select('#', ...) return a + b + n, ... end return f(1, 2, 3, 4)",
    "local c = 0 local function inc() c += 1 return c end for i = 1, 5 do inc() end return c, inc()",
    "local p = {x = 1, y = 2} function p:sum() return self.x + self.y end return p:sum(), #'abc' .. 'd'",
    "local ok, e = pcall(function() error('x') end) return ok, e, tostring(nil)",
    "local t = {1, 2, 3, n = 'a'} local k = {} for key, v in t do k[#k + 1] = key end return #k, t.n",
    "local s = 'a' for i = 1, 3 do s = s .. i end if s == 'a123' and #s > 2 then return s else return nil end",
    "local x = 0 while x < 10 do x += 1 if x % 3 == 0 then continue end end repeat x -= 2 until x < 0 return x",
    "local fs = {} for i = 1, 3 do fs[i] = function() return i end end return fs[1]() + fs[3](), {fs[2]()}",
    "return select(2, table.unpack({1, 2, 3})), math.max(1, 5, 3), string.upper('ab')",
];

/// Every prototype in the tree, depth first, as paths of child indices.
fn paths(p: &Proto, prefix: Vec<usize>, out: &mut Vec<Vec<usize>>) {
    out.push(prefix.clone());
    for (i, child) in p.protos.iter().enumerate() {
        let mut path = prefix.clone();
        path.push(i);
        paths(child, path, out);
    }
}

fn at_path<'a>(p: &'a mut Proto, path: &[usize]) -> &'a mut Proto {
    match path.split_first() {
        None => p,
        Some((&i, rest)) => at_path(Rc::make_mut(&mut p.protos[i]), rest),
    }
}

fn uses_concat(p: &Proto) -> bool {
    p.code.iter().any(|i| i.op() == Some(Op::Concat)) || p.protos.iter().any(|c| uses_concat(c))
}

#[derive(Clone, Debug)]
enum Damage {
    Opcode(u8),
    FieldA(u8),
    FieldB(u8),
    FieldC(u8),
    Word(u32),
    MaxStack(u8),
}

fn damage() -> impl Strategy<Value = Damage> {
    prop_oneof![
        (0..Op::ALL.len() as u8 + 2).prop_map(Damage::Opcode),
        any::<u8>().prop_map(Damage::FieldA),
        (0u8..12).prop_map(Damage::FieldB),
        (0u8..12).prop_map(Damage::FieldC),
        any::<u32>().prop_map(Damage::Word),
        (0u8..20).prop_map(Damage::MaxStack),
    ]
}

fn apply(p: &mut Proto, at: usize, d: &Damage) {
    if p.code.is_empty() {
        return;
    }
    let pc = at % p.code.len();
    let w = p.code[pc].0;
    p.code[pc] = Instr(match *d {
        Damage::Opcode(op) => (w & !0xff) | op as u32,
        Damage::FieldA(a) => (w & !0xff00) | (a as u32) << 8,
        Damage::FieldB(b) => (w & !0xff_0000) | (b as u32) << 16,
        Damage::FieldC(c) => (w & !0xff00_0000) | (c as u32) << 24,
        Damage::Word(word) => word,
        Damage::MaxStack(m) => {
            p.max_stack = m;
            w
        }
    });
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4000))]

    #[test]
    fn verified_bytecode_never_panics(
        program in 0..PROGRAMS.len(),
        edits in prop::collection::vec((any::<prop::sample::Index>(), any::<usize>(), damage()), 1..4),
    ) {
        let mut e = Engine::new();
        let proto = e.compile("damaged", PROGRAMS[program]).expect("the samples compile");
        prop_assert!(verify(&proto).is_ok());
        let mut p = (*proto).clone();
        let mut all = Vec::new();
        paths(&p, Vec::new(), &mut all);
        for (which, at, d) in &edits {
            let path = which.get(&all).clone();
            apply(at_path(&mut p, &path), *at, d);
        }
        // damage can turn `s = s .. x` into `s = s .. s` in a loop, which
        // doubles a string until memory runs out: a script can do that on
        // purpose too (Rhue has no memory limit), so it isn't a VM bug
        if verify(&p).is_ok() && !uses_concat(&p) {
            e.set_instruction_limit(Some(20_000));
            let p = Rc::new(p);
            let ran = catch_unwind(AssertUnwindSafe(|| {
                let _ = e.run(&p);
            }));
            prop_assert!(ran.is_ok(), "verified bytecode panicked the VM:\n{}", p.disassemble());
        }
    }
}

/// The property above can't be vacuous, a fair chunk of damaged programs still
/// verifies (and so actually runs).
#[test]
fn damage_often_still_verifies() {
    let mut e = Engine::new();
    let mut runner = proptest::test_runner::TestRunner::deterministic();
    let mut accepted = 0;
    let total = 2000;
    for _ in 0..total {
        let (program, at, d) = (0..PROGRAMS.len(), any::<usize>(), damage())
            .new_tree(&mut runner)
            .unwrap()
            .current();
        let proto = e.compile("damaged", PROGRAMS[program]).unwrap();
        let mut p = (*proto).clone();
        apply(&mut p, at, &d);
        if verify(&p).is_ok() {
            accepted += 1;
        }
    }
    assert!(
        accepted * 5 >= total,
        "only {accepted} of {total} damaged programs verified"
    );
}
