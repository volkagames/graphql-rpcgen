//! The Rust targets of the fixtures no wire test compiles, compiled.
//!
//! `http_wire`, `raw_wire` and `oneof_wire` each build the Rust of their own
//! fixture and run it. `snapshots`, `snapshots_http` and `mcp_version` are
//! only snapshotted by `golden`, which pins the text but not that rustc
//! accepts it — and they hold constructs the others do not: newtype scalars
//! with constraints, recursive types, versioned paths, MCP flags. Each is
//! built here as types, server and client in one crate, in the layout the wire
//! tests use, and held to `rustfmt --check` and `cargo clippy -D warnings`.
//! Nothing runs: there is no driver, only the question whether the output
//! compiles warning-free.
//!
//! An API whose types are spelt like the runtime's items is built the same way,
//! in both layouts, to hold the generated code to naming those items by path —
//! once in a 2021-edition crate, once in a 2024-edition one, formatted and
//! checked for that edition.
//!
//! The five projects share `raw_wire`'s target directory and its dependency
//! set, the widest of the wire tests', so the dependencies compile once for
//! all of them and each project here compiles only its own crate.

#![allow(
    clippy::panic_in_result_fn,
    reason = "a test asserts, and returns `Result` only to use `?` on its setup"
)]

/// What a test body answers with: the first failure ends it, carrying the
/// message of whatever actually went wrong rather than a fixed one.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

mod common;

use common::fixtures::FIXTURES;
use common::{Dependency, ScratchProject};
use graphql_rpcgen::config::Config;
use graphql_rpcgen::{generate_rust, generate_rust_client};

/// `raw_wire`'s dependencies, so the shared target directory holds one build
/// of each. `reqwest` and `tokio` are that test's driver's and unused here;
/// an unused dependency costs nothing once built.
const DEPENDENCIES: &[Dependency] = &[
    Dependency::plain("async-trait"),
    Dependency::plain("axum"),
    Dependency::with("chrono", &["serde"]),
    Dependency::with("derive_more", &["from", "into", "display", "as_ref"]),
    Dependency::plain("error_set"),
    Dependency::only("reqwest", &["json"]),
    Dependency::with("serde", &["derive"]),
    Dependency::plain("serde_json"),
    Dependency::with("tokio", &["macros", "net", "rt"]),
    Dependency::with(
        "treat",
        &[
            "rpc-client",
            "rpc-server",
            "rpc-status-header",
            "rpc-stream",
        ],
    ),
    Dependency::with("uuid", &["serde"]),
    Dependency::with("validator", &["derive"]),
];

/// The scratch project of one fixture: `target/fixture-<name>-test`, building
/// in `raw_wire`'s target directory.
fn project(name: &'static str, dir: &'static str, edition: &'static str) -> ScratchProject {
    ScratchProject {
        name,
        dir,
        dependencies: DEPENDENCIES,
        test_args: &[],
        target: Some("raw-wire-test-target"),
        edition,
    }
}

/// Generate the fixture `name`'s three Rust targets with its own config, the
/// server and the client reaching for `crate::types` as `golden`'s split
/// layout does, and hold them to the gates.
fn builds(name: &str, project: &ScratchProject) -> TestResult {
    let fixture = FIXTURES
        .iter()
        .find(|f| f.name == name)
        .ok_or_else(|| format!("no fixture named `{name}`"))?;
    let api = graphql_rpcgen::compile_str(fixture.sdl)?;
    let mut config: Config = toml::from_str(fixture.config)?;
    config.rust.types_path = Some("crate::types".to_string());
    config.rust_client.types_path = Some("crate::types".to_string());
    let types = generate_rust::generate_types(&api, &config)?;
    let server = generate_rust::generate(&api, &config)?;
    let client = generate_rust_client::generate(&api, &config)?;
    let lib = "pub mod client;\npub mod server;\npub mod types;\n";
    project.lint(
        &[
            ("types.rs", &types),
            ("server.rs", &server),
            ("client.rs", &client),
            ("lib.rs", lib),
        ],
        &["types.rs", "server.rs", "client.rs"],
    )
}

#[test]
fn the_snapshots_fixture_builds() -> TestResult {
    builds(
        "snapshots",
        &project("fixture_snapshots", "fixture-snapshots-test", "2021"),
    )
}

#[test]
fn the_snapshots_http_fixture_builds() -> TestResult {
    builds(
        "snapshots_http",
        &project(
            "fixture_snapshots_http",
            "fixture-snapshots-http-test",
            "2021",
        ),
    )
}

#[test]
fn the_mcp_version_fixture_builds() -> TestResult {
    builds(
        "mcp_version",
        &project("fixture_mcp_version", "fixture-mcp-version-test", "2021"),
    )
}

/// SDL whose types are spelt like the items the generated code names from
/// its dependencies — `Request`, `State`, `Ctx`, `Refusal`, `RawBody`,
/// `RawRequest`, `EventStream`, `RpcClient`, `NoInput`, `ClientError`,
/// `ApiErrors`, `Router`, `Response`, `Serialize`, `WireEnum`, `Validate`, … —
/// each used where a misresolved name would not type-check. Not a fixture:
/// no golden file pins it, and `ts_check` type-checks its TypeScript, whose
/// types are spelt like the globals the client runtime uses.
const RUNTIME_NAMES_SDL: &str = include_str!("sdl/runtime_names.graphql");

/// An SDL type spelt like a runtime item is the only item of that name in the
/// generated modules, and means the SDL's type wherever they write it: the
/// generated code names the runtime's items by their absolute paths, and
/// re-exports none an SDL type is spelt like. Built in both layouts, since
/// they fail differently — a duplicate item where the types are declared
/// beside the re-exports, a silently shadowed type where a module
/// glob-imports them.
#[test]
fn types_spelt_like_runtime_items_build() -> TestResult {
    runtime_names_build(&project(
        "fixture_runtime_names",
        "fixture-runtime-names-test",
        "2021",
    ))
}

/// The same API in a 2024-edition crate: formatted for that edition — whose
/// style sorts and breaks differently — and held to `rustfmt --edition 2024
/// --check`, and compiled where `gen` is a reserved word, which the SDL's
/// field, input field and operation named `gen` reach as `r#gen`.
#[test]
fn the_output_builds_in_the_2024_edition() -> TestResult {
    runtime_names_build(&project(
        "fixture_edition_2024",
        "fixture-edition-2024-test",
        "2024",
    ))
}

/// [`RUNTIME_NAMES_SDL`] generated in both layouts — types, server and client
/// apart, and the server alone in one file — and held to the gates in
/// `project`.
fn runtime_names_build(project: &ScratchProject) -> TestResult {
    let api = graphql_rpcgen::compile_str(RUNTIME_NAMES_SDL)?;
    let single_file = Config::default();
    let mut config = Config::default();
    config.rust.types_path = Some("crate::types".to_string());
    config.rust_client.types_path = Some("crate::types".to_string());
    let types = generate_rust::generate_types(&api, &config)?;
    let server = generate_rust::generate(&api, &config)?;
    let client = generate_rust_client::generate(&api, &config)?;
    let single = generate_rust::generate(&api, &single_file)?;
    let lib = "pub mod client;\npub mod server;\npub mod single;\npub mod types;\n";
    project.lint(
        &[
            ("types.rs", &types),
            ("server.rs", &server),
            ("client.rs", &client),
            ("single.rs", &single),
            ("lib.rs", lib),
        ],
        &["types.rs", "server.rs", "client.rs", "single.rs"],
    )
}
