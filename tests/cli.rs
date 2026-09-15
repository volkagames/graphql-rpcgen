//! The `graphql-rpcgen` binary itself.
//!
//! Every other suite calls the library. Nothing put the CLI in front of a
//! directory, so its documented contract — `check` regenerates in memory and
//! exits non-zero when the checked-in output is stale — held only by reading.
//! That contract is what a consumer's CI hangs on, and a swapped branch or a
//! mis-parsed flag would break it without failing a single test.

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
