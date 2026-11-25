//! Golden files: every target's full output for every fixture SDL.
//!
//! `snapshots.rs` pins the constructs that are easy to get wrong with targeted
//! substring assertions, and the `*_wire` tests pin behaviour by compiling and
//! running what is emitted. Neither shows the rest of a file, so a change that
//! moves, rewrites or drops code nobody asserted on goes unseen. This test
//! keeps the whole of each output under `tests/snapshots/`, so any change to
//! what the generator writes surfaces as a diff in review.
//!
//! The fixtures are the SDL the other tests already run, read from
//! `tests/fixtures/` by both, so there is one copy of each; the list of them
//! is `common::fixtures`, checked here against the directory. Every fixture
//! gets every target, in the layout the wire tests compile — the server and
//! the client reaching for a shared types module through `types_path` — and
//! the server once more as a single file, its types inline, which takes a
//! different path through the emitter.
//!
//! The Rust is the emitters' plain output, as `generate_all` returns it, not
//! what the formatting step makes of it: that is `rustfmt`'s, and depends on
//! its version and on the configuration it finds, while the plain output
//! depends on the generator alone. `cli` checks that the formatting step is
//! what the binary writes; the `*_wire` and `fixture_builds` tests compile the
//! formatted files.
//!
//! A change that is meant: `INSTA_UPDATE=always cargo test --test golden`
//! rewrites the files, and the diff is the review. The test stops at its first
//! mismatching file; `INSTA_FORCE_PASS=1` lists every one.

#![allow(
    clippy::panic_in_result_fn,
    reason = "a test asserts, and returns `Result` only to use `?` on its setup"
)]

/// What a test body answers with: the first failure ends it, carrying the
/// message of whatever actually went wrong rather than a fixed one.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

mod common;

use common::fixtures::{Fixture, FIXTURES};
use graphql_rpcgen::config::Config;
use graphql_rpcgen::{
    generate_openapi, generate_rust, generate_rust_client, generate_typescript, generate_zod,
};

/// Snapshot every target of one fixture, each in its own file:
/// `tests/snapshots/golden__<fixture>__<target>.snap`.
///
/// The fixture's own config lines are read as `rpcgen.toml` would be, then
/// given the split layout `http_wire` and `raw_wire` compile: the server and
/// the client name their types through `crate::types` rather than inlining
/// them. `rust_server_single_file` is the same server with the layout taken
/// away again.
fn every_target(fixture: &Fixture) -> TestResult {
    let api = graphql_rpcgen::compile_str(fixture.sdl)?;
    let single_file: Config = toml::from_str(fixture.config)?;
    let mut split = single_file.clone();
    split.rust.types_path = Some("crate::types".to_string());
    split.rust_client.types_path = Some("crate::types".to_string());
    let outputs = [
        ("rust_types", generate_rust::generate_types(&api, &split)?),
        ("rust_server", generate_rust::generate(&api, &split)?),
        ("rust_client", generate_rust_client::generate(&api, &split)?),
        ("typescript", generate_typescript::generate(&api)),
        ("zod", generate_zod::generate(&api)),
        ("openapi", generate_openapi::generate(&api, &split)?),
        (
            "rust_server_single_file",
            generate_rust::generate(&api, &single_file)?,
        ),
    ];
    for (target, output) in outputs {
        insta::assert_snapshot!(format!("{}__{target}", fixture.name), output);
    }
    Ok(())
}

#[test]
fn every_fixture_every_target() -> TestResult {
    for fixture in FIXTURES {
        every_target(fixture)?;
    }
    Ok(())
}

/// The fixture list is the directory: every `*.graphql` under
/// `tests/fixtures/` is listed, nothing else is, and each entry's SDL is its
/// own file's.
#[test]
fn the_fixture_list_is_the_fixture_directory() -> TestResult {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    let mut on_disk = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        let is_sdl = path.extension().is_some_and(|ext| ext == "graphql");
        if !is_sdl {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| format!("{} has no UTF-8 stem", path.display()))?;
        on_disk.push(stem.to_string());
    }
    on_disk.sort();

    let mut listed: Vec<String> = FIXTURES.iter().map(|f| f.name.to_string()).collect();
    listed.sort();
    assert_eq!(
        listed, on_disk,
        "common::fixtures::FIXTURES must list every SDL file under tests/fixtures/"
    );

    for fixture in FIXTURES {
        let sdl = std::fs::read_to_string(dir.join(format!("{}.graphql", fixture.name)))?;
        assert!(
            sdl == fixture.sdl,
            "`{}` must carry tests/fixtures/{}.graphql",
            fixture.name,
            fixture.name
        );
    }
    Ok(())
}
