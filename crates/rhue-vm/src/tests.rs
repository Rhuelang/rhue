use super::*;
use rhue_bytecode::Instr;

fn proto(code: Vec<Instr>, constants: Vec<Constant>, max_stack: u8) -> Rc<Proto> {
    let spans = vec![Span::new(0, 1); code.len()];
    Rc::new(Proto {
        code,
        constants,
        spans,
        max_stack,
        is_vararg: true,
        ..Proto::default()
    })
}

#[test]
fn runs_hand_written_bytecode() {
    let p = proto(
        vec![
            Instr::asbx(Op::LoadN, 0, 1),
            Instr::abc(Op::AddK, 0, 0, 0),
            Instr::asbx(Op::LoadN, 1, 4),
            Instr::abc(Op::Mul, 0, 0, 1),
            Instr::abc(Op::Return, 0, 1, 0),
        ],
        vec![Constant::Number(2.5)],
        2,
    );
    assert_eq!(Vm::new().execute(&p).unwrap(), vec![Value::Number(14.0)]);
}

#[test]
fn rejects_corrupt_bytecode_without_panicking() {
    let p = proto(vec![Instr(0xff)], vec![], 1);
    assert!(
        Vm::new()
            .execute(&p)
            .unwrap_err()
            .message
            .starts_with("invalid opcode")
    );
    let p = proto(vec![Instr::asbx(Op::LoadN, 0, 1)], vec![], 1);
    assert_eq!(
        Vm::new().execute(&p).unwrap_err().message,
        "fell off the end of the bytecode"
    );
}

#[test]
fn instruction_limit_stops_infinite_loops() {
    let p = proto(vec![Instr::asax(Op::Jmp, -1)], vec![], 0);
    let mut vm = Vm::new();
    vm.set_instruction_limit(Some(1000));
    assert_eq!(
        vm.execute(&p).unwrap_err().message,
        "script exceeded its instruction limit (1000)"
    );
}

#[test]
fn globals_persist_across_executions() {
    let mut vm = Vm::new();
    vm.set_global("answer", Value::Number(42.0));
    let k = vec![Constant::String("answer".into())];
    let p = proto(
        vec![
            Instr::abx(Op::GetGlobal, 0, 0),
            Instr::abc(Op::Return, 0, 1, 0),
        ],
        k,
        1,
    );
    assert_eq!(vm.execute(&p).unwrap(), vec![Value::Number(42.0)]);
    vm.set_global("answer", Value::Nil);
    assert_eq!(vm.execute(&p).unwrap(), vec![Value::Nil]);
}

#[test]
fn natives_are_callable_and_print_is_captured() {
    use std::cell::RefCell;
    let out = Rc::new(RefCell::new(Vec::<String>::new()));
    let mut vm = Vm::new();
    let sink = out.clone();
    vm.set_output(move |line| sink.borrow_mut().push(line.to_string()));
    // print("hi", 1)
    let k = vec![
        Constant::String("print".into()),
        Constant::String("hi".into()),
    ];
    let p = proto(
        vec![
            Instr::abx(Op::GetGlobal, 0, 0),
            Instr::abx(Op::LoadK, 1, 1),
            Instr::asbx(Op::LoadN, 2, 1),
            Instr::abc(Op::Call, 0, 3, 1),
            Instr::abc(Op::Return, 0, 0, 0),
        ],
        k,
        3,
    );
    vm.execute(&p).unwrap();
    assert_eq!(*out.borrow(), ["hi\t1"]);
}

#[test]
fn calling_a_non_function_is_an_error() {
    let p = proto(
        vec![
            Instr::abc(Op::LoadNil, 0, 0, 0),
            Instr::abc(Op::Call, 0, 1, 1),
        ],
        vec![],
        1,
    );
    assert_eq!(
        Vm::new().execute(&p).unwrap_err().message,
        "attempt to call a nil value"
    );
}

#[test]
fn concat_interns_its_result() {
    let mut vm = Vm::new();
    let k = vec![
        Constant::String("ab".into()),
        Constant::String("a".into()),
        Constant::String("b".into()),
    ];
    let p = proto(
        vec![
            Instr::abx(Op::LoadK, 0, 1),
            Instr::abx(Op::LoadK, 1, 2),
            Instr::abc(Op::Concat, 0, 0, 1),
            Instr::abx(Op::LoadK, 1, 0),
            Instr::abc(Op::Return, 0, 2, 0),
        ],
        k,
        2,
    );
    let out = vm.execute(&p).unwrap();
    assert_eq!(
        out[0], out[1],
        "\"a\" .. \"b\" should be the same handle as \"ab\""
    );
}

#[test]
fn builtins_get_fast_paths() {
    let mut vm = Vm::new();
    for name in ["type", "setmetatable", "getmetatable", "rawget", "next"] {
        let f = vm.global(name);
        assert!(fastcall::has_fast_path(&vm, f), "{name}");
    }
    let Value::Table(math) = vm.global("math") else {
        panic!("no math")
    };
    let floor = vm.heap.intern("floor");
    let f = vm.heap.table(math).get_str(floor);
    assert!(fastcall::has_fast_path(&vm, f));
    // Natives the host registers, and the rest of the library, don't.
    let print = vm.global("print");
    assert!(!fastcall::has_fast_path(&vm, print));
    let user = vm.native("mine", Rc::new(|_, _| Ok(Rets::new())));
    assert!(!fastcall::has_fast_path(&vm, user));
    // `ipairs` hands out an iterator that has one.
    let p = proto(
        vec![
            Instr::abx(Op::GetGlobal, 0, 0),
            Instr::abc(Op::NewTable, 1, 0, 0),
            Instr::abc(Op::Call, 0, 2, 4),
            Instr::abc(Op::Return, 0, 1, 0),
        ],
        vec![Constant::String("ipairs".into())],
        4,
    );
    let iter = vm.execute(&p).unwrap()[0];
    assert!(fastcall::has_fast_path(&vm, iter));
}
