//! What the integration tests share: the fixture SDL, and scratch cargo
//! projects that compile and run generated Rust.
//!
//! `http_wire`, `raw_wire`, `oneof_wire`, `fixture_builds` and `error_groups`
//! each put generated code in front of rustc in a project of its own under this
//! crate's `target/`.
//! The arrangement is the same for all of them and is kept here:
//!
//! - A stable location. The project's target directory survives across runs,
//!   so the dependencies compile once and later runs reuse them. Only the
//!   sources, the manifest and the lockfile are rewritten.
//! - Versions pinned to this crate's lockfile. Every dependency the scratch
//!   manifest lists carries the version `Cargo.lock` has for it, and the
//!   lockfile itself is copied in before every build, so the transitive
//!   versions are this crate's too: cargo prunes what the project does not
//!   use and keeps the rest as it is. That keeps the build hermetic and in
//!   step — a version bump there lands here on the next run, and a version
//!   the lock does not carry is an error instead of a fetch.
//! - This crate's `[patch]` tables, copied. A crate patched to a local
//!   checkout or a git branch — `treat`, say — resolves to the same source in
//!   the scratch project as in this crate's tests, whether the generated code
//!   names it directly or reaches it through another crate. A relative `path`
//!   is made absolute against this crate's root, since the scratch manifest
//!   lives elsewhere.
//!
//! What differs per test — the crates and their features, the sources, the
//! `cargo test` arguments — is the caller's.
//!
//! The generated files are written formatted, by the generator's own
//! formatting step (`graphql_rpcgen::format::rustfmt`) — what the CLI writes —
//! and every project is held to the same gates before its tests run: `rustfmt
//! --check` on the generated files and `cargo clippy -- -D warnings` on the
//! crate, so the generated code is formatted and warning-free as the CLI
//! writes it. A project with nothing to run — `fixture_builds` — stops at the
//! gates. A project whose sources misuse the generated items on purpose is
//! not held to them: it must fail to build, with the error it pins
//! ([`ScratchProject::refuses`]).

#![allow(
    dead_code,
    reason = "every test binary compiles this module and uses a part of it"
)]

pub mod fixtures;

use std::error::Error;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// One crate the generated code reaches for, as the scratch manifest names it.
///
/// The version is not stated: it is whatever this crate's lockfile carries.
pub struct Dependency {
    pub name: &'static str,
    /// Off for a crate whose default features pull in what the generated code
    /// must not need, `reqwest`'s TLS stack for one.
    pub default_features: bool,
    pub features: &'static [&'static str],
}

impl Dependency {
    /// A crate with its default features and nothing more.
    pub const fn plain(name: &'static str) -> Self {
        Self::with(name, &[])
    }

    /// A crate with its default features and `features` on top.
    pub const fn with(name: &'static str, features: &'static [&'static str]) -> Self {
        Self {
            name,
            default_features: true,
            features,
        }
    }

    /// A crate with exactly `features`, its defaults off.
    pub const fn only(name: &'static str, features: &'static [&'static str]) -> Self {
        Self {
            name,
            default_features: false,
            features,
        }
    }

    /// The manifest entry for this crate at `version`.
    fn entry(&self, version: String) -> toml::Value {
        let mut entry = toml::Table::new();
        entry.insert("version".to_string(), toml::Value::String(version));
        if !self.default_features {
            entry.insert("default-features".to_string(), toml::Value::Boolean(false));
        }
        if !self.features.is_empty() {
            let features = self
                .features
                .iter()
                .map(|f| toml::Value::String((*f).to_string()))
                .collect();
            entry.insert("features".to_string(), toml::Value::Array(features));
        }
        toml::Value::Table(entry)
    }
}

/// The version of the one entry `Cargo.lock` carries for `name`.
///
/// Whatever the entry's `source` — a registry, a git repository, or none for
/// a path — the version is what the scratch manifest asks for: a `[patch]`
/// copied beside it is what decides where that version comes from.
///
/// Every entry carrying the name, not the first: the lockfile is sorted by
/// (name, version), so taking the first would silently pin the *older* half of
/// a duplicated major. Two entries mean the assumption this pinning rests on
/// is gone, and picking either one is a guess.
pub fn locked(lock: &toml::Table, name: &str) -> Result<String, Box<dyn Error>> {
    let packages = lock
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or("lockfile has no [[package]] array")?;
    let entries: Vec<&toml::Value> = packages
        .iter()
        .filter(|p| p.get("name").and_then(toml::Value::as_str) == Some(name))
        .collect();
    let [entry] = entries.as_slice() else {
        let versions: Vec<&str> = entries
            .iter()
            .filter_map(|p| p.get("version").and_then(toml::Value::as_str))
            .collect();
        return Err(match versions.as_slice() {
            [] => format!(
                "`{name}` is in the generated code's dependency set but not in Cargo.lock"
            ),
            many => format!(
                "Cargo.lock carries {} versions of `{name}` ({}); this harness pins one version per crate",
                many.len(),
                many.join(", ")
            ),
        }
        .into());
    };
    let version = entry
        .get("version")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| format!("Cargo.lock's `{name}` has no version"))?;
    Ok(version.to_string())
}

/// The `[patch]` tables of the manifest at `root`, ready for a manifest
/// elsewhere: every relative `path` joined onto `root`, every other key —
/// `git`, `rev`, `branch`, `package`, … — kept as it is.
///
/// `None` when the manifest patches nothing.
pub fn patches(manifest: &toml::Table, root: &Path) -> Result<Option<toml::Value>, Box<dyn Error>> {
    let Some(patch) = manifest.get("patch") else {
        return Ok(None);
    };
    let mut patch = patch.clone();
    let sources = patch
        .as_table_mut()
        .ok_or("`patch` in Cargo.toml is not a table")?;
    for (source, crates) in sources.iter_mut() {
        let crates = crates
            .as_table_mut()
            .ok_or_else(|| format!("`patch.{source}` in Cargo.toml is not a table"))?;
        for (name, spec) in crates.iter_mut() {
            let Some(path) = spec.get_mut("path") else {
                continue;
            };
            let relative = path
                .as_str()
                .ok_or_else(|| format!("`patch.{source}.{name}.path` is not a string"))?;
            let absolute = root.join(relative);
            let absolute = absolute.to_str().ok_or_else(|| {
                format!("`{name}`'s patch path {} is not UTF-8", absolute.display())
            })?;
            *path = toml::Value::String(absolute.to_string());
        }
    }
    Ok(Some(patch))
}

/// A manifest for package `name` of `edition` depending on `dependencies`, each at the
/// version `lock` carries, with the `[patch]` tables of `root_manifest` (the
/// manifest of the crate at `root`) copied in.
///
/// The scratch project is a workspace of its own, so cargo does not go
/// looking for one in the directories above it.
pub fn manifest(
    name: &str,
    edition: &str,
    dependencies: &[Dependency],
    root_manifest: &toml::Table,
    root: &Path,
    lock: &toml::Table,
) -> Result<String, Box<dyn Error>> {
    let mut package = toml::Table::new();
    package.insert("name".to_string(), toml::Value::String(name.to_string()));
    package.insert(
        "version".to_string(),
        toml::Value::String("0.0.0".to_string()),
    );
    package.insert(
        "edition".to_string(),
        toml::Value::String(edition.to_string()),
    );

    let mut deps = toml::Table::new();
    for dep in dependencies {
        let version = locked(lock, dep.name)?;
        deps.insert(dep.name.to_string(), dep.entry(version));
    }

    let mut manifest = toml::Table::new();
    manifest.insert("package".to_string(), toml::Value::Table(package));
    manifest.insert("dependencies".to_string(), toml::Value::Table(deps));
    let patch = patches(root_manifest, root)?;
    if let Some(patch) = patch {
        manifest.insert("patch".to_string(), patch);
    }
    manifest.insert(
        "workspace".to_string(),
        toml::Value::Table(toml::Table::new()),
    );
    Ok(toml::to_string(&manifest)?)
}

/// The cargo running this test, so the scratch build uses the same toolchain;
/// `cargo` from `PATH` when the test binary was started some other way.
fn cargo() -> OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"))
}

/// A cargo project under this crate's `target/` that generated code is
/// compiled and tested in.
pub struct ScratchProject {
    /// The package name.
    pub name: &'static str,
    /// The project's directory under `target/`.
    pub dir: &'static str,
    pub dependencies: &'static [Dependency],
    /// Arguments after `cargo test --quiet --offline`: `--lib` where the
    /// emitted doc comments are documentation, not doctests.
    pub test_args: &'static [&'static str],
    /// The target directory under `target/` to build in when it is another
    /// project's, so that projects with one dependency set compile those
    /// dependencies once between them. `None` gives the project its own:
    /// `dir` with `-target` appended. Either way it sits beside the project
    /// rather than inside, so rewriting the sources never touches the build
    /// cache.
    pub target: Option<&'static str>,
    /// The `package.edition` of the project: what `rustfmt` formats the
    /// generated files for and checks them against, and what rustc compiles
    /// the crate in.
    pub edition: &'static str,
}

impl ScratchProject {
    /// Write `sources` (paths relative to `src/`), the manifest and this
    /// crate's lockfile, then hold the project to three gates in turn, and
    /// fail with the output of the first that does not pass. The sources
    /// `generated` names are the generator's plain output, and are written
    /// through its formatting step, as the CLI writes them.
    ///
    /// - `rustfmt --check` on the files `generated` names — the formatting
    ///   step's output is what `rustfmt` keeps, so a project formatting its
    ///   tree does not rewrite it;
    /// - `cargo clippy --lib -- -D warnings` — the generated code carries no
    ///   blanket `allow`, so a warning in it is a warning in every consumer.
    ///   The driver's own module allows what a test driver does;
    /// - `cargo test` with the project's `test_args`.
    ///
    /// The lockfile is copied over whatever the last run left, so a stale one
    /// never decides a version.
    pub fn check(
        &self,
        sources: &[(&str, &str)],
        generated: &[&str],
    ) -> Result<(), Box<dyn Error>> {
        let (project, project_target) = self.write_and_lint(sources, generated)?;
        let test = Command::new(cargo())
            .args(["test", "--quiet", "--offline"])
            .args(self.test_args)
            .current_dir(&project)
            .env("CARGO_TARGET_DIR", &project_target)
            .output()?;
        passed("cargo test", &test)
    }

    /// [`Self::check`] without its last gate: the project is written and held
    /// to `rustfmt --check` and `cargo clippy --lib -- -D warnings`, and no
    /// test runs. For generated code compiled for its own sake, with no driver
    /// beside it to exercise it.
    pub fn lint(&self, sources: &[(&str, &str)], generated: &[&str]) -> Result<(), Box<dyn Error>> {
        self.write_and_lint(sources, generated).map(|_| ())
    }

    /// Write the project and hold it to the two gates every project passes —
    /// see [`Self::check`] — answering with its directory and its target
    /// directory, for whatever runs there next.
    fn write_and_lint(
        &self,
        sources: &[(&str, &str)],
        generated: &[&str],
    ) -> Result<(PathBuf, PathBuf), Box<dyn Error>> {
        let (project, project_target) = self.write(sources, generated)?;
        let src = project.join("src");

        let rustfmt = Command::new("rustfmt")
            .args(["--check", "--edition", self.edition])
            .args(generated.iter().map(|path| src.join(path)))
            .output()?;
        passed("rustfmt --check on the generated files", &rustfmt)?;

        let clippy = Command::new(cargo())
            .args([
                "clippy",
                "--quiet",
                "--offline",
                "--lib",
                "--",
                "-D",
                "warnings",
            ])
            .current_dir(&project)
            .env("CARGO_TARGET_DIR", &project_target)
            .output()?;
        passed("cargo clippy --lib -- -D warnings", &clippy)?;
        Ok((project, project_target))
    }

    /// Write the project and build it, which must fail: rustc refusing code
    /// that misuses the generated items is what the case pins. `Ok` only when
    /// `cargo check --lib` fails with exactly one error and its output carries
    /// every one of `expected`; otherwise an error with what it printed, so a
    /// build that fails for another reason does not pass for this one.
    pub fn refuses(
        &self,
        sources: &[(&str, &str)],
        generated: &[&str],
        expected: &[&str],
    ) -> Result<(), Box<dyn Error>> {
        let (project, project_target) = self.write(sources, generated)?;
        let check = Command::new(cargo())
            .args(["check", "--quiet", "--offline", "--lib"])
            .current_dir(&project)
            .env("CARGO_TARGET_DIR", &project_target)
            .output()?;
        let printed = format!(
            "{}\n{}",
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        );
        if check.status.success() {
            return Err(format!("cargo check was meant to fail, and passed:\n{printed}").into());
        }
        let missing: Vec<&&str> = expected.iter().filter(|e| !printed.contains(**e)).collect();
        if !missing.is_empty() {
            return Err(format!("cargo check failed without {missing:?}:\n{printed}").into());
        }
        // One error, the expected one: a second, unrelated one would otherwise
        // pass hidden behind it.
        if !printed.contains("due to 1 previous error") {
            return Err(format!("cargo check failed with more than one error:\n{printed}").into());
        }
        Ok(())
    }

    /// Write the sources, formatting those `generated` names, the manifest
    /// and this crate's lockfile, answering with the project's directory and
    /// its target directory.
    fn write(
        &self,
        sources: &[(&str, &str)],
        generated: &[&str],
    ) -> Result<(PathBuf, PathBuf), Box<dyn Error>> {
        let root = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;
        let project = root.join("target").join(self.dir);
        let target = match self.target {
            Some(shared) => shared.to_string(),
            None => format!("{}-target", self.dir),
        };
        let project_target = root.join("target").join(target);
        let src = project.join("src");
        // Rewritten whole, so a source an earlier run wrote and this one does
        // not cannot linger in the build.
        if src.exists() {
            std::fs::remove_dir_all(&src)?;
        }
        std::fs::create_dir_all(&src)?;
        for (path, contents) in sources {
            let file = src.join(path);
            let contents = match generated.contains(path) {
                true => graphql_rpcgen::format::rustfmt(contents, &src, self.edition)?,
                false => (*contents).to_string(),
            };
            std::fs::write(file, contents)?;
        }

        let root_manifest = std::fs::read_to_string(root.join("Cargo.toml"))?;
        let root_manifest: toml::Table = toml::from_str(&root_manifest)?;
        let lock = std::fs::read_to_string(root.join("Cargo.lock"))?;
        let parsed_lock: toml::Table = toml::from_str(&lock)?;
        let manifest = manifest(
            self.name,
            self.edition,
            self.dependencies,
            &root_manifest,
            &root,
            &parsed_lock,
        )?;
        std::fs::write(project.join("Cargo.toml"), manifest)?;
        std::fs::write(project.join("Cargo.lock"), lock)?;
        Ok((project, project_target))
    }
}

/// `Ok` for a step that exited successfully, else an error carrying what it
/// printed.
fn passed(step: &str, output: &Output) -> Result<(), Box<dyn Error>> {
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "{step} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .into())
}

/// `code` as one line: every run of whitespace a single space, and the space
/// or trailing comma a line break leaves inside brackets dropped, as is the
/// space before a chain's `.method`. What a substring assertion reads, so it
/// pins the code rather than its line breaks — `async fn f(&self, input: A) -> R;`
/// matches the plain output, which writes a signature on one line, and what
/// `rustfmt` makes of it alike; and a struct literal's fields, one per line,
/// read as one.
pub fn flat(code: &str) -> String {
    let mut out = code.split_whitespace().collect::<Vec<_>>().join(" ");
    for (from, to) in [
        ("( ", "("),
        ("[ ", "["),
        ("< ", "<"),
        (", )", ")"),
        (",)", ")"),
        (" )", ")"),
        (", ]", "]"),
        (",]", "]"),
        (" ]", "]"),
        (", >", ">"),
        (",>", ">"),
        (" >", ">"),
        (", }", " }"),
        (" .", "."),
    ] {
        out = out.replace(from, to);
    }
    out
}

/// The `operations::ALL` row of operation `id` in generated Rust, flattened:
/// `Operation { id: "x", method: "POST", .., role: None }`.
pub fn operation_row(out: &str, id: &str) -> Option<String> {
    let flat = flat(out);
    let start = flat.find(&format!("Operation {{ id: {id:?}, "))?;
    let row = flat.get(start..)?;
    let end = row.find(" }")?;
    row.get(..end.checked_add(2)?).map(str::to_string)
}
