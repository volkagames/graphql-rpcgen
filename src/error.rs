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

    pub fn into_result(self) -> Result<(), CompileError> {
        if self.items.is_empty() {
            return Ok(());
        }
        let joined = self
            .items
            .iter()
            .map(|e| format!("  - {e}"))
            .collect::<Vec<_>>()
            .join("\n");
        Err(CompileError::new(format!(
            "{} semantic error(s):\n{joined}",
            self.items.len()
        )))
    }
}
