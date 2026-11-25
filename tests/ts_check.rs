//! The generated TypeScript and zod, through the TypeScript compiler.
//!
//! `snapshots.rs` asserts what the TypeScript emitters write and `golden.rs`
//! keeps all of it; neither shows that `tsc --strict` accepts it. This test
//! does, for every fixture SDL and for `tests/sdl/runtime_names.graphql`,
//! whose types are spelt like the globals the client runtime uses: it writes
//! `api.ts` and `zod.ts` under
//! `target/ts-check/fixtures/<fixture>/` and runs the tsc step of
//! `just check-ts` there.
//!
//! Ignored by default: the compiler and `zod` come from npm, and the mandatory
//! tests do not reach the network. `just check-ts` installs them and writes
//! the `tsconfig.json` this runs with; after that,
//! `cargo test --test ts_check -- --ignored` checks the library's output
//! without regenerating through the CLI. Asked for without them, it fails and
//! says so rather than passing with nothing checked.

#![allow(
    clippy::panic_in_result_fn,
    reason = "a test asserts, and returns `Result` only to use `?` on its setup"
)]

/// What a test body answers with: the first failure ends it, carrying the
/// message of whatever actually went wrong rather than a fixed one.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

mod common;

use common::fixtures::FIXTURES;
use std::process::Command;

/// The SDL whose types are spelt like the items the generated code names —
/// the Rust runtime's, and the globals the TypeScript client runtime uses
/// (`Response`, `Headers`, `Promise`, `Record`, …). Not a fixture, so not in
/// [`FIXTURES`]; type-checked beside them, as `just check-ts` does.
const RUNTIME_NAMES_SDL: &str = include_str!("sdl/runtime_names.graphql");

#[test]
#[ignore = "needs typescript and zod from npm: run `just check-ts` once"]
fn the_generated_typescript_type_checks() -> TestResult {
    let root = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;
    let dir = root.join("target").join("ts-check");
    let tsc = dir.join("node_modules").join(".bin").join("tsc");
    let tsconfig = dir.join("tsconfig.json");
    let tsc_exists = tsc.exists();
    let tsconfig_exists = tsconfig.exists();
    if !tsc_exists || !tsconfig_exists {
        return Err(format!(
            "{} or {} is missing: run `just check-ts` once to install typescript \
             and zod there and write the tsconfig",
            tsc.display(),
            tsconfig.display()
        )
        .into());
    }

    // Rewritten whole, as `just check-ts` does, so a fixture since renamed or
    // removed is not type-checked from a stale copy.
    let out = dir.join("fixtures");
    let stale = out.exists();
    if stale {
        std::fs::remove_dir_all(&out)?;
    }
    let sdls = FIXTURES
        .iter()
        .map(|fixture| (fixture.name, fixture.sdl))
        .chain([("runtime_names", RUNTIME_NAMES_SDL)]);
    for (name, sdl) in sdls {
        let api = graphql_rpcgen::compile_str(sdl)?;
        let fixture_dir = out.join(name);
        std::fs::create_dir_all(&fixture_dir)?;
        std::fs::write(
            fixture_dir.join("api.ts"),
            graphql_rpcgen::generate_typescript::generate(&api),
        )?;
        std::fs::write(
            fixture_dir.join("zod.ts"),
            graphql_rpcgen::generate_zod::generate(&api),
        )?;
    }

    let output = Command::new(&tsc)
        .args(["--noEmit", "--strict", "-p"])
        .arg(&tsconfig)
        .current_dir(&dir)
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "tsc --strict must accept the generated TypeScript:\n{stdout}\n{stderr}"
    );
    Ok(())
}

/// The SDL of the runtime test: every kind of field, on every operation form
/// that sends one as text.
const FIELD_KINDS_SDL: &str = include_str!("ts/field_kinds.graphql");

/// The runtime test itself, compiled beside the `api.ts` generated from
/// [`FIELD_KINDS_SDL`].
const CLIENT_RUNTIME_TEST: &str = include_str!("ts/client_runtime.test.ts");

/// What the generated TypeScript client sends and how it reads an answer, run
/// under node against a `fetch` that records each request.
///
/// `tsc` shows the client is well typed; this shows it writes what the server
/// reads: each field kind in a query string, a form body and a path as the
/// field table says, `Content-Type` exactly the one the body needs whatever
/// the caller's default headers hold, and a refusal told from a raw answer by
/// `x-rpc-status` and then only by an `application/json` envelope with
/// `errors[]` — see `tests/ts/client_runtime.test.ts`.
///
/// Ignored with the type check, for the same reason, and needs `node` (one
/// with `fetch`, `Response` and `Blob`: 18 or later) on the `PATH`.
#[test]
#[ignore = "needs typescript from npm and node: run `just check-ts` once"]
fn the_generated_client_sends_what_the_server_reads() -> TestResult {
    let root = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;
    let dir = root.join("target").join("ts-check");
    let tsc = dir.join("node_modules").join(".bin").join("tsc");
    let tsc_exists = tsc.exists();
    if !tsc_exists {
        return Err(format!(
            "{} is missing: run `just check-ts` once to install typescript there",
            tsc.display()
        )
        .into());
    }

    let out = dir.join("runtime");
    let stale = out.exists();
    if stale {
        std::fs::remove_dir_all(&out)?;
    }
    std::fs::create_dir_all(&out)?;
    let api = graphql_rpcgen::compile_str(FIELD_KINDS_SDL)?;
    std::fs::write(
        out.join("api.ts"),
        graphql_rpcgen::generate_typescript::generate(&api),
    )?;
    std::fs::write(out.join("client_runtime.test.ts"), CLIENT_RUNTIME_TEST)?;
    // ES modules, so the test may `await` at its top level.
    std::fs::write(
        out.join("package.json"),
        "{ \"private\": true, \"type\": \"module\" }\n",
    )?;
    std::fs::write(
        out.join("tsconfig.json"),
        r#"{
  "compilerOptions": {
    "strict": true,
    "target": "es2022",
    "lib": ["es2022", "dom", "dom.iterable"],
    "module": "nodenext",
    "moduleResolution": "nodenext",
    "rootDir": ".",
    "outDir": "js",
    "skipLibCheck": true
  },
  "include": ["*.ts"]
}
"#,
    )?;

    let compiled = Command::new(&tsc)
        .arg("-p")
        .arg(out.join("tsconfig.json"))
        .current_dir(&out)
        .output()?;
    assert!(
        compiled.status.success(),
        "tsc must compile the runtime test:\n{}\n{}",
        String::from_utf8_lossy(&compiled.stdout),
        String::from_utf8_lossy(&compiled.stderr)
    );

    let ran = Command::new("node")
        .arg(out.join("js").join("client_runtime.test.js"))
        .current_dir(&out)
        .output()?;
    let stdout = String::from_utf8_lossy(&ran.stdout);
    let stderr = String::from_utf8_lossy(&ran.stderr);
    assert!(
        ran.status.success(),
        "the generated client must pass its runtime test:\n{stdout}\n{stderr}"
    );
    println!("{stdout}");
    Ok(())
}
