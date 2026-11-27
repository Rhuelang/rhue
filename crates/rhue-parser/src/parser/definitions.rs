//! `.d.rhue` files: Luau's declaration syntax, for what a host has.
//!
//! ```lua
//! type Signal<T...> = { Connect: (any, (T...) -> ()) -> () }
//!
//! declare class Node
//!     Name: string
//!     read Parent: Node?
//!     function Destroy(self): ()
//!     [string]: Node
//! end
//!
//! declare class Shape extends Node
//!     read Touched: Signal<Shape>
//! end
//!
//! declare world: Node
//! declare function spawn(f: () -> ()): ()
//! ```

use std::rc::Rc;

use rhue_common::{Diagnostic, Span};
use rhue_lexer::Token;

use super::{BoxedDiagnosticExt, PResult, Parser, error};
use crate::ast::*;

/// Parses a definitions file. On failure you get every error it found (up to
/// [`MAX_ERRORS`](crate::MAX_ERRORS)), it skips to the next `declare` or
/// `type` after one.
pub fn parse_definitions(source: &str) -> Result<Definitions, Vec<Diagnostic>> {
    let mut p = Parser::new(source)?;
    let mut items = Vec::new();
    while !p.at(Token::Eof) && p.errors.len() < crate::MAX_ERRORS {
        match p.declaration() {
            Ok(d) => items.push(d),
            Err(d) => {
                p.report(d);
                p.skip_to_declaration();
            }
        }
    }
    if p.errors.is_empty() {
        Ok(Definitions { items })
    } else {
        Err(p.errors)
    }
}

impl Parser<'_> {
    fn at_word(&self, word: &str) -> bool {
        self.at(Token::Ident) && self.text(self.peek().span) == word
    }

    /// Past the bad bit, to the next line starting with `declare` or `type`.
    fn skip_to_declaration(&mut self) {
        self.bump();
        while !self.at(Token::Eof) {
            if self.newline_before() && (self.at_word("declare") || self.type_alias_starts()) {
                return;
            }
            self.bump();
        }
    }

    fn declaration(&mut self) -> PResult<Declaration> {
        if self.type_alias_starts() {
            let stmt = self.type_alias()?;
            let StmtKind::TypeAlias(a) = stmt.kind else {
                unreachable!("type_alias makes an alias");
            };
            return Ok(Declaration::Alias(a));
        }
        if !self.at_word("declare") {
            let t = self.peek();
            return Err(error(
                format!("expected `declare` or `type`, found {}", t.token.describe()),
                t.span,
            )
            .with_label_boxed("a definitions file only declares things"));
        }
        let start = self.bump().span;
        if self.at_word("class") && self.peek_nth(1).token == Token::Ident {
            let opener = self.bump();
            return self.class_decl(start, opener).map(Declaration::Class);
        }
        if self.eat(Token::Function).is_some() {
            let name = self.expect(Token::Ident, "after `declare function`")?;
            let ty = self.function_decl(None)?;
            return Ok(Declaration::Global {
                name: Rc::from(self.text(name.span)),
                span: start.to(ty.span),
                ty,
            });
        }
        let name = self.expect(Token::Ident, "after `declare`")?;
        self.expect(Token::Colon, "after the declared name")?;
        let ty = self.type_expr()?;
        Ok(Declaration::Global {
            name: Rc::from(self.text(name.span)),
            span: start.to(ty.span),
            ty,
        })
    }

    /// `Name [extends Parent] members end`, after `declare class`.
    fn class_decl(&mut self, start: Span, opener: rhue_lexer::Spanned) -> PResult<ClassDecl> {
        let name = self.bump();
        let parent = if self.at_word("extends") {
            self.bump();
            let p = self.expect(Token::Ident, "after `extends`")?;
            Some((Rc::from(self.text(p.span)), p.span))
        } else {
            None
        };
        let mut members = Vec::new();
        while !self.at(Token::End) && !self.at(Token::Eof) {
            members.push(self.class_item(name.span)?);
            if self.eat(Token::Comma).is_none() {
                let _ = self.eat(Token::Semi);
            }
        }
        let end = self.expect_closer(Token::End, opener, "declare class")?;
        Ok(ClassDecl {
            name: Rc::from(self.text(name.span)),
            span: start.to(end.span),
            parent,
            members,
        })
    }

    fn class_item(&mut self, class: Span) -> PResult<ClassItem> {
        let start = self.peek().span;
        if self.eat(Token::Function).is_some() {
            let name = self.expect(Token::Ident, "after `function`")?;
            let ty = self.function_decl(Some(class))?;
            return Ok(ClassItem::Method {
                name: Rc::from(self.text(name.span)),
                span: start.to(ty.span),
                ty,
            });
        }
        if self.eat(Token::LBracket).is_some() {
            let key = self.type_expr()?;
            self.expect(Token::RBracket, "to close `[`")?;
            self.expect(Token::Colon, "after `[key]`")?;
            let value = self.type_expr()?;
            return Ok(ClassItem::Index {
                span: start.to(value.span),
                key,
                value,
            });
        }
        let access = if (self.at_word("read") || self.at_word("write"))
            && self.peek_nth(1).token == Token::Ident
        {
            let w = self.bump();
            if self.text(w.span) == "read" {
                Access::Read
            } else {
                Access::Write
            }
        } else {
            Access::ReadWrite
        };
        if !self.at(Token::Ident) {
            let t = self.peek();
            return Err(error(
                format!(
                    "expected a property, `function` or `[string]: T`, found {}",
                    t.token.describe()
                ),
                t.span,
            )
            .with_label_boxed("expected a class member"));
        }
        let name = self.bump();
        self.expect(Token::Colon, "after the property's name")?;
        let ty = self.type_expr()?;
        Ok(ClassItem::Prop {
            name: Rc::from(self.text(name.span)),
            span: start.to(ty.span),
            ty,
            access,
        })
    }

    /// `<T>(a: T, ...: U): R` after a declared function's name, as a function
    /// type. In a class (`class` is where its name is) the first parameter
    /// has to be `self`, and it becomes the class.
    fn function_decl(&mut self, class: Option<Span>) -> PResult<TypeExpr> {
        let start = self.peek().span;
        let generics = self.generic_params(false)?;
        let open = self.expect(Token::LParen, "to start the parameters")?;
        let mut params = Vec::new();
        let mut variadic = None;
        let mut more = !self.at(Token::RParen);
        if let Some(class) = class {
            if !self.at_word("self") {
                return Err(
                    error("a method's first parameter is `self`", self.peek().span)
                        .with_label_boxed("expected `self`"),
                );
            }
            let s = self.bump();
            if self.at(Token::Colon) {
                return Err(error(
                    "`self` is the class, it doesn't get a type",
                    self.peek().span,
                )
                .with_label_boxed("remove the type"));
            }
            params.push(FnParam {
                name: Some(Rc::from("self")),
                ty: TypeExpr {
                    kind: TypeKind::Name {
                        name: Rc::from(self.text(class)),
                        args: Vec::new(),
                    },
                    span: s.span,
                },
            });
            more = self.eat(Token::Comma).is_some();
        }
        while more {
            if self.eat(Token::Ellipsis).is_some() {
                self.expect(Token::Colon, "after `...`")?;
                variadic = Some(Box::new(self.variadic_type()?));
                break;
            }
            let name = self.expect(Token::Ident, "for a parameter")?;
            self.expect(Token::Colon, "after the parameter's name")?;
            let ty = self.type_expr()?;
            params.push(FnParam {
                name: Some(Rc::from(self.text(name.span))),
                ty,
            });
            more = self.eat(Token::Comma).is_some();
        }
        self.expect_closer(Token::RParen, open, "(")?;
        self.expect(Token::Colon, "before the result types")?;
        let returns = self.return_pack()?;
        Ok(TypeExpr {
            span: start.to(returns.span),
            kind: TypeKind::Function {
                generics,
                params,
                variadic,
                returns,
            },
        })
    }
}
