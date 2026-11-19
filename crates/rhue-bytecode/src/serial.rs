//! A compact binary form of a compiled chunk, so a host can compile once
//! and load it a bunch of times without parsing, resolving or type-checking.
//!
//! ```text
//! chunk   := MAGIC version:u8 name:str text:str types proto
//! types   := 0 | 1 returns:str [name:str type:str] [alias:str]
//!            (version 2 on; the aliases from version 3)
//! proto   := flags:u8 [name:str] code:[u32] constants:[constant]
//!            spans:[span] max_stack:u8 num_params:u8 protos:[proto]
//!            upvalues:[in_stack:u8 index:u8 name:str]
//!            locals:[name:str reg:u8 start:uv end:uv]
//!            [templates:[array:uv hash:uv keys:[uv]]] span
//!            (templates when flag 4 is set, from version 4)
//! constant:= 0 f64 | 1 str
//! span    := start:uv len:uv
//! str     := len:uv bytes          [x] := count:uv x*
//! ```
//!
//! `uv` is an unsigned LEB128 integer. The source text stays in so runtime
//! errors and debuggers can still point at lines. The optional types are
//! Luau type syntax (what the chunk returns and the globals it defines) so
//! a loaded chunk or module keeps its types for the checker. This crate
//! just carries them around.
//!
//! Loading only checks the *shape* of the bytes (lengths fit, strings are
//! UTF-8, nesting is bounded, spans are inside the text). Whether the code
//! is safe to run is the [verifier](crate::verify)'s job, and hosts have
//! to run it on every loaded chunk (`Engine::load` does).

use std::fmt;
use std::rc::Rc;

use rhue_common::{SourceFile, Span};

use crate::{Constant, Instr, LocalVar, Proto, TableTemplate, UpvalDesc};

/// The first bytes of every serialized chunk.
pub const MAGIC: &[u8; 5] = b"\x1bRhue";

/// Bumped whenever the encoding or instruction set changes. Chunks from a newer
/// version get refused instead of misread (older versions are subsets of this
/// one and still load).
pub const FORMAT_VERSION: u8 = 4;

/// The types saved with a chunk, as Luau type syntax.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChunkTypes {
    /// `type Name = ...` definitions the types below can refer to (so recursive
    /// types and classes save exactly).
    pub aliases: Vec<String>,
    /// What the chunk returns (its type as a module).
    pub returns: String,
    /// The globals it defines, and their types.
    pub globals: Vec<(String, String)>,
}

/// How deep functions can nest in a loaded chunk (same as the parser's limit).
const MAX_NESTING: u32 = 1000;

/// Why some bytes couldn't load as a chunk.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadError {
    pub offset: usize,
    pub message: String,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (at byte {})", self.message, self.offset)
    }
}

impl std::error::Error for LoadError {}

/// Whether `bytes` start like a saved chunk.
pub fn is_chunk(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

impl Proto {
    /// Saves this main chunk (and everything nested in it), without types (see
    /// [`write_chunk`]).
    pub fn to_bytes(&self) -> Vec<u8> {
        write_chunk(self, None)
    }

    /// Reads a chunk written by [`to_bytes`](Self::to_bytes) or
    /// [`write_chunk`], ignoring any types. It's *not* verified, run
    /// [`verify`](crate::verify) before executing it.
    pub fn from_bytes(bytes: &[u8]) -> Result<Rc<Proto>, LoadError> {
        read_chunk(bytes).map(|(p, _)| p)
    }
}

/// Saves a main chunk, plus the types it exports if you give them.
pub fn write_chunk(proto: &Proto, types: Option<&ChunkTypes>) -> Vec<u8> {
    let mut w = Writer(Vec::with_capacity(64 + proto.source.text.len()));
    w.0.extend_from_slice(MAGIC);
    w.0.push(FORMAT_VERSION);
    w.str(&proto.source.name);
    w.str(&proto.source.text);
    match types {
        None => w.0.push(0),
        Some(t) => {
            w.0.push(1);
            w.str(&t.returns);
            w.uv(t.globals.len() as u64);
            for (n, ty) in &t.globals {
                w.str(n);
                w.str(ty);
            }
            w.uv(t.aliases.len() as u64);
            for a in &t.aliases {
                w.str(a);
            }
        }
    }
    w.proto(proto);
    w.0
}

/// Reads a chunk and the types saved with it (none for chunks saved without, or
/// in format 1). Not verified.
pub fn read_chunk(bytes: &[u8]) -> Result<(Rc<Proto>, Option<ChunkTypes>), LoadError> {
    let mut r = Reader { bytes, pos: 0 };
    if !is_chunk(bytes) {
        return Err(r.fail("not a compiled Rhue chunk"));
    }
    r.pos = MAGIC.len();
    let version = r.u8()?;
    if !(1..=FORMAT_VERSION).contains(&version) {
        return Err(LoadError {
            offset: MAGIC.len(),
            message: format!(
                "compiled for bytecode format {version}, but this Rhue reads formats 1 to {FORMAT_VERSION}"
            ),
        });
    }
    let name = r.str()?;
    let text = r.str()?;
    let types = if version >= 2 {
        match r.u8()? {
            0 => None,
            1 => {
                let returns = r.str()?.to_string();
                let n = r.count(2)?;
                let mut globals = Vec::with_capacity(n);
                for _ in 0..n {
                    globals.push((r.str()?.to_string(), r.str()?.to_string()));
                }
                let mut aliases = Vec::new();
                if version >= 3 {
                    let n = r.count(1)?;
                    for _ in 0..n {
                        aliases.push(r.str()?.to_string());
                    }
                }
                Some(ChunkTypes {
                    aliases,
                    returns,
                    globals,
                })
            }
            _ => return Err(r.fail("bad types flag")),
        }
    } else {
        None
    };
    let source = SourceFile::new(name, text);
    let proto = r.proto(&source, 0)?;
    if r.pos != bytes.len() {
        return Err(r.fail("trailing bytes after the chunk"));
    }
    Ok((Rc::new(proto), types))
}

struct Writer(Vec<u8>);

impl Writer {
    fn uv(&mut self, mut n: u64) {
        loop {
            let byte = (n & 0x7f) as u8;
            n >>= 7;
            if n == 0 {
                self.0.push(byte);
                return;
            }
            self.0.push(byte | 0x80);
        }
    }

    fn str(&mut self, s: &str) {
        self.uv(s.len() as u64);
        self.0.extend_from_slice(s.as_bytes());
    }

    fn span(&mut self, s: Span) {
        self.uv(s.start as u64);
        self.uv(s.end.saturating_sub(s.start) as u64);
    }

    fn proto(&mut self, p: &Proto) {
        let flags = p.is_vararg as u8
            | (p.name.is_some() as u8) << 1
            | (!p.templates.is_empty() as u8) << 2;
        self.0.push(flags);
        if let Some(name) = &p.name {
            self.str(name);
        }
        self.uv(p.code.len() as u64);
        for ins in &p.code {
            self.0.extend_from_slice(&ins.0.to_le_bytes());
        }
        self.uv(p.constants.len() as u64);
        for k in &p.constants {
            match k {
                Constant::Number(n) => {
                    self.0.push(0);
                    self.0.extend_from_slice(&n.to_le_bytes());
                }
                Constant::String(s) => {
                    self.0.push(1);
                    self.str(s);
                }
            }
        }
        self.uv(p.spans.len() as u64);
        for s in &p.spans {
            self.span(*s);
        }
        self.0.push(p.max_stack);
        self.0.push(p.num_params);
        self.uv(p.protos.len() as u64);
        for child in &p.protos {
            self.proto(child);
        }
        self.uv(p.upvalues.len() as u64);
        for up in &p.upvalues {
            self.0.push(up.in_stack as u8);
            self.0.push(up.index);
            self.str(&up.name);
        }
        self.uv(p.locals.len() as u64);
        for l in &p.locals {
            self.str(&l.name);
            self.0.push(l.reg);
            self.uv(l.start_pc as u64);
            self.uv(l.end_pc as u64);
        }
        if !p.templates.is_empty() {
            self.uv(p.templates.len() as u64);
            for t in &p.templates {
                self.uv(t.array as u64);
                self.uv(t.hash as u64);
                self.uv(t.keys.len() as u64);
                for &k in &t.keys {
                    self.uv(k as u64);
                }
            }
        }
        self.span(p.span);
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn fail(&self, message: &str) -> LoadError {
        LoadError {
            offset: self.pos,
            message: message.to_string(),
        }
    }

    fn take(&mut self, n: usize) -> Result<&[u8], LoadError> {
        if self.bytes.len() - self.pos < n {
            return Err(self.fail("unexpected end of the chunk"));
        }
        let s = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8, LoadError> {
        Ok(self.take(1)?[0])
    }

    fn uv(&mut self) -> Result<u64, LoadError> {
        let mut n: u64 = 0;
        for shift in (0..64).step_by(7) {
            let b = self.u8()?;
            n |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(n);
            }
        }
        Err(self.fail("integer too long"))
    }

    fn u32(&mut self) -> Result<u32, LoadError> {
        let n = self.uv()?;
        u32::try_from(n).map_err(|_| self.fail("number out of range"))
    }

    /// A count of items that are each at least `min_size` bytes, checked
    /// against what's left so a damaged length can't allocate like crazy.
    fn count(&mut self, min_size: usize) -> Result<usize, LoadError> {
        let n = self.uv()?;
        let left = (self.bytes.len() - self.pos) as u64;
        if n.saturating_mul(min_size as u64) > left {
            return Err(self.fail("length runs past the end of the chunk"));
        }
        Ok(n as usize)
    }

    fn str(&mut self) -> Result<Rc<str>, LoadError> {
        let n = self.count(1)?;
        let start = self.pos;
        let bytes = self.take(n)?;
        std::str::from_utf8(bytes)
            .map(Rc::from)
            .map_err(|_| LoadError {
                offset: start,
                message: "string isn't UTF-8".into(),
            })
    }

    fn span(&mut self, text_len: u32) -> Result<Span, LoadError> {
        let start = self.u32()?;
        let len = self.u32()?;
        match start.checked_add(len) {
            Some(end) if end <= text_len => Ok(Span::new(start, end)),
            _ => Err(self.fail("span outside the source text")),
        }
    }

    fn proto(&mut self, source: &Rc<SourceFile>, depth: u32) -> Result<Proto, LoadError> {
        if depth > MAX_NESTING {
            return Err(self.fail("functions nested too deeply"));
        }
        let text_len = source.text.len() as u32;
        let flags = self.u8()?;
        if flags & !0b111 != 0 {
            return Err(self.fail("unknown prototype flags"));
        }
        let name = if flags & 2 != 0 {
            Some(self.str()?)
        } else {
            None
        };
        let n = self.count(4)?;
        let mut code = Vec::with_capacity(n);
        for _ in 0..n {
            let b = self.take(4)?;
            code.push(Instr(u32::from_le_bytes([b[0], b[1], b[2], b[3]])));
        }
        let n = self.count(2)?;
        let mut constants = Vec::with_capacity(n);
        for _ in 0..n {
            constants.push(match self.u8()? {
                0 => {
                    let b = self.take(8)?;
                    Constant::Number(f64::from_le_bytes(b.try_into().unwrap()))
                }
                1 => Constant::String(self.str()?),
                _ => return Err(self.fail("unknown constant kind")),
            });
        }
        let n = self.count(2)?;
        let mut spans = Vec::with_capacity(n);
        for _ in 0..n {
            spans.push(self.span(text_len)?);
        }
        let max_stack = self.u8()?;
        let num_params = self.u8()?;
        let n = self.count(1)?;
        let mut protos = Vec::with_capacity(n);
        for _ in 0..n {
            protos.push(Rc::new(self.proto(source, depth + 1)?));
        }
        let n = self.count(3)?;
        let mut upvalues = Vec::with_capacity(n);
        for _ in 0..n {
            let in_stack = match self.u8()? {
                0 => false,
                1 => true,
                _ => return Err(self.fail("bad upvalue kind")),
            };
            let index = self.u8()?;
            let name = self.str()?;
            upvalues.push(UpvalDesc {
                in_stack,
                index,
                name,
            });
        }
        let n = self.count(4)?;
        let mut locals = Vec::with_capacity(n);
        for _ in 0..n {
            let name = self.str()?;
            let reg = self.u8()?;
            let start_pc = self.u32()?;
            let end_pc = self.u32()?;
            locals.push(LocalVar {
                name,
                reg,
                start_pc,
                end_pc,
            });
        }
        let mut templates = Vec::new();
        if flags & 4 != 0 {
            let n = self.count(3)?;
            templates.reserve(n);
            for _ in 0..n {
                let array = self.u32()?;
                let hash = self.u32()?;
                let n = self.count(1)?;
                let mut keys = Vec::with_capacity(n);
                for _ in 0..n {
                    keys.push(self.u32()?);
                }
                templates.push(TableTemplate { array, hash, keys });
            }
        }
        let span = self.span(text_len)?;
        Ok(Proto {
            templates,
            name,
            code,
            constants,
            spans,
            max_stack,
            num_params,
            is_vararg: flags & 1 != 0,
            protos,
            upvalues,
            locals,
            span,
            source: source.clone(),
            ..Proto::default()
        })
    }
}
