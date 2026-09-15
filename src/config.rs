//! Generation settings: everything about a project that is not its SDL.
//!
//! [`Config`] is the public Rust API; `rpcgen.toml` is its serialized form. A
//! project embedding graphql-rpcgen as a library builds a `Config` directly; a
//! project driving the CLI writes the TOML. Both land in the same struct, so
//! the two paths cannot drift.
//!
//! Every field has a default that reproduces a plain GraphQL-over-HTTP API,
//! which is why an empty config is still a valid one.
//!
//! Scalar mappings are *not* here: they are declared in the SDL with `@scalar`,
//! beside the `scalar` keyword itself, so a scalar is stated once.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::CompileError;
use crate::ir::{ApiType, Constraints, ScalarType};

/// Everything the generators need beyond the IR itself.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// The generator version this project was written against, as
    /// `major.minor.patch`.
    ///
    /// Required in `rpcgen.toml` and checked by [`Config::from_toml_file`]:
    /// generated code is a contract with one generator, so a project states
    /// which one rather than accepting whatever is installed. `None` is what a
    /// hand-built [`Config`] carries, since a caller compiled against this
    /// crate already has its version pinned by cargo.
    pub rpcgen_version: Option<String>,
    /// Where each target writes. A target with no path is not generated.
    pub output: Output,
    /// Document metadata for the OpenAPI target.
    pub openapi: OpenApi,
    /// Overrides for the Rust emitter's templates and the names they refer to.
    pub rust: RustConfig,
    /// Overrides for the Rust client emitter.
    pub rust_client: RustClientConfig,
}

/// Output path per target, relative to the project root.
///
/// `None` means "do not generate this target", so a Rust-only or
/// TypeScript-only project simply omits the others.
///
/// The three Rust targets are separate because their dependencies are: the
/// wire types need only `serde`, the server adds a web framework, and the
/// client adds an HTTP stack. A project consuming an API it does not serve
/// takes `rust_types` + `rust_client` and never compiles `axum`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Output {
    /// Wire types alone: structs, enums, unions, scalar newtypes.
    pub rust_types: Option<PathBuf>,
    /// Service traits and the web bindings that dispatch to them.
    pub rust_server: Option<PathBuf>,
    /// Typed client methods over an HTTP transport.
    pub rust_client: Option<PathBuf>,
    pub typescript: Option<PathBuf>,
    /// zod schemas for the input types, so the browser can check a body against
    /// the same constraints the server enforces.
    ///
    /// Separate from `typescript` because it is the one target with a runtime
    /// dependency: a project that does not want `zod` in its bundle omits this
    /// path and keeps a client that imports nothing.
    pub typescript_zod: Option<PathBuf>,
    pub openapi: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OpenApi {
    pub title: String,
    pub version: String,
    pub description: Option<String>,
    /// Emitted as the document's `servers` list.
    pub servers: Vec<String>,
}

impl Default for OpenApi {
    fn default() -> Self {
        Self {
            title: "RPC API".to_string(),
            version: "0.1.0".to_string(),
            description: None,
            servers: vec!["/".to_string()],
        }
    }
}

/// Names and templates the Rust emitter substitutes.
///
/// The defaults target `treat` + `axum`. A project on another stack overrides
/// the templates; a project on the same stack usually overrides nothing.
/// A `None` template means "use the built-in", which is why the derived
/// default is the treat + axum one.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RustConfig {
    /// Path to a mustache template replacing the built-in preamble, relative
    /// to the config file. The preamble carries the imports, the `Error`
    /// aliases and the failure-envelope helper.
    pub preamble_template: Option<PathBuf>,
    /// Template for one operation's handler function.
    pub handler_template: Option<PathBuf>,
    /// Template for the handler of an operation taking no `input` argument,
    /// which extracts no request body. Separate from `handler_template`
    /// because a flat mustache context cannot branch on whether one exists.
    pub handler_no_input_template: Option<PathBuf>,
    /// Template for a service's router constructor.
    pub router_template: Option<PathBuf>,
    /// Extra `{{{key}}}` values exposed to all three templates, so a project
    /// can parameterize its own template without a code change here.
    pub template_vars: BTreeMap<String, String>,
    /// Rust path of the module holding the wire types, imported by the server
    /// and client targets with a glob `use`.
    ///
    /// `None` means the types are in the same file, which is what a project
    /// generating `rust_server` alone gets: the server target then emits the
    /// types itself, reproducing the single-file layout.
    pub types_path: Option<String>,
}

/// Settings for the generated Rust client.
///
/// The built-in templates target `reqwest`. As with the server, a project on
/// another HTTP stack replaces the templates rather than forking the emitter.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RustClientConfig {
    /// Template for the client preamble: imports, transport, error type.
    pub preamble_template: Option<PathBuf>,
    /// Template for one operation's method on a service group.
    pub method_template: Option<PathBuf>,
    /// Template for the method of an operation taking no `input` argument.
    pub method_no_input_template: Option<PathBuf>,
    /// Extra `{{{key}}}` values exposed to both client templates.
    pub template_vars: BTreeMap<String, String>,
    /// Rust path of the module holding the wire types. Defaults to
    /// [`RustConfig::types_path`] so a project states it once.
    pub types_path: Option<String>,
}

/// Scalars the compiler maps without an `@scalar` directive in the SDL.
///
/// These are the GraphQL standard set plus the three almost every JSON API
/// needs. Anything domain-specific is declared in the SDL; an SDL `@scalar`
/// naming one of these replaces it.
pub fn builtin_scalars() -> Vec<ApiType> {
    fn plain(
        name: &str,
        rust: &str,
        typescript: &str,
        openapi_type: &str,
        openapi_format: Option<&str>,
    ) -> ApiType {
        ApiType::Scalar(ScalarType {
            name: name.to_string(),
            description: None,
            rust: rust.to_string(),
            typescript: typescript.to_string(),
            openapi_type: openapi_type.to_string(),
            openapi_format: openapi_format.map(str::to_string),
            constraints: Constraints::default(),
            rust_newtype: false,
            rust_copy: false,
            rust_range: false,
        })
    }

    vec![
        plain("String", "String", "string", "string", None),
        plain("Int", "i32", "number", "integer", Some("int32")),
        plain("Float", "f64", "number", "number", Some("double")),
        plain("Boolean", "bool", "boolean", "boolean", None),
        plain("ID", "String", "string", "string", None),
        plain("UUID", "uuid::Uuid", "string", "string", Some("uuid")),
        plain(
            "DateTime",
            "chrono::DateTime<chrono::Utc>",
            "string",
            "string",
            Some("date-time"),
        ),
        // Intentionally unconstrained: any JSON value is valid.
        plain("JSON", "serde_json::Value", "unknown", "", None),
    ]
}

impl Config {
    /// Read `rpcgen.toml`. Paths inside it resolve against its directory.
    ///
    /// The file must declare `rpcgen_version`, and this build must satisfy it,
    /// or the read fails. Both are errors about the file, so they are reported
    /// with its path like any other.
    pub fn from_toml_file(path: &Path) -> Result<Self, CompileError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| CompileError::new(format!("{}: {e}", path.display())))?;
        let mut config: Config = toml::from_str(&text)
            .map_err(|e| CompileError::new(format!("{}: {e}", path.display())))?;

        config
            .check_version()
            .map_err(|e| CompileError::new(format!("{}: {e}", path.display())))?;

        if let Some(dir) = path.parent() {
            config.rebase_template_paths(dir);
        }
        Ok(config)
    }

    /// Verify `rpcgen_version` against this build.
    ///
    /// Absent is an error: a config file that names no generator is the case
    /// this check exists to rule out. A [`Config`] built in Rust rather than
    /// read from a file has no such gap — cargo already pinned the version —
    /// so library callers do not go through here.
    pub fn check_version(&self) -> Result<(), CompileError> {
        let Some(required) = &self.rpcgen_version else {
            return Err(CompileError::new(format!(
                "missing `rpcgen_version`. Add the generator version this project \
                 is written against at the top of the file, e.g. \
                 `rpcgen_version = \"{}\"`",
                crate::version::CURRENT
            )));
        };
        crate::version::check(required)
    }

    /// Template paths are written relative to the config file, which is what a
    /// reader expects; resolve them once so the emitter sees usable paths.
    fn rebase_template_paths(&mut self, dir: &Path) {
        for slot in [
            &mut self.rust.preamble_template,
            &mut self.rust.handler_template,
            &mut self.rust.handler_no_input_template,
            &mut self.rust.router_template,
            &mut self.rust_client.preamble_template,
            &mut self.rust_client.method_template,
            &mut self.rust_client.method_no_input_template,
        ] {
            if let Some(path) = slot.as_ref() {
                *slot = Some(dir.join(path));
            }
        }
    }
}

impl RustClientConfig {
    /// Where the client imports its wire types from.
    ///
    /// Falls back to the server's setting, since a project almost always puts
    /// the types in one crate and points both targets at it.
    pub fn resolved_types_path<'a>(&'a self, rust: &'a RustConfig) -> Option<&'a str> {
        self.types_path.as_deref().or(rust.types_path.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a test body answers with: the first failure ends it, carrying the
    /// message of whatever actually went wrong rather than a fixed one.
    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    /// The message of an error the test requires, or a failure naming what went
    /// through instead.
    fn refusal<T, E: std::fmt::Display>(outcome: Result<T, E>, what: &str) -> TestResult<String> {
        match outcome {
            Err(error) => Ok(error.to_string()),
            Ok(_) => Err(format!("{what} must be refused").into()),
        }
    }

    /// A scratch directory of this process's own, for the file-reading tests.
    fn scratch() -> TestResult<std::path::PathBuf> {
        let dir = std::env::temp_dir().join(format!("rpcgen-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    #[test]
    fn the_builtin_scalars_are_the_standard_set_only() {
        let names: Vec<String> = builtin_scalars()
            .iter()
            .map(|t| t.name().to_string())
            .collect();

        assert!(names.contains(&"String".to_string()));
        assert!(names.contains(&"UUID".to_string()));
        // Domain scalars are declared in the SDL with `@scalar`, not built in.
        assert!(!names.contains(&"PlayerId".to_string()));
    }

    /// A scalar mapping is SDL, not config: `[[scalar]]` must not quietly
    /// reappear as an ignored key.
    #[test]
    fn the_config_has_no_scalar_section() -> TestResult {
        let err = refusal(
            toml::from_str::<Config>(
                r#"
            [[scalar]]
            name = "PlayerId"
            rust = "String"
            typescript = "string"
            "#,
            ),
            "a `[[scalar]]` section",
        )?;

        assert!(err.contains("scalar"), "got: {err}");
        Ok(())
    }

    #[test]
    fn unknown_keys_are_rejected_rather_than_silently_ignored() -> TestResult {
        let err = refusal(
            toml::from_str::<Config>(
                r#"
            [output]
            rust_server = "a.rs"
            rust_serverr = "typo.rs"
            "#,
            ),
            "an unknown key",
        )?;

        assert!(err.contains("rust_serverr"), "got: {err}");
        Ok(())
    }

    #[test]
    fn openapi_defaults_apply_when_the_section_is_absent() {
        let config = Config::default();
        assert_eq!(config.openapi.title, "RPC API");
        assert_eq!(config.openapi.servers, vec!["/".to_string()]);
    }

    /// The version is a top-level key, not a section, so it parses alongside
    /// the rest rather than needing its own table.
    #[test]
    fn the_required_version_parses_from_the_top_level() -> TestResult {
        let config: Config = toml::from_str(
            r#"
            rpcgen_version = "1.2.3"

            [output]
            rust_server = "a.rs"
            "#,
        )?;

        assert_eq!(config.rpcgen_version.as_deref(), Some("1.2.3"));
        Ok(())
    }

    /// A file naming no generator is the case the check exists to rule out,
    /// and the message has to say what to add.
    #[test]
    fn a_config_without_a_version_is_rejected_by_the_check() -> TestResult {
        let config: Config = toml::from_str(
            r#"[output]
rust_server = "a.rs"
"#,
        )?;

        let err = refusal(config.check_version(), "a config naming no version")?;
        assert!(err.contains("rpcgen_version"), "got: {err}");
        assert!(
            err.contains(crate::version::CURRENT),
            "must suggest a usable value: {err}"
        );
        Ok(())
    }

    #[test]
    fn a_satisfiable_version_passes_the_check() -> TestResult {
        let config = Config {
            rpcgen_version: Some(crate::version::CURRENT.to_string()),
            ..Config::default()
        };
        config.check_version()?;
        Ok(())
    }

    /// `from_toml_file` is the enforcing boundary, so the failure has to reach
    /// a caller that only ever touches the file.
    #[test]
    fn reading_a_file_without_a_version_fails_and_names_the_file() -> TestResult {
        let path = scratch()?.join("no-version.toml");
        std::fs::write(&path, "[output]\nrust_server = \"a.rs\"\n")?;

        let err = refusal(Config::from_toml_file(&path), "a file naming no version")?;
        assert!(err.contains("no-version.toml"), "got: {err}");
        assert!(err.contains("rpcgen_version"), "got: {err}");
        Ok(())
    }

    #[test]
    fn reading_a_file_with_an_incompatible_major_fails() -> TestResult {
        let path = scratch()?.join("wrong-major.toml");
        let current = crate::version::Version::parse(crate::version::CURRENT)?;
        std::fs::write(
            &path,
            format!("rpcgen_version = \"{}.0.0\"\n", current.major + 1),
        )?;

        let err = refusal(Config::from_toml_file(&path), "a file naming another major")?;
        assert!(err.contains("wrong-major.toml"), "got: {err}");
        assert!(err.contains("major version"), "got: {err}");
        Ok(())
    }

    /// A version present and satisfied must not disturb anything else the file
    /// says, including the template rebasing that happens after the check.
    #[test]
    fn a_valid_version_leaves_the_rest_of_the_file_intact() -> TestResult {
        let dir = scratch()?;
        let path = dir.join("valid.toml");
        std::fs::write(
            &path,
            format!(
                "rpcgen_version = \"{}\"\n\n[output]\nrust_server = \"a.rs\"\n\n\
                 [rust]\nhandler_template = \"t/handler.mustache\"\n",
                crate::version::CURRENT
            ),
        )?;

        let config = Config::from_toml_file(&path)?;
        assert_eq!(config.output.rust_server, Some(PathBuf::from("a.rs")));
        assert_eq!(
            config.rust.handler_template,
            Some(dir.join("t/handler.mustache")),
            "template paths must still rebase against the config's directory"
        );
        Ok(())
    }
}
