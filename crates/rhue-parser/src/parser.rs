use std::rc::Rc;

use rhue_common::{Diagnostic, Span};
use rhue_lexer::{Spanned, Token, lex};

use crate::ast::*;
use crate::strings;

mod definitions;
pub use definitions::parse_definitions;

/// Max nesting (recursion plus left-associative operator chains) before the
/// parser gives up, so hostile input can't build an endlessly deep tree.
pub const MAX_DEPTH: u32 = 1000;

/// Stop collecting after this many errors, the later ones are usually cascades
/// anyway.
pub const MAX_ERRORS: usize = 20;

/// Boxed so `Result`s stay small on the hot (successful) path.
type PResult<T> = Result<T, Box<Diagnostic>>;

/// Parses a whole chunk (a file or `Engine::eval` source). On failure you get
/// every error it found (up to [`MAX_ERRORS`]), in source order.
pub fn parse_chunk(source: &str) -> Result<Chunk, Vec<Diagnostic>> {
    let mut p = Parser::new(source)?;
    let body = p.block_until_eof();
    if p.errors.is_empty() {
        Ok(Chunk {
            span: body.span,
            body,
            name_count: p.next_name,
            mode: directive(source),
        })
    } else {
        Err(p.errors)
    }
}

/// Parses one expression and wraps it as `return <expr>` so it goes through the
/// same pipeline as a chunk (REPL / `eval_expr`).
pub fn parse_expr(source: &str) -> Result<Chunk, Vec<Diagnostic>> {
    let mut p = Parser::new(source)?;
    let result = p.expr().and_then(|e| {
        p.expect(Token::Eof, "after expression")?;
        Ok(e)
    });
    match result {
        Ok(e) => {
            let span = e.span;
            let body = Block {
                stmts: vec![],
                ret: Some(Return {
                    values: vec![e],
                    span,
                }),
                span,
            };
            Ok(Chunk {
                body,
                span,
                name_count: p.next_name,
                mode: None,
            })
        }
        Err(d) => Err(vec![*d]),
    }
}

/// Parses a type by itself (`(number, string) -> boolean`), the way host
/// declarations and definition strings are written.
pub fn parse_type(source: &str) -> Result<TypeExpr, Vec<Diagnostic>> {
    let mut p = Parser::new(source)?;
    let t = p.type_expr().and_then(|t| {
        p.expect(Token::Eof, "after the type")?;
        Ok(t)
    });
    t.map_err(|d| vec![*d])
}

struct Parser<'s> {
    source: &'s str,
    tokens: Vec<Spanned>,
    pos: usize,
    depth: u32,
    next_name: NameId,
    errors: Vec<Diagnostic>,
}

/// Runs `f` on a fresh stack segment if there's less than 64 KiB left, so deep
/// (but within the limit) nesting is fine even on an embedder's small threads.
fn grow<R>(f: impl FnOnce() -> R) -> R {
    stacker::maybe_grow(64 * 1024, 1024 * 1024, f)
}

/// Finds a `--!strict`, `--!nonstrict` or `--!nocheck` comment in the lines
/// before the first code. Other `--!` directives (`--!native`, ...) get
/// ignored.
fn directive(source: &str) -> Option<(TypeMode, Span)> {
    let mut offset = 0usize;
    for line in source.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let rest = trimmed.strip_prefix("--")?;
        let Some(word) = rest.strip_prefix('!') else {
            if rest.starts_with('[') {
                // a block comment might hide code after it, stop looking
                return None;
            }
            continue;
        };
        let word = word.split_whitespace().next().unwrap_or("");
        let mode = match word {
            "strict" => TypeMode::Strict,
            "nonstrict" => TypeMode::NonStrict,
            "nocheck" => TypeMode::NoCheck,
            _ => continue,
        };
        let lead = line.len() - line.trim_start().len();
        let s = rhue_common::offset(start + lead);
        return Some((mode, Span::new(s, s + 3 + rhue_common::offset(word.len()))));
    }
    None
}

fn error(message: impl Into<String>, span: Span) -> Box<Diagnostic> {
    Box::new(Diagnostic::error(message, span))
}

impl<'s> Parser<'s> {
    fn new(source: &'s str) -> Result<Self, Vec<Diagnostic>> {
        let lexed = lex(source);
        if !lexed.errors.is_empty() {
            return Err(lexed.errors);
        }
        Ok(Parser {
            source,
            tokens: lexed.tokens,
            pos: 0,
            depth: 0,
            next_name: 0,
            errors: Vec::new(),
        })
    }

    // ─ Token Helpers ─────────────────────────────────────────────────────────

    fn peek(&self) -> Spanned {
        self.tokens[self.pos]
    }

    fn peek_nth(&self, n: usize) -> Spanned {
        self.tokens[(self.pos + n).min(self.tokens.len() - 1)]
    }

    /// Whether there's a line break between the current token and the one
    /// before.
    fn newline_before(&self) -> bool {
        let prev_end = if self.pos == 0 {
            0
        } else {
            self.tokens[self.pos - 1].span.end
        };
        self.source[prev_end as usize..self.peek().span.start as usize].contains('\n')
    }

    fn at(&self, token: Token) -> bool {
        self.peek().token == token
    }

    fn bump(&mut self) -> Spanned {
        let t = self.peek();
        if t.token != Token::Eof {
            self.pos += 1;
        }
        t
    }

    fn eat(&mut self, token: Token) -> Option<Spanned> {
        self.at(token).then(|| self.bump())
    }

    fn expect(&mut self, token: Token, context: &str) -> PResult<Spanned> {
        match self.eat(token) {
            Some(t) => Ok(t),
            None => {
                let found = self.peek();
                Err(Box::new(
                    Diagnostic::error(
                        format!(
                            "expected {} {context}, found {}",
                            token.describe(),
                            found.token.describe()
                        ),
                        found.span,
                    )
                    .with_label(format!("expected {}", token.describe())),
                ))
            }
        }
    }

    /// Expects the `end` (or `until`) closing a construct, pointing back at the
    /// opener when it's missing.
    fn expect_closer(&mut self, closer: Token, opener: Spanned, what: &str) -> PResult<Spanned> {
        self.expect(closer, &format!("to close `{what}`"))
            .map_err(|mut d| {
                if opener.span.start != d.span().start {
                    d.secondary.push(rhue_common::Label {
                        span: opener.span,
                        message: format!("`{what}` opened here"),
                    });
                }
                d
            })
    }

    fn text(&self, span: Span) -> &'s str {
        span.text(self.source)
    }

    fn new_name(&mut self, span: Span) -> Name {
        let id = self.next_name;
        self.next_name += 1;
        Name {
            text: Rc::from(self.text(span)),
            id,
            span,
        }
    }

    fn name(&mut self, context: &str) -> PResult<Name> {
        let t = self.expect(Token::Ident, context)?;
        Ok(self.new_name(t.span))
    }

    // ─ Blocks & Error Recovery ───────────────────────────────────────────────

    fn block_until_eof(&mut self) -> Block {
        let mut block = self.block();
        // a stray closer at the top level (an `end` with no opener, usually
        // after an earlier error) gets reported, skipped, and we keep going
        while !self.at(Token::Eof) && self.errors.len() < MAX_ERRORS {
            let t = self.bump();
            self.report(
                error(format!("unexpected {}", t.token.describe()), t.span).with_label_boxed(
                    if matches!(
                        t.token,
                        Token::End | Token::Until | Token::Else | Token::Elseif
                    ) {
                        "nothing here to close"
                    } else {
                        "expected a statement"
                    },
                ),
            );
            let more = self.block();
            block.stmts.extend(more.stmts);
            if more.ret.is_some() {
                block.ret = more.ret;
            }
        }
        block
    }

    #[allow(clippy::boxed_local)] // errors arrive boxed from `PResult`
    fn report(&mut self, d: Box<Diagnostic>) {
        if self.errors.len() < MAX_ERRORS {
            self.errors.push(*d);
        }
    }

    fn block(&mut self) -> Block {
        // blocks nest through statements (`do do do ... end end end`) which the
        // expression depth counter can't see, so count them too
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            let span = self.peek().span;
            self.report(
                error("blocks nest too deeply", span)
                    .with_label_boxed(format!("nesting limit of {MAX_DEPTH} reached here")),
            );
            // nothing sensible can come after this, stop
            self.pos = self.tokens.len() - 1;
            self.depth -= 1;
            return Block {
                stmts: Vec::new(),
                ret: None,
                span,
            };
        }
        let block = grow(|| self.block_inner());
        self.depth -= 1;
        block
    }

    fn block_inner(&mut self) -> Block {
        let start = self.peek().span;
        let mut stmts = Vec::new();
        let mut ret = None;
        while !self.block_follow() && self.errors.len() < MAX_ERRORS {
            if self.eat(Token::Semi).is_some() {
                continue;
            }
            if self.at(Token::Return) {
                match self.return_stmt() {
                    Ok(r) => ret = Some(r),
                    Err(d) => {
                        self.report(d);
                        self.synchronize();
                    }
                }
                if !self.block_follow() {
                    let t = self.peek();
                    self.report(
                        error(
                            format!(
                                "expected end of block after `return`, found {}",
                                t.token.describe()
                            ),
                            t.span,
                        )
                        .with_label_boxed("`return` must be the last statement in a block"),
                    );
                    self.synchronize();
                    continue;
                }
                break;
            }
            let before = self.pos;
            match self.statement() {
                Ok(s) => stmts.push(s),
                Err(d) => {
                    self.report(d);
                    self.synchronize();
                    if self.pos == before {
                        self.bump();
                    }
                }
            }
        }
        let end = ret
            .as_ref()
            .map(|r: &Return| r.span)
            .or(stmts.last().map(|s: &Stmt| s.span))
            .unwrap_or(start);
        Block {
            stmts,
            ret,
            span: start.to(end),
        }
    }

    /// Skips ahead to a token that could plausibly start or end a statement.
    fn synchronize(&mut self) {
        loop {
            match self.peek().token {
                Token::Eof
                | Token::End
                | Token::Else
                | Token::Elseif
                | Token::Until
                | Token::Local
                | Token::If
                | Token::While
                | Token::For
                | Token::Repeat
                | Token::Do
                | Token::Return
                | Token::Function
                | Token::Break => return,
                Token::Semi => {
                    self.bump();
                    return;
                }
                // statements usually start on a new line, an identifier there
                // is most likely the next assignment
                Token::Ident if self.newline_before() => return,
                _ => {
                    self.bump();
                }
            }
        }
    }

    /// Tokens that end a block (Lua's `block_follow`).
    fn block_follow(&self) -> bool {
        matches!(
            self.peek().token,
            Token::Eof | Token::End | Token::Else | Token::Elseif | Token::Until
        )
    }

    // ─ Statements ────────────────────────────────────────────────────────────

    fn return_stmt(&mut self) -> PResult<Return> {
        let kw = self.bump();
        let mut values = Vec::new();
        if !self.block_follow() && !self.at(Token::Semi) {
            values = self.expr_list()?;
        }
        let mut span = values.last().map_or(kw.span, |e| kw.span.to(e.span));
        if let Some(semi) = self.eat(Token::Semi) {
            span = span.to(semi.span);
        }
        Ok(Return { values, span })
    }

    fn statement(&mut self) -> PResult<Stmt> {
        let t = self.peek();
        let stmt = |kind, end: Span| Stmt {
            kind,
            span: t.span.to(end),
        };
        match t.token {
            Token::Local => {
                self.bump();
                if self.eat(Token::Function).is_some() {
                    let name = self.name("after `local function`")?;
                    let func = self.function_body(Some(name.text.clone()), t, false)?;
                    let span = func.span;
                    return Ok(stmt(
                        StmtKind::LocalFunction {
                            name,
                            func: Rc::new(func),
                        },
                        span,
                    ));
                }
                let (names, types) = self.binding_list("after `local`")?;
                let values = if self.eat(Token::Eq).is_some() {
                    self.expr_list()?
                } else {
                    Vec::new()
                };
                let end = values.last().map_or_else(
                    || {
                        types
                            .last()
                            .and_then(|t| t.as_ref().map(|t| t.span))
                            .unwrap_or(names.last().unwrap().span)
                    },
                    |e| e.span,
                );
                Ok(stmt(
                    StmtKind::Local {
                        names,
                        types,
                        values,
                    },
                    end,
                ))
            }
            Token::Do => {
                self.bump();
                let body = self.block();
                let end = self.expect_closer(Token::End, t, "do")?;
                Ok(stmt(StmtKind::Do(body), end.span))
            }
            Token::While => {
                self.bump();
                let cond = self.expr()?;
                self.expect(Token::Do, "after `while` condition")?;
                let body = self.block();
                let end = self.expect_closer(Token::End, t, "while")?;
                Ok(stmt(StmtKind::While { cond, body }, end.span))
            }
            Token::Repeat => {
                self.bump();
                let body = self.block();
                self.expect_closer(Token::Until, t, "repeat")?;
                let cond = self.expr()?;
                let end = cond.span;
                Ok(stmt(StmtKind::Repeat { body, cond }, end))
            }
            Token::If => self.if_stmt(),
            Token::For => self.for_stmt(),
            Token::Break => {
                self.bump();
                Ok(stmt(StmtKind::Break, t.span))
            }
            Token::Function => {
                self.bump();
                let name = self.name("after `function`")?;
                let mut full_name = name.text.to_string();
                let mut target = Expr {
                    span: name.span,
                    kind: ExprKind::Name(name),
                };
                // `function a.b.c()` and `function a.b:m()`
                let mut method = None;
                while matches!(self.peek().token, Token::Dot | Token::Colon) {
                    let sep = self.bump();
                    let field = self.expect(Token::Ident, "after `.` or `:` in a function name")?;
                    let text = self.text(field.span);
                    full_name.push(if sep.token == Token::Colon { ':' } else { '.' });
                    full_name.push_str(text);
                    let key = Expr {
                        kind: ExprKind::String(Rc::from(text)),
                        span: field.span,
                    };
                    let span = target.span.to(field.span);
                    target = Expr {
                        kind: ExprKind::Index {
                            object: Box::new(target),
                            key: Box::new(key),
                        },
                        span,
                    };
                    if sep.token == Token::Colon {
                        method = Some(sep.span);
                        break;
                    }
                }
                let mut func =
                    self.function_body(Some(Rc::from(full_name)), t, method.is_some())?;
                if let Some(colon) = method {
                    // methods get an implicit first parameter, `self`
                    let id = self.next_name;
                    self.next_name += 1;
                    func.params.insert(
                        0,
                        Name {
                            text: Rc::from("self"),
                            id,
                            span: colon,
                        },
                    );
                    func.param_types.insert(0, None);
                }
                let span = func.span;
                Ok(stmt(
                    StmtKind::Function {
                        target,
                        func: Rc::new(func),
                    },
                    span,
                ))
            }
            Token::Ident if self.type_alias_starts() => self.type_alias(),
            Token::Ident if self.text(t.span) == "continue" && self.continue_is_statement() => {
                self.bump();
                Ok(stmt(StmtKind::Continue, t.span))
            }
            _ => self.expr_statement(),
        }
    }

    /// `continue` is only a keyword in context in Luau. It's a statement unless
    /// what follows makes it a normal name (`continue = 1`, `continue(...)`).
    fn continue_is_statement(&self) -> bool {
        !matches!(
            self.peek_nth(1).token,
            Token::Eq
                | Token::Comma
                | Token::LParen
                | Token::Dot
                | Token::LBracket
                | Token::Colon
                | Token::String
                | Token::LBrace
                | Token::PlusEq
                | Token::MinusEq
                | Token::StarEq
                | Token::SlashEq
                | Token::SlashSlashEq
                | Token::PercentEq
                | Token::CaretEq
                | Token::DotDotEq
        )
    }

    fn if_stmt(&mut self) -> PResult<Stmt> {
        let kw = self.bump();
        let mut branches = Vec::new();
        let cond = self.expr()?;
        self.expect(Token::Then, "after `if` condition")?;
        branches.push((cond, self.block()));
        let mut else_block = None;
        loop {
            if self.eat(Token::Elseif).is_some() {
                let cond = self.expr()?;
                self.expect(Token::Then, "after `elseif` condition")?;
                branches.push((cond, self.block()));
            } else if self.eat(Token::Else).is_some() {
                else_block = Some(self.block());
                break;
            } else {
                break;
            }
        }
        let end = self.expect_closer(Token::End, kw, "if")?;
        Ok(Stmt {
            kind: StmtKind::If {
                branches,
                else_block,
            },
            span: kw.span.to(end.span),
        })
    }

    fn for_stmt(&mut self) -> PResult<Stmt> {
        let kw = self.bump();
        let (mut names, mut types) = self.binding_list("after `for`")?;
        if names.len() > 1 || self.at(Token::In) {
            self.expect(Token::In, "after the `for` variables")?;
            let exprs = self.expr_list()?;
            self.expect(Token::Do, "after the `for ... in` list")?;
            let body = self.block();
            let end = self.expect_closer(Token::End, kw, "for")?;
            return Ok(Stmt {
                kind: StmtKind::GenericFor {
                    names,
                    types,
                    exprs,
                    body,
                },
                span: kw.span.to(end.span),
            });
        }
        let var = names.pop().expect("one name");
        let var_type = types.pop().expect("one type slot").map(Box::new);
        self.expect(Token::Eq, "in numeric `for` (`for i = start, limit do`)")?;
        let start = self.expr()?;
        self.expect(Token::Comma, "after the `for` start value")?;
        let limit = self.expr()?;
        let step = if self.eat(Token::Comma).is_some() {
            Some(self.expr()?)
        } else {
            None
        };
        self.expect(Token::Do, "after the `for` range")?;
        let body = self.block();
        let end = self.expect_closer(Token::End, kw, "for")?;
        Ok(Stmt {
            kind: StmtKind::NumericFor {
                var,
                var_type,
                start,
                limit,
                step,
                body,
            },
            span: kw.span.to(end.span),
        })
    }

    /// Assignment or compound assignment. A bare expression is an error, only
    /// calls can stand alone.
    fn expr_statement(&mut self) -> PResult<Stmt> {
        let first = self.peek();
        if !matches!(first.token, Token::Ident | Token::LParen) {
            return Err(error(
                format!("expected a statement, found {}", first.token.describe()),
                first.span,
            )
            .with_label_boxed("expected a statement"));
        }
        let target = self.expr()?;

        if let ExprKind::Call { .. } | ExprKind::MethodCall { .. } = target.kind
            && !matches!(self.peek().token, Token::Eq | Token::Comma)
            && compound_op(self.peek().token).is_none()
        {
            let span = target.span;
            return Ok(Stmt {
                kind: StmtKind::Call(target),
                span,
            });
        }

        if let Some(op) = compound_op(self.peek().token) {
            let op_tok = self.bump();
            self.check_assignable(&target)?;
            let value = self.expr()?;
            let span = target.span.to(value.span);
            return Ok(Stmt {
                kind: StmtKind::CompoundAssign {
                    op,
                    op_span: op_tok.span,
                    target,
                    value,
                },
                span,
            });
        }

        if matches!(self.peek().token, Token::Eq | Token::Comma) {
            self.check_assignable(&target)?;
            let mut targets = vec![target];
            while self.eat(Token::Comma).is_some() {
                let t = self.expr()?;
                self.check_assignable(&t)?;
                targets.push(t);
            }
            self.expect(Token::Eq, "in assignment")?;
            let values = self.expr_list()?;
            let span = targets[0].span.to(values.last().unwrap().span);
            return Ok(Stmt {
                kind: StmtKind::Assign { targets, values },
                span,
            });
        }

        Err(
            error("incomplete statement: expected an assignment", target.span)
                .with_label_boxed("this expression's value is never used")
                .with_help_boxed("only assignments and function calls can be statements"),
        )
    }

    /// `name[: T], name[: T], ...` (locals and `for` variables).
    fn binding_list(&mut self, context: &str) -> PResult<(Vec<Name>, Vec<Option<TypeExpr>>)> {
        let mut names = Vec::new();
        let mut types = Vec::new();
        loop {
            names.push(self.name(context)?);
            types.push(if self.eat(Token::Colon).is_some() {
                Some(self.type_expr()?)
            } else {
                None
            });
            if self.eat(Token::Comma).is_none() {
                return Ok((names, types));
            }
        }
    }

    /// Parses `<T>(params): R body end` after the `function` keyword (and
    /// name). `is_method` means it was declared with `:` (the caller adds
    /// `self`).
    fn function_body(
        &mut self,
        name: Option<Rc<str>>,
        kw: Spanned,
        is_method: bool,
    ) -> PResult<FunctionBody> {
        // an error in the header would leave the body's `end` behind as a stray
        // closer, so skip the whole function
        let header = self.function_header();
        let (generics, params, param_types, is_vararg, vararg_type, return_type) = match header {
            Ok(h) => h,
            Err(d) => {
                self.skip_to_end(1);
                return Err(d);
            }
        };
        let body = self.block();
        let end = self.expect_closer(Token::End, kw, "function")?;
        Ok(FunctionBody {
            generics,
            params,
            param_types,
            is_vararg,
            vararg_type,
            return_type,
            is_method,
            body,
            name,
            span: kw.span.to(end.span),
        })
    }

    /// Skips to the `end` (or `until`) matching `open` block openers that were
    /// already eaten.
    fn skip_to_end(&mut self, open: u32) {
        let mut depth = open;
        loop {
            match self.peek().token {
                Token::Eof => return,
                Token::If | Token::Do | Token::Function | Token::Repeat => depth += 1,
                Token::End | Token::Until => {
                    depth -= 1;
                    if depth == 0 {
                        self.bump();
                        return;
                    }
                }
                _ => {}
            }
            self.bump();
        }
    }

    #[allow(clippy::type_complexity)]
    fn function_header(
        &mut self,
    ) -> PResult<(
        Vec<GenericParam>,
        Vec<Name>,
        Vec<Option<TypeExpr>>,
        bool,
        Option<TypeExpr>,
        Option<TypePack>,
    )> {
        let generics = self.generic_params(false)?;
        self.expect(Token::LParen, "to start the parameter list")?;
        let mut params = Vec::new();
        let mut param_types = Vec::new();
        let mut is_vararg = false;
        let mut vararg_type = None;
        if !self.at(Token::RParen) {
            loop {
                if self.eat(Token::Ellipsis).is_some() {
                    is_vararg = true;
                    if self.eat(Token::Colon).is_some() {
                        vararg_type = Some(self.variadic_type()?);
                    }
                    break;
                }
                params.push(self.name("in parameter list")?);
                param_types.push(if self.eat(Token::Colon).is_some() {
                    Some(self.type_expr()?)
                } else {
                    None
                });
                if self.eat(Token::Comma).is_none() {
                    break;
                }
            }
        }
        self.expect(Token::RParen, "to close the parameter list")?;
        let return_type = if self.eat(Token::Colon).is_some() {
            Some(self.return_pack()?)
        } else {
            None
        };
        Ok((
            generics,
            params,
            param_types,
            is_vararg,
            vararg_type,
            return_type,
        ))
    }

    // ─ Types ─────────────────────────────────────────────────────────────────

    /// `expr :: T`. Binds tighter than any binary operator, like Luau.
    fn cast_expr(&mut self) -> PResult<Expr> {
        let mut e = self.simple_expr()?;
        while self.eat(Token::ColonColon).is_some() {
            let ty = self.type_expr()?;
            let span = e.span.to(ty.span);
            e = Expr {
                kind: ExprKind::Cast {
                    expr: Box::new(e),
                    ty: Box::new(ty),
                },
                span,
            };
        }
        Ok(e)
    }

    /// `type` and `export` are only keywords when a type alias comes next.
    fn type_alias_starts(&self) -> bool {
        match self.text(self.peek().span) {
            "type" => self.peek_nth(1).token == Token::Ident,
            "export" => {
                self.peek_nth(1).token == Token::Ident
                    && self.text(self.peek_nth(1).span) == "type"
                    && self.peek_nth(2).token == Token::Ident
            }
            _ => false,
        }
    }

    /// `[export] type Name<T, U> = Type`
    fn type_alias(&mut self) -> PResult<Stmt> {
        let first = self.bump();
        let exported = self.text(first.span) == "export";
        if exported {
            self.bump();
        }
        let name = self.expect(Token::Ident, "after `type`")?;
        let generics = self.generic_params(true)?;
        self.expect(Token::Eq, "after the type's name")?;
        let ty = self.type_expr()?;
        let span = first.span.to(ty.span);
        Ok(Stmt {
            kind: StmtKind::TypeAlias(Rc::new(TypeAlias {
                name: Rc::from(self.text(name.span)),
                name_span: name.span,
                generics,
                ty,
                exported,
            })),
            span,
        })
    }

    /// `<T, U...>`, or nothing. Defaults (`<T = string>`) only when `defaults`
    /// is set (type aliases).
    fn generic_params(&mut self, defaults: bool) -> PResult<Vec<GenericParam>> {
        let mut out: Vec<GenericParam> = Vec::new();
        if self.eat(Token::Lt).is_none() {
            return Ok(out);
        }
        loop {
            let t = self.expect(Token::Ident, "in a generic parameter list")?;
            let pack = self.eat(Token::Ellipsis).is_some();
            if !pack && out.iter().any(|g| g.pack) {
                return Err(error(
                    "generic type parameters must come before type packs",
                    t.span,
                )
                .with_label_boxed("move this before the packs"));
            }
            let default = if let Some(eq) = self.eat(Token::Eq) {
                if !defaults {
                    return Err(error(
                        "default type parameters are only allowed in type aliases",
                        eq.span,
                    )
                    .with_label_boxed("not allowed here"));
                }
                Some(if pack {
                    self.type_arg()?
                } else {
                    self.type_expr()?
                })
            } else {
                if out.iter().any(|g| g.default.is_some()) {
                    return Err(error(
                        "generic parameters after one with a default need defaults too",
                        t.span,
                    )
                    .with_label_boxed("needs a default"));
                }
                None
            };
            out.push(GenericParam {
                name: Rc::from(self.text(t.span)),
                span: t.span,
                pack,
                default,
            });
            if self.eat(Token::Comma).is_none() {
                break;
            }
        }
        self.expect(Token::Gt, "to close the generic parameter list")?;
        Ok(out)
    }

    /// `T...` at the cursor (a generic pack named in a variadic slot).
    fn at_generic_pack(&self) -> bool {
        self.at(Token::Ident) && self.peek_nth(1).token == Token::Ellipsis
    }

    fn generic_pack(&mut self) -> PResult<TypeExpr> {
        let name = self.bump();
        let dots = self.bump();
        Ok(TypeExpr {
            kind: TypeKind::GenericPack(Rc::from(self.text(name.span))),
            span: name.span.to(dots.span),
        })
    }

    /// What can come after `...:` or `...`: a type, or a generic pack `T...`.
    fn variadic_type(&mut self) -> PResult<TypeExpr> {
        if self.at_generic_pack() {
            self.generic_pack()
        } else {
            self.type_expr()
        }
    }

    /// One type alias argument, a type or a pack (`(A, B)`, `()`, `...T`,
    /// `T...`).
    fn type_arg(&mut self) -> PResult<TypeExpr> {
        let start = self.peek().span;
        if self.eat(Token::Ellipsis).is_some() {
            let t = self.type_expr()?;
            return Ok(TypeExpr {
                span: start.to(t.span),
                kind: TypeKind::Pack(TypePack {
                    types: Vec::new(),
                    variadic: Some(Box::new(t)),
                    span: start,
                }),
            });
        }
        if self.at_generic_pack() {
            return self.generic_pack();
        }
        if self.at(Token::LParen) {
            let (pos, names) = (self.pos, self.next_name);
            self.bump();
            let (params, variadic) = self.type_param_list()?;
            let close = self.expect(Token::RParen, "to close the type pack")?;
            let pack_like = !matches!(
                self.peek().token,
                Token::Arrow | Token::Pipe | Token::Amp | Token::Question
            );
            if pack_like && params.iter().all(|p| p.name.is_none()) {
                let span = start.to(close.span);
                return Ok(TypeExpr {
                    span,
                    kind: TypeKind::Pack(TypePack {
                        types: params.into_iter().map(|p| p.ty).collect(),
                        variadic,
                        span,
                    }),
                });
            }
            self.pos = pos;
            self.next_name = names;
        }
        self.type_expr()
    }

    pub(crate) fn type_expr(&mut self) -> PResult<TypeExpr> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.depth -= 1;
            return Err(error("type nests too deeply", self.peek().span)
                .with_label_boxed(format!("nesting limit of {MAX_DEPTH} reached here")));
        }
        let result = grow(|| self.type_expr_inner());
        self.depth -= 1;
        result
    }

    /// Unions and intersections (a leading `|` or `&` is fine). Mixing the two
    /// needs parentheses.
    fn type_expr_inner(&mut self) -> PResult<TypeExpr> {
        let lead = self.peek().token;
        if matches!(lead, Token::Pipe | Token::Amp) {
            self.bump();
        }
        let first = self.optional_type()?;
        let sep = match self.peek().token {
            Token::Pipe | Token::Amp => self.peek().token,
            _ if matches!(lead, Token::Pipe | Token::Amp) => lead,
            _ => return Ok(first),
        };
        let mut parts = vec![first];
        while self.eat(sep).is_some() {
            parts.push(self.optional_type()?);
        }
        if matches!(self.peek().token, Token::Pipe | Token::Amp) {
            return Err(error(
                "mixing `|` and `&` in one type needs parentheses",
                self.peek().span,
            )
            .with_label_boxed("ambiguous"));
        }
        let span = parts[0].span.to(parts.last().unwrap().span);
        if parts.len() == 1 {
            return Ok(parts.pop().unwrap());
        }
        Ok(TypeExpr {
            kind: if sep == Token::Pipe {
                TypeKind::Union(parts)
            } else {
                TypeKind::Intersection(parts)
            },
            span,
        })
    }

    fn optional_type(&mut self) -> PResult<TypeExpr> {
        let mut t = self.primary_type()?;
        while let Some(q) = self.eat(Token::Question) {
            t = TypeExpr {
                span: t.span.to(q.span),
                kind: TypeKind::Optional(Box::new(t)),
            };
        }
        Ok(t)
    }

    fn primary_type(&mut self) -> PResult<TypeExpr> {
        let tok = self.peek();
        let simple = |kind| TypeExpr {
            kind,
            span: tok.span,
        };
        match tok.token {
            Token::Nil => {
                self.bump();
                Ok(simple(TypeKind::Name {
                    name: Rc::from("nil"),
                    args: Vec::new(),
                }))
            }
            Token::True | Token::False => {
                self.bump();
                Ok(simple(TypeKind::Singleton(Singleton::Bool(
                    tok.token == Token::True,
                ))))
            }
            Token::String => {
                self.bump();
                let text = strings::decode(self.text(tok.span), tok.span).map_err(Box::new)?;
                Ok(simple(TypeKind::Singleton(Singleton::String(Rc::from(
                    text,
                )))))
            }
            Token::Ident
                if self.text(tok.span) == "typeof" && self.peek_nth(1).token == Token::LParen =>
            {
                self.bump();
                self.bump();
                let e = self.expr()?;
                let close = self.expect(Token::RParen, "to close `typeof(`")?;
                Ok(TypeExpr {
                    kind: TypeKind::Typeof(Box::new(e)),
                    span: tok.span.to(close.span),
                })
            }
            Token::Ident => {
                self.bump();
                let mut name = self.text(tok.span).to_string();
                let mut span = tok.span;
                // `module.Type`, kept as one dotted name
                if self.at(Token::Dot) && self.peek_nth(1).token == Token::Ident {
                    self.bump();
                    let field = self.bump();
                    name.push('.');
                    name.push_str(self.text(field.span));
                    span = span.to(field.span);
                }
                let mut args = Vec::new();
                if self.eat(Token::Lt).is_some() {
                    loop {
                        args.push(self.type_arg()?);
                        if self.eat(Token::Comma).is_none() {
                            break;
                        }
                    }
                    span = span.to(self.expect(Token::Gt, "to close the type arguments")?.span);
                }
                Ok(TypeExpr {
                    kind: TypeKind::Name {
                        name: Rc::from(name),
                        args,
                    },
                    span,
                })
            }
            Token::LBrace => self.table_type(),
            Token::LParen | Token::Lt => self.function_type_or_group(),
            other => Err(error(
                format!("expected a type, found {}", other.describe()),
                tok.span,
            )
            .with_label_boxed("expected a type")),
        }
    }

    /// `{T}`, `{ name: T, [K]: V }`.
    fn table_type(&mut self) -> PResult<TypeExpr> {
        let open = self.bump();
        let mut props = Vec::new();
        let mut indexer: Option<Box<(TypeExpr, TypeExpr)>> = None;
        // `read name: T` / `write name: T` (contextual words)
        let modifier = |p: &Self| {
            p.at(Token::Ident)
                && matches!(p.text(p.peek().span), "read" | "write")
                && (p.peek_nth(1).token == Token::Ident || p.peek_nth(1).token == Token::LBracket)
        };
        let is_entry = |p: &Self| {
            p.at(Token::LBracket)
                || (p.at(Token::Ident) && p.peek_nth(1).token == Token::Colon)
                || modifier(p)
        };
        if !self.at(Token::RBrace) && !is_entry(self) {
            // `{T}`, an array
            let elem = self.type_expr()?;
            let close = self.expect(Token::RBrace, "to close the array type `{T}`")?;
            let key = TypeExpr {
                kind: TypeKind::Name {
                    name: Rc::from("number"),
                    args: Vec::new(),
                },
                span: elem.span,
            };
            return Ok(TypeExpr {
                kind: TypeKind::Table {
                    props,
                    indexer: Some(Box::new((key, elem))),
                },
                span: open.span.to(close.span),
            });
        }
        while !self.at(Token::RBrace) {
            let access = if modifier(self) {
                let word = self.bump();
                if self.text(word.span) == "read" {
                    Access::Read
                } else {
                    Access::Write
                }
            } else {
                Access::ReadWrite
            };
            if self.at(Token::LBracket) {
                let lb = self.bump();
                let key = self.type_expr()?;
                self.expect(Token::RBracket, "to close `[` in a table type")?;
                self.expect(Token::Colon, "after `[key]` in a table type")?;
                let value = self.type_expr()?;
                match key.kind {
                    // `["name"]: T` is a property, not an indexer
                    TypeKind::Singleton(Singleton::String(name)) => props.push(TableProp {
                        name,
                        span: lb.span.to(value.span),
                        ty: value,
                        access,
                    }),
                    _ => {
                        if access != Access::ReadWrite {
                            return Err(error(
                                "`read` and `write` apply to properties, not indexers",
                                lb.span,
                            )
                            .with_label_boxed("an indexer"));
                        }
                        if indexer.is_some() {
                            return Err(error("a table type can have only one indexer", lb.span)
                                .with_label_boxed("second indexer"));
                        }
                        indexer = Some(Box::new((key, value)));
                    }
                }
            } else if self.at(Token::Ident) && self.peek_nth(1).token == Token::Colon {
                let name = self.bump();
                self.bump();
                let ty = self.type_expr()?;
                props.push(TableProp {
                    name: Rc::from(self.text(name.span)),
                    span: name.span.to(ty.span),
                    ty,
                    access,
                });
            } else {
                let t = self.peek();
                return Err(error(
                    format!(
                        "expected a property (`name: type`) or an indexer (`[key]: type`), found {}",
                        t.token.describe()
                    ),
                    t.span,
                )
                .with_label_boxed("expected a table type entry"));
            }
            if self.eat(Token::Comma).is_none() && self.eat(Token::Semi).is_none() {
                break;
            }
        }
        let close = self.expect(Token::RBrace, "to close the table type")?;
        Ok(TypeExpr {
            kind: TypeKind::Table { props, indexer },
            span: open.span.to(close.span),
        })
    }

    /// `(A, B) -> R`, `<T>(T) -> T`, or a parenthesised type `(A | B)`.
    fn function_type_or_group(&mut self) -> PResult<TypeExpr> {
        let start = self.peek().span;
        let generics = self.generic_params(false)?;
        self.expect(Token::LParen, "to start a function type's parameters")?;
        let (params, variadic) = self.type_param_list()?;
        let close = self.expect(Token::RParen, "to close the parameter types")?;
        if self.eat(Token::Arrow).is_some() {
            let returns = self.return_pack()?;
            return Ok(TypeExpr {
                span: start.to(returns.span),
                kind: TypeKind::Function {
                    generics,
                    params,
                    variadic,
                    returns,
                },
            });
        }
        if generics.is_empty()
            && variadic.is_none()
            && params.len() == 1
            && let [FnParam { name: None, .. }] = params.as_slice()
        {
            let mut t = params.into_iter().next().unwrap().ty;
            t.span = start.to(close.span);
            return Ok(t);
        }
        Err(error(
            format!(
                "expected `->` after a function type's parameters, found {}",
                self.peek().token.describe()
            ),
            self.peek().span,
        )
        .with_label_boxed("expected `->`"))
    }

    /// The inside of `( ... )` in a function type: `a: T, U, ...V`.
    fn type_param_list(&mut self) -> PResult<(Vec<FnParam>, Option<Box<TypeExpr>>)> {
        let mut params = Vec::new();
        let mut variadic = None;
        if self.at(Token::RParen) {
            return Ok((params, variadic));
        }
        loop {
            if self.eat(Token::Ellipsis).is_some() {
                variadic = Some(Box::new(self.type_expr()?));
                break;
            }
            if self.at_generic_pack() {
                variadic = Some(Box::new(self.generic_pack()?));
                break;
            }
            let name = if self.at(Token::Ident) && self.peek_nth(1).token == Token::Colon {
                let n = self.bump();
                self.bump();
                Some(Rc::from(self.text(n.span)))
            } else {
                None
            };
            let ty = self.type_expr()?;
            params.push(FnParam { name, ty });
            if self.eat(Token::Comma).is_none() {
                break;
            }
        }
        Ok((params, variadic))
    }

    /// A function's results: `T`, `(A, B)`, `()`, `...T`, or a function type
    /// `(A) -> B` (which `(` alone can't tell apart from a pack).
    fn return_pack(&mut self) -> PResult<TypePack> {
        let start = self.peek().span;
        if self.at_generic_pack() {
            let t = self.generic_pack()?;
            return Ok(TypePack {
                span: t.span,
                types: Vec::new(),
                variadic: Some(Box::new(t)),
            });
        }
        if self.eat(Token::Ellipsis).is_some() {
            let t = self.type_expr()?;
            return Ok(TypePack {
                span: start.to(t.span),
                types: Vec::new(),
                variadic: Some(Box::new(t)),
            });
        }
        if self.at(Token::LParen) {
            let (pos, names) = (self.pos, self.next_name);
            self.bump();
            let (params, variadic) = self.type_param_list()?;
            let close = self.expect(Token::RParen, "to close the result types")?;
            if self.at(Token::Arrow)
                || matches!(
                    self.peek().token,
                    Token::Pipe | Token::Amp | Token::Question
                )
            {
                // it was a function type or a parenthesised type after all
                self.pos = pos;
                self.next_name = names;
            } else {
                if let Some(p) = params.iter().find(|p| p.name.is_some()) {
                    return Err(error("result types can't be named", p.ty.span)
                        .with_label_boxed("remove the name"));
                }
                return Ok(TypePack {
                    types: params.into_iter().map(|p| p.ty).collect(),
                    variadic,
                    span: start.to(close.span),
                });
            }
        }
        let t = self.type_expr()?;
        Ok(TypePack {
            span: t.span,
            types: vec![t],
            variadic: None,
        })
    }

    fn check_assignable(&self, target: &Expr) -> PResult<()> {
        match target.kind {
            ExprKind::Name(_) | ExprKind::Index { .. } => Ok(()),
            _ => Err(error("cannot assign to this expression", target.span)
                .with_label_boxed("only variables can be assigned to")),
        }
    }

    // ─ Expressions (Pratt) ───────────────────────────────────────────────────

    fn expr_list(&mut self) -> PResult<Vec<Expr>> {
        let mut values = vec![self.expr()?];
        while self.eat(Token::Comma).is_some() {
            values.push(self.expr()?);
        }
        Ok(values)
    }

    fn expr(&mut self) -> PResult<Expr> {
        self.expr_bp(0)
    }

    fn too_deep(&self) -> Box<Diagnostic> {
        error("expression nests too deeply", self.peek().span)
            .with_label_boxed(format!("nesting limit of {MAX_DEPTH} reached here"))
    }

    fn expr_bp(&mut self, min_bp: u8) -> PResult<Expr> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.depth -= 1;
            return Err(self.too_deep());
        }
        let result = grow(|| self.expr_bp_inner(min_bp));
        self.depth -= 1;
        result
    }

    fn expr_bp_inner(&mut self, min_bp: u8) -> PResult<Expr> {
        let mut lhs = match unary_op(self.peek().token) {
            Some(op) => {
                let op_tok = self.bump();
                let operand = self.expr_bp(UNARY_BP)?;
                Expr {
                    span: op_tok.span.to(operand.span),
                    kind: ExprKind::Unary {
                        op,
                        op_span: op_tok.span,
                        operand: Box::new(operand),
                    },
                }
            }
            None => self.cast_expr()?,
        };

        // left-associative chains (`1 + 1 + 1 ...`) grow the tree without
        // recursing, so they count toward the depth limit too. keeps the AST
        // depth bounded for later passes (and for `Drop`)
        let mut chain = 0;
        while let Some(op) = binary_op(self.peek().token) {
            let (left_bp, right_bp) = op.binding_power();
            if left_bp < min_bp {
                break;
            }
            chain += 1;
            if self.depth + chain > MAX_DEPTH {
                return Err(self.too_deep());
            }
            let op_tok = self.bump();
            let rhs = self.expr_bp(right_bp)?;
            lhs = Expr {
                span: lhs.span.to(rhs.span),
                kind: ExprKind::Binary {
                    op,
                    op_span: op_tok.span,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
            };
        }
        Ok(lhs)
    }

    /// Splits an interpolated string token into text and holes, lexing and
    /// parsing each hole as its own expression. (The caller bumps the token.)
    fn interp_string(&mut self, span: Span) -> PResult<ExprKind> {
        let raw = self.text(span);
        let mut parts = Vec::new();
        for piece in rhue_lexer::interp::pieces(raw) {
            match piece {
                rhue_lexer::interp::Piece::Text(r) => {
                    let text = strings::decode_interp_text(
                        &raw[r.clone()],
                        span.start + rhue_common::offset(r.start),
                    )
                    .map_err(Box::new)?;
                    parts.push(InterpPart::Text(Rc::from(text)));
                }
                rhue_lexer::interp::Piece::Hole(r) => {
                    let start = span.start + rhue_common::offset(r.start);
                    let hole = Span::new(start - 1, span.start + rhue_common::offset(r.end) + 1);
                    if raw[r.clone()].starts_with('{') {
                        return Err(Box::new(
                            Diagnostic::error(
                                "double braces aren't allowed in an interpolated string",
                                hole,
                            )
                            .with_label("use `\\{` for a literal brace"),
                        ));
                    }
                    parts.push(InterpPart::Expr(self.hole(&raw[r.clone()], start, hole)?));
                }
            }
        }
        if !parts.iter().any(|p| matches!(p, InterpPart::Expr(_))) {
            let text: String = parts
                .iter()
                .map(|p| match p {
                    InterpPart::Text(t) => &**t,
                    InterpPart::Expr(_) => "",
                })
                .collect();
            return Ok(ExprKind::String(Rc::from(text)));
        }
        Ok(ExprKind::Interp(parts))
    }

    /// Parses the expression inside one `{...}` hole, starting at byte `start`
    /// of the source.
    fn hole(&mut self, text: &str, start: u32, hole: Span) -> PResult<Expr> {
        let lexed = rhue_lexer::lex(text);
        let shift = |s: Span| Span::new(s.start + start, s.end + start);
        if let Some(mut e) = lexed.errors.into_iter().next() {
            e.primary.span = shift(e.primary.span);
            for label in &mut e.secondary {
                label.span = shift(label.span);
            }
            return Err(Box::new(e));
        }
        let tokens: Vec<Spanned> = lexed
            .tokens
            .into_iter()
            .map(|t| Spanned {
                token: t.token,
                span: shift(t.span),
            })
            .collect();
        if tokens.len() == 1 {
            return Err(Box::new(
                Diagnostic::error("empty `{}` in an interpolated string", hole)
                    .with_label("expected an expression"),
            ));
        }
        let mut sub = Parser {
            source: self.source,
            tokens,
            pos: 0,
            depth: self.depth,
            next_name: self.next_name,
            errors: Vec::new(),
        };
        let result = sub.expr().and_then(|e| {
            sub.expect(Token::Eof, "in an interpolated string's `{}`")?;
            Ok(e)
        });
        self.next_name = sub.next_name;
        self.errors.append(&mut sub.errors);
        result
    }

    fn simple_expr(&mut self) -> PResult<Expr> {
        let tok = self.peek();
        let kind = match tok.token {
            Token::Number(n) => ExprKind::Number(n),
            Token::Nil => ExprKind::Nil,
            Token::True => ExprKind::Bool(true),
            Token::False => ExprKind::Bool(false),
            Token::String => {
                let text = strings::decode(self.text(tok.span), tok.span).map_err(Box::new)?;
                ExprKind::String(Rc::from(text))
            }
            Token::InterpString => self.interp_string(tok.span)?,
            Token::Ident => {
                self.bump();
                let name = self.new_name(tok.span);
                return self.suffixes(Expr {
                    kind: ExprKind::Name(name),
                    span: tok.span,
                });
            }
            Token::If => return self.if_expr(),
            Token::LParen => {
                let open = self.bump();
                let inner = self.expr()?;
                let close = self.eat(Token::RParen).ok_or_else(|| {
                    let found = self.peek();
                    Box::new(
                        Diagnostic::error(
                            format!(
                                "expected `)` to close `(`, found {}",
                                found.token.describe()
                            ),
                            found.span,
                        )
                        .with_label("expected `)`")
                        .with_secondary(open.span, "unclosed `(` opened here"),
                    )
                })?;
                return self.suffixes(Expr {
                    span: open.span.to(close.span),
                    kind: ExprKind::Group(Box::new(inner)),
                });
            }
            Token::LBrace => return self.table_constructor(),
            Token::Function => {
                let kw = self.bump();
                let func = self.function_body(None, kw, false)?;
                return Ok(Expr {
                    span: func.span,
                    kind: ExprKind::Function(Rc::new(func)),
                });
            }
            Token::Ellipsis => ExprKind::Vararg,
            other => {
                return Err(error(
                    format!("expected an expression, found {}", other.describe()),
                    tok.span,
                )
                .with_label_boxed("expected an expression"));
            }
        };
        self.bump();
        let expr = Expr {
            kind,
            span: tok.span,
        };
        if matches!(expr.kind, ExprKind::String(_)) {
            return self.no_suffix(expr);
        }
        Ok(expr)
    }

    /// Call and index suffixes after a name or parenthesised expression:
    /// `f(a)`, `f "s"`, `f(a)(b)`, `t.x[k]`.
    fn suffixes(&mut self, mut expr: Expr) -> PResult<Expr> {
        loop {
            match self.peek().token {
                Token::LParen => {
                    self.bump();
                    let args = if self.at(Token::RParen) {
                        Vec::new()
                    } else {
                        self.expr_list()?
                    };
                    let close = self.expect(Token::RParen, "to close the argument list")?;
                    let span = expr.span.to(close.span);
                    expr = Expr {
                        kind: ExprKind::Call {
                            callee: Box::new(expr),
                            args,
                        },
                        span,
                    };
                }
                Token::String => {
                    let t = self.bump();
                    let text = strings::decode(self.text(t.span), t.span).map_err(Box::new)?;
                    let arg = Expr {
                        kind: ExprKind::String(Rc::from(text)),
                        span: t.span,
                    };
                    let span = expr.span.to(t.span);
                    expr = Expr {
                        kind: ExprKind::Call {
                            callee: Box::new(expr),
                            args: vec![arg],
                        },
                        span,
                    };
                }
                Token::LBrace => {
                    let arg = self.table_constructor()?;
                    let span = expr.span.to(arg.span);
                    expr = Expr {
                        kind: ExprKind::Call {
                            callee: Box::new(expr),
                            args: vec![arg],
                        },
                        span,
                    };
                }
                Token::Dot => {
                    self.bump();
                    let field = self.expect(Token::Ident, "after `.`")?;
                    let key = Expr {
                        kind: ExprKind::String(Rc::from(self.text(field.span))),
                        span: field.span,
                    };
                    let span = expr.span.to(field.span);
                    expr = Expr {
                        kind: ExprKind::Index {
                            object: Box::new(expr),
                            key: Box::new(key),
                        },
                        span,
                    };
                }
                Token::LBracket => {
                    self.bump();
                    let key = self.expr()?;
                    let close = self.expect(Token::RBracket, "to close `[`")?;
                    let span = expr.span.to(close.span);
                    expr = Expr {
                        kind: ExprKind::Index {
                            object: Box::new(expr),
                            key: Box::new(key),
                        },
                        span,
                    };
                }
                Token::Colon => {
                    self.bump();
                    let name = self.expect(Token::Ident, "after `:` (a method name)")?;
                    let method: Rc<str> = Rc::from(self.text(name.span));
                    let (args, end) = match self.peek().token {
                        Token::LParen => {
                            self.bump();
                            let args = if self.at(Token::RParen) {
                                Vec::new()
                            } else {
                                self.expr_list()?
                            };
                            (
                                args,
                                self.expect(Token::RParen, "to close the argument list")?
                                    .span,
                            )
                        }
                        Token::String => {
                            let t = self.bump();
                            let text =
                                strings::decode(self.text(t.span), t.span).map_err(Box::new)?;
                            (
                                vec![Expr {
                                    kind: ExprKind::String(Rc::from(text)),
                                    span: t.span,
                                }],
                                t.span,
                            )
                        }
                        Token::LBrace => {
                            let t = self.table_constructor()?;
                            let span = t.span;
                            (vec![t], span)
                        }
                        other => {
                            return Err(error(
                                format!(
                                    "expected arguments after method `{method}`, found {}",
                                    other.describe()
                                ),
                                self.peek().span,
                            )
                            .with_label_boxed("expected `(`")
                            .with_help_boxed("`a:b` is only valid as a call: `a:b(...)`"));
                        }
                    };
                    let span = expr.span.to(end);
                    expr = Expr {
                        kind: ExprKind::MethodCall {
                            object: Box::new(expr),
                            method,
                            method_span: name.span,
                            args,
                        },
                        span,
                    };
                }
                _ => return Ok(expr),
            }
        }
    }

    /// `{ a, b; name = c, [k] = d, }` (`,` or `;` between, trailing is fine).
    fn table_constructor(&mut self) -> PResult<Expr> {
        let open = self.bump();
        let mut items = Vec::new();
        while !self.at(Token::RBrace) {
            let item = match self.peek().token {
                Token::LBracket => {
                    self.bump();
                    let key = self.expr()?;
                    self.expect(Token::RBracket, "to close `[` in a table key")?;
                    self.expect(Token::Eq, "after a `[key]` in a table constructor")?;
                    TableItem::Keyed {
                        key,
                        value: self.expr()?,
                    }
                }
                Token::Ident if self.peek_nth(1).token == Token::Eq => {
                    let name = self.bump();
                    self.bump();
                    let key = Expr {
                        kind: ExprKind::String(Rc::from(self.text(name.span))),
                        span: name.span,
                    };
                    TableItem::Keyed {
                        key,
                        value: self.expr()?,
                    }
                }
                _ => TableItem::Positional(self.expr()?),
            };
            items.push(item);
            if self.eat(Token::Comma).is_none() && self.eat(Token::Semi).is_none() {
                break;
            }
        }
        let close = self.eat(Token::RBrace).ok_or_else(|| {
            let found = self.peek();
            Box::new(
                Diagnostic::error(
                    format!(
                        "expected `}}` to close the table, found {}",
                        found.token.describe()
                    ),
                    found.span,
                )
                .with_label("expected `}` or `,`")
                .with_secondary(open.span, "table opened here"),
            )
        })?;
        Ok(Expr {
            kind: ExprKind::Table(items),
            span: open.span.to(close.span),
        })
    }

    /// Strings can't be called or indexed without parentheses.
    fn no_suffix(&self, expr: Expr) -> PResult<Expr> {
        match self.peek().token {
            Token::LParen | Token::String | Token::LBrace => Err(error(
                "a string literal can't be called directly",
                self.peek().span,
            )
            .with_help_boxed("wrap it in parentheses: `(\"...\")(...)`")),
            Token::Dot | Token::LBracket | Token::Colon => Err(error(
                "a string literal can't be indexed directly",
                self.peek().span,
            )
            .with_help_boxed("wrap it in parentheses: `(\"...\"):upper()`")),
            _ => Ok(expr),
        }
    }

    /// `if c then a elseif c2 then b else d` (the `else` isn't optional).
    fn if_expr(&mut self) -> PResult<Expr> {
        let kw = self.bump();
        let mut branches = Vec::new();
        let cond = self.expr()?;
        self.expect(Token::Then, "after `if` condition")?;
        branches.push((cond, self.expr()?));
        while self.eat(Token::Elseif).is_some() {
            let cond = self.expr()?;
            self.expect(Token::Then, "after `elseif` condition")?;
            branches.push((cond, self.expr()?));
        }
        self.expect(
            Token::Else,
            "in `if` expression (the `else` branch is required)",
        )?;
        let else_expr = self.expr()?;
        let span = kw.span.to(else_expr.span);
        Ok(Expr {
            kind: ExprKind::IfElse {
                branches,
                else_expr: Box::new(else_expr),
            },
            span,
        })
    }
}

trait BoxedDiagnosticExt {
    fn with_label_boxed(self, label: impl Into<String>) -> Self;
    fn with_help_boxed(self, help: impl Into<String>) -> Self;
}

impl BoxedDiagnosticExt for Box<Diagnostic> {
    fn with_label_boxed(mut self, label: impl Into<String>) -> Self {
        self.primary.message = label.into();
        self
    }

    fn with_help_boxed(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }
}

fn unary_op(token: Token) -> Option<UnOp> {
    Some(match token {
        Token::Minus => UnOp::Neg,
        Token::Not => UnOp::Not,
        Token::Hash => UnOp::Len,
        _ => return None,
    })
}

fn binary_op(token: Token) -> Option<BinOp> {
    Some(match token {
        Token::Plus => BinOp::Add,
        Token::Minus => BinOp::Sub,
        Token::Star => BinOp::Mul,
        Token::Slash => BinOp::Div,
        Token::SlashSlash => BinOp::IDiv,
        Token::Percent => BinOp::Mod,
        Token::Caret => BinOp::Pow,
        Token::DotDot => BinOp::Concat,
        Token::EqEq => BinOp::Eq,
        Token::NotEq => BinOp::Ne,
        Token::Lt => BinOp::Lt,
        Token::LtEq => BinOp::Le,
        Token::Gt => BinOp::Gt,
        Token::GtEq => BinOp::Ge,
        Token::And => BinOp::And,
        Token::Or => BinOp::Or,
        _ => return None,
    })
}

fn compound_op(token: Token) -> Option<BinOp> {
    Some(match token {
        Token::PlusEq => BinOp::Add,
        Token::MinusEq => BinOp::Sub,
        Token::StarEq => BinOp::Mul,
        Token::SlashEq => BinOp::Div,
        Token::SlashSlashEq => BinOp::IDiv,
        Token::PercentEq => BinOp::Mod,
        Token::CaretEq => BinOp::Pow,
        Token::DotDotEq => BinOp::Concat,
        _ => return None,
    })
}
