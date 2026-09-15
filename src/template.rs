//! Mustache rendering for the Rust emitter's overridable pieces.
//!
//! Templates exist so a project on a different web stack can retarget the
//! generated bindings without forking the compiler. The built-in templates
//! target `treat` + `axum` and are compiled in, so the default path needs no
//! template files on disk.
//!
//! # Two mustache behaviours that matter for code generation
//!
//! `{{x}}` HTML-escapes its value, which corrupts every Rust generic:
//! `Vec<Option<T>>` renders as `Vec&lt;Option&lt;T&gt;&gt;`. Templates here use
//! the triple-stache `{{{x}}}` form throughout, and [`Template::render`]
//! rejects a template that uses the escaping form.
//!
//! A missing key renders as empty rather than failing, so a typo like
//! `{{{handlr}}}` would silently emit `async fn <S: …>`. [`Template::compile`]
//! therefore checks every referenced variable against the names the caller
//! declares, turning a typo into a generation-time error.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::CompileError;

/// Values substituted into a template: a flat name -> text map.
pub type Vars = BTreeMap<String, String>;

/// A validated template, ready to render.
///
/// `mustache::Template` is not `Debug`, so only the source is shown.
pub struct Template {
    compiled: mustache::Template,
    source: String,
    origin: String,
}

impl Template {
    /// Compile `source`, rejecting unknown variables and escaping tags.
    ///
    /// `allowed` lists every variable the caller will supply. `origin`
    /// identifies the template in error messages.
    pub fn compile(source: &str, origin: &str, allowed: &[&str]) -> Result<Self, CompileError> {
        let referenced = referenced_variables(source);

        for name in &referenced {
            if !allowed.contains(&name.as_str()) {
                let mut known: Vec<&str> = allowed.to_vec();
                known.sort_unstable();
                return Err(CompileError::new(format!(
                    "{origin}: unknown template variable `{name}`; available: {}",
                    known.join(", ")
                )));
            }
        }

        if let Some(name) = escaping_tag(source) {
            return Err(CompileError::new(format!(
                "{origin}: `{{{{{name}}}}}` HTML-escapes its value, which corrupts Rust \
                 syntax such as `Vec<T>`. Use `{{{{{{{name}}}}}}}` instead."
            )));
        }

        let compiled = mustache::compile_str(source)
            .map_err(|e| CompileError::new(format!("{origin}: {e}")))?;

        Ok(Self {
            compiled,
            source: source.to_string(),
            origin: origin.to_string(),
        })
    }

    /// Read and compile a template file.
    pub fn from_file(path: &Path, allowed: &[&str]) -> Result<Self, CompileError> {
        let source = std::fs::read_to_string(path)
            .map_err(|e| CompileError::new(format!("{}: {e}", path.display())))?;
        Template::compile(&source, &path.display().to_string(), allowed)
    }

    pub fn render(&self, vars: &Vars) -> Result<String, CompileError> {
        let mut out = Vec::new();
        self.compiled
            .render(&mut out, vars)
            .map_err(|e| CompileError::new(format!("{}: {e}", self.origin)))?;
        String::from_utf8(out).map_err(|e| {
            CompileError::new(format!("{}: template produced non-UTF-8: {e}", self.origin))
        })
    }

    /// The template text as compiled, for diagnostics.
    pub fn source(&self) -> &str {
        &self.source
    }
}

impl std::fmt::Debug for Template {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Template")
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

/// Variable names referenced by `{{name}}`, `{{{name}}}` or `{{&name}}`.
///
/// Section tags (`{{#x}}`, `{{/x}}`, `{{^x}}`), comments and partials are
/// skipped: the emitter supplies a flat string map, so sections have nothing
/// to iterate and are not part of the contract.
fn referenced_variables(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = source;

    while let Some(start) = rest.find("{{") {
        rest = &rest[start + 2..];
        let Some(end) = rest.find("}}") else { break };
        let (tag, after) = rest.split_at(end);
        rest = &after[2..];

        // A triple-stache `{{{x}}}` is found as `{{` + `{x` + `}}`, so strip
        // the brace the outer match left on either side.
        let tag = tag.trim().trim_start_matches('{').trim_end_matches('}');
        let tag = tag.trim();

        // Sections, comments, partials and delimiter changes carry a sigil.
        if tag.starts_with(['#', '/', '^', '!', '>', '=', '<']) {
            continue;
        }
        let name = tag.trim_start_matches('&').trim();
        if !name.is_empty() && !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
    }
    names
}

/// The first variable used in escaping `{{x}}` form, if any.
fn escaping_tag(source: &str) -> Option<String> {
    let mut rest = source;

    while let Some(start) = rest.find("{{") {
        let before = &rest[start..];
        rest = &rest[start + 2..];

        // Triple-stache and the explicit `&` form do not escape.
        if before.starts_with("{{{") || rest.trim_start().starts_with('&') {
            continue;
        }
        let Some(end) = rest.find("}}") else { break };
        let (tag, after) = rest.split_at(end);
        rest = &after[2..];

        let tag = tag.trim();
        if tag.starts_with(['#', '/', '^', '!', '>', '=', '<']) {
            continue;
        }
        if !tag.is_empty() {
            return Some(tag.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a test body answers with: the first failure ends it, carrying the
    /// message of whatever actually went wrong rather than a fixed one.
    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    /// The error a test requires, or a failure naming what compiled instead.
    fn refusal(source: &str, known: &[&str]) -> TestResult<CompileError> {
        match Template::compile(source, "<test>", known) {
            Err(error) => Ok(error),
            Ok(_) => Err(format!("`{source}` must be refused").into()),
        }
    }

    fn vars(pairs: &[(&str, &str)]) -> Vars {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn renders_triple_stache_without_escaping() -> TestResult {
        let t = Template::compile("fn f() -> {{{output}}}", "<test>", &["output"])?;
        assert_eq!(
            t.render(&vars(&[("output", "Vec<Option<T>>")]))?,
            "fn f() -> Vec<Option<T>>"
        );
        Ok(())
    }

    /// The failure this guard exists for: a typo would otherwise render as
    /// empty and emit syntactically broken Rust.
    /// The triple-stache form must yield the bare name: an earlier version
    /// read `{{{x}}}` as the variable `{x` and rejected every valid template.
    #[test]
    fn triple_stache_names_are_parsed_without_their_braces() {
        assert_eq!(referenced_variables("a {{{x}}} b"), vec!["x".to_string()]);
        assert_eq!(
            referenced_variables("{{{a}}}{{b}}{{& c }}"),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn a_misspelled_variable_is_an_error_not_an_empty_string() -> TestResult {
        let message = refusal("async fn {{{handlr}}}()", &["handler"])?.to_string();
        assert!(message.contains("handlr"), "got: {message}");
        assert!(message.contains("handler"), "must list what is available");
        Ok(())
    }

    /// `{{x}}` would turn `Vec<T>` into `Vec&lt;T&gt;`.
    #[test]
    fn the_escaping_form_is_rejected() -> TestResult {
        let err = refusal("-> {{output}}", &["output"])?;
        assert!(err.to_string().contains("HTML-escapes"), "got: {err}");
        assert!(
            err.to_string().contains("{{{output}}}"),
            "must show the fix"
        );
        Ok(())
    }

    #[test]
    fn the_ampersand_form_is_accepted_as_non_escaping() -> TestResult {
        let t = Template::compile("-> {{&output}}", "<test>", &["output"])?;
        assert_eq!(t.render(&vars(&[("output", "Vec<T>")]))?, "-> Vec<T>");
        Ok(())
    }

    #[test]
    fn section_and_comment_tags_are_not_treated_as_variables() -> TestResult {
        let source = "{{! a note }}{{#items}}x{{/items}}{{{name}}}";
        let t = Template::compile(source, "<test>", &["name"])?;
        assert_eq!(t.render(&vars(&[("name", "ok")]))?, "ok");
        Ok(())
    }

    #[test]
    fn malformed_template_fails_to_compile() {
        assert!(Template::compile("{{#a}}oops", "<test>", &[]).is_err());
    }
}
