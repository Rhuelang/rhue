use std::fmt;

use ariadne::{CharSet, Color, Config, IndexType, Label as ALabel, Report, ReportKind, Source};

use crate::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// A labelled span inside a [`Diagnostic`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

/// A single error or warning, independent of how it is rendered.
///
/// Every phase (lexer, parser, compiler, VM) reports through this type so the
/// embedder sees one uniform error shape, and the CLI can pretty-print any of
/// them with [`Diagnostic::render`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub primary: Label,
    pub secondary: Vec<Label>,
    pub notes: Vec<String>,
    pub help: Option<String>,
    /// Edits that would fix it, for editors to offer (the CLI shows only
    /// `help`).
    pub fixes: Vec<Fix>,
}

/// A suggested change to the source: replacing each span with its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    /// What it does, as a command: "Change to 'name'".
    pub title: String,
    /// Non-overlapping replacements (an empty span inserts).
    pub edits: Vec<(Span, String)>,
}

impl Diagnostic {
    pub fn error(message: impl Into<String>, span: Span) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
            primary: Label {
                span,
                message: String::new(),
            },
            secondary: Vec::new(),
            notes: Vec::new(),
            help: None,
            fixes: Vec::new(),
        }
    }

    /// Sets the text shown next to the primary span.
    pub fn with_label(mut self, message: impl Into<String>) -> Self {
        self.primary.message = message.into();
        self
    }

    pub fn with_secondary(mut self, span: Span, message: impl Into<String>) -> Self {
        self.secondary.push(Label {
            span,
            message: message.into(),
        });
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    pub fn with_fix(mut self, title: impl Into<String>, edits: Vec<(Span, String)>) -> Self {
        self.fixes.push(Fix {
            title: title.into(),
            edits,
        });
        self
    }

    pub fn span(&self) -> Span {
        self.primary.span
    }

    /// Renders this diagnostic with ariadne, underlining the source.
    ///
    /// `color` should be `false` when the output is not a terminal (and in tests).
    pub fn render(&self, source_name: &str, source: &str, color: bool) -> String {
        let kind = match self.severity {
            Severity::Error => ReportKind::Error,
            Severity::Warning => ReportKind::Warning,
        };
        let clamp = |s: Span| {
            let len = source.len();
            (s.start as usize).min(len)..(s.end as usize).min(len)
        };
        let config = Config::default()
            .with_color(color)
            .with_index_type(IndexType::Byte)
            .with_char_set(if color {
                CharSet::Unicode
            } else {
                CharSet::Ascii
            });

        let mut primary = ALabel::new((source_name, clamp(self.primary.span)));
        if !self.primary.message.is_empty() {
            primary = primary.with_message(&self.primary.message);
        }
        if color {
            primary = primary.with_color(Color::Red);
        }

        let mut report = Report::build(kind, (source_name, clamp(self.primary.span)))
            .with_config(config)
            .with_message(&self.message)
            .with_label(primary);
        for label in &self.secondary {
            let mut l = ALabel::new((source_name, clamp(label.span))).with_message(&label.message);
            if color {
                l = l.with_color(Color::Blue);
            }
            report = report.with_label(l);
        }
        for note in &self.notes {
            report = report.with_note(note);
        }
        if let Some(help) = &self.help {
            report = report.with_help(help);
        }

        let mut out = Vec::new();
        report
            .finish()
            .write((source_name, Source::from(source)), &mut out)
            .expect("writing to a Vec cannot fail");
        String::from_utf8_lossy(&out).into_owned()
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Diagnostic {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_message_label_and_note() {
        let src = "return 1 + true";
        let d = Diagnostic::error(
            "attempt to perform arithmetic (add) on boolean",
            Span::new(7, 15),
        )
        .with_label("this is a boolean")
        .with_note("arithmetic needs numbers");
        let out = d.render("test.rhue", src, false);
        assert!(
            out.contains("attempt to perform arithmetic (add) on boolean"),
            "{out}"
        );
        assert!(out.contains("this is a boolean"), "{out}");
        assert!(out.contains("arithmetic needs numbers"), "{out}");
        assert!(out.contains("test.rhue"), "{out}");
    }

    #[test]
    fn spans_past_end_are_clamped() {
        let d = Diagnostic::error("unexpected end of input", Span::new(3, 4));
        let out = d.render("t", "abc", false);
        assert!(out.contains("unexpected end of input"));
    }
}
