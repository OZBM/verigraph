//! Diagnostics: stable codes located in the agent's own words (docs/15).

use std::fmt;

/// A location: 1-based line and column in Unicode scalar values, and a length in the same unit.
/// `offset` is the 0-based index of the first character, for slicing the source.
#[derive(Clone, Copy, Debug, Default)]
pub struct Span {
    pub line: u32,
    pub col: u32,
    pub len: u32,
    pub offset: u32,
}

/// Spans are metadata: two syntax trees are equal when they differ only in where they were written,
/// which is what "parse after print is the identity" compares.
impl PartialEq for Span {
    fn eq(&self, _: &Span) -> bool {
        true
    }
}

impl Eq for Span {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub severity: Severity,
    /// The node's label, such as `@soft_knee`.
    pub node: Option<String>,
    /// The op's first result port as written, such as `%4`.
    pub op: Option<String>,
    pub span: Span,
    pub message: String,
}

impl Diagnostic {
    pub fn error(code: &'static str, span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            code,
            severity: Severity::Error,
            node: None,
            op: None,
            span,
            message: message.into(),
        }
    }

    pub fn warning(code: &'static str, span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            severity: Severity::Warning,
            ..Diagnostic::error(code, span, message)
        }
    }

    pub fn at_node(mut self, label: &str) -> Diagnostic {
        self.node.get_or_insert_with(|| label.to_string());
        self
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sev = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        write!(
            f,
            "{}:{}: {sev} {}",
            self.span.line, self.span.col, self.code
        )?;
        if let Some(n) = &self.node {
            write!(f, " {n}")?;
        }
        if let Some(o) = &self.op {
            write!(f, " {o}")?;
        }
        write!(f, ": {}", self.message)
    }
}
