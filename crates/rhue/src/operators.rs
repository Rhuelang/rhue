//! Operators on host classes: `a + b`, `-a`, `a == b`, `#a`, `a(...)`,
//! `for x in a` and friends, with overloads picked by the operands' types.

use std::cell::RefCell;
use std::rc::Rc;

use rhue_types::Type;
use rhue_vm::{NativeFn, Value};

use crate::IntoNative;
use crate::userdata::declare;

/// An operator a host class can have, see [`TypeBuilder::operator`](crate::TypeBuilder::operator).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Operator {
    /// `a + b`
    Add,
    /// `a - b`
    Sub,
    /// `a * b`
    Mul,
    /// `a / b`
    Div,
    /// `a // b`
    IDiv,
    /// `a % b`
    Mod,
    /// `a ^ b`
    Pow,
    /// `-a`, the handler gets `a` twice like Lua does it
    Unm,
    /// `a .. b`
    Concat,
    /// `#a`
    Len,
    /// `a == b` (and `~=`), for two different objects
    Eq,
    /// `a < b` (and `>`)
    Lt,
    /// `a <= b` (and `>=`)
    Le,
    /// `tostring(a)`, `print(a)` and string interpolation
    ToString,
    /// `a(...)`, the handler gets `a` and then the arguments
    Call,
    /// `for k, v in a`, the handler gives back the iterator function (and
    /// optionally its state and first control value)
    Iter,
}

impl Operator {
    /// The metamethod's name, `__add` for [`Operator::Add`].
    pub fn event(self) -> &'static str {
        match self {
            Operator::Add => "__add",
            Operator::Sub => "__sub",
            Operator::Mul => "__mul",
            Operator::Div => "__div",
            Operator::IDiv => "__idiv",
            Operator::Mod => "__mod",
            Operator::Pow => "__pow",
            Operator::Unm => "__unm",
            Operator::Concat => "__concat",
            Operator::Len => "__len",
            Operator::Eq => "__eq",
            Operator::Lt => "__lt",
            Operator::Le => "__le",
            Operator::ToString => "__tostring",
            Operator::Call => "__call",
            Operator::Iter => "__iter",
        }
    }

    fn arith_name(self) -> Option<&'static str> {
        Some(match self {
            Operator::Add => "add",
            Operator::Sub => "sub",
            Operator::Mul => "mul",
            Operator::Div => "div",
            Operator::IDiv => "idiv",
            Operator::Mod => "mod",
            Operator::Pow => "pow",
            Operator::Unm => "unm",
            _ => return None,
        })
    }
}

/// What one operand of an overload takes.
#[derive(Clone, Copy)]
enum Param {
    Any,
    Nil,
    Boolean,
    Number,
    String,
    Vector,
    Table,
    Function,
    /// That class or one under it.
    Class(u32),
    /// Something the runtime can't tell apart cheaply (a union, say), so it
    /// takes anything and the handler's own conversion has the last word.
    Loose,
}

pub(crate) struct Overload {
    params: Vec<(Param, bool)>,
    native: NativeFn,
    /// Copied from a parent class, so the class's own come first.
    inherited: bool,
}

pub(crate) type Overloads = Rc<RefCell<Vec<Overload>>>;

fn param(vm: &rhue_vm::Vm, t: &Type) -> (Param, bool) {
    let nullable = t.can_be_nil() && !matches!(t, Type::Nil | Type::Any);
    let t = if nullable { t.strip_nil() } else { t.clone() };
    let p = match &t {
        Type::Any | Type::Unknown => Param::Any,
        Type::Nil => Param::Nil,
        Type::Boolean | Type::BoolLit(_) => Param::Boolean,
        Type::Number => Param::Number,
        Type::String | Type::StrLit(_) => Param::String,
        Type::Table(_) => Param::Table,
        Type::Function(_) => Param::Function,
        Type::Class(c) if &**c == "vector" => Param::Vector,
        Type::Class(c) => match vm.class_by_name(c) {
            Some(id) => Param::Class(id),
            None => Param::Loose,
        },
        _ => Param::Loose,
    };
    (p, nullable)
}

fn takes(vm: &rhue_vm::Vm, (p, nullable): (Param, bool), v: Value) -> bool {
    if nullable && v == Value::Nil {
        return true;
    }
    match (p, v) {
        (Param::Any | Param::Loose, _) => true,
        (Param::Nil, Value::Nil)
        | (Param::Boolean, Value::Bool(_))
        | (Param::Number, Value::Number(_))
        | (Param::String, Value::String(_))
        | (Param::Vector, Value::Vector(_))
        | (Param::Table, Value::Table(_))
        | (Param::Function, Value::Function(_)) => true,
        (Param::Class(c), Value::UserData(u)) => {
            vm.class_is_under(vm.heap().userdata(u).type_id, c)
        }
        _ => false,
    }
}

/// A value's type for an error, a host object's class name.
fn type_name(vm: &rhue_vm::Vm, v: Value) -> String {
    match v {
        Value::UserData(u) => vm.class_name(vm.heap().userdata(u).type_id).to_string(),
        other => other.type_name().to_string(),
    }
}

/// Adds overload `f` of `op` to class `ty` (named `class`).
pub(crate) fn add<M, F: IntoNative<M>>(
    vm: &mut rhue_vm::Vm,
    ty: u32,
    class: &str,
    op: Operator,
    f: F,
) {
    F::declare_types(vm);
    let sig = F::signature();
    let event = op.event();
    let declared = crate::type_env(vm).declare_operator(class, event, &sig);
    let params = match &declared {
        Ok(Type::Function(f)) => f.params.iter().map(|t| param(vm, t)).collect(),
        _ => Vec::new(),
    };
    declare(declared.map(|_| ()), &sig);
    let native = f.into_native(Rc::from(format!("{class} {event}")));
    let overload = Overload {
        params,
        native,
        inherited: false,
    };

    // this class's own list, starting from the nearest parent's
    let key = (ty, event);
    let list = match crate::modules::host(vm).operators.get(&key) {
        Some(l) => l.clone(),
        None => {
            let mut inherited = Vec::new();
            let mut up = vm.class_parent(ty);
            while let Some(p) = up {
                if let Some(l) = crate::modules::host(vm).operators.get(&(p, event)) {
                    inherited = l
                        .borrow()
                        .iter()
                        .map(|o| Overload {
                            params: o.params.clone(),
                            native: o.native.clone(),
                            inherited: true,
                        })
                        .collect();
                    break;
                }
                up = vm.class_parent(p);
            }
            let l: Overloads = Rc::new(RefCell::new(inherited));
            crate::modules::host(vm).operators.insert(key, l.clone());
            let handler = dispatch(l.clone(), op, Rc::from(class));
            let v = vm.native(event, handler);
            vm.set_class_operator(ty, event, v)
                .expect("every Operator is one a class can have");
            l
        }
    };
    let mut list = list.borrow_mut();
    let at = list.iter().position(|o| o.inherited).unwrap_or(list.len());
    list.insert(at, overload);
}

/// The native the class gets for `op`: the first overload that takes the
/// operands, or an error saying none does.
fn dispatch(list: Overloads, op: Operator, class: Rc<str>) -> NativeFn {
    Rc::new(move |ctx, args| {
        let found = {
            let vm = ctx.vm();
            let list = list.borrow();
            list.iter()
                .find(|o| {
                    o.params
                        .iter()
                        .enumerate()
                        .all(|(i, &p)| takes(vm, p, args.get(i).copied().unwrap_or(Value::Nil)))
                })
                .map(|o| o.native.clone())
        };
        match found {
            Some(native) => native(ctx, args),
            None => {
                let vm = ctx.vm();
                let a = args.first().map_or("nil".into(), |&v| type_name(vm, v));
                let b = args.get(1).map_or("nil".into(), |&v| type_name(vm, v));
                Err(match op.arith_name() {
                    Some("unm") => format!("attempt to perform arithmetic (unm) on {a}"),
                    Some(name) => format!("attempt to perform arithmetic ({name}) on {a} and {b}"),
                    None => format!("no overload of {class}'s '{}' takes ({a}, {b})", op.event()),
                }
                .into())
            }
        }
    })
}
