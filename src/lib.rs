//! GraphQL SDL -> RPC API compiler.
//!
//! Pipeline: SDL -> AST -> semantic validation -> IR -> generators.
//! Generators never see the AST; they read only [`ir::Api`].

pub mod config;
pub mod error;
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

/// One generated file: a project-relative path and its full contents.
pub struct GeneratedFile {
    pub path: PathBuf,
    pub contents: String,
}

/// Produce every configured artifact. Deterministic for a given input.
///
/// A target with no path in [`config::Output`] is skipped, so a project
/// generating only a server or only a client gets exactly what it asked for.
pub fn generate_all(api: &Api, config: &Config) -> erris::Result<Vec<GeneratedFile>> {
    let mut files = Vec::new();

    if let Some(path) = &config.output.rust_types {
        files.push(GeneratedFile {
            path: path.clone(),
            contents: generate_rust::generate_types(api, config)?,
        });
    }
    if let Some(path) = &config.output.rust_server {
        files.push(GeneratedFile {
            path: path.clone(),
            contents: generate_rust::generate(api, config)?,
        });
    }
    if let Some(path) = &config.output.rust_client {
        files.push(GeneratedFile {
            path: path.clone(),
            contents: generate_rust_client::generate(api, config)?,
        });
    }
    if let Some(path) = &config.output.typescript {
        files.push(GeneratedFile {
            path: path.clone(),
            contents: generate_typescript::generate(api),
        });
    }
    if let Some(path) = &config.output.typescript_zod {
        files.push(GeneratedFile {
            path: path.clone(),
            contents: generate_zod::generate(api),
        });
    }
    if let Some(path) = &config.output.openapi {
        files.push(GeneratedFile {
            path: path.clone(),
            contents: generate_openapi::generate(api, config)?,
        });
    }

    Ok(files)
}
