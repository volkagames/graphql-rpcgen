//! The formatting step: every Rust target through `rustfmt`.
//!
//! The emitters write plain Rust of a fixed shape — one statement per line, a
//! signature or a `use` list on one line however long — and leave the layout
//! to `rustfmt`, run when the files are generated. A project's `rustfmt.toml`
//! is then honoured as `cargo fmt` would honour it, and the files are exactly
//! what formatting the tree writes, whatever its settings and however long the
//! SDL's names are. Predicting that layout in the emitter instead meant
//! restating `rustfmt`'s width rules for its default settings only, and
//! getting them wrong for most name lengths.
//!
//! The step is separate from [`crate::generate_all`], which stays a pure
//! function of the SDL and the settings: a caller that wants the plain output
//! — a test pinning it, a build script that formats its own way — skips this
//! step, and `[rust] format = false` skips it for the CLI.
//!
//! `rustfmt` is the one found as `$RUSTFMT`, else `rustfmt` on `PATH`, and
//! reads the source on stdin. Its working directory is the directory the file
//! is written to — or the nearest one of its ancestors that exists yet — so
//! the configuration it finds is the one `cargo fmt` would use for that file:
//! a `rustfmt.toml` or `.rustfmt.toml` there or in any directory above it.
//!
//! It is run with `--edition` set to the edition `cargo fmt` would pass for
//! that file (see [`edition_for`]): `[rust] edition` when the settings name
//! one, else the edition of the package the file is written into. The edition
//! matters beyond parsing — `rustfmt` derives its `style_edition` from it
//! unless a `rustfmt.toml` sets one, and the 2024 style edition sorts `use`
//! lists and breaks lines differently — so formatting a 2024 crate's files
//! as 2021 would leave them failing that crate's own `cargo fmt --check`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use erris::prelude::*;
use erris::report;

use crate::config::Config;
use crate::GeneratedFile;

/// The environment variable naming the `rustfmt` binary, as `cargo fmt`
/// reads it.
pub const RUSTFMT_ENV: &str = "RUSTFMT";

/// The editions `rustfmt --edition` accepts, and so the values `[rust]
/// edition` may take.
pub const EDITIONS: &[&str] = &["2015", "2018", "2021", "2024"];

/// The edition a file is formatted for when neither the settings nor a
/// `Cargo.toml` above it say. Not cargo's own default for a package that
/// names no edition, which is 2015: the generated code uses `async fn`, which
/// 2015 does not parse, so a package it compiles in names a later one.
pub const DEFAULT_EDITION: &str = "2021";

/// Format every Rust file of `files` in place with `rustfmt`, unless
/// `[rust] format = false`; every other target is left as it is.
///
/// `root` is the directory the files' paths are relative to — the CLI's
/// `--root` — and decides which `rustfmt.toml` and which `Cargo.toml` apply
/// (see the module documentation). A file `rustfmt` cannot format, or a
/// `rustfmt` that cannot be run, is an error naming the target and the way to
/// go without it.
pub fn format_outputs(
    files: &mut [GeneratedFile],
    root: &Path,
    config: &Config,
) -> erris::Result<()> {
    if !config.rust.format {
        return Ok(());
    }
    for file in files.iter_mut().filter(|file| file.target.is_rust()) {
        let dir = existing_dir(&root.join(&file.path), root);
        let edition = edition_for(&dir, config).wrap_report_with(|| {
            report!(
                "cannot tell the edition to format the {} target ({}) for; \
                 name it with `[rust] edition` in the settings file",
                file.target.name(),
                file.path.display()
            )
        })?;
        let formatted = rustfmt(&file.contents, &dir, &edition).wrap_report_with(|| {
            report!(
                "cannot format the {} target ({})",
                file.target.name(),
                file.path.display()
            )
        })?;
        file.contents = formatted;
    }
    Ok(())
}

/// The edition a file written into `dir` is formatted for: `[rust] edition`
/// when set, else the edition of the nearest `Cargo.toml` at or above `dir`
/// (see [`manifest_edition`]), else [`DEFAULT_EDITION`].
pub fn edition_for(dir: &Path, config: &Config) -> erris::Result<String> {
    if let Some(edition) = &config.rust.edition {
        return Ok(edition.clone());
    }
    // Absolute and without `..`, so walking up from it reaches the
    // filesystem root rather than stopping at the `.` a relative `--root`
    // starts from.
    let dir = std::fs::canonicalize(dir)
        .wrap_report_with(|| report!("cannot resolve {}", dir.display()))?;
    let manifest = nearest_manifest(&dir, |_| true)?;
    match manifest {
        Some((path, table)) => manifest_edition(&path, &table),
        None => Ok(DEFAULT_EDITION.to_string()),
    }
}

/// The edition `cargo` compiles the package of `manifest` (read from `path`)
/// with: its `package.edition`, or `workspace.package.edition` of its
/// workspace root when the package says `edition.workspace = true`.
///
/// A manifest with no `[package]` — a virtual workspace root, the files then
/// belonging to no package — or a package naming no edition answers
/// [`DEFAULT_EDITION`].
fn manifest_edition(path: &Path, manifest: &toml::Table) -> erris::Result<String> {
    let Some(package) = manifest.get("package").and_then(toml::Value::as_table) else {
        return Ok(DEFAULT_EDITION.to_string());
    };
    match package.get("edition") {
        None => Ok(DEFAULT_EDITION.to_string()),
        Some(toml::Value::String(edition)) => Ok(edition.clone()),
        Some(toml::Value::Table(inherit))
            if inherit.get("workspace") == Some(&toml::Value::Boolean(true)) =>
        {
            workspace_edition(path, manifest, package)
        }
        Some(other) => Err(report!(
            "{}: `package.edition` is neither an edition nor `{{ workspace = true }}`: {other}",
            path.display()
        )),
    }
}

/// `workspace.package.edition` of the workspace the package of `manifest`
/// (read from `path`) belongs to. Its root is found as cargo finds it: the
/// manifest itself when it has a `[workspace]`, the one `package.workspace`
/// points at when set, else the nearest `Cargo.toml` above with a
/// `[workspace]`.
fn workspace_edition(
    path: &Path,
    manifest: &toml::Table,
    package: &toml::Table,
) -> erris::Result<String> {
    let package_dir = path.parent().unwrap_or(Path::new("."));
    let root = if manifest.contains_key("workspace") {
        Some((path.to_path_buf(), manifest.clone()))
    } else if let Some(toml::Value::String(explicit)) = package.get("workspace") {
        let root_path = package_dir.join(explicit).join("Cargo.toml");
        let root_manifest = read_manifest(&root_path)?;
        Some((root_path, root_manifest))
    } else {
        match package_dir.parent() {
            Some(above) => nearest_manifest(above, |table| table.contains_key("workspace"))?,
            None => None,
        }
    };
    let Some((root_path, root_manifest)) = root else {
        return Err(report!(
            "{}: the package inherits its edition from a workspace, and no `Cargo.toml` \
             above it has a `[workspace]`",
            path.display()
        ));
    };
    let edition = root_manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("package"))
        .and_then(|package| package.get("edition"))
        .and_then(toml::Value::as_str);
    match edition {
        Some(edition) => Ok(edition.to_string()),
        None => Err(report!(
            "{}: the package inherits its edition from the workspace, and {} sets no \
             `workspace.package.edition`",
            path.display(),
            root_path.display()
        )),
    }
}

/// The nearest `Cargo.toml` at or above `dir` that `wanted` accepts, with its
/// path. `None` when none does.
fn nearest_manifest(
    dir: &Path,
    wanted: impl Fn(&toml::Table) -> bool,
) -> erris::Result<Option<(PathBuf, toml::Table)>> {
    for candidate in dir.ancestors() {
        let path = candidate.join("Cargo.toml");
        let is_file = path.is_file();
        if !is_file {
            continue;
        }
        let manifest = read_manifest(&path)?;
        let accepted = wanted(&manifest);
        if accepted {
            return Ok(Some((path, manifest)));
        }
    }
    Ok(None)
}

/// The `Cargo.toml` at `path`, parsed.
fn read_manifest(path: &Path) -> erris::Result<toml::Table> {
    let text = std::fs::read_to_string(path)
        .wrap_report_with(|| report!("cannot read {}", path.display()))?;
    toml::from_str(&text).wrap_report_with(|| report!("cannot parse {}", path.display()))
}

/// `source` as `rustfmt --edition <edition>` writes it, run in `dir` so that
/// the configuration found from there applies.
pub fn rustfmt(source: &str, dir: &Path, edition: &str) -> erris::Result<String> {
    let program = rustfmt_program();
    let mut child = Command::new(&program)
        .args(["--edition", edition, "--emit", "stdout"])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .wrap_report_with(|| {
            report!(
                "cannot run `{}` in {}; install rustfmt (`rustup component add rustfmt`), \
                 point ${RUSTFMT_ENV} at one, or set `[rust] format = false` in the \
                 settings file to write the Rust unformatted",
                program.to_string_lossy(),
                dir.display()
            )
        })?;

    // Written from a thread of its own, so a `rustfmt` that starts answering
    // before it has read everything cannot leave both ends waiting on a full
    // pipe.
    let stdin = child
        .stdin
        .take()
        .ok_or_report("rustfmt's stdin is not piped")?;
    let input = source.to_string();
    let writer = std::thread::spawn(move || {
        let mut stdin = stdin;
        stdin.write_all(input.as_bytes())
    });
    let output = child
        .wait_with_output()
        .wrap_report("rustfmt did not finish")?;
    let written = writer
        .join()
        .map_err(|_| report!("writing to rustfmt panicked"))?;
    // A write cut short is `rustfmt` having exited early, which its status
    // below explains better than the broken pipe would.
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        // The rustup proxy runs and fails when the toolchain it picked has no
        // rustfmt, which is a missing install rather than a bad config or bad
        // source; rustup says so in these words.
        let missing_component = stderr.contains("is not installed for");
        if missing_component {
            return Err(report!(
                "`{}` is rustup's proxy, and the toolchain it picked has no rustfmt \
                 component: install it (`rustup component add rustfmt`), point \
                 ${RUSTFMT_ENV} at another rustfmt, or set `[rust] format = false` in the \
                 settings file to write the Rust unformatted: {stderr}",
                program.to_string_lossy()
            ));
        }
        return Err(report!(
            "`{}` failed ({}) — a `rustfmt.toml` it cannot read, or generated Rust that \
             does not parse, as a project template can write: {stderr}",
            program.to_string_lossy(),
            output.status
        ));
    }
    written.wrap_report("cannot write the source to rustfmt")?;
    String::from_utf8(output.stdout).wrap_report("rustfmt wrote non-UTF-8")
}

/// The `rustfmt` to run: `$RUSTFMT` when set and not empty, else `rustfmt`
/// from `PATH`.
fn rustfmt_program() -> std::ffi::OsString {
    match std::env::var_os(RUSTFMT_ENV) {
        Some(program) if !program.is_empty() => program,
        _ => "rustfmt".into(),
    }
}

/// The directory `file` sits in, or the nearest of its ancestors that exists —
/// a file generated for the first time may be headed for a directory nobody
/// has created yet. `fallback` when none does.
fn existing_dir(file: &Path, fallback: &Path) -> PathBuf {
    let mut dir = file.parent();
    while let Some(candidate) = dir {
        let exists = candidate.is_dir();
        if exists {
            return candidate.to_path_buf();
        }
        dir = candidate.parent();
    }
    fallback.to_path_buf()
}

#[cfg(test)]
#[allow(
    clippy::panic_in_result_fn,
    reason = "a test asserts, and returns `Result` only to use `?` on its setup"
)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// A fresh directory under the system temp dir, named per test.
    fn scratch(name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let dir =
            std::env::temp_dir().join(format!("rpcgen-edition-{}-{name}", std::process::id()));
        let stale = dir.exists();
        if stale {
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::create_dir_all(dir.join("crate").join("src"))?;
        Ok(dir)
    }

    fn edition_of(dir: &Path) -> erris::Result<String> {
        edition_for(dir, &Config::default())
    }

    #[test]
    fn the_package_edition_applies() -> TestResult {
        let dir = scratch("package")?;
        std::fs::write(
            dir.join("crate").join("Cargo.toml"),
            "[package]\nname = \"x\"\nedition = \"2024\"\n",
        )?;
        assert_eq!(edition_of(&dir.join("crate").join("src"))?, "2024");
        Ok(())
    }

    #[test]
    fn an_inherited_edition_is_the_workspaces() -> TestResult {
        let dir = scratch("inherited")?;
        std::fs::write(
            dir.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crate\"]\n\n[workspace.package]\nedition = \"2024\"\n",
        )?;
        std::fs::write(
            dir.join("crate").join("Cargo.toml"),
            "[package]\nname = \"x\"\nedition.workspace = true\n",
        )?;
        assert_eq!(edition_of(&dir.join("crate").join("src"))?, "2024");
        Ok(())
    }

    #[test]
    fn an_inherited_edition_the_workspace_does_not_set_is_an_error() -> TestResult {
        let dir = scratch("inherited-unset")?;
        std::fs::write(dir.join("Cargo.toml"), "[workspace]\n")?;
        std::fs::write(
            dir.join("crate").join("Cargo.toml"),
            "[package]\nname = \"x\"\nedition = { workspace = true }\n",
        )?;
        let err = crate::error::describe(
            &edition_of(&dir.join("crate").join("src"))
                .err()
                .ok_or("refused")?,
        );
        assert!(err.contains("workspace.package.edition"), "{err}");
        Ok(())
    }

    #[test]
    fn a_package_naming_no_edition_answers_the_default() -> TestResult {
        let dir = scratch("default")?;
        std::fs::write(
            dir.join("crate").join("Cargo.toml"),
            "[package]\nname = \"x\"\n",
        )?;
        assert_eq!(edition_of(&dir.join("crate").join("src"))?, DEFAULT_EDITION);
        Ok(())
    }

    #[test]
    fn the_settings_override_the_manifest() -> TestResult {
        let dir = scratch("override")?;
        std::fs::write(
            dir.join("crate").join("Cargo.toml"),
            "[package]\nname = \"x\"\nedition = \"2024\"\n",
        )?;
        let mut config = Config::default();
        config.rust.edition = Some("2018".to_string());
        assert_eq!(
            edition_for(&dir.join("crate").join("src"), &config)?,
            "2018"
        );
        Ok(())
    }
}
