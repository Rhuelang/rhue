//! Hot reload: merging what a module's new code made into what its old code
//! made, so everything already holding the old stuff picks up the new code.
//!
//! [`Vm::hot_swap`] walks the old and new values side by side. Functions found
//! at the same spot (a table field, or an upvalue with the same name) get
//! paired, and so do tables. Then:
//!
//! - each old function's slot gets the new function's code, so every handle to
//!   it (a callback the host kept, a field another module copied) runs the new
//!   code from its next call. Calls already running finish on the old code,
//!   their frames hold their own `Rc` of it
//! - upvalues get matched by name, and the new code uses the old cells, so a
//!   module's `local count = 0` keeps counting from where it was
//! - each old table takes the new table's functions and any keys it didn't
//!   have, keeps its own values for everything else (that's the state), and
//!   loses functions the new code doesn't have anymore
//!
//! Closures the old code made at runtime (a callback built inside a function)
//! aren't reachable from the module, so they keep their code until something
//! makes them again.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::heap::{FnRef, Function, LuaClosure, TableRef, UpvalRef, Upvalue};
use crate::{Value, Vm};

/// What a [`Vm::hot_swap`] changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SwapStats {
    /// Old functions that now run new code.
    pub functions: usize,
    /// Old tables the new ones got merged into.
    pub tables: usize,
    /// Upvalues whose old values carried over.
    pub upvalues: usize,
}

#[derive(Default)]
struct Pairs {
    /// new → old
    functions: HashMap<FnRef, FnRef>,
    tables: HashMap<TableRef, TableRef>,
    /// new cell (by address) → old cell
    cells: HashMap<*const (), (UpvalRef, UpvalRef)>,
    /// things both sides point at, left alone
    shared: HashSet<Obj>,
}

/// A table or function, as a set key (`Value` isn't `Hash`).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Obj {
    F(FnRef),
    T(TableRef),
}

fn obj(v: Value) -> Option<Obj> {
    match v {
        Value::Function(f) => Some(Obj::F(f)),
        Value::Table(t) => Some(Obj::T(t)),
        _ => None,
    }
}

impl Vm {
    /// Merges `new` (what a module's new code returned) into `old` (what its
    /// old code returned) and gives back the value to keep: `old` when the two
    /// could be paired (both tables, or both functions), `new` otherwise. See
    /// the module docs for what carries over.
    pub fn hot_swap(&mut self, old: Value, new: Value) -> (Value, SwapStats) {
        let mut pairs = Pairs::default();
        self.pair(old, new, &mut pairs);
        let paired = match new {
            Value::Function(f) => pairs.functions.contains_key(&f),
            Value::Table(t) => pairs.tables.contains_key(&t),
            _ => false,
        };
        let stats = SwapStats {
            functions: pairs.functions.len(),
            tables: pairs.tables.len(),
            upvalues: pairs.cells.len(),
        };

        // new closures and tables the pairing didn't match still point at new
        // things that should be their old counterparts now
        let mut fresh_fns = Vec::new();
        let mut fresh_tables = Vec::new();
        let mut seen = HashSet::new();
        self.collect_new(new, &pairs, &mut seen, &mut fresh_fns, &mut fresh_tables);

        // cells: the old value stays, unless there wasn't one
        for (new_cell, old_cell) in pairs.cells.values() {
            if self.cell_get(old_cell) == Value::Nil {
                let v = translate(&pairs, self.cell_get(new_cell));
                self.cell_set(old_cell, v);
            }
        }
        for (&new_t, &old_t) in &pairs.tables {
            self.merge_table(old_t, new_t, &pairs);
        }
        for &t in &fresh_tables {
            self.translate_table(t, &pairs);
        }
        for (&new_f, &old_f) in &pairs.functions {
            let cl = self.remapped(new_f, &pairs);
            self.heap.replace_closure(old_f, cl);
        }
        for &f in &fresh_fns {
            let cl = self.remapped(f, &pairs);
            self.heap.replace_closure(f, cl);
        }
        (if paired { old } else { new }, stats)
    }

    fn pair(&self, old: Value, new: Value, pairs: &mut Pairs) {
        if old == new {
            if let Some(o) = obj(new) {
                pairs.shared.insert(o);
            }
            return;
        }
        match (old, new) {
            (Value::Function(o), Value::Function(n)) => {
                if pairs.functions.contains_key(&n) {
                    return;
                }
                let (Function::Lua(oc), Function::Lua(nc)) =
                    (self.heap.function(o), self.heap.function(n))
                else {
                    return;
                };
                let (oc, nc) = (oc.clone(), nc.clone());
                pairs.functions.insert(n, o);
                for (i, desc) in nc.proto.upvalues.iter().enumerate() {
                    let Some(j) = oc.proto.upvalues.iter().position(|d| d.name == desc.name) else {
                        continue;
                    };
                    let (ocell, ncell) = (oc.upvalues[j].clone(), nc.upvalues[i].clone());
                    if Rc::ptr_eq(&ocell, &ncell) {
                        continue;
                    }
                    let key = Rc::as_ptr(&ncell) as *const ();
                    if pairs.cells.contains_key(&key) {
                        continue;
                    }
                    let (ov, nv) = (self.cell_get(&ocell), self.cell_get(&ncell));
                    pairs.cells.insert(key, (ncell, ocell));
                    self.pair(ov, nv, pairs);
                }
            }
            (Value::Table(o), Value::Table(n)) => {
                if pairs.tables.contains_key(&n) || self.heap.table(o).is_frozen() {
                    return;
                }
                pairs.tables.insert(n, o);
                let mut key = Value::Nil;
                while let Ok(Some((k, nv))) = self.heap.table(n).next(key) {
                    let ov = self.heap.table(o).get(k);
                    self.pair(ov, nv, pairs);
                    key = k;
                }
                if let (Some(om), Some(nm)) = (self.heap.table(o).meta(), self.heap.table(n).meta())
                {
                    self.pair(Value::Table(om), Value::Table(nm), pairs);
                }
            }
            _ => {}
        }
    }

    /// Everything reachable from `v` on the new side that didn't get paired.
    fn collect_new(
        &self,
        v: Value,
        pairs: &Pairs,
        seen: &mut HashSet<Obj>,
        fns: &mut Vec<FnRef>,
        tables: &mut Vec<TableRef>,
    ) {
        let Some(o) = obj(v) else {
            return;
        };
        if pairs.shared.contains(&o) || !seen.insert(o) {
            return;
        }
        match v {
            Value::Function(f) => {
                let Function::Lua(cl) = self.heap.function(f) else {
                    return;
                };
                if !pairs.functions.contains_key(&f) {
                    fns.push(f);
                }
                for cell in cl.upvalues.iter() {
                    // a paired cell's old value is what the new code ends up
                    // seeing, so its new value doesn't matter
                    if !pairs.cells.contains_key(&(Rc::as_ptr(cell) as *const ())) {
                        self.collect_new(self.cell_get(cell), pairs, seen, fns, tables);
                    }
                }
            }
            Value::Table(t) => {
                if self.heap.table(t).is_frozen() {
                    return;
                }
                if !pairs.tables.contains_key(&t) {
                    tables.push(t);
                }
                let mut key = Value::Nil;
                while let Ok(Some((k, nv))) = self.heap.table(t).next(key) {
                    self.collect_new(nv, pairs, seen, fns, tables);
                    key = k;
                }
                if let Some(m) = self.heap.table(t).meta() {
                    self.collect_new(Value::Table(m), pairs, seen, fns, tables);
                }
            }
            _ => {}
        }
    }

    fn merge_table(&mut self, old: TableRef, new: TableRef, pairs: &Pairs) {
        let mut entries = Vec::new();
        let mut key = Value::Nil;
        while let Ok(Some((k, v))) = self.heap.table(new).next(key) {
            entries.push((k, v));
            key = k;
        }
        let mut gone = Vec::new();
        let mut key = Value::Nil;
        while let Ok(Some((k, v))) = self.heap.table(old).next(key) {
            if matches!(v, Value::Function(_)) && self.heap.table(new).get(k) == Value::Nil {
                gone.push(k);
            }
            key = k;
        }
        let meta = self
            .heap
            .table(new)
            .meta()
            .map(|m| match pairs.tables.get(&m) {
                Some(&o) => o,
                None => m,
            });
        self.heap.with_table(old, |t| {
            for (k, nv) in entries {
                let ov = t.get(k);
                let take = match nv {
                    // functions always come from the new code
                    Value::Function(_) | Value::Table(_) => true,
                    _ => ov == Value::Nil,
                };
                if take {
                    let _ = t.set(k, translate(pairs, nv));
                }
            }
            for k in gone {
                let _ = t.set(k, Value::Nil);
            }
            t.set_meta(meta);
        });
    }

    fn translate_table(&mut self, t: TableRef, pairs: &Pairs) {
        let mut changes = Vec::new();
        let mut key = Value::Nil;
        while let Ok(Some((k, v))) = self.heap.table(t).next(key) {
            let tv = translate(pairs, v);
            if tv != v {
                changes.push((k, tv));
            }
            key = k;
        }
        let meta = self.heap.table(t).meta();
        let new_meta = meta.map(|m| pairs.tables.get(&m).copied().unwrap_or(m));
        self.heap.with_table(t, |tbl| {
            for (k, v) in changes {
                let _ = tbl.set(k, v);
            }
            tbl.set_meta(new_meta);
        });
    }

    /// New closure `f`'s code, with its upvalues pointing at the old cells
    /// wherever they got paired.
    fn remapped(&self, f: FnRef, pairs: &Pairs) -> Rc<LuaClosure> {
        let Function::Lua(cl) = self.heap.function(f) else {
            unreachable!("only Lua closures get paired or collected")
        };
        let upvalues: Rc<[UpvalRef]> = cl
            .upvalues
            .iter()
            .map(|c| match pairs.cells.get(&(Rc::as_ptr(c) as *const ())) {
                Some((_, old)) => old.clone(),
                None => c.clone(),
            })
            .collect();
        Rc::new(LuaClosure {
            proto: cl.proto.clone(),
            upvalues,
            constants: cl.constants.clone(),
            tree: cl.tree.clone(),
            env: cl.env,
        })
    }

    fn cell_get(&self, c: &UpvalRef) -> Value {
        match c.get() {
            Upvalue::Open { index, thread } if thread == self.current => self.stack[index as usize],
            Upvalue::Open { index, thread } => self.heap.thread(thread).state.stack[index as usize],
            Upvalue::Closed(v) => v,
        }
    }

    fn cell_set(&mut self, c: &UpvalRef, v: Value) {
        match c.get() {
            Upvalue::Open { index, thread } if thread == self.current => {
                self.stack[index as usize] = v
            }
            Upvalue::Open { index, thread } => {
                self.heap.thread_state_mut(thread).stack[index as usize] = v
            }
            Upvalue::Closed(_) => c.set(Upvalue::Closed(v)),
        }
    }
}

/// New things swapped for their old counterparts.
fn translate(pairs: &Pairs, v: Value) -> Value {
    match v {
        Value::Function(f) => pairs.functions.get(&f).map_or(v, |&o| Value::Function(o)),
        Value::Table(t) => pairs.tables.get(&t).map_or(v, |&o| Value::Table(o)),
        _ => v,
    }
}
