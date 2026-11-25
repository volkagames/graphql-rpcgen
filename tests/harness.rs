//! The scratch-project harness the wire tests share, on manifests and
//! lockfiles of its own.
//!
//! The wire tests build the scratch manifest from this crate's real
//! `Cargo.toml` and `Cargo.lock` on every run, but those patch nothing until
//! someone points `treat` at a local checkout. What the harness does with a
//! `[patch]` — a path made absolute, a git source kept — is pinned here on
//! synthetic ones instead, alongside the version pinning.

#![allow(
    clippy::panic_in_result_fn,
    reason = "a test asserts, and returns `Result` only to use `?` on its setup"
)]

/// What a test body answers with: the first failure ends it, carrying the
/// message of whatever actually went wrong rather than a fixed one.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

mod common;

use common::Dependency;
use std::path::Path;

/// A lockfile with a registry crate, a path package without a `source`, a git
/// package, the root package, and a crate at two versions.
const LOCK: &str = r#"
version = 4

[[package]]
name = "serde"
version = "1.0.228"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "treat"
version = "0.25.1"

[[package]]
name = "treat-core"
version = "0.25.1"
source = "git+https://github.com/volkagames/treat?branch=main#0123456789abcdef"

[[package]]
name = "graphql-rpcgen"
version = "0.15.0"

[[package]]
name = "twice"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "twice"
version = "2.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;

/// A root manifest patching `treat` to a relative path (with a quote in it,
/// so the written manifest has to escape it), `treat-core` to a git branch,
/// and a second source to an absolute path.
const PATCHED: &str = r#"
[package]
name = "graphql-rpcgen"
version = "0.15.0"

[patch.crates-io]
treat = { path = "../treat \"local\"/crates/treat" }
treat-core = { git = "https://github.com/volkagames/treat", branch = "main" }

[patch."https://github.com/example/other"]
other = { path = "/abs/other", package = "other-renamed" }
"#;

/// A root manifest with no `[patch]` at all.
const UNPATCHED: &str = r#"
[package]
name = "graphql-rpcgen"
version = "0.15.0"
"#;

const ROOT: &str = "/work/graphql-rpcgen";

/// The scratch manifest for `dependencies` over `root_manifest`, parsed back.
fn scratch(root_manifest: &str, dependencies: &[Dependency]) -> TestResult<toml::Table> {
    let lock: toml::Table = toml::from_str(LOCK)?;
    let root_manifest: toml::Table = toml::from_str(root_manifest)?;
    let written = common::manifest(
        "scratch",
        "2021",
        dependencies,
        &root_manifest,
        Path::new(ROOT),
        &lock,
    )?;
    Ok(toml::from_str(&written)?)
}

/// `manifest[key1][key2]…`, or an error naming the path that is not there.
fn at<'a>(manifest: &'a toml::Table, keys: &[&str]) -> TestResult<&'a toml::Value> {
    let (first, rest) = keys.split_first().ok_or("at least one key")?;
    let mut value = manifest
        .get(*first)
        .ok_or_else(|| format!("no `{first}`"))?;
    for key in rest {
        value = value
            .get(*key)
            .ok_or_else(|| format!("no `{key}` in {}", keys.join(".")))?;
    }
    Ok(value)
}

fn string<'a>(manifest: &'a toml::Table, keys: &[&str]) -> TestResult<&'a str> {
    at(manifest, keys)?
        .as_str()
        .ok_or_else(|| format!("{} is not a string", keys.join(".")).into())
}

/// Every dependency carries the version the lockfile has for it, whatever its
/// `source` — registry, path or git — with its features as given.
#[test]
fn every_dependency_carries_its_locked_version() -> TestResult {
    let manifest = scratch(
        UNPATCHED,
        &[
            Dependency::with("serde", &["derive"]),
            Dependency::only("treat", &["axum", "rpc-status-header"]),
            Dependency::plain("treat-core"),
        ],
    )?;
    assert_eq!(string(&manifest, &["package", "name"])?, "scratch");
    assert_eq!(
        string(&manifest, &["dependencies", "serde", "version"])?,
        "1.0.228"
    );
    assert_eq!(
        string(&manifest, &["dependencies", "treat", "version"])?,
        "0.25.1"
    );
    assert_eq!(
        string(&manifest, &["dependencies", "treat-core", "version"])?,
        "0.25.1"
    );

    let serde = at(&manifest, &["dependencies", "serde"])?;
    assert_eq!(
        serde.get("features"),
        Some(&toml::Value::Array(vec![toml::Value::String(
            "derive".to_string()
        )]))
    );
    assert_eq!(serde.get("default-features"), None);
    let treat = at(&manifest, &["dependencies", "treat"])?;
    assert_eq!(
        treat.get("default-features"),
        Some(&toml::Value::Boolean(false))
    );
    assert_eq!(
        treat.get("features"),
        Some(&toml::Value::Array(vec![
            toml::Value::String("axum".to_string()),
            toml::Value::String("rpc-status-header".to_string()),
        ]))
    );

    // A workspace of its own, so cargo does not adopt it into this one.
    assert!(at(&manifest, &["workspace"])?.is_table());
    Ok(())
}

/// A relative patch path is joined onto the root, quote and all; an absolute
/// one and the keys beside it stay as they are.
#[test]
fn a_path_patch_is_made_absolute() -> TestResult {
    let manifest = scratch(PATCHED, &[Dependency::plain("treat")])?;
    assert_eq!(
        string(&manifest, &["patch", "crates-io", "treat", "path"])?,
        Path::new(ROOT)
            .join("../treat \"local\"/crates/treat")
            .to_str()
            .ok_or("UTF-8")?
    );
    let other = "https://github.com/example/other";
    assert_eq!(
        string(&manifest, &["patch", other, "other", "path"])?,
        "/abs/other"
    );
    assert_eq!(
        string(&manifest, &["patch", other, "other", "package"])?,
        "other-renamed"
    );
    Ok(())
}

/// A git patch is copied as written: the scratch project resolves it the same
/// way this crate does.
#[test]
fn a_git_patch_is_kept() -> TestResult {
    let manifest = scratch(PATCHED, &[Dependency::plain("treat")])?;
    let git = at(&manifest, &["patch", "crates-io", "treat-core"])?;
    let mut expected = toml::Table::new();
    expected.insert(
        "git".to_string(),
        toml::Value::String("https://github.com/volkagames/treat".to_string()),
    );
    expected.insert(
        "branch".to_string(),
        toml::Value::String("main".to_string()),
    );
    assert_eq!(git, &toml::Value::Table(expected));
    Ok(())
}

/// No `[patch]` in the root manifest, none in the scratch one.
#[test]
fn no_patch_writes_no_patch_table() -> TestResult {
    let manifest = scratch(UNPATCHED, &[Dependency::plain("serde")])?;
    assert_eq!(manifest.get("patch"), None);
    Ok(())
}

#[test]
fn a_missing_or_duplicated_crate_is_an_error() -> TestResult {
    let missing = scratch(UNPATCHED, &[Dependency::plain("absent")]).map_err(|e| e.to_string());
    assert!(
        matches!(&missing, Err(e) if e.contains("not in Cargo.lock")),
        "{missing:?}"
    );
    let twice = scratch(UNPATCHED, &[Dependency::plain("twice")]).map_err(|e| e.to_string());
    assert!(
        matches!(&twice, Err(e) if e.contains("2 versions of `twice` (1.0.0, 2.0.0)")),
        "{twice:?}"
    );
    Ok(())
}
