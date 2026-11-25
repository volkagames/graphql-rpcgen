//! The Rust emitters for the forms that are not JSON in and JSON out.
//!
//! A `@subscription` and a `@raw` body change the signature, the route verb and
//! where the input is read from. These pin each of those, on both sides, since
//! the server and the client have to agree about all three.

#![allow(
    clippy::panic_in_result_fn,
    reason = "a test asserts, and returns `Result` only to use `?` on its setup"
)]

/// What a test body answers with: the first failure ends it, carrying the
/// message of whatever actually went wrong rather than a fixed one.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

mod common;

use common::flat;
use graphql_rpcgen::config::Config;
use graphql_rpcgen::{generate_rust, generate_rust_client};

const SDL: &str = r#"
scalar UUID
scalar DateTime
scalar JSON

"An opaque body, marked as one by its OpenAPI format."
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
}

enum Kind {
  plain
  with_tag @variant(tag: "with-tag")
}

type Event { line: String! }

input WatchInput { id: UUID! }

input ExportInput {
  id: UUID!
  kind: Kind!
}

input UploadInput {
  id: UUID!
  kind: Kind!
  "A list repeats its key in the query string."
  tags: [String!]
  count: Int
  body: Binary!
}

type UploadResult { written: Int! }

type Files @service {
  "A stream of events, one per frame."
  watch(input: WatchInput!): Event! @subscription @rpc(path: "/watch")

  "Bytes out."
  export(input: ExportInput!): Binary!
    @query
    @rpc(path: "/export")
    @raw(response: ["text/csv"])
    @throws(codes: ["not_found"])

  "Bytes in, envelope out."
  upload(input: UploadInput!): UploadResult!
    @mutation
    @rpc(path: "/upload")
    @raw(request: ["text/csv", "application/json"])

  "Bytes in, handed over unread."
  store(input: UploadInput!): UploadResult!
    @mutation
    @rpc(path: "/store")
    @raw(request: ["application/octet-stream"], stream: true)
}
"#;

fn api() -> TestResult<graphql_rpcgen::ir::Api> {
    Ok(graphql_rpcgen::compile_str(SDL)?)
}

fn server() -> TestResult<String> {
    Ok(generate_rust::generate(&api()?, &Config::default())?)
}

fn client() -> TestResult<String> {
    Ok(generate_rust_client::generate(&api()?, &Config::default())?)
}

/// A subscription answers with a stream of the declared event, not with one of
/// them, and is routed as the GET an `EventSource` can open.
#[test]
fn a_subscription_is_a_get_returning_a_stream() -> TestResult {
    let out = server()?;

    assert!(
        flat(&out).contains(
            "async fn watch(&self, input: WatchInput) -> Result<::treat::rpc::server::EventStream<Event>, \
             ::treat::ApiError<FilesWatchCodes>>;"
        ),
        "{out}"
    );
    assert!(
        out.contains(r#".route("/watch", ::axum::routing::get(files_watch::<S>))"#),
        "{out}"
    );
    // The input has no body to arrive in, so it is decoded from the URL, and
    // the events go back as SSE: both `treat::rpc`'s.
    assert!(
        flat(&out).contains("::axum::extract::RawQuery(query): ::axum::extract::RawQuery)"),
        "{out}"
    );
    assert!(
        out.contains(
            "    let input = Rt::query_input(query.as_deref(), wire::FILES_WATCH_FIELDS)?;\n"
        ),
        "{out}"
    );
    assert!(
        out.contains("    Ok(Rt::respond_events(result))\n"),
        "{out}"
    );
    Ok(())
}

/// A raw response replaces the envelope with the bytes and the headers that
/// describe them.
#[test]
fn a_raw_response_returns_bytes() -> TestResult {
    let out = server()?;

    assert!(
        flat(&out).contains(
            "async fn export(&self, input: ExportInput) -> Result<::treat::rpc::server::RawBody, \
             ::treat::ApiError<FilesExportCodes>>;"
        ),
        "{out}"
    );
    // Still a POST with a JSON body: only the answer is raw.
    assert!(
        out.contains(r#".route("/export", ::axum::routing::post(files_export::<S>))"#),
        "{out}"
    );
    let handler = out
        .split("async fn files_export<")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .ok_or("the raw-response handler is emitted")?;
    assert!(
        handler.contains("let input = Rt::json_input(request).await?;"),
        "{handler}"
    );
    assert!(handler.contains("Ok(Rt::respond_raw(result))"), "{handler}");
    Ok(())
}

/// A raw request spends its one body on the payload, so everything else moves
/// to the query string and is decoded against the field's declared type.
#[test]
fn a_raw_request_reads_body_and_query() -> TestResult {
    let out = server()?;

    let handler = out
        .split("async fn files_upload<")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .ok_or("the raw-request handler is emitted")?;
    // The query string first, then the body under a declared type and within
    // the bound, then the input's rules.
    let steps = [
        "let mut input: UploadInput = Rt::input_from_query(request.uri().query(), wire::FILES_UPLOAD_FIELDS)?;",
        "input.body = Rt::raw_body(request, wire::FILES_UPLOAD_TYPES, ::treat::rpc::server::RAW_BODY_LIMIT).await?;",
        "Rt::validate(&input)?;",
        "let result = service.upload(input).await;",
    ];
    let handler = flat(handler);
    let at: Vec<Option<usize>> = steps.iter().map(|step| handler.find(step)).collect();
    assert!(at.iter().all(Option::is_some), "{handler}");
    assert!(
        at.windows(2).all(|pair| pair.first() < pair.get(1)),
        "{handler}"
    );
    // The types the operation declares, which the body is checked against.
    assert!(
        out.contains(
            r#"pub static FILES_UPLOAD_TYPES: &[&str] = &["text/csv", "application/json"];"#
        ),
        "{out}"
    );
    // The payload is not one of the parameters it travels beside.
    assert!(
        !flat(&out).contains(r#"("body")"#),
        "the payload must not be decoded as a query parameter:\n{out}"
    );
    Ok(())
}

/// A query string carries only text, so the JSON type each parameter decodes to
/// comes from the schema. Only a form that reads its input from the URL has
/// them: a raw *response* still takes an ordinary JSON body.
#[test]
fn query_parameters_are_typed_by_the_schema() -> TestResult {
    let out = flat(&server()?);

    // Each entry through `QueryField`'s constructor for its kind, `.list()`
    // after a list.
    for (entry, list) in [
        (r#"QueryField::str("id")"#, false),
        // An enum is a string on the wire whatever it is called in Rust.
        (r#"QueryField::str("kind")"#, false),
        (r#"QueryField::str("tags")"#, true),
        (r#"QueryField::int("count")"#, false),
    ] {
        assert!(out.contains(entry), "{entry}: {out}");
        assert_eq!(
            out.contains(&format!("{entry}.list()")),
            list,
            "{entry}: {out}"
        );
    }
    Ok(())
}

/// The payload is absent from the object the other fields are decoded from, and
/// must never be serialized into the query string the client builds from it.
#[test]
fn the_payload_field_is_skipped_on_both_sides() -> TestResult {
    let out = generate_rust::generate_types(&api()?, &Config::default())?;
    assert!(
        out.contains("#[serde(default, skip_serializing)]\n    pub body: Vec<u8>,"),
        "{out}"
    );
    Ok(())
}

/// The client mirrors the server: a stream to read, a response to consume, and
/// a payload to send with the content type the caller chose.
#[test]
fn the_client_has_a_method_per_form() -> TestResult {
    let out = flat(&client()?);

    assert!(
        out.contains(
            "pub async fn watch(&self, input: &WatchInput) -> \
             ::treat::rpc::client::ClientResult<::treat::rpc::client::EventStream<Event>, FilesWatchCodes>"
        ),
        "{out}"
    );
    assert!(
        out.contains(
            "pub async fn export(&self, input: &ExportInput) -> \
             ::treat::rpc::client::ClientResult<::treat::rpc::client::reqwest::Response, FilesExportCodes>"
        ),
        "{out}"
    );
    // The SDL lists what the operation accepts; only the caller knows which of
    // them it is sending.
    assert!(
        out.contains(
            "pub async fn upload(&self, input: &UploadInput, content_type: &str) -> \
             ::treat::rpc::client::ClientResult<UploadResult, FilesUploadCodes>"
        ),
        "{out}"
    );
    assert!(
        out.contains("/// Accepts: `text/csv`, `application/json`."),
        "{out}"
    );
    assert!(out.contains("/// Answers with: `text/csv`."), "{out}");
    Ok(())
}

/// The runtime is emitted only where something needs it, so a plain JSON API
/// compiles neither an SSE parser nor a query-string decoder.
#[test]
fn a_plain_api_gets_no_streaming_runtime() -> TestResult {
    let plain = graphql_rpcgen::compile_str(
        r#"
scalar UUID
enum ErrorCode { invalid_body internal_error }
input GetInput { id: UUID! }
type Row { id: UUID! }
type Rows @service {
  get(input: GetInput!): Row! @query @rpc(path: "/get")
}
"#,
    )?;

    let server = generate_rust::generate(&plain, &Config::default())?;
    assert!(!server.contains("EventStream"), "{server}");
    assert!(!server.contains("QueryField"), "{server}");
    assert!(!server.contains("RAW_BODY_LIMIT"), "{server}");

    let client = generate_rust_client::generate(&plain, &Config::default())?;
    // Nor does it name `rpc-stream`'s types, so it builds without the feature.
    assert!(!client.contains("EventStream"), "{client}");
    assert!(!client.contains("StreamError"), "{client}");
    Ok(())
}

/// A streamed request keeps the input it would have had and takes the unread
/// body beside it, last, since that is what is left of the request.
#[test]
fn a_streamed_request_hands_the_body_over_unread() -> TestResult {
    let out = server()?;

    assert!(
        flat(&out).contains(
            "async fn store(&self, input: UploadInput, body: ::treat::rpc::server::RawRequest) -> \
             Result<::treat::ApiResponse<UploadResult>, ::treat::ApiError<FilesStoreCodes>>;"
        ),
        "{out}"
    );
    // The signature cannot say where the payload went or that nothing bounds
    // it, so the method's doc does.
    assert!(
        out.contains(
            "    /// Bytes in, handed over unread.\n    ///\n    \
             /// The request body arrives unread as `body`; `input.body` is always empty.\n    \
             /// No size limit is applied to it: this method must bound it — see\n    \
             /// [`treat::rpc::server::RawRequest`].\n    async fn store("
        ),
        "{out}"
    );
    // `treat`'s, re-exported for the implementation.
    assert!(out.contains("RawRequest"), "{out}");
    assert!(!out.contains("pub struct RawRequest {"), "{out}");

    let handler = out
        .split("async fn files_store<")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .ok_or("the streamed handler is emitted")?;
    // The unread request is the last extractor; its type is checked before
    // anything else, then the query string read from it.
    assert!(
        handler.contains("raw: ::treat::rpc::server::RawRequest)"),
        "{handler}"
    );
    assert!(
        handler.contains(
            "    Rt::require_content_type(raw.headers(), wire::FILES_STORE_TYPES)?;\n    \
             let input = Rt::query_input(raw.query(), wire::FILES_STORE_FIELDS)?;\n"
        ),
        "{handler}"
    );
    assert!(
        handler.contains("service.store(input, raw).await"),
        "{handler}"
    );
    assert!(
        !handler.contains("raw_body("),
        "nothing is buffered:\n{handler}"
    );
    Ok(())
}

/// The client cannot tell a streamed operation from a buffered one: both send
/// the payload as the body, and only the server decides not to hold it.
#[test]
fn the_client_sends_a_streamed_body_like_any_raw_one() -> TestResult {
    let out = flat(&client()?);
    assert!(
        out.contains(
            "pub async fn store(&self, input: &UploadInput, content_type: &str) -> \
             ::treat::rpc::client::ClientResult<UploadResult, FilesStoreCodes>"
        ),
        "{out}"
    );
    Ok(())
}

/// An API whose raw requests are all buffered names no `RawRequest`.
#[test]
fn a_buffered_api_gets_no_raw_request() -> TestResult {
    let buffered = graphql_rpcgen::compile_str(
        r#"
scalar UUID
scalar Binary @scalar(rust: "Vec<u8>", typescript: "Blob", openapiType: "string", openapiFormat: "binary")
enum ErrorCode { invalid_body internal_error }
input StoreInput { id: UUID! body: Binary! }
type Done { ok: Boolean! }
type Files @service {
  store(input: StoreInput!): Done! @mutation @raw(request: ["text/csv"])
}
"#,
    )?;
    let server = generate_rust::generate(&buffered, &Config::default())?;
    assert!(server.contains("RAW_BODY_LIMIT"), "{server}");
    assert!(!server.contains("RawRequest"), "{server}");
    Ok(())
}
