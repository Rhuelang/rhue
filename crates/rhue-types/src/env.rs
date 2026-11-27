//! What scripts can see before they declare anything, the standard library's
//! types and whatever the host declared.

use crate::{HashMap, HashSet};
use std::rc::Rc;

use rhue_common::Diagnostic;

use crate::Exported;

/// A saved chunk's types read back, what it returns and its globals.
pub type Imported = (Type, Vec<(Rc<str>, Type)>);
use crate::check::Checker;
use crate::types::{AliasDef, ClosedScope, FnType, GenericVar, NamedType, Pack, Type};
use rhue_parser::ast::StmtKind;

/// A host type's members. Inherited ones live on the parent, see
/// [`Env::class_member`].
#[derive(Clone, Debug, Default)]
pub struct ClassType {
    /// The class it inherits from: a `Box` goes wherever its parent `Shape`
    /// does, and has its members.
    pub parent: Option<Rc<str>>,
    /// `(name, type, writable)`
    pub props: Vec<(Rc<str>, Type, bool)>,
    /// Function types whose first parameter is the object.
    pub methods: Vec<(Rc<str>, Type)>,
    /// What reading a name that isn't a member gives, when the host has an
    /// index hook for the class (children by name, say).
    pub index: Option<Type>,
}

/// A class member found by [`Env::class_member`].
pub enum ClassMember<'a> {
    Prop(&'a Type, bool),
    Method(&'a Type),
}

impl ClassType {
    pub fn prop(&self, name: &str) -> Option<(&Type, bool)> {
        self.props
            .iter()
            .find(|(n, _, _)| &**n == name)
            .map(|(_, t, w)| (t, *w))
    }

    pub fn method(&self, name: &str) -> Option<&Type> {
        self.methods
            .iter()
            .find(|(n, _)| &**n == name)
            .map(|(_, t)| t)
    }
}

/// Global names and host types, shared by every chunk an engine checks.
#[derive(Clone, Debug, Default)]
pub struct Env {
    pub(crate) globals: HashMap<Rc<str>, Type>,
    pub(crate) classes: HashMap<Rc<str>, ClassType>,
    /// Globals the host (or the standard library) declared. Scripts have to
    /// respect their types, globals scripts defined can be redefined.
    pub(crate) host: HashSet<Rc<str>>,
    /// The types of modules that already got checked, for `require("name")`.
    pub(crate) modules: HashMap<Rc<str>, Type>,
    /// The aliases of imported types (see [`Env::import`]), kept alive here
    /// since the aliases only point at their scope weakly.
    pub(crate) imported: Vec<Rc<ClosedScope>>,
}

/// The standard library in Luau type syntax. Overloads are intersections.
const BUILTINS: &[(&str, &str)] = &[
    ("print", "(...any) -> ()"),
    ("type", "(any) -> string"),
    ("typeof", "(any) -> string"),
    ("tostring", "(any) -> string"),
    ("tonumber", "(any, number?) -> number?"),
    ("error", "(any, number?) -> never"),
    ("assert", "<T>(T, any?) -> T"),
    (
        "select",
        "((\"#\", ...any) -> number) & ((number, ...any) -> ...any)",
    ),
    ("rawequal", "(any, any) -> boolean"),
    ("rawget", "(any, any) -> any"),
    ("rawset", "<T>(T, any, any) -> T"),
    ("rawlen", "(any) -> number"),
    // calls to these get typed specially (see `meta.rs`), these types are for
    // when the functions get used as values
    ("setmetatable", "<T>(T, any) -> T"),
    ("getmetatable", "(any) -> any"),
    ("unpack", "<T>({T}, number?, number?) -> ...T"),
    ("collectgarbage", "(string?) -> number"),
    ("require", "(string) -> any"),
    ("pcall", "(any, ...any) -> (boolean, ...any)"),
    ("xpcall", "(any, any, ...any) -> (boolean, ...any)"),
    ("next", "<K, V>({[K]: V}, K?) -> (K?, V)"),
    (
        "pairs",
        "<K, V>({[K]: V}) -> (({[K]: V}, K?) -> (K?, V), {[K]: V}, nil)",
    ),
    (
        "ipairs",
        "<V>({V}) -> (({V}, number) -> (number?, V), {V}, number)",
    ),
    (
        "table",
        "{
            insert: (<T>({T}, T) -> ()) & (<T>({T}, number, T) -> ()),
            remove: <T>({T}, number?) -> T?,
            concat: ({any}, string?, number?, number?) -> string,
            unpack: <T>({T}, number?, number?) -> ...T,
            pack: (...any) -> {[any]: any},
            clear: ({[any]: any}) -> (),
            find: <T>({T}, T, number?) -> number?,
            create: <T>(number, T?) -> {T},
            move: <T>({T}, number, number, number, {T}?) -> {T},
            sort: <T>({T}, ((T, T) -> boolean)?) -> (),
            freeze: <T>(T) -> T,
            isfrozen: ({[any]: any}) -> boolean,
            clone: <T>(T) -> T,
        }",
    ),
    (
        "string",
        "{
            len: (string) -> number,
            sub: (string, number, number?) -> string,
            upper: (string) -> string,
            lower: (string) -> string,
            reverse: (string) -> string,
            rep: (string, number) -> string,
            byte: (string, number?, number?) -> ...number,
            char: (...number) -> string,
            find: (string, string, number?, boolean?) -> (number?, number?, ...string),
            match: (string, string, number?) -> ...string?,
            gmatch: (string, string, number?) -> () -> ...string,
            gsub: (string, string, string | number | {[string]: any} | (...string) -> any, number?) -> (string, number),
            split: (string, string?) -> {string},
            format: (string, ...any) -> string,
        }",
    ),
    (
        "math",
        "{
            abs: (number) -> number, floor: (number) -> number, ceil: (number) -> number,
            sqrt: (number) -> number, exp: (number) -> number, sin: (number) -> number,
            cos: (number) -> number, tan: (number) -> number, round: (number) -> number,
            sign: (number) -> number, log: (number, number?) -> number,
            fmod: (number, number) -> number, min: (number, ...number) -> number,
            max: (number, ...number) -> number, clamp: (number, number, number) -> number,
            random: (number?, number?) -> number, randomseed: (number) -> (),
            pi: number, huge: number,
        }",
    ),
    (
        "os",
        "{
            time: ({ year: number, month: number, day: number, hour: number?, min: number?, sec: number? }?) -> number,
            clock: () -> number,
            date: (string?, number?) -> any,
            difftime: (number, number?) -> number,
        }",
    ),
    (
        "utf8",
        "{
            char: (...number) -> string,
            codepoint: (string, number?, number?) -> ...number,
            len: (string, number?, number?) -> (number?, number?),
            offset: (string, number, number?) -> number?,
            codes: (string) -> ((string, number) -> (number?, number), string, number),
        }",
    ),
    (
        "bit32",
        "{
            band: (...number) -> number, bor: (...number) -> number,
            bxor: (...number) -> number, btest: (...number) -> boolean,
            bnot: (number) -> number, lshift: (number, number) -> number,
            rshift: (number, number) -> number, arshift: (number, number) -> number,
            lrotate: (number, number) -> number, rrotate: (number, number) -> number,
            extract: (number, number, number?) -> number,
            replace: (number, number, number, number?) -> number,
            countlz: (number) -> number, countrz: (number) -> number,
            byteswap: (number) -> number,
        }",
    ),
    (
        "debug",
        "{
            traceback: ((string?, number?) -> string) & ((thread, string?, number?) -> string),
            info: ((number | (...any) -> ...any, string) -> ...any)
                & ((thread, number, string) -> ...any),
        }",
    ),
    // `vector` is a host type with its components declared in `Env::new`, the
    // checker has its arithmetic built in
    (
        "vector",
        "{
            create: (number, number, number?) -> vector,
            magnitude: (vector) -> number, normalize: (vector) -> vector,
            cross: (vector, vector) -> vector, dot: (vector, vector) -> number,
            angle: (vector, vector, vector?) -> number,
            floor: (vector) -> vector, ceil: (vector) -> vector,
            abs: (vector) -> vector, sign: (vector) -> vector,
            clamp: (vector, vector, vector) -> vector,
            max: (vector, ...vector) -> vector, min: (vector, ...vector) -> vector,
            zero: vector, one: vector,
        }",
    ),
    // `buffer` is a host type with no members, declared just by mentioning it
    (
        "buffer",
        "{
            create: (number) -> buffer, fromstring: (string) -> buffer,
            tostring: (buffer) -> string, len: (buffer) -> number,
            readi8: (buffer, number) -> number, readu8: (buffer, number) -> number,
            readi16: (buffer, number) -> number, readu16: (buffer, number) -> number,
            readi32: (buffer, number) -> number, readu32: (buffer, number) -> number,
            readf32: (buffer, number) -> number, readf64: (buffer, number) -> number,
            writei8: (buffer, number, number) -> (), writeu8: (buffer, number, number) -> (),
            writei16: (buffer, number, number) -> (), writeu16: (buffer, number, number) -> (),
            writei32: (buffer, number, number) -> (), writeu32: (buffer, number, number) -> (),
            writef32: (buffer, number, number) -> (), writef64: (buffer, number, number) -> (),
            readstring: (buffer, number, number) -> string,
            writestring: (buffer, number, string, number?) -> (),
            copy: (buffer, number, buffer, number?, number?) -> (),
            fill: (buffer, number, number, number?) -> (),
            readbits: (buffer, number, number) -> number,
            writebits: (buffer, number, number, number) -> (),
        }",
    ),
    // same for `thread`
    (
        "coroutine",
        "{
            create: ((...any) -> ...any) -> thread,
            resume: (thread, ...any) -> (boolean, ...any),
            yield: (...any) -> ...any,
            status: (thread) -> string,
            wrap: ((...any) -> ...any) -> (...any) -> ...any,
            running: () -> (thread, boolean),
            isyieldable: () -> boolean,
            close: (thread) -> boolean,
        }",
    ),
];

impl Env {
    /// The standard library's globals.
    pub fn new() -> Self {
        let mut env = Env::default();
        for (name, ty) in BUILTINS {
            env.declare_global(name, ty)
                .unwrap_or_else(|e| panic!("builtin `{name}`: {}", e[0].message));
        }
        // `vector`'s components, read-only (Roblox's capitals too)
        for c in ["x", "y", "z", "X", "Y", "Z"] {
            env.declare_property("vector", c, "number", false)
                .expect("a vector component's type");
        }
        env
    }

    /// No globals at all (for `Engine::bare`).
    pub fn bare() -> Self {
        Env::default()
    }

    /// Reads back a chunk's exported types (see [`Exported`]). Its aliases
    /// become aliases that resolve among themselves wherever they're used, so
    /// recursive types stay recursive and classes keep their methods and
    /// operators. Returns the chunk's return type and its globals' types, or
    /// `None` if something doesn't parse or resolve here.
    pub fn import(&mut self, ex: &Exported) -> Option<Imported> {
        let scope: Rc<ClosedScope> = Rc::default();
        for def in &ex.aliases {
            let chunk = rhue_parser::parse_chunk(def).ok()?;
            for stmt in &chunk.body.stmts {
                let StmtKind::TypeAlias(a) = &stmt.kind else {
                    return None;
                };
                let alias = Rc::new(AliasDef {
                    name: a.name.clone(),
                    generics: a.generics.clone(),
                    body: a.ty.clone(),
                    scope_depth: 0,
                    generic_depth: 0,
                    closed: Some(Rc::downgrade(&scope)),
                });
                scope.borrow_mut().insert(a.name.clone(), alias);
            }
        }
        let parse = |s: &str| rhue_parser::parse_type(s).ok();
        let result = {
            let aliases = scope.borrow().clone();
            let mut c = Checker::for_declarations(self);
            // every alias once, so a broken one fails here instead of in
            // whatever checks code that uses it
            for alias in aliases.values() {
                let mut args = Vec::new();
                let mut packs = Vec::new();
                for g in &alias.generics {
                    let var = Type::Generic(GenericVar::new(g.name.clone(), g.pack));
                    if g.pack {
                        packs.push(Pack {
                            types: Vec::new(),
                            variadic: Some(var),
                        });
                    } else {
                        args.push(var);
                    }
                }
                c.expand(&Type::Named(Rc::new(NamedType {
                    alias: alias.clone(),
                    args,
                    packs,
                    expanded: Default::default(),
                })));
            }
            let returns = c.resolve_with_aliases(aliases.clone(), &parse(&ex.returns)?);
            let mut globals = Vec::new();
            for (name, ty) in &ex.globals {
                let t = c.resolve_with_aliases(aliases.clone(), &parse(ty)?);
                globals.push((Rc::from(name.as_str()), t));
            }
            c.errors.is_empty().then_some((returns, globals))
        };
        if result.is_some() {
            self.imported.push(scope);
        }
        result
    }

    /// Parses and resolves a type in Luau syntax against what's declared (host
    /// types, other globals' types aren't in scope). `None` if it doesn't parse
    /// or names something unknown. For reading back
    /// [`portable`](crate::portable) types.
    pub fn parse_type(&self, ty: &str) -> Option<Type> {
        let t = rhue_parser::parse_type(ty).ok()?;
        Checker::for_declarations(self).resolve_declaration(&t).ok()
    }

    /// Resolves a host declaration. Names that aren't known types are assumed
    /// to be host types declared later (a function can mention a userdata type
    /// before the type itself is set up).
    fn resolve(&mut self, ty: &str) -> Result<Type, Vec<Diagnostic>> {
        let t = rhue_parser::parse_type(ty)?;
        match Checker::for_declarations(self).resolve_declaration(&t) {
            Ok(t) => Ok(t),
            Err(errors) => {
                let unknown: Vec<String> = errors
                    .iter()
                    .filter_map(|d| {
                        d.message
                            .strip_prefix("Unknown type '")
                            .and_then(|r| r.strip_suffix('\''))
                            .map(str::to_string)
                    })
                    .collect();
                if unknown.len() != errors.len() {
                    return Err(errors);
                }
                for name in unknown {
                    self.declare_class(&name, None);
                }
                Checker::for_declarations(self).resolve_declaration(&t)
            }
        }
    }

    /// Adds a field to a global table's type (making the table type if needed),
    /// like a userdata type's constructor `Player.new`.
    pub fn declare_global_field(
        &mut self,
        table: &str,
        field: &str,
        ty: &str,
    ) -> Result<(), Vec<Diagnostic>> {
        let t = self.resolve(ty)?;
        let mut tt = match self.globals.get(table) {
            Some(Type::Table(existing)) => (**existing).clone(),
            _ => crate::types::TableType::default(),
        };
        tt.props.retain(|(n, _)| &**n != field);
        tt.props.push((Rc::from(field), t));
        self.declare_global_type(table, Type::table(tt));
        Ok(())
    }

    /// Every global the checker knows, with its type.
    pub fn globals(&self) -> impl Iterator<Item = (&Rc<str>, &Type)> {
        self.globals.iter()
    }

    /// What `require(name)` gives, if the module's been checked.
    pub fn module_type(&self, name: &str) -> Option<&Type> {
        self.modules.get(name)
    }

    /// Records a module's type (what its chunk returns).
    pub fn define_module(&mut self, name: &str, ty: Type) {
        self.modules.insert(Rc::from(name), ty);
    }

    /// Declares (or redeclares) a global with a type in Luau syntax:
    /// `env.declare_global("add", "(number, number) -> number")`.
    pub fn declare_global(&mut self, name: &str, ty: &str) -> Result<(), Vec<Diagnostic>> {
        let t = self.resolve(ty)?;
        self.declare_global_type(name, t);
        Ok(())
    }

    pub fn declare_global_type(&mut self, name: &str, ty: Type) {
        self.globals.insert(Rc::from(name), ty);
        self.host.insert(Rc::from(name));
    }

    /// Records a global a checked chunk defined, for later chunks.
    pub fn define_script_global(&mut self, name: &str, ty: Type) {
        if !self.host.contains(name) {
            self.globals.insert(Rc::from(name), ty);
        }
    }

    /// Swaps the globals scripts defined for `globals` and returns the old ones
    /// (for checking chunks that run in their own environment, whose globals
    /// other chunks can't see).
    pub fn swap_script_globals(&mut self, globals: Vec<(Rc<str>, Type)>) -> Vec<(Rc<str>, Type)> {
        let host = &self.host;
        let mut old: Vec<(Rc<str>, Type)> = self
            .globals
            .iter()
            .filter(|(n, _)| !host.contains(*n))
            .map(|(n, t)| (n.clone(), t.clone()))
            .collect();
        old.sort_by(|a, b| a.0.cmp(&b.0));
        for (n, _) in &old {
            self.globals.remove(n);
        }
        for (n, t) in globals {
            self.define_script_global(&n, t);
        }
        old
    }

    pub fn is_host_global(&self, name: &str) -> bool {
        self.host.contains(name)
    }

    /// A global's type, if declared.
    pub fn global(&self, name: &str) -> Option<&Type> {
        self.globals.get(name)
    }

    /// Declares a host type (fine to call twice). Its name can then be used in
    /// types: `(Player) -> number`. With a parent it's a subtype of it and has
    /// its members. A later call with a parent sets it, one without leaves it
    /// alone, and a parent that would make a loop is ignored.
    pub fn declare_class(&mut self, name: &str, parent: Option<&str>) {
        if let Some(p) = parent {
            self.declare_class(p, None);
        }
        let loops = parent.is_some_and(|p| self.is_subclass(p, name));
        let c = self.classes.entry(Rc::from(name)).or_default();
        if let Some(p) = parent
            && !loops
        {
            c.parent = Some(Rc::from(p));
        }
    }

    /// Class `name` and its ancestors, nearest first.
    pub fn class_chain<'a>(&'a self, name: &str) -> impl Iterator<Item = &'a ClassType> + 'a {
        let mut next = self.classes.get(name);
        // classes can't loop (see declare_class), the cap is a backstop
        let mut left = 1000;
        std::iter::from_fn(move || {
            let c = next.take()?;
            left -= 1;
            if left > 0 {
                next = c.parent.as_deref().and_then(|p| self.classes.get(p));
            }
            Some(c)
        })
    }

    /// Member `name` of class `class`, its own or the nearest ancestor's. A
    /// class having both a method and a property by that name gives `prefer`.
    pub fn class_member(
        &self,
        class: &str,
        name: &str,
        prefer_method: bool,
    ) -> Option<ClassMember<'_>> {
        self.class_chain(class).find_map(|c| {
            let prop = c.prop(name).map(|(t, w)| ClassMember::Prop(t, w));
            let method = c.method(name).map(ClassMember::Method);
            if prefer_method {
                method.or(prop)
            } else {
                prop.or(method)
            }
        })
    }

    /// What reading a name that isn't a member of `class` gives: the nearest
    /// index type up the chain, if there's one.
    pub fn class_index(&self, class: &str) -> Option<&Type> {
        self.class_chain(class).find_map(|c| c.index.as_ref())
    }

    /// Says reading a name that isn't a member of class `class` gives `ty`
    /// (and the classes under it, unless they say otherwise). Goes with an
    /// index hook on the class at run time.
    pub fn declare_index(&mut self, class: &str, ty: &str) -> Result<(), Vec<Diagnostic>> {
        self.declare_class(class, None);
        let t = self.resolve(ty)?;
        self.classes.get_mut(class).expect("declared above").index = Some(t);
        Ok(())
    }

    /// Whether class `x` is `y` or has it among its ancestors.
    pub fn is_subclass(&self, x: &str, y: &str) -> bool {
        if x == y {
            return true;
        }
        let mut cur = self.classes.get(x).and_then(|c| c.parent.clone());
        let mut left = 1000;
        while let Some(p) = cur {
            if &*p == y {
                return true;
            }
            left -= 1;
            if left == 0 {
                return false;
            }
            cur = self.classes.get(&*p).and_then(|c| c.parent.clone());
        }
        false
    }

    /// A property on a host type.
    pub fn declare_property(
        &mut self,
        class: &str,
        name: &str,
        ty: &str,
        writable: bool,
    ) -> Result<(), Vec<Diagnostic>> {
        self.declare_class(class, None);
        let t = self.resolve(ty)?;
        let c = self.classes.get_mut(class).expect("declared above");
        match c.props.iter_mut().find(|(n, _, _)| &**n == name) {
            Some(p) => {
                p.1 = t;
                p.2 = p.2 || writable;
            }
            None => c.props.push((Rc::from(name), t, writable)),
        }
        Ok(())
    }

    /// A method on a host type. `ty` is the function's type *including* the
    /// object as its first parameter: `(Player, number) -> ()`.
    pub fn declare_method(
        &mut self,
        class: &str,
        name: &str,
        ty: &str,
    ) -> Result<(), Vec<Diagnostic>> {
        self.declare_class(class, None);
        let t = self.resolve(ty)?;
        let c = self.classes.get_mut(class).expect("declared above");
        c.methods.retain(|(n, _)| &**n != name);
        c.methods.push((Rc::from(name), t));
        Ok(())
    }

    pub fn class(&self, name: &str) -> Option<&ClassType> {
        self.classes.get(name)
    }

    /// The `string` library's function for `s:name(...)`.
    pub(crate) fn string_method(&self, name: &str) -> Option<Type> {
        match self.globals.get("string")? {
            Type::Table(t) => t.prop(name).cloned(),
            _ => None,
        }
    }
}

/// `(...any) -> ...any`, the type of an opaque function value.
pub fn any_function() -> Type {
    Type::function(FnType::any())
}
