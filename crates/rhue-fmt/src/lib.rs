//! `rhue fmt`, a formatter that keeps your line breaks and comments and
//! fixes everything else.
//!
//! It works on tokens, not a syntax tree, so nothing a tree would lose
//! (comments, blank lines, where you broke a long call) gets lost. For each
//! line it works out:
//!
//! - **Indentation**, from the blocks and brackets open at its start:
//!   `function`, `then`, `do`, `repeat`, `(`, `[`, `{`. A few opened on one
//!   line only indent once (`foo(function()` indents its body one level),
//!   and a line that starts by closing some (`end)`, `})`) goes back to
//!   where they opened. A line continuing an expression (ending or starting
//!   with a binary operator, `.`, `:`) gets one extra level. Luau's
//!   if-expressions (`if a then b else c`) don't indent.
//! - **Spacing** between tokens: one space around binary operators, `=`,
//!   `->`, `|`, `&`, after commas and keywords, and inside non-empty
//!   braces. None inside parentheses and brackets, before `(` in calls,
//!   around `.`, or after unary operators. `obj:method(...)` and generics
//!   (`f<T>(x)`) stay tight while `name: type` and comparisons get spaces,
//!   and call sugar (`f "x"`, `f {..}`) keeps whatever you had.
//! - **Blank lines**: one in a row at most, none at the start or end.
//!
//! The output gets checked. It has to lex to exactly the same tokens and
//! comments as the input, or formatting fails with an internal error
//! instead of changing what the program means. It's idempotent, and a
//! property test checks that inputs that only differ in spacing and
//! indentation format the same.

use rhue_common::Diagnostic;
use rhue_lexer::Token;

/// How to indent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatOptions {
    /// One level of indentation (a tab by default).
    pub indent: String,
}

impl Default for FormatOptions {
    fn default() -> Self {
        FormatOptions {
            indent: "\t".into(),
        }
    }
}

/// Why a source couldn't be formatted.
#[derive(Debug)]
pub enum FormatError {
    /// It doesn't parse, formatting only works on valid code.
    Syntax(Vec<Diagnostic>),
    /// The formatter would've changed the program (a bug).
    Internal(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Tok(Token),
    LineComment,
    BlockComment,
}

#[derive(Clone, Copy, Debug)]
struct Item<'a> {
    kind: Kind,
    text: &'a str,
    /// Newlines in the whitespace before it.
    newlines: usize,
    /// Whether there was any whitespace before it.
    space: bool,
}

/// Formats `source`, or tells you why it can't.
pub fn format(source: &str, options: &FormatOptions) -> Result<String, FormatError> {
    let source = source.replace("\r\n", "\n");
    rhue_parser::parse_chunk(&source).map_err(FormatError::Syntax)?;
    let items = items(&source);
    let out = render(&items, options);
    verify(&source, &out)?;
    Ok(out)
}

/// The tokens and comments of a source that lexes cleanly.
fn items(source: &str) -> Vec<Item<'_>> {
    let lexed = rhue_lexer::lex(source);
    let mut out = Vec::new();
    let mut pos = 0usize;
    for t in &lexed.tokens {
        let start = t.span.start as usize;
        gap(source, pos, start, &mut out);
        if t.token == Token::Eof {
            break;
        }
        let end = t.span.end as usize;
        let (newlines, space) = pending(source, &out, pos, start);
        out.push(Item {
            kind: Kind::Tok(t.token),
            text: &source[start..end],
            newlines,
            space,
        });
        pos = end;
    }
    out
}

/// Whitespace info between the last thing pushed (ending at `from`) and `to`,
/// ignoring comments already pushed from that range.
fn pending(source: &str, out: &[Item<'_>], from: usize, to: usize) -> (usize, bool) {
    // the whitespace right before `to`, back to the end of the item before
    let prev_end = out
        .last()
        .map(|i| i.text.as_ptr() as usize - source.as_ptr() as usize + i.text.len())
        .unwrap_or(0)
        .max(from);
    let ws = &source[prev_end..to];
    (ws.matches('\n').count(), !ws.is_empty())
}

/// Pushes the comments in `source[from..to]` (the rest is whitespace).
fn gap<'a>(source: &'a str, from: usize, to: usize, out: &mut Vec<Item<'a>>) {
    let mut i = from;
    let bytes = source.as_bytes();
    while i < to {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // a comment, `--[[...]]` (any level) or to the end of the line
        let start = i;
        let rest = &source[i + 2..to];
        let block_level = rest.strip_prefix('[').and_then(|r| {
            let level = r.bytes().take_while(|&b| b == b'=').count();
            (r.as_bytes().get(level) == Some(&b'[')).then_some(level)
        });
        let (end, kind) = match block_level {
            Some(level) => {
                let close = format!("]{}]", "=".repeat(level));
                let body = i + 2 + 2 + level;
                let at = source[body..to]
                    .find(&close)
                    .map_or(to, |k| body + k + close.len());
                (at, Kind::BlockComment)
            }
            None => (
                source[i..to].find('\n').map_or(to, |k| i + k),
                Kind::LineComment,
            ),
        };
        let (newlines, space) = pending(source, out, from, start);
        out.push(Item {
            kind,
            text: source[start..end].trim_end(),
            newlines,
            space,
        });
        i = end;
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Frame {
    /// A block or bracket, and the line it opened on.
    Open(usize),
    /// An if-expression (no `end`, no indent).
    IfExpr,
}

fn is_closer(k: Kind) -> bool {
    matches!(
        k,
        Kind::Tok(
            Token::End
                | Token::Until
                | Token::RParen
                | Token::RBracket
                | Token::RBrace
                | Token::Else
                | Token::Elseif
        )
    )
}

/// Tokens after which an expression is expected (so an `if` there is an
/// if-expression, and `-` is unary).
fn expects_operand(prev: Option<Token>) -> bool {
    use Token::*;
    match prev {
        None => true,
        Some(t) => !matches!(
            t,
            Ident
                | Number(_)
                | String
                | InterpString
                | RParen
                | RBracket
                | RBrace
                | Ellipsis
                | True
                | False
                | Nil
                | End
        ),
    }
}

fn is_binary(t: Token) -> bool {
    use Token::*;
    matches!(
        t,
        Plus | Minus
            | Star
            | Slash
            | SlashSlash
            | Percent
            | Caret
            | DotDot
            | EqEq
            | NotEq
            | LtEq
            | GtEq
            | And
            | Or
            | Eq
            | PlusEq
            | MinusEq
            | StarEq
            | SlashEq
            | SlashSlashEq
            | PercentEq
            | CaretEq
            | DotDotEq
            | Arrow
            | Pipe
            | Amp
            | ColonColon
    )
}

struct Indenter {
    stack: Vec<Frame>,
    /// The last token was the `else` of an if-expression (so an `if` right
    /// after is one too).
    after_expr_else: bool,
    /// Lines whose opened frames got closed at the start of a line (their
    /// leftover frames don't add indentation anymore).
    retired: Vec<usize>,
}

impl Indenter {
    fn level(&self) -> usize {
        let mut lines: Vec<usize> = self
            .stack
            .iter()
            .filter_map(|f| match f {
                Frame::Open(l) if !self.retired.contains(l) => Some(*l),
                _ => None,
            })
            .collect();
        lines.dedup();
        lines.len()
    }

    fn pop_open(&mut self) -> Option<usize> {
        while let Some(f) = self.stack.pop() {
            if let Frame::Open(l) = f {
                return Some(l);
            }
        }
        None
    }

    fn top_is_if_expr(&self) -> bool {
        self.stack.last() == Some(&Frame::IfExpr)
    }

    /// Applies one token, returns the line of a frame it closed if any.
    fn token(&mut self, t: Token, prev: Option<Token>, line: usize) -> Option<usize> {
        use Token::*;
        let after_expr_else = std::mem::replace(&mut self.after_expr_else, false);
        match t {
            Function | Do | Repeat | LParen | LBracket | LBrace => {
                self.stack.push(Frame::Open(line));
                None
            }
            If => {
                let statement_start =
                    matches!(prev, None | Some(Then | Do | Else | Repeat | Semi | Break));
                let expr = after_expr_else
                    || (expects_operand(prev) && !statement_start)
                    || (matches!(prev, Some(Then)) && self.top_is_if_expr());
                if expr {
                    self.stack.push(Frame::IfExpr);
                }
                None
            }
            Then if self.top_is_if_expr() => None,
            Then => {
                self.stack.push(Frame::Open(line));
                None
            }
            Elseif if self.top_is_if_expr() => None,
            Else if self.top_is_if_expr() => {
                self.stack.pop();
                self.after_expr_else = true;
                None
            }
            Elseif => self.pop_open(),
            Else => {
                let l = self.pop_open();
                self.stack.push(Frame::Open(line));
                l
            }
            End | Until => self.pop_open(),
            RParen | RBracket | RBrace => {
                while self.top_is_if_expr() {
                    self.stack.pop();
                }
                self.pop_open()
            }
            _ => None,
        }
    }
}

fn render(items: &[Item<'_>], options: &FormatOptions) -> String {
    let mut ind = Indenter {
        stack: Vec::new(),
        after_expr_else: false,
        retired: Vec::new(),
    };
    // the token before each item (across lines), for unary minus
    let mut before: Vec<Option<Token>> = Vec::with_capacity(items.len());
    let mut last = None;
    for it in items {
        before.push(last);
        if let Kind::Tok(t) = it.kind {
            last = Some(t);
        }
    }
    let generic = generics(items);
    let mut lines: Vec<String> = Vec::new();
    let mut prev_line_last: Option<Token> = None;
    let mut prev_line_opened = false;
    let mut i = 0;
    let mut line_no = 0usize;
    while i < items.len() {
        // one line: items up to the next one that starts on a new line
        let start = i;
        i += 1;
        while i < items.len() && items[i].newlines == 0 {
            i += 1;
        }
        line_no += 1;
        if start > 0 && items[start].newlines > 1 {
            lines.push(String::new());
        }
        let depth_before = ind.stack.len();
        // leading closers decide this line's own indentation
        let mut k = start;
        let mut reopen = false;
        while k < i
            && is_closer(items[k].kind)
            && !(matches!(items[k].kind, Kind::Tok(Token::Else | Token::Elseif))
                && ind.top_is_if_expr())
        {
            if let Kind::Tok(t) = items[k].kind {
                // a statement `else` closes the block above it and opens its
                // own, which only indents the lines after it
                let closed = if t == Token::Else && !ind.top_is_if_expr() {
                    reopen = true;
                    ind.pop_open()
                } else {
                    ind.token(t, before[k], line_no)
                };
                if let Some(l) = closed
                    && !ind.retired.contains(&l)
                {
                    ind.retired.push(l);
                }
            }
            k += 1;
        }
        let mut level = ind.level();
        if reopen {
            ind.stack.push(Frame::Open(line_no));
        }
        let if_expr_branch = matches!(
            items[start].kind,
            Kind::Tok(Token::Then | Token::Else | Token::Elseif)
        ) && ind.top_is_if_expr();
        let continues = k == start
            && (!prev_line_opened || if_expr_branch)
            && (if_expr_branch
                || prev_line_last.is_some_and(is_binary)
                || matches!(
                    items[start].kind,
                    Kind::Tok(t) if (is_binary(t) && t != Token::Eq)
                        || matches!(t, Token::Dot | Token::Colon)
                ));
        if continues {
            level += 1;
        }
        let mut text = options.indent.repeat(level);
        for n in start..i {
            let item = &items[n];
            if n > start && space_between(items, n - 1, before[n - 1], &generic) {
                text.push(' ');
            }
            text.push_str(item.text);
            if n >= k
                && let Kind::Tok(t) = item.kind
            {
                ind.token(t, before[n], line_no);
            }
        }
        prev_line_last = items[start..i].iter().rev().find_map(|it| match it.kind {
            Kind::Tok(t) => Some(t),
            _ => None,
        });
        prev_line_opened = ind.stack.len() > depth_before;
        lines.push(text.trim_end().to_string());
    }
    if lines.is_empty() {
        return String::new();
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Which `<` / `>` tokens are generics (`f<T>(x)`, `type Map<K, V>`,
/// `Array<number>`) and not comparisons: a `<` right after a name whose
/// matching `>` on the same line only wraps things a type list can hold.
fn generics(items: &[Item<'_>]) -> Vec<bool> {
    use Token::*;
    let mut out = vec![false; items.len()];
    for i in 1..items.len() {
        if items[i].kind != Kind::Tok(Lt) || items[i - 1].kind != Kind::Tok(Ident) {
            continue;
        }
        let mut depth = 0;
        for j in i..items.len() {
            if j > i && items[j].newlines > 0 {
                break;
            }
            match items[j].kind {
                Kind::Tok(Lt) => depth += 1,
                Kind::Tok(Gt) => {
                    depth -= 1;
                    if depth == 0 {
                        // mark this pair and any nested ones inside
                        for k in i..=j {
                            if matches!(items[k].kind, Kind::Tok(Lt | Gt)) {
                                out[k] = true;
                            }
                        }
                        break;
                    }
                }
                Kind::Tok(
                    Ident | Comma | Ellipsis | LBrace | RBrace | LParen | RParen | LBracket
                    | RBracket | Question | Pipe | Amp | Arrow | String | Colon | Eq | Nil | True
                    | False,
                ) => {}
                _ => break,
            }
        }
    }
    out
}

/// Whether to put a space between items `a` and `a + 1` (next to each other on
/// one line). `before_a` is the token before `a`.
fn space_between(items: &[Item<'_>], a: usize, before_a: Option<Token>, generic: &[bool]) -> bool {
    use Token::*;
    let (x, y) = (&items[a], &items[a + 1]);
    let (ta, tb) = match (x.kind, y.kind) {
        (_, Kind::LineComment) => return true,
        (Kind::BlockComment, _) | (_, Kind::BlockComment) => return y.space,
        (Kind::LineComment, _) => return true,
        (Kind::Tok(p), Kind::Tok(q)) => (p, q),
    };
    // never glue two minuses into a comment
    if ta == Minus && tb == Minus {
        return true;
    }
    // unary minus sticks to its operand
    if ta == Minus && expects_operand(before_a) {
        return false;
    }
    let tok = |k: usize| match items.get(k).map(|i| i.kind) {
        Some(Kind::Tok(t)) => Some(t),
        _ => None,
    };
    match (ta, tb) {
        (LBrace, RBrace) => false,
        (LBrace, _) | (_, RBrace) => true,
        (_, RParen | RBracket | Comma | Semi | Dot | Question) => false,
        (LParen | LBracket | Dot | Hash, _) => false,
        (Comma | Semi, _) => true,
        (_, Colon) => false,
        // `obj:method(...)` (a name, then call arguments) vs `name: type`
        (Colon, _) => {
            !(tb == Ident && matches!(tok(a + 2), Some(LParen | String | LBrace | InterpString)))
        }
        (Lt, _) if generic[a] => false,
        (_, Lt) if generic[a + 1] => false,
        (_, Gt) if generic[a + 1] => false,
        (Gt, _) if generic[a] => !matches!(tb, LParen | Gt | Question),
        (_, LParen) => !matches!(
            ta,
            Ident | RParen | RBracket | RBrace | String | Function | Question
        ),
        (_, LBracket) => !matches!(ta, Ident | RParen | RBracket | String),
        // a generic type pack, `T...` (a name never gets followed by `...` in
        // an expression)
        (Ident, Ellipsis) => false,
        // `f "x"` / `f {..}` call sugar, keep the space or the lack of one
        (Ident | RParen | RBracket, String | LBrace) => y.space,
        _ => true,
    }
}

fn verify(source: &str, out: &str) -> Result<(), FormatError> {
    let before = items(source);
    let after = items(out);
    let key = |it: &Item<'_>| (it.kind, it.text.trim_end().to_string());
    let a: Vec<_> = before.iter().map(key).collect();
    let b: Vec<_> = after.iter().map(key).collect();
    if a != b {
        let at = a
            .iter()
            .zip(&b)
            .position(|(x, y)| x != y)
            .unwrap_or(a.len().min(b.len()));
        return Err(FormatError::Internal(format!(
            "formatting would change the program near token {at}: {:?} became {:?}",
            a.get(at),
            b.get(at)
        )));
    }
    Ok(())
}
