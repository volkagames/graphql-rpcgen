//! The generated `@raw` server and client, compiled.
//!
//! `streaming.rs` asserts what the raw templates *write*; `oneof_wire.rs`
//! compiles what the types target emits. Between them nothing ever put rustc in
//! front of a raw handler or a raw client method — so a macro that stops
//! parsing, an extractor ordering axum rejects, a trait bound that cannot be
//! proved, or a crate the emitted runtime reaches for but no consumer knows to
//! declare, all passed both. The last of those is not hypothetical: the
//! streaming runtime calls `form_urlencoded::parse`, and the client reads a raw
//! response through `reqwest`'s `stream` feature, neither of which any other
//! test or document mentioned.
//!
//! This builds all three Rust targets from one SDL reaching every raw form, in
//! a scratch cargo project, offline, with versions pinned to this crate's
//! lockfile — the arrangement `oneof_wire` already uses and for the same
//! reasons.

/// What a test body answers with: the first failure ends it, carrying the
/// message of whatever actually went wrong rather than a fixed one.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

use std::process::Command;

/// Every shape the raw emitters branch on, and a subscription beside them
/// because both forms share one runtime.
///
/// The four raw operations are chosen to reach all four handler templates and
/// all four client-method templates: a raw response with an input and without
/// one, a raw request, and both directions at once.
const SDL: &str = r#"
scalar UUID

"An opaque body: bytes on the wire rather than a value in the JSON envelope."
scalar Binary
  @scalar(
    rust: "Vec<u8>"
    typescript: "Blob"
    openapiType: "string"
    openapiFormat: "binary"
  )

enum ErrorCode {
  invalid_body
  internal_error
  not_found
  "Declared by no operation, so every generated set keeps a reachable fallback."
  database_error
}

type Event { line: String! }

input WatchInput { id: UUID! }

input ReportInput {
  id: UUID!
  "A parameter of a raw-response call still travels as JSON."
  rows: Int
}

input IngestInput {
  id: UUID!
  "A list repeats its key in the query string once the body is spent."
  tags: [String!]
  "The one binary field: this is the request body itself."
  body: Binary!
}

input ConvertInput {
  id: UUID!
  body: Binary!
}

type IngestResult { written: Int! }

type Files @service {
  "A stream of events, sharing the runtime the raw forms emit."
  watch(input: WatchInput!): Event! @subscription @rpc(path: "/watch")

  "Bytes out, JSON in."
  report(input: ReportInput!): Binary!
    @query
    @rpc(path: "/report")
    @raw(response: ["text/csv"])
    @throws(codes: ["not_found"])

  "Bytes out, nothing in."
  manifest: Binary!
    @query
    @rpc(path: "/manifest")
    @raw(response: ["application/json"])

  "Bytes in, JSON out."
  ingest(input: IngestInput!): IngestResult!
    @mutation
    @rpc(path: "/ingest")
    @raw(request: ["text/csv", "application/json"])

  "Bytes in, bytes out."
  convert(input: ConvertInput!): Binary!
    @mutation
    @rpc(path: "/convert")
    @raw(request: ["text/csv"], response: ["application/json"])
}
"#;

/// An implementation of the generated trait, and one use of each client method.
///
/// Emitting a handler is not the same as being able to mount one: `post(h::<S>)`
/// only proves out when axum's `Handler` bound is discharged, which is what
/// building the router forces. The bodies answer with the smallest value of the
/// right shape — nothing here asserts behaviour, only that the shapes check.
const DRIVER: &str = r##"

// --- driver ------------------------------------------------------------------

#[derive(Clone)]
pub struct Files;

#[async_trait::async_trait]
impl server::FilesService for Files {
    async fn watch(
        &self,
        _input: server::WatchInput,
    ) -> Result<server::EventStream<server::Event>, server::ApiError<server::FilesWatchCodes>> {
        Ok(server::event_stream(futures_util::stream::empty()))
    }

    async fn convert(
        &self,
        input: server::ConvertInput,
    ) -> Result<server::RawBody, server::ApiError<server::FilesConvertCodes>> {
        Ok(server::RawBody::new("application/json", input.body))
    }

    async fn ingest(
        &self,
        input: server::IngestInput,
    ) -> Result<
        server::ApiResponse<server::IngestResult>,
        server::ApiError<server::FilesIngestCodes>,
    > {
        Ok(server::success(server::IngestResult {
            written: input.body.len() as i32,
        }))
    }

    async fn manifest(
        &self,
    ) -> Result<server::RawBody, server::ApiError<server::FilesManifestCodes>> {
        Ok(server::RawBody::new("application/json", b"{}".to_vec()))
    }

    async fn report(
        &self,
        _input: server::ReportInput,
    ) -> Result<server::RawBody, server::ApiError<server::FilesReportCodes>> {
        // The one path carrying a header of its own: a raw response is bytes
        // plus what describes them, which is why `RawBody` is a struct rather
        // than a `(String, Vec<u8>)`.
        Ok(server::RawBody::new("text/csv", Vec::new()).attachment("report.csv"))
    }
}

/// Mounting every raw handler, which is what discharges axum's `Handler` bound.
pub fn router() -> axum::Router {
    server::files_router(Files)
}

/// Never called: compiling the body is the assertion.
///
/// The client forms differ in their signatures rather than their behaviour — a
/// raw response answers with `reqwest::Response`, a raw request takes the
/// content type as a parameter, a no-input operation takes nothing — so naming
/// each correctly is the whole check, and running it would need a server on the
/// other end to prove nothing extra.
#[allow(dead_code)]
async fn client_forms(api: &client::ApiClient) {
    let convert = types::ConvertInput { id: uuid::Uuid::nil(), body: Vec::new() };
    let ingest = types::IngestInput { id: uuid::Uuid::nil(), tags: None, body: Vec::new() };
    let report = types::ReportInput { id: uuid::Uuid::nil(), rows: None };
    let watch = types::WatchInput { id: uuid::Uuid::nil() };

    let files = api.files();
    let _ = files.convert(&convert, "text/csv").await;
    let _ = files.ingest(&ingest, "text/csv").await;
    let _ = files.manifest().await;
    let _ = files.report(&report).await;
    let _ = files.watch(&watch).await;
}
"##;

/// The three Rust targets, emitted into one crate.
///
/// `types_path` points at a module of the same crate rather than a sibling one,
/// which is the single-crate layout a scratch project wants; the split layout is
/// what every other test already covers.
fn generated() -> TestResult<(String, String, String)> {
    let api = graphql_rpcgen::compile_str(SDL)?;
    let mut config = graphql_rpcgen::config::Config::default();
    config.rust.types_path = Some("crate::types".to_string());
    config.rust_client.types_path = Some("crate::types".to_string());

    Ok((
        graphql_rpcgen::generate_rust::generate_types(&api, &config)?,
        graphql_rpcgen::generate_rust::generate(&api, &config)?,
        graphql_rpcgen::generate_rust_client::generate(&api, &config)?,
    ))
}

/// What the generated code reaches for, with this crate's lockfile as the
/// source of truth for versions.
///
/// `form_urlencoded` and `reqwest`'s `stream` feature are in this list for the
/// raw and streaming runtime alone: drop either and the emitted code stops
/// compiling, which is the failure this test exists to make visible.
const DEPENDENCIES: &[&str] = &[
    "async-trait",
    "axum",
    "bytes",
    "chrono",
    "derive_more",
    "error_set",
    "form_urlencoded",
    "futures-core",
    "futures-util",
    "regex",
    "reqwest",
    "serde",
    "serde_json",
    "treat",
    "uuid",
    "validator",
];

/// Per-dependency features, exactly as the generated code needs them.
fn features_of(dep: &str) -> &'static str {
    match dep {
        "chrono" => r#"{ version = "{v}", features = ["serde"] }"#,
        "derive_more" => r#"{ version = "{v}", features = ["from", "into", "display", "as_ref"] }"#,
        "reqwest" => {
            r#"{ version = "{v}", default-features = false, features = ["json", "rustls", "stream"] }"#
        }
        "serde" => r#"{ version = "{v}", features = ["derive"] }"#,
        // `validator-extract` already implies `serde-path`, which is what
        // `treat::extract_axum` needs; `rpc-status-header` compiles nothing
        // here but is what emits the `x-rpc-status` the OpenAPI document
        // describes, so the README names it and this set matches the README.
        "treat" => {
            r#"{ version = "{v}", features = ["axum", "validator-extract", "rpc-status-header"] }"#
        }
        "uuid" => r#"{ version = "{v}", features = ["serde"] }"#,
        "validator" => r#"{ version = "{v}", features = ["derive"] }"#,
        _ => r#""{v}""#, // a plain version requirement
    }
}

fn manifest_for(root: &std::path::Path) -> TestResult<String> {
    // Pinning to this crate's lockfile is what keeps the build hermetic and in
    // step: a version bump there lands here on the next run, and a version the
    // lock does not carry is an error instead of a fetch.
    let lock = std::fs::read_to_string(root.join("Cargo.lock"))?;
    let lock: toml::Table = toml::from_str(&lock)?;
    let packages = lock
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or("lockfile has no [[package]] array")?;

    let mut deps = Vec::new();
    for dep in DEPENDENCIES {
        // Every version carrying this name, not the first: the lockfile is
        // sorted by (name, version), so taking the first would silently pin the
        // *older* half of a duplicated major. Two versions mean the assumption
        // this pinning rests on is gone, and picking either one is a guess.
        let versions: Vec<&str> = packages
            .iter()
            .filter(|p| p.get("name").and_then(toml::Value::as_str) == Some(*dep))
            .filter_map(|p| p.get("version").and_then(toml::Value::as_str))
            .collect();
        let version = match versions.as_slice() {
            [only] => *only,
            [] => {
                return Err(format!(
                    "`{dep}` is in the generated code's dependency set but not in Cargo.lock"
                )
                .into());
            }
            many => {
                return Err(format!(
                    "Cargo.lock carries {} versions of `{dep}` ({}); this test pins one version per crate",
                    many.len(),
                    many.join(", ")
                )
                .into());
            }
        };
        deps.push(format!(
            "{dep} = {}",
            features_of(dep).replacen("{v}", version, 1)
        ));
    }

    Ok(format!(
        "[package]\nname = \"raw_wire\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\n{}\n\n[workspace]\n",
        deps.join("\n")
    ))
}

#[test]
fn the_generated_raw_server_and_client_compile() -> TestResult {
    let root = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;

    // Stable locations: the project's lockfile and target directory survive
    // across runs, so the dependency tree resolves once, offline, and later runs
    // reuse it. Only the generated sources are rewritten.
    let project = root.join("target").join("raw-wire-test");
    let project_target = root.join("target").join("raw-wire-test-target");
    let src = project.join("src");
    std::fs::create_dir_all(&src)?;

    let (types, server, client) = generated()?;
    std::fs::write(src.join("types.rs"), types)?;
    std::fs::write(src.join("server.rs"), server)?;
    std::fs::write(src.join("client.rs"), client)?;
    std::fs::write(
        src.join("lib.rs"),
        format!("pub mod client;\npub mod server;\npub mod types;\n{DRIVER}"),
    )?;
    std::fs::write(project.join("Cargo.toml"), manifest_for(&root)?)?;

    // `build`, not `test`: what is being asserted is that rustc accepts the
    // emitted code. The wire behaviour it would run is `oneof_wire`'s job.
    let output = Command::new("cargo")
        .args(["build", "--quiet", "--offline"])
        .current_dir(&project)
        .env("CARGO_TARGET_DIR", &project_target)
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the generated raw server and client must compile:\n{stdout}\n{stderr}"
    );
    Ok(())
}
