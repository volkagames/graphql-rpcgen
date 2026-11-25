//! The `graphql-rpcgen` binary itself.
//!
//! Every other suite calls the library. Nothing put the CLI in front of a
//! directory, so its documented contract — `check` regenerates in memory and
//! exits non-zero when the checked-in output is stale — held only by reading.
//! That contract is what a consumer's CI hangs on, and a swapped branch or a
//! mis-parsed flag would break it without failing a single test.

#![allow(
    clippy::panic_in_result_fn,
    reason = "a test asserts, and returns `Result` only to use `?` on its setup"
)]

/// What a test body answers with: the first failure ends it, carrying the
/// message of whatever actually went wrong rather than a fixed one.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The binary under test, built by cargo before this suite runs.
const BIN: &str = env!("CARGO_BIN_EXE_graphql-rpcgen");

/// Exit code `main` returns through `ExitCode::FAILURE`.
const FAILURE: i32 = 1;

/// Minimal SDL: one service, and the error registry every SDL must carry.
const SDL: &str = r#"
enum ErrorCode { invalid_body internal_error not_found }

input GetInput { id: ID! }
type GetOutput { id: ID!, name: String }

type Query @service {
  get(input: GetInput!): GetOutput! @rpc(path: "/get") @query
}
"#;

/// A project tree: SDL in `api/`, settings at the root, output under `gen/`.
///
/// Named per test so the suite's own parallelism cannot make one case observe
/// another's files.
fn project(name: &str) -> TestResult<PathBuf> {
    let root = unconfigured_project(name)?;
    // The gate wants the running generator's version, and cargo already knows
    // it: writing a literal here would need editing at every release.
    std::fs::write(
        root.join("rpcgen.toml"),
        format!(
            "rpcgen_version = \"{}\"\n\n[output]\nrust_types = \"gen/types.rs\"\nopenapi = \"gen/openapi.json\"\n",
            env!("CARGO_PKG_VERSION")
        ),
    )?;
    Ok(root)
}

/// The same tree without the settings file, which is the state every project
/// starts in and the one the no-output cases need.
fn unconfigured_project(name: &str) -> TestResult<PathBuf> {
    let root = std::env::temp_dir().join(format!("rpcgen-cli-{}-{name}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir_all(root.join("api"))?;
    std::fs::write(root.join("api").join("schema.graphql"), SDL)?;
    Ok(root)
}

fn run(root: &Path, args: &[&str]) -> TestResult<Output> {
    Ok(Command::new(BIN).args(args).current_dir(root).output()?)
}

/// `generate` in a project, asserting it succeeded so a later `check` is
/// testing staleness rather than a failure to produce anything.
fn generate(root: &Path) -> TestResult<Output> {
    let out = run(root, &["generate"])?;
    assert!(
        out.status.success(),
        "generate must succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(out)
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn generate_writes_every_configured_target() -> TestResult {
    let root = project("generate")?;
    let out = generate(&root)?;

    assert!(root.join("gen").join("types.rs").exists());
    assert!(root.join("gen").join("openapi.json").exists());
    // The count is the contract `[output]` states: two paths, two files.
    assert!(
        stdout(&out).contains("generated 2 file(s)"),
        "{}",
        stdout(&out)
    );
    Ok(())
}

#[test]
fn check_succeeds_on_freshly_generated_output() -> TestResult {
    let root = project("check-fresh")?;
    generate(&root)?;

    let out = run(&root, &["check"])?;
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("up to date"));
    Ok(())
}

#[test]
fn check_fails_and_names_the_file_when_output_drifts() -> TestResult {
    let root = project("check-drift")?;
    generate(&root)?;
    // What a hand edit to generated code looks like: same file, other bytes.
    std::fs::write(root.join("gen").join("types.rs"), "// edited by hand\n")?;

    let out = run(&root, &["check"])?;
    assert_eq!(out.status.code(), Some(FAILURE), "{}", stdout(&out));
    assert!(stderr(&out).contains("stale"));
    // Naming the file is the point: a consumer's CI log has to say what to
    // regenerate, not merely that something is wrong.
    assert!(stderr(&out).contains("gen/types.rs"), "{}", stderr(&out));
    assert!(
        !stderr(&out).contains("gen/openapi.json"),
        "{}",
        stderr(&out)
    );
    Ok(())
}

#[test]
fn check_treats_a_missing_file_as_stale_rather_than_an_error() -> TestResult {
    let root = project("check-missing")?;
    generate(&root)?;
    std::fs::remove_file(root.join("gen").join("openapi.json"))?;

    let out = run(&root, &["check"])?;
    assert_eq!(out.status.code(), Some(FAILURE), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("gen/openapi.json"),
        "{}",
        stderr(&out)
    );
    // A missing file is drift, not a read failure: the message must be the
    // stale one, which tells the reader to regenerate.
    assert!(stderr(&out).contains("stale"), "{}", stderr(&out));
    Ok(())
}

#[test]
fn check_never_writes() -> TestResult {
    let root = project("check-readonly")?;
    generate(&root)?;
    std::fs::remove_file(root.join("gen").join("types.rs"))?;

    run(&root, &["check"])?;
    assert!(
        !root.join("gen").join("types.rs").exists(),
        "check must report staleness, not repair it"
    );
    Ok(())
}

#[test]
fn a_second_generate_leaves_unchanged_files_alone() -> TestResult {
    let root = project("idempotent")?;
    generate(&root)?;

    // `write_if_changed` announces only the files it actually wrote, so an
    // empty announcement is the observable form of "nothing was touched" —
    // which is what keeps mtime-watching build systems quiet.
    let second = generate(&root)?;
    assert!(!stdout(&second).contains("wrote"), "{}", stdout(&second));
    Ok(())
}

#[test]
fn output_paths_resolve_against_root_not_the_working_directory() -> TestResult {
    let root = project("root-flag")?;
    let elsewhere = std::env::temp_dir();

    let out = Command::new(BIN)
        .args(["generate", "--root"])
        .arg(&root)
        .arg("--api")
        .arg(root.join("api"))
        .current_dir(&elsewhere)
        .output()?;

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(root.join("gen").join("types.rs").exists());
    Ok(())
}

#[test]
fn an_explicit_config_that_does_not_exist_is_an_error() -> TestResult {
    let root = project("missing-config")?;

    // The default `rpcgen.toml` is optional, but one named on the command line
    // was named deliberately: silently falling back would generate a different
    // set of files than the caller asked for.
    let out = run(&root, &["generate", "--config", "no-such.toml"])?;
    assert_eq!(out.status.code(), Some(FAILURE), "{}", stdout(&out));
    Ok(())
}

#[test]
fn an_unknown_command_fails_with_usage() -> TestResult {
    let root = project("unknown-command")?;

    let out = run(&root, &["regenerate"])?;
    assert_eq!(out.status.code(), Some(FAILURE));
    assert!(
        stderr(&out).contains("unknown command `regenerate`"),
        "{}",
        stderr(&out)
    );
    assert!(stderr(&out).contains("USAGE:"));
    Ok(())
}

#[test]
fn no_command_fails_with_usage() -> TestResult {
    let root = project("no-command")?;

    let out = run(&root, &[])?;
    assert_eq!(out.status.code(), Some(FAILURE));
    assert!(stderr(&out).contains("missing command"), "{}", stderr(&out));
    Ok(())
}

#[test]
fn help_succeeds_and_prints_usage_on_stdout() -> TestResult {
    let root = project("help")?;

    // Asked-for help is not an error: it exits zero and goes to stdout, so
    // `graphql-rpcgen --help | less` works and a wrapper script does not abort.
    for flag in ["-h", "--help", "help"] {
        let out = run(&root, &[flag])?;
        assert!(out.status.success(), "`{flag}` must exit zero");
        assert!(
            stdout(&out).contains("USAGE:"),
            "`{flag}`: {}",
            stdout(&out)
        );
    }
    Ok(())
}

#[test]
fn version_succeeds_and_prints_the_package_version_on_stdout() -> TestResult {
    let root = project("version")?;
    let expected = format!("graphql-rpcgen {}\n", env!("CARGO_PKG_VERSION"));

    for args in [
        &["-V"][..],
        &["--version"],
        &["version"],
        &["generate", "--version"],
    ] {
        let out = run(&root, args)?;
        assert!(out.status.success(), "`{args:?}` must exit zero");
        assert_eq!(stdout(&out), expected, "`{args:?}`");
    }
    Ok(())
}

/// The failure this whole suite exists for, in the form it actually took: with
/// no settings file the defaults name no target, so `check` had nothing to
/// compare, said the files were up to date and exited zero. A consumer's CI
/// hangs on that exit code, and a project that never wrote `rpcgen.toml` — or
/// one whose `--root` points a directory too high — would pass forever.
#[test]
fn check_fails_when_no_settings_file_configures_an_output() -> TestResult {
    let root = unconfigured_project("no-settings")?;

    let out = run(&root, &["check"])?;
    assert_eq!(out.status.code(), Some(FAILURE), "{}", stdout(&out));
    assert!(
        !stdout(&out).contains("up to date"),
        "must not claim to have checked anything: {}",
        stdout(&out)
    );
    let err = stderr(&out);
    assert!(err.contains("no outputs configured"), "{err}");
    // The message has to name the file that was missing, since that is the fix.
    assert!(err.contains("rpcgen.toml"), "{err}");
    Ok(())
}

/// A settings file that parses but names no target is the same hole reached
/// the other way, and the version gate does not close it.
#[test]
fn check_fails_when_the_settings_file_names_no_output() -> TestResult {
    let root = unconfigured_project("empty-output")?;
    std::fs::write(
        root.join("rpcgen.toml"),
        format!("rpcgen_version = \"{}\"\n", env!("CARGO_PKG_VERSION")),
    )?;

    let out = run(&root, &["check"])?;
    assert_eq!(out.status.code(), Some(FAILURE), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("no outputs configured"),
        "{}",
        stderr(&out)
    );
    Ok(())
}

/// `generate` answers the same way: writing nothing is not a run that worked,
/// and reporting it differently from `check` would mean one of the two is lying.
#[test]
fn generate_fails_when_nothing_is_configured() -> TestResult {
    let root = unconfigured_project("no-settings-generate")?;

    let out = run(&root, &["generate"])?;
    assert_eq!(out.status.code(), Some(FAILURE), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("nothing was generated"),
        "{}",
        stderr(&out)
    );
    Ok(())
}

/// A project generating the three Rust targets and OpenAPI, with `extra`
/// appended to its settings: `[rust] format = false`, say.
fn rust_project(name: &str, extra: &str) -> TestResult<PathBuf> {
    let root = unconfigured_project(name)?;
    std::fs::write(
        root.join("rpcgen.toml"),
        format!(
            "rpcgen_version = \"{}\"\n\n[output]\nrust_types = \"gen/types.rs\"\n\
             rust_server = \"gen/server.rs\"\nrust_client = \"gen/client.rs\"\n\
             openapi = \"gen/openapi.json\"\n\n[rust]\ntypes_path = \"crate::types\"\n{extra}",
            env!("CARGO_PKG_VERSION")
        ),
    )?;
    Ok(root)
}

/// What the library generates for the project at `root`, before the
/// formatting step: each file's path and contents.
fn plain_outputs(root: &Path) -> TestResult<Vec<(PathBuf, String, bool)>> {
    let config = graphql_rpcgen::config::Config::from_toml_file(&root.join("rpcgen.toml"))?;
    let api = graphql_rpcgen::compile(&root.join("api"))?;
    let files = graphql_rpcgen::generate_all(&api, &config)?;
    Ok(files
        .into_iter()
        .map(|file| (file.path, file.contents, file.target.is_rust()))
        .collect())
}

/// `source` as `rustfmt --edition 2021` writes it when run in `dir` — run here
/// rather than through the library, so the test does not grade the
/// formatting step by itself.
fn rustfmt_in(dir: &Path, source: &str) -> TestResult<String> {
    use std::io::Write;

    let mut child = Command::new("rustfmt")
        .args(["--edition", "2021", "--emit", "stdout"])
        .current_dir(dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("rustfmt has no stdin")?;
    stdin.write_all(source.as_bytes())?;
    drop(stdin);
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(format!("rustfmt failed: {}", stderr(&output)).into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

/// `generate` writes each Rust target as `rustfmt` formats the library's
/// plain output, and every other target as the library returns it. The plain
/// output is what the golden files pin; this is the step between the two.
#[test]
fn generate_writes_the_rust_targets_through_rustfmt() -> TestResult {
    let root = rust_project("format", "")?;
    generate(&root)?;

    for (path, plain, rust) in plain_outputs(&root)? {
        let written = std::fs::read_to_string(root.join(&path))?;
        if !rust {
            assert!(
                written == plain,
                "{} must be the plain output",
                path.display()
            );
            continue;
        }
        let dir = root.join(&path);
        let dir = dir.parent().ok_or("an output path has a directory")?;
        let formatted = rustfmt_in(dir, &plain)?;
        assert!(
            written == formatted,
            "{} must be rustfmt's output:\n{written}",
            path.display()
        );
        // Not vacuous: the plain output is not laid out.
        assert!(
            plain != formatted,
            "{} came out already formatted",
            path.display()
        );
    }
    Ok(())
}

/// `check` formats before comparing: the formatted files are up to date, and
/// the plain output in their place is stale.
#[test]
fn check_compares_the_formatted_output() -> TestResult {
    let root = rust_project("check-format", "")?;
    generate(&root)?;
    let out = run(&root, &["check"])?;
    assert!(out.status.success(), "{}", stderr(&out));

    let plain = plain_outputs(&root)?;
    let (path, contents, _) = plain
        .iter()
        .find(|(path, _, _)| path.ends_with("server.rs"))
        .ok_or("the server is generated")?;
    std::fs::write(root.join(path), contents)?;
    let out = run(&root, &["check"])?;
    assert_eq!(out.status.code(), Some(FAILURE), "{}", stdout(&out));
    assert!(stderr(&out).contains("gen/server.rs"), "{}", stderr(&out));
    assert!(!stderr(&out).contains("gen/types.rs"), "{}", stderr(&out));
    Ok(())
}

/// `[rust] format = false` writes the plain output, needs no `rustfmt`, and
/// `check` compares against the same.
#[test]
fn format_false_writes_the_plain_output_without_rustfmt() -> TestResult {
    let root = rust_project("no-format", "format = false\n")?;
    let out = Command::new(BIN)
        .arg("generate")
        .current_dir(&root)
        .env("RUSTFMT", "/nonexistent/rustfmt")
        .output()?;
    assert!(out.status.success(), "{}", stderr(&out));

    for (path, plain, _) in plain_outputs(&root)? {
        let written = std::fs::read_to_string(root.join(&path))?;
        assert!(
            written == plain,
            "{} must be the plain output",
            path.display()
        );
    }
    let out = run(&root, &["check"])?;
    assert!(out.status.success(), "{}", stderr(&out));
    Ok(())
}

/// A `rustfmt` that cannot be run fails the run before anything is written,
/// naming the target, the binary, and the setting that does without it.
#[test]
fn a_missing_rustfmt_names_the_target_and_the_way_out() -> TestResult {
    let root = rust_project("no-rustfmt", "")?;
    for command in ["generate", "check"] {
        let out = Command::new(BIN)
            .arg(command)
            .current_dir(&root)
            .env("RUSTFMT", "/nonexistent/rustfmt")
            .output()?;
        assert_eq!(out.status.code(), Some(FAILURE), "{}", stdout(&out));
        let err = stderr(&out);
        for needle in [
            "rust_types",
            "gen/types.rs",
            "/nonexistent/rustfmt",
            "`[rust] format = false`",
        ] {
            assert!(err.contains(needle), "{command}: `{needle}` in {err}");
        }
    }
    assert!(
        !root.join("gen").exists(),
        "nothing is written when a target cannot be formatted"
    );
    Ok(())
}

/// The project's `rustfmt.toml` applies, as it does to `cargo fmt`: found
/// from the directory the file is written to, here two levels up.
#[test]
fn the_projects_rustfmt_toml_applies() -> TestResult {
    let root = rust_project("rustfmt-toml", "")?;
    std::fs::write(
        root.join("rustfmt.toml"),
        "hard_tabs = true\nmax_width = 120\n",
    )?;
    generate(&root)?;

    let server = std::fs::read_to_string(root.join("gen").join("server.rs"))?;
    assert!(
        server.contains("\n\tlet result = service.get(input).await;\n"),
        "{server}"
    );
    // A signature 118 columns wide, which the default `max_width` breaks.
    assert!(
        server.contains(
            "\tasync fn get(&self, input: GetInput) -> Result<::treat::ApiResponse<GetOutput>, \
             ::treat::ApiError<QueryGetCodes>>;\n"
        ),
        "{server}"
    );
    Ok(())
}

/// An SDL whose server re-exports items of every casing — `event_stream`,
/// `EventStream`, `Refusal` — which the 2021 and 2024 style editions sort
/// differently, so the edition a file was formatted for shows in it.
const SUBSCRIPTION_SDL: &str = r#"
enum ErrorCode { invalid_body internal_error not_found }

input GetInput { id: ID! }
type GetOutput { id: ID!, name: String }

type Query @service {
  get(input: GetInput!): GetOutput! @rpc(path: "/get") @query
  watch(input: GetInput!): GetOutput! @rpc(path: "/watch") @subscription
}
"#;

/// Whether `rustfmt --edition <edition> --check` passes on `file`.
fn formatted_for(file: &Path, edition: &str) -> TestResult<bool> {
    let out = Command::new("rustfmt")
        .args(["--edition", edition, "--check"])
        .arg(file)
        .output()?;
    Ok(out.status.success())
}

/// Holds the generated Rust of `root` to `rustfmt --edition <edition>
/// --check`, and the server to failing the other edition's check — so the
/// edition is the one that decided the layout, not one that happens to agree.
fn assert_formatted_for(root: &Path, edition: &str, other: &str) -> TestResult {
    for name in ["types.rs", "server.rs", "client.rs"] {
        let file = root.join("gen").join(name);
        let passes = formatted_for(&file, edition)?;
        assert!(
            passes,
            "{name} must pass `rustfmt --edition {edition} --check`"
        );
    }
    let also_other = formatted_for(&root.join("gen").join("server.rs"), other)?;
    assert!(
        !also_other,
        "server.rs must not be formatted for {other} too"
    );
    Ok(())
}

/// A package of the 2024 edition gets its files formatted as `cargo fmt`
/// formats them there — with the 2024 style edition, which `rustfmt` derives
/// from `--edition` — so the crate's own `cargo fmt --check` keeps them.
#[test]
fn a_2024_package_gets_2024_formatting() -> TestResult {
    let root = rust_project("edition-2024", "")?;
    std::fs::write(root.join("api").join("schema.graphql"), SUBSCRIPTION_SDL)?;
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    )?;
    generate(&root)?;
    assert_formatted_for(&root, "2024", "2021")?;
    let out = run(&root, &["check"])?;
    assert!(out.status.success(), "{}", stderr(&out));
    Ok(())
}

/// The edition a member inherits from its workspace is the one it gets.
#[test]
fn an_inherited_edition_applies() -> TestResult {
    let root = rust_project("edition-inherited", "")?;
    std::fs::write(root.join("api").join("schema.graphql"), SUBSCRIPTION_SDL)?;
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\n\n[workspace.package]\nedition = \"2024\"\n\n\
         [package]\nname = \"x\"\nversion = \"0.0.0\"\nedition.workspace = true\n",
    )?;
    generate(&root)?;
    assert_formatted_for(&root, "2024", "2021")
}

/// `[rust] edition` overrides the package's, for files written outside the
/// package that compiles them.
#[test]
fn the_settings_edition_overrides_the_packages() -> TestResult {
    let root = rust_project("edition-override", "edition = \"2024\"\n")?;
    std::fs::write(root.join("api").join("schema.graphql"), SUBSCRIPTION_SDL)?;
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )?;
    generate(&root)?;
    assert_formatted_for(&root, "2024", "2021")
}

/// With neither, a project with no `Cargo.toml` above it gets the 2021
/// edition.
#[test]
fn the_default_edition_is_2021() -> TestResult {
    let root = rust_project("edition-default", "")?;
    std::fs::write(root.join("api").join("schema.graphql"), SUBSCRIPTION_SDL)?;
    generate(&root)?;
    assert_formatted_for(&root, "2021", "2024")
}

/// A rustup proxy whose toolchain has no rustfmt fails in words that say so,
/// rather than blaming a `rustfmt.toml` or the generated code.
#[cfg(unix)]
#[test]
fn a_toolchain_without_rustfmt_is_named_as_such() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let root = rust_project("no-component", "")?;
    // What rustup's proxy prints, and its status, when the component is not
    // installed.
    let proxy = root.join("rustfmt");
    std::fs::write(
        &proxy,
        "#!/bin/sh\ncat >/dev/null\necho \"error: 'rustfmt' is not installed for the toolchain \
         'stable-x86_64-unknown-linux-gnu'.\" >&2\nexit 1\n",
    )?;
    std::fs::set_permissions(&proxy, std::fs::Permissions::from_mode(0o755))?;
    let out = Command::new(BIN)
        .arg("generate")
        .current_dir(&root)
        .env("RUSTFMT", &proxy)
        .output()?;
    assert_eq!(out.status.code(), Some(FAILURE), "{}", stdout(&out));
    let err = stderr(&out);
    for needle in [
        "no rustfmt component",
        "rustup component add rustfmt",
        "`[rust] format = false`",
    ] {
        assert!(err.contains(needle), "`{needle}` in {err}");
    }
    assert!(!err.contains("rustfmt.toml"), "{err}");
    Ok(())
}

/// A checkout that turned the files' line endings into `\r\n` — Git for
/// Windows' `core.autocrlf` — is up to date, and `generate` leaves its files
/// alone; any other difference is still stale.
#[test]
fn check_reads_crlf_line_endings_as_unchanged() -> TestResult {
    let root = rust_project("crlf", "")?;
    generate(&root)?;
    for name in ["types.rs", "server.rs", "client.rs", "openapi.json"] {
        let path = root.join("gen").join(name);
        let text = std::fs::read_to_string(&path)?;
        std::fs::write(&path, text.replace('\n', "\r\n"))?;
    }
    let out = run(&root, &["check"])?;
    assert!(out.status.success(), "{}", stderr(&out));

    let out = generate(&root)?;
    assert!(!stdout(&out).contains("wrote"), "{}", stdout(&out));
    let server = std::fs::read_to_string(root.join("gen").join("server.rs"))?;
    assert!(server.contains("\r\n"), "generate must not rewrite it");

    std::fs::write(
        root.join("gen").join("server.rs"),
        server.replacen("\r\n", "\r\n// edited\r\n", 1),
    )?;
    let out = run(&root, &["check"])?;
    assert_eq!(out.status.code(), Some(FAILURE), "{}", stdout(&out));
    assert!(stderr(&out).contains("gen/server.rs"), "{}", stderr(&out));
    Ok(())
}
