//! Fixes the checker attaches to its errors for editors to offer, the name you
//! probably meant or a nil check.

use rhue_common::{Span, closest};

use crate::check::Checker;
use crate::types::Type;

impl Checker<'_> {
    /// Errors since `from` saying `name` isn't a member of `ot` (reading or
    /// adding it) get the closest member as a suggestion, replacing `key` (the
    /// name as written, not for `t["name"]`).
    pub(crate) fn suggest_member(&mut self, from: usize, ot: &Type, name: &str, key: Span) {
        if (key.end - key.start) as usize != name.len() {
            return;
        }
        let missing = |m: &str| {
            m.starts_with(&format!("Key '{name}' not found"))
                || m.starts_with(&format!("Cannot add property '{name}'"))
        };
        if !self.errors[from.min(self.errors.len())..]
            .iter()
            .any(|d| missing(&d.message))
        {
            return;
        }
        let members = self.members_of(ot);
        let Some(best) = closest(name, members.iter().map(|(n, _)| &**n)) else {
            return;
        };
        let best = best.to_string();
        for d in &mut self.errors[from..] {
            if missing(&d.message) && d.fixes.is_empty() {
                d.help = Some(format!("did you mean '{best}'?"));
                d.fixes.push(rhue_common::Fix {
                    title: format!("Change to '{best}'"),
                    edits: vec![(key, best.clone())],
                });
            }
        }
    }

    /// The closest name a misspelled global could've meant, a global or a local
    /// declared before it.
    pub(crate) fn suggest_global(&self, name: &str, at: Span) -> Option<String> {
        let mut names: Vec<&str> = self.env.globals().map(|(n, _)| &**n).collect();
        names.extend(self.chunk_globals.keys().map(|n| &**n));
        if let Some(res) = self.res {
            names.extend(
                res.locals
                    .iter()
                    .filter(|l| l.span.end <= at.start)
                    .map(|l| &*l.name),
            );
        }
        closest(name, names).map(str::to_string)
    }

    /// Errors since `from` at `at` saying a value could be nil get a fix that
    /// wraps `operand` (the value) in `assert(...)`, which makes it non-nil.
    pub(crate) fn suggest_assert(&mut self, from: usize, at: Span, operand: Span) {
        let from = from.min(self.errors.len());
        for d in &mut self.errors[from..] {
            if d.primary.span == at && d.message.ends_with("could be nil") && d.fixes.is_empty() {
                d.fixes.push(rhue_common::Fix {
                    title: "Assert that it isn't nil".into(),
                    edits: vec![
                        (Span::new(operand.start, operand.start), "assert(".into()),
                        (Span::new(operand.end, operand.end), ")".into()),
                    ],
                });
            }
        }
    }
}
