//! The generated `@raw` server and client, compiled.
//!
//! `streaming.rs` asserts what the raw templates *write*; `oneof_wire.rs`
//! compiles what the types target emits. Between them nothing ever put rustc in
//! front of a raw handler or a raw client method — so a macro that stops
//! parsing, an extractor ordering axum rejects, a trait bound that cannot be
//! proved, or a crate the emitted runtime reaches for but no consumer knows to
//! declare, all passed both. The last of those is not hypothetical: the
//! streaming runtime once called `form_urlencoded::parse`, and the client reads
//! a raw response through `reqwest`'s `stream` feature, neither of which any
//! other test or document mentioned.
//!
//! This builds all three Rust targets from one SDL reaching every raw form, and
//! the server of a second one checking permissions, in a scratch cargo
//! project, offline, with versions pinned to this crate's lockfile — the
//! arrangement `oneof_wire` already uses and for the same reasons.
//!
//! A streamed request is the one form whose point is behaviour rather than
//! shape — a body larger than `RAW_BODY_LIMIT` getting through — so the scratch
//! project also serves the router on a local port and sends it one.
//!
//! The same project carries the one error-code form whose wire string differs
//! from its Rust name, a `@variant(tag:)` code, because what reaches the
//! envelope is decided by `treat`'s and `error_set`'s derives, which only a
//! build exercises.

#![allow(
    clippy::panic_in_result_fn,
    reason = "a test asserts, and returns `Result` only to use `?` on its setup"
)]

/// What a test body answers with: the first failure ends it, carrying the
/// message of whatever actually went wrong rather than a fixed one.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

mod common;

use common::Dependency;

/// Every shape the raw emitters branch on, and a subscription beside them
/// because both forms share one runtime.
///
/// The four raw operations are chosen to reach all four handler templates and
/// all four client-method templates: a raw response with an input and without
/// one, a raw request, and both directions at once.
const SDL: &str = include_str!("fixtures/raw_wire.graphql");

/// The permission check, in an API of its own: an API declares a role set or a
/// permission set, not both. Emitted as one self-contained server module beside
/// the first, and mounted on the same router.
const PERMISSIONS_SDL: &str = include_str!("fixtures/raw_wire_permissions.graphql");

/// An implementation of the generated trait, one use of each client method, and
/// the streamed request on the wire.
///
/// Emitting a handler is not the same as being able to mount one: `post(h::<S>)`
/// only proves out when axum's `Handler` bound is discharged, which is what
/// building the router forces. The buffered bodies answer with the smallest
/// value of the right shape — only their shapes are checked. The streamed ones
/// read what they were sent, since that is what `wire` below asserts.
const DRIVER: &str = r##"
//! The test driver: implementations of the generated traits, one use of each
//! client method, and the wire tests. Not generated code, so the lints the
//! generated modules are held to are not this module's.
#![allow(clippy::all, dead_code, unused_imports, reason = "a test driver, not generated code")]

use crate::{client, permissions, server, types};
use treat::rpc::futures_util::io::AsyncReadExt;
use treat::rpc::WireEnum;

#[derive(Clone)]
pub struct Files;

/// A caller every request resolves to, so `echo` reaches its extractor and its
/// call with a context without the test having a session to present. Assigned
/// `files_write`, so `echo` — assigned to `files_read` — is reached through the
/// generated hierarchy rather than a direct match.
pub struct Caller;

/// Resolving the caller is `treat`'s contract, shared by both generated
/// modules; each adds its own access checks.
impl treat::rpc::server::ResolveContext for Caller {
    async fn from_parts(
        _parts: &mut axum::http::request::Parts,
    ) -> Result<Self, treat::rpc::server::Refusal> {
        Ok(Caller)
    }
}

impl server::FromRequestContext for Caller {
    fn require_role(&self, role: server::Role) -> Result<(), server::Refusal> {
        if server::Role::FilesWrite.authorizes(role) {
            return Ok(());
        }
        Err(axum::response::IntoResponse::into_response(
            axum::http::StatusCode::FORBIDDEN,
        )
        .into())
    }
}

/// The same caller, to the permission API: that module names its own context
/// trait, with the permission check and no role check.
impl permissions::FromRequestContext for Caller {
    /// Holds `files.purge` only, read by its id as an implementation keeping the
    /// assignment in data would: `wipe` is refused before its method runs.
    async fn require_permission(
        &self,
        permission: permissions::Permission,
    ) -> Result<(), permissions::Refusal> {
        if permissions::Permission::from_name("files.purge") == Some(permission) {
            return Ok(());
        }
        Err(axum::response::IntoResponse::into_response((
            axum::http::StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({
                "errors": [{ "code": "permission_required", "message": permission.as_str() }]
            })),
        ))
        .into())
    }
}

/// The first `len` bytes of what `wire` sends.
pub fn pattern_bytes(len: usize) -> Vec<u8> {
    (0..len).map(pattern).collect()
}

/// The byte at offset `i` of what `wire` sends, so the service can tell a body
/// that arrived whole from one merely of the right length.
pub fn pattern(i: usize) -> u8 {
    (i % 251) as u8
}

#[async_trait::async_trait]
impl server::FilesService for Files {
    type Ctx = Caller;

    async fn watch(
        &self,
        _input: server::WatchInput,
    ) -> Result<server::EventStream<server::Event>, server::ApiError<server::FilesWatchCodes>> {
        Ok(server::event_stream(treat::rpc::futures_util::stream::empty()))
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

    async fn upload(
        &self,
        input: server::UploadInput,
        body: server::RawRequest,
    ) -> Result<
        server::ApiResponse<server::UploadResult>,
        server::ApiError<server::FilesUploadCodes>,
    > {
        assert!(input.body.is_empty(), "a streamed body is never put in the field");
        let declared = body.content_length().map(|n| n as i32);
        let mut reader = body.into_reader();
        let mut received = 0usize;
        let mut intact = true;
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let n = match reader.read(&mut chunk).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => return Err(server::error(server::FilesUploadCodes::InternalError)),
            };
            intact &= chunk[..n].iter().enumerate().all(|(i, b)| *b == pattern(received + i));
            received += n;
        }
        Ok(server::success(server::UploadResult {
            received: received as i32,
            declared,
            intact,
        }))
    }

    async fn echo(
        &self,
        _ctx: &Caller,
        _input: server::ConvertInput,
        body: server::RawRequest,
    ) -> Result<server::RawBody, server::ApiError<server::FilesEchoCodes>> {
        let content_type = body.content_type().unwrap_or_default().to_string();
        let mut bytes = Vec::new();
        if body.into_reader().read_to_end(&mut bytes).await.is_err() {
            return Err(server::error(server::FilesEchoCodes::InternalError));
        }
        Ok(server::RawBody::new(content_type, bytes))
    }
}

#[derive(Clone)]
pub struct Admin;

#[async_trait::async_trait]
impl permissions::AdminService for Admin {
    type Ctx = Caller;

    async fn purge(
        &self,
        _ctx: &Caller,
        _input: permissions::WatchInput,
    ) -> Result<
        permissions::ApiResponse<permissions::IngestResult>,
        permissions::ApiError<permissions::AdminPurgeCodes>,
    > {
        Ok(permissions::success(permissions::IngestResult { written: 1 }))
    }

    async fn wipe(
        &self,
        _ctx: &Caller,
        _input: permissions::WatchInput,
    ) -> Result<
        permissions::ApiResponse<permissions::IngestResult>,
        permissions::ApiError<permissions::AdminWipeCodes>,
    > {
        panic!("a permission the caller lacks must stop the call before the method")
    }

    async fn wipe_all(
        &self,
        _ctx: &Caller,
    ) -> Result<
        permissions::ApiResponse<permissions::IngestResult>,
        permissions::ApiError<permissions::AdminWipeAllCodes>,
    > {
        panic!("refused before the method")
    }

    async fn watch(
        &self,
        _ctx: &Caller,
        _input: permissions::WatchInput,
    ) -> Result<
        permissions::EventStream<permissions::Event>,
        permissions::ApiError<permissions::AdminWatchCodes>,
    > {
        panic!("refused before the method")
    }

    async fn watch_all(
        &self,
        _ctx: &Caller,
    ) -> Result<
        permissions::EventStream<permissions::Event>,
        permissions::ApiError<permissions::AdminWatchAllCodes>,
    > {
        panic!("refused before the method")
    }

    async fn report(
        &self,
        _ctx: &Caller,
        _input: permissions::WatchInput,
    ) -> Result<permissions::RawBody, permissions::ApiError<permissions::AdminReportCodes>> {
        panic!("refused before the method")
    }

    async fn manifest(
        &self,
        _ctx: &Caller,
    ) -> Result<permissions::RawBody, permissions::ApiError<permissions::AdminManifestCodes>> {
        panic!("refused before the method")
    }

    async fn ingest(
        &self,
        _ctx: &Caller,
        _input: permissions::IngestInput,
    ) -> Result<
        permissions::ApiResponse<permissions::IngestResult>,
        permissions::ApiError<permissions::AdminIngestCodes>,
    > {
        panic!("refused before the method")
    }

    async fn convert(
        &self,
        _ctx: &Caller,
        _input: permissions::IngestInput,
    ) -> Result<permissions::RawBody, permissions::ApiError<permissions::AdminConvertCodes>> {
        panic!("refused before the method")
    }

    async fn upload(
        &self,
        _ctx: &Caller,
        _input: permissions::IngestInput,
        _body: permissions::RawRequest,
    ) -> Result<
        permissions::ApiResponse<permissions::IngestResult>,
        permissions::ApiError<permissions::AdminUploadCodes>,
    > {
        panic!("refused before the method")
    }
}

/// Mounting every handler, which is what discharges axum's `Handler` bound.
pub fn router() -> axum::Router {
    server::files_router(Files).merge(permissions::admin_router(Admin))
}

/// Never called: compiling the body is the assertion.
///
/// The client forms differ in their signatures rather than their behaviour — a
/// raw response answers with `reqwest::Response`, a raw request takes the
/// content type as a parameter, a no-input operation takes nothing — so naming
/// each correctly is the whole check, and running it would need a server on the
/// other end to prove nothing extra.
async fn client_forms(api: &client::ApiClient) {
    let convert = types::ConvertInput { id: uuid::Uuid::nil(), body: Vec::new() };
    let ingest = types::IngestInput { id: uuid::Uuid::nil(), tags: None, body: Vec::new() };
    let report = types::ReportInput { id: uuid::Uuid::nil(), rows: None };
    let upload = types::UploadInput { id: uuid::Uuid::nil(), body: Vec::new() };
    let watch = types::WatchInput { id: uuid::Uuid::nil() };

    let files = api.files();
    let _ = files.convert(&convert, "text/csv").await;
    // A streamed operation is a raw request to the client: the server alone
    // decides not to buffer it.
    let _ = files.echo(&convert, "application/octet-stream").await;
    let _ = files.ingest(&ingest, "text/csv").await;
    let _ = files.manifest().await;
    let _ = files.report(&report).await;
    let _ = files.upload(&upload, "application/octet-stream").await;
    let _ = files.watch(&watch).await;
}

#[cfg(test)]
mod wire {
    use treat::rpc::WireEnum;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    /// Past the buffered bound by a margin, so a handler that still buffered
    /// would answer `invalid_body` rather than the service's result.
    const LARGE: usize = crate::server::RAW_BODY_LIMIT + 1024 * 1024 + 7;

    const ID: &str = "00000000-0000-0000-0000-000000000000";

    /// The router on an ephemeral local port, and the base URL reaching it.
    async fn serve() -> TestResult<String> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        tokio::spawn(async move { axum::serve(listener, super::router()).await });
        // The tables the lookups search are ones they can search.
        assert_eq!(treat::rpc::route::check_table(crate::types::operations::ALL), Ok(()));
        assert_eq!(treat::rpc::route::check_table(crate::permissions::operations::ALL), Ok(()));
        Ok(format!("http://{addr}"))
    }

    fn body(len: usize) -> Vec<u8> {
        (0..len).map(super::pattern).collect()
    }

    /// A body with a `Content-Length` larger than any buffered body may be,
    /// read whole by the service.
    #[tokio::test]
    async fn a_large_body_reaches_the_service_whole() -> TestResult {
        let base = serve().await?;
        let response = reqwest::Client::new()
            .post(format!("{base}/upload?id={ID}"))
            .header("content-type", "application/octet-stream")
            .body(body(LARGE))
            .send()
            .await?;
        assert_eq!(response.status(), 200);
        let json: serde_json::Value = response.json().await?;
        assert_eq!(json["data"]["received"], LARGE, "{json}");
        assert_eq!(json["data"]["declared"], LARGE, "{json}");
        assert_eq!(json["data"]["intact"], true, "{json}");
        Ok(())
    }

    /// A tagged code reaches the envelope by its tag, and a client reads the
    /// tag back as the registry value.
    #[tokio::test]
    async fn a_tagged_code_travels_by_its_tag() -> TestResult {
        use axum::response::IntoResponse;

        let error: crate::server::ApiError<crate::server::FilesReportCodes> =
            crate::server::error(crate::server::FilesReportCodes::FilesGone);
        let body = axum::body::to_bytes(error.into_response().into_body(), usize::MAX).await?;
        let json: serde_json::Value = serde_json::from_slice(&body)?;
        assert_eq!(json["errors"][0]["code"], "files::gone", "{json}");

        let code: crate::types::ErrorCode = serde_json::from_value(json["errors"][0]["code"].clone())?;
        assert_eq!(code, crate::types::ErrorCode::files__gone);
        assert_eq!(crate::types::ErrorCode::files__gone.to_string(), "files::gone");
        Ok(())
    }

    /// The query string is still the input, and still rejected as one.
    #[tokio::test]
    async fn a_bad_query_is_invalid_body() -> TestResult {
        let base = serve().await?;
        let response = reqwest::Client::new()
            .post(format!("{base}/upload?id=nope"))
            .header("content-type", "application/octet-stream")
            .body(body(10))
            .send()
            .await?;
        // At the envelope's `200`, as an RPC route answers an input it cannot
        // read: not the `415` of a type it does not take.
        assert_eq!(response.status(), 200);
        let json: serde_json::Value = response.json().await?;
        assert_eq!(json["errors"][0]["code"], "invalid_body", "{json}");
        Ok(())
    }

    /// A raw request is read only under a type its `@raw(request:)` lists —
    /// the generated list, checked before the body is: a buffered one and a
    /// streamed one alike.
    #[tokio::test]
    async fn a_raw_body_of_an_undeclared_type_is_refused() -> TestResult {
        let base = serve().await?;
        let http = reqwest::Client::new();
        for (path, declared) in [
            ("ingest", "text/csv"),
            ("ingest", "application/json"),
            ("upload", "application/octet-stream"),
        ] {
            let response = http
                .post(format!("{base}/{path}?id={ID}"))
                .header("content-type", declared)
                .body("a,b")
                .send()
                .await?;
            assert_eq!(response.status(), 200, "{path} under {declared}");
        }
        for (path, other) in [("ingest", "text/plain"), ("upload", "text/csv")] {
            let response = http
                .post(format!("{base}/{path}?id={ID}"))
                .header("content-type", other)
                .body("a,b")
                .send()
                .await?;
            assert_eq!(response.status(), 415, "{path} under {other}");
            let json: serde_json::Value = response.json().await?;
            assert_eq!(json["errors"][0]["code"], "invalid_body", "{json}");
        }
        Ok(())
    }

    /// The permission check runs before the method: a held permission reaches
    /// the service, a missing one is answered by the implementation's refusal.
    #[tokio::test]
    async fn a_permission_is_checked_before_the_method() -> TestResult {
        let base = serve().await?;
        let client = reqwest::Client::new();
        let held = client.post(format!("{base}/purge")).json(&serde_json::json!({ "id": ID })).send().await?;
        assert_eq!(held.status(), 200);
        let json: serde_json::Value = held.json().await?;
        assert_eq!(json["data"]["written"], 1, "{json}");

        let missing = client.post(format!("{base}/wipe")).json(&serde_json::json!({ "id": ID })).send().await?;
        assert_eq!(missing.status(), 403);
        let json: serde_json::Value = missing.json().await?;
        assert_eq!(json["errors"][0]["code"], "permission_required", "{json}");
        assert_eq!(json["errors"][0]["message"], "files.wipe", "{json}");

        // What a caller other than the router reads instead of the guard: each
        // operation's check by its id, and no row for an id the API lacks.
        let check = |id: &str| crate::permissions::operations::by_id(id).map(|op| op.permission);
        assert_eq!(check("admin_purge"), Some(Some(crate::permissions::Permission::FilesPurge)));
        assert_eq!(check("admin_wipe"), Some(Some(crate::permissions::Permission::FilesWipe)));
        assert_eq!(check("admin_wip"), None);

        // And by the route a gate in front of the router reads off the request:
        // the method counts, and a route the router does not mount has no row.
        use crate::server::operations::{by_route, Auth, Kind};
        let echo = by_route("POST", "/echo").ok_or("the router mounts POST /echo")?;
        assert_eq!(echo.id, "files_echo");
        assert_eq!((echo.kind, echo.auth), (Kind::Mutation, Auth::Session));
        assert_eq!(echo.role, Some(crate::server::Role::FilesRead));
        assert_eq!(echo.scope, Some(crate::server::Scope::Write));
        let watch = by_route("GET", "/watch").ok_or("the router mounts GET /watch")?;
        assert_eq!((watch.kind, watch.auth, watch.scope), (Kind::Subscription, Auth::Public, None));
        assert!(by_route("POST", "/watch").is_none());
        // axum answers HEAD on a GET route; the table does not, so a gate refuses it.
        assert!(by_route("HEAD", "/watch").is_none());
        assert!(by_route("POST", "/nowhere").is_none());
        let upload = by_route("POST", "/upload").ok_or("the router mounts POST /upload")?;
        assert_eq!((upload.role, upload.scope), (None, None));
        assert_eq!(crate::server::Scope::from_name("files.purge"), Some(crate::server::Scope::Purge));
        assert_eq!(crate::server::Scope::Purge.as_str(), "files.purge");

        // The body is read after the check: a caller without the permission
        // hears that, never what its body got wrong; with it, the same body is
        // the `invalid_body` it is.
        for (path, code) in [("wipe", "permission_required"), ("purge", "invalid_body")] {
            let response = client
                .post(format!("{base}/{path}"))
                .header("content-type", "application/json")
                .body("{ not json")
                .send()
                .await?;
            let json: serde_json::Value = response.json().await?;
            assert_eq!(json["errors"][0]["code"], code, "{path}: {json}");
        }

        // Every other handler form refuses the same way, before its query
        // string or body is read: none of these sends a valid one, and every
        // method panics if reached.
        for (method, path) in [
            ("POST", "/admin/no_input"),
            ("GET", "/admin/watch"),
            ("GET", "/admin/watch_all"),
            ("POST", "/admin/report"),
            ("POST", "/admin/manifest"),
            ("POST", "/admin/ingest"),
            ("POST", "/admin/convert"),
            ("POST", "/admin/upload"),
        ] {
            let method = reqwest::Method::from_bytes(method.as_bytes())?;
            let response = client
                .request(method, format!("{base}{path}"))
                .header("content-type", "application/json")
                .body("{ not json")
                .send()
                .await?;
            assert_eq!(response.status(), 403, "{path}");
            let json: serde_json::Value = response.json().await?;
            assert_eq!(json["errors"][0]["code"], "permission_required", "{path}: {json}");
            assert_eq!(json["errors"][0]["message"], "files.wipe", "{path}: {json}");
        }
        Ok(())
    }

    /// The generated client against the generated router, one call per form:
    /// what each method sends is what its handler reads — the path, the
    /// query table, the body and its declared type — and each answer is read
    /// back as the form says.
    #[tokio::test]
    async fn the_generated_client_reaches_every_form() -> TestResult {
        use treat::rpc::futures_util::StreamExt;

        let base = serve().await?;
        let api = crate::client::ApiClient::new(&base);
        let files = api.files();
        let id = uuid::Uuid::nil();

        let events: Vec<_> = files.watch(&crate::types::WatchInput { id }).await?.collect().await;
        assert!(events.is_empty());

        let report = files.report(&crate::types::ReportInput { id, rows: Some(3) }).await?;
        assert_eq!(
            report.headers().get("content-disposition").map(|v| v.as_bytes()),
            Some(&b"attachment; filename=\"report.csv\""[..])
        );
        let manifest = files.manifest().await?;
        assert_eq!(manifest.bytes().await?.as_ref(), b"{}");

        let ingest = crate::types::IngestInput { id, tags: Some(vec!["a b".to_string()]), body: b"x,y".to_vec() };
        let written = files.ingest(&ingest, "text/csv").await?;
        assert_eq!(written.written, 3);
        let refused = files.ingest(&ingest, "text/plain").await;
        assert!(matches!(refused, Err(crate::client::ClientError::Api(_))), "{refused:?}");

        let convert = crate::types::ConvertInput { id, body: br#"{"k":1}"#.to_vec() };
        let converted = files.convert(&convert, "text/csv").await?;
        assert_eq!(converted.bytes().await?.as_ref(), br#"{"k":1}"#);

        let upload = crate::types::UploadInput { id, body: super::pattern_bytes(1000) };
        let uploaded = files.upload(&upload, "application/octet-stream").await?;
        assert_eq!((uploaded.received, uploaded.intact), (1000, true));

        let echoed = files.echo(&convert, "application/octet-stream").await?;
        assert_eq!(echoed.bytes().await?.as_ref(), br#"{"k":1}"#);
        Ok(())
    }

    /// The plain RPC forms, by the generated client: an input and none, a
    /// success and a refusal read from `errors[]` against the operation's set.
    #[tokio::test]
    async fn the_generated_client_calls_rpc_operations() -> TestResult {
        let base = serve().await?;
        let admin = crate::permissions_client::ApiClient::new(&base);
        let input = crate::permissions::WatchInput { id: uuid::Uuid::nil() };

        let purged = admin.admin().purge(&input).await?;
        assert_eq!(purged.written, 1);

        let wiped = admin.admin().wipe(&input).await;
        match wiped {
            Err(crate::permissions_client::ClientError::Api(errors)) => {
                assert!(errors.is(crate::permissions::AdminWipeCodes::PermissionRequired));
                assert_eq!(
                    errors.code::<crate::permissions::ErrorCode>(),
                    Some(crate::permissions::ErrorCode::permission_required)
                );
            }
            other => return Err(format!("expected the refusal, got {other:?}").into()),
        }
        let all = admin.admin().wipe_all().await;
        assert!(matches!(all, Err(crate::permissions_client::ClientError::Api(_))), "{all:?}");
        Ok(())
    }

    /// A streamed request answering with raw bytes, through the context
    /// extractor.
    #[tokio::test]
    async fn a_streamed_raw_response_echoes_the_body() -> TestResult {
        let base = serve().await?;
        let sent = body(LARGE);
        let response = reqwest::Client::new()
            .post(format!("{base}/echo?id={ID}"))
            .header("content-type", "application/octet-stream")
            .body(sent.clone())
            .send()
            .await?;
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers().get("content-type").map(|v| v.as_bytes()),
            Some(&b"application/octet-stream"[..])
        );
        assert!(response.bytes().await? == sent, "the echo must be the body sent");
        Ok(())
    }
}
"##;

/// The three Rust targets, emitted into one crate.
///
/// `types_path` points at a module of the same crate rather than a sibling one,
/// which is the single-crate layout a scratch project wants; the split layout is
/// what every other test already covers.
fn generated() -> TestResult<(String, String, String, String, String)> {
    let api = graphql_rpcgen::compile_str(SDL)?;
    let mut config = graphql_rpcgen::config::Config::default();
    config.rust.types_path = Some("crate::types".to_string());
    config.rust_client.types_path = Some("crate::types".to_string());

    // Self-contained, its types inline: nothing else in the crate shares them.
    // Its client reaches them there, and carries the plain RPC forms.
    let permissions = graphql_rpcgen::compile_str(PERMISSIONS_SDL)?;
    let mut permissions_client = graphql_rpcgen::config::Config::default();
    permissions_client.rust_client.types_path = Some("crate::permissions".to_string());

    Ok((
        graphql_rpcgen::generate_rust::generate_types(&api, &config)?,
        graphql_rpcgen::generate_rust::generate(&api, &config)?,
        graphql_rpcgen::generate_rust_client::generate(&api, &config)?,
        graphql_rpcgen::generate_rust::generate(
            &permissions,
            &graphql_rpcgen::config::Config::default(),
        )?,
        graphql_rpcgen::generate_rust_client::generate(&permissions, &permissions_client)?,
    ))
}

/// The scratch project, and what the generated code reaches for: `treat`'s
/// server and client runtime with `rpc-stream`, which the raw and streaming
/// forms need and nothing else does. No `futures-*`, `bytes` or `reqwest`
/// `stream` feature: the generated code names none of them, and the driver
/// reaches `futures_util` through `treat::rpc`. `reqwest` and `tokio` are the
/// driver's, for requests the generated client would never send.
///
/// `--lib`: the emitted doc comments are documentation, not doctests.
const PROJECT: common::ScratchProject = common::ScratchProject {
    name: "raw_wire",
    dir: "raw-wire-test",
    dependencies: &[
        Dependency::plain("async-trait"),
        Dependency::plain("axum"),
        Dependency::with("chrono", &["serde"]),
        Dependency::with("derive_more", &["from", "into", "display", "as_ref"]),
        Dependency::plain("error_set"),
        Dependency::only("reqwest", &["json"]),
        Dependency::with("serde", &["derive"]),
        Dependency::plain("serde_json"),
        Dependency::with("tokio", &["macros", "net", "rt"]),
        // `rpc-status-header` changes no line of the generated code, but is
        // what emits the `x-rpc-status` the OpenAPI document describes, so the
        // README names it and this set matches the README.
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
    ],
    test_args: &["--lib"],
    target: None,
    edition: "2021",
};

#[test]
fn the_generated_raw_server_and_client_compile_and_serve() -> TestResult {
    let (types, server, client, permissions, permissions_client) = generated()?;
    let lib = "pub mod client;\nmod driver;\npub mod permissions;\npub mod permissions_client;\n\
               pub mod server;\npub mod types;\n";
    PROJECT.check(
        &[
            ("types.rs", &types),
            ("server.rs", &server),
            ("client.rs", &client),
            ("permissions.rs", &permissions),
            ("permissions_client.rs", &permissions_client),
            ("driver.rs", DRIVER),
            ("lib.rs", lib),
        ],
        &[
            "types.rs",
            "server.rs",
            "client.rs",
            "permissions.rs",
            "permissions_client.rs",
        ],
    )
}
