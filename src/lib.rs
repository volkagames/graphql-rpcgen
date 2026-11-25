//! GraphQL SDL -> RPC API compiler.
//!
//! Pipeline: SDL -> AST -> semantic validation -> IR -> generators, then the
//! formatting step for the Rust targets.
//! Generators never see the AST; they read only [`ir::Api`].
//!
//! [`generate_all`] is a pure function of the IR and the settings: the Rust it
//! returns is plain, one statement per line with no attempt at a layout.
//! [`format_outputs`] is the step the CLI takes next, passing every Rust
//! target through `rustfmt` so the files are what formatting the project's
//! tree writes — see [`format`].

pub mod config;
mod emit_common;
pub mod error;
pub mod format;
pub mod generate_openapi;
pub mod generate_rust;
pub mod generate_rust_client;
pub mod generate_typescript;
pub mod generate_zod;
pub mod ir;
pub mod naming;
pub mod parser;
pub mod semantic;
pub mod template;
pub mod version;

use std::path::{Path, PathBuf};

use erris::prelude::*;

use config::Config;
use ir::Api;

pub use format::format_outputs;

/// Default name of the settings file, looked up beside the SDL directory.
pub const CONFIG_FILE: &str = "rpcgen.toml";

/// Parse and validate every `*.graphql` under `dir` into a single IR.
///
/// Scalar mappings come from the SDL itself, so this needs no settings.
pub fn compile(dir: &Path) -> erris::Result<Api> {
    let files = parser::parse_dir(dir)?;
    semantic::build(&files)
}

/// Compile SDL supplied as a string. Used by tests.
pub fn compile_str(sdl: &str) -> erris::Result<Api> {
    let leaked: &'static str = Box::leak(sdl.to_string().into_boxed_str());
    let document =
        graphql_parser::parse_schema::<String>(leaked).wrap_report("failed to parse SDL")?;
    let files = vec![parser::SdlFile {
        path: std::path::PathBuf::from("<memory>"),
        document,
    }];
    semantic::build(&files)
}

/// Which of the configured outputs a [`GeneratedFile`] is: one per path of
/// [`config::Output`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    RustTypes,
    RustServer,
    RustClient,
    TypeScript,
    TypeScriptZod,
    OpenApi,
}

impl Target {
    /// Whether the target is Rust source, which [`format_outputs`] formats.
    pub fn is_rust(self) -> bool {
        matches!(self, Self::RustTypes | Self::RustServer | Self::RustClient)
    }

    /// The target's key under `[output]`, for messages.
    pub fn name(self) -> &'static str {
        match self {
            Self::RustTypes => "rust_types",
            Self::RustServer => "rust_server",
            Self::RustClient => "rust_client",
            Self::TypeScript => "typescript",
            Self::TypeScriptZod => "typescript_zod",
            Self::OpenApi => "openapi",
        }
    }
}

/// One generated file: what it is, a project-relative path and its full
/// contents.
pub struct GeneratedFile {
    pub target: Target,
    pub path: PathBuf,
    pub contents: String,
}

/// Produce every configured artifact. Deterministic for a given input: the
/// same `api` and `config` give byte-identical files.
///
/// The Rust targets come back plain — one statement per line, nothing laid
/// out — and [`format_outputs`] is the step that formats them, as the CLI
/// does before writing or comparing.
///
/// A target with no path in [`config::Output`] is skipped, so a project
/// generating only a server or only a client gets exactly what it asked for.
pub fn generate_all(api: &Api, config: &Config) -> erris::Result<Vec<GeneratedFile>> {
    let output = &config.output;
    let targets = [
        (Target::RustTypes, &output.rust_types),
        (Target::RustServer, &output.rust_server),
        (Target::RustClient, &output.rust_client),
        (Target::TypeScript, &output.typescript),
        (Target::TypeScriptZod, &output.typescript_zod),
        (Target::OpenApi, &output.openapi),
    ];

    let mut files = Vec::new();
    for (target, path) in targets {
        let Some(path) = path else {
            continue;
        };
        let contents = match target {
            Target::RustTypes => generate_rust::generate_types(api, config)?,
            Target::RustServer => generate_rust::generate(api, config)?,
            Target::RustClient => generate_rust_client::generate(api, config)?,
            Target::TypeScript => generate_typescript::generate(api),
            Target::TypeScriptZod => generate_zod::generate(api),
            Target::OpenApi => generate_openapi::generate(api, config)?,
        };
        files.push(GeneratedFile {
            target,
            path: path.clone(),
            contents,
        });
    }
    Ok(files)
}
