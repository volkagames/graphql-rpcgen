//! Compile diagnostics.
//!
//! Every semantic error must let the reader find the SDL declaration, so
//! messages carry file and `line:col` whenever the AST provides a position.

use std::fmt;
use std::path::Path;

use graphql_parser::Pos;

#[derive(Debug, Clone)]
pub struct CompileError {
    pub message: String,
    pub location: Option<String>,
}

impl CompileError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            location: None,
        }
    }

    pub fn at(message: impl Into<String>, file: &Path, pos: Pos) -> Self {
        Self {
            message: message.into(),
            location: Some(format!("{}:{}:{}", file.display(), pos.line, pos.column)),
        }
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.location {
            Some(loc) => write!(f, "{loc}: {}", self.message),
            None => write!(f, "{}", self.message),
        }
    }
}

impl std::error::Error for CompileError {}

/// A report as one line: every message in its chain, outermost first, joined
/// by `: ` — `cannot read api/a.graphql: No such file or directory`.
///
/// A report's `Display` is its outermost message alone, which names where a
/// failure happened and not what it was; its `Debug` adds the generator's own
/// source locations. This is the text for a person running the generator,
/// and what the CLI prints. A link repeating the one before it is dropped: an
/// error carried as a report displays as itself and is its own first source.
pub fn describe(report: &erris::Report) -> String {
    let root: &(dyn std::error::Error + 'static) = report.as_ref();
    let mut parts: Vec<String> = Vec::new();
    let mut link = Some(root);
    while let Some(error) = link {
        let text = error.to_string();
        let repeated = parts.last().is_some_and(|last| *last == text);
        if !text.is_empty() && !repeated {
            parts.push(text);
        }
        link = error.source();
    }
    parts.join(": ")
}

/// Collects diagnostics so one run can report every problem, not just the first.
#[derive(Debug, Default)]
pub struct Errors {
    items: Vec<CompileError>,
}

impl Errors {
    pub fn push(&mut self, error: CompileError) {
        self.items.push(error);
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// `Ok` when nothing was collected; otherwise one report listing every
    /// diagnostic, so a run names all of its problems at once.
    pub fn into_result(self) -> erris::Result<()> {
        if self.items.is_empty() {
            return Ok(());
        }
        let joined = self
            .items
            .iter()
            .map(|e| format!("  - {e}"))
            .collect::<Vec<_>>()
            .join("\n");
        Err(erris::report!(
            "{} semantic error(s):\n{joined}",
            self.items.len()
        ))
    }
}
