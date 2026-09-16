//! Tests for the settings that make the generator reusable across projects.
//!
//! The claim under test is that another project can retarget graphql-rpcgen
//! without editing it: its own scalars, its own output paths, its own
//! templates.

/// What a test body answers with: the first failure ends it, carrying the
/// message of whatever actually went wrong rather than a fixed one.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

use std::path::PathBuf;

use graphql_rpcgen::config::{Config, Output};

/// SDL using only the standard scalars, so it compiles with no config at all.
const PLAIN_SDL: &str = r#"
input GetInput { id: ID! }
type GetOutput { id: ID!, name: String }

type Query @service {
  get(input: GetInput!): GetOutput!
    @rpc(path: "/get")
    @query
    @throws(codes: ["not_found"])
}
"#;

/// The error registry every SDL must carry, plus what these cases throw.
const REGISTRY: &str = r#"
enum ErrorCode { invalid_body internal_error not_found }
"#;

fn sdl(body: &str) -> String {
    format!("{REGISTRY}{body}")
}

fn scratch() -> TestResult<PathBuf> {
    let dir = std::env::temp_dir().join(format!("rpcgen-reuse-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// The headline claim: a project with no `rpcgen.toml` still generates.
#[test]
fn a_default_config_generates_from_standard_scalars_alone() -> TestResult {
    let config = Config::default();
    let api = graphql_rpcgen::compile_str(&sdl(PLAIN_SDL))?;

    let rust = graphql_rpcgen::generate_rust::generate(&api, &config)?;
    assert!(rust.contains("pub trait QueryService"));
    assert!(rust.contains("async fn get("));
    Ok(())
}

/// A domain scalar is an SDL declaration, not a generator change.
#[test]
fn a_project_scalar_reaches_every_target() -> TestResult {
    let config = Config::default();

    let api = graphql_rpcgen::compile_str(&sdl(r#"
scalar AccountId
  @scalar(rust: "u64", typescript: "number", openapiType: "integer", openapiFormat: "int64")
input GetInput { id: AccountId! }
type GetOutput { id: AccountId! }
type Query @service {
  get(input: GetInput!): GetOutput! @rpc(path: "/get") @query
}
"#))?;

    let rust = graphql_rpcgen::generate_rust::generate(&api, &config)?;
    assert!(rust.contains("pub id: u64,"), "rust mapping applied");

    let ts = graphql_rpcgen::generate_typescript::generate(&api);
    assert!(ts.contains("id: number"), "typescript mapping applied");

    let openapi = graphql_rpcgen::generate_openapi::generate(&api, &config)?;
    assert!(openapi.contains("int64"), "openapi mapping applied");
    Ok(())
}

/// An SDL scalar with no mapping is an error rather than a silent passthrough,
/// because emitting `pub id: Money` would fail to compile far from the cause.
#[test]
fn an_unmapped_scalar_is_rejected_with_a_useful_message() -> TestResult {
    let Err(err) = graphql_rpcgen::compile_str(&sdl("scalar Money\n")) else {
        return Err("an unmapped scalar must fail".into());
    };

    let message = err.to_string();
    assert!(message.contains("Money"), "got: {message}");
    assert!(message.contains("@scalar"), "must name the fix: {message}");
    Ok(())
}

/// A built-in may be retargeted from the SDL without touching the generator.
#[test]
fn an_sdl_scalar_overrides_a_builtin() -> TestResult {
    let api = graphql_rpcgen::compile_str(&sdl(r#"
scalar DateTime @scalar(rust: "time::OffsetDateTime", typescript: "string", openapiType: "string")
type GetOutput { at: DateTime! }
input GetInput { id: ID! }
type Query @service {
  get(input: GetInput!): GetOutput! @rpc(path: "/get") @query
}
"#))?;

    let rust = graphql_rpcgen::generate_rust::generate(&api, &Config::default())?;
    assert!(rust.contains("pub at: time::OffsetDateTime,"), "{rust}");
    assert!(
        !rust.contains("chrono::DateTime"),
        "the built-in must be replaced"
    );
    Ok(())
}

/// `rustCopy` on a non-newtype would silently do nothing, so it is rejected.
#[test]
fn rust_copy_without_a_newtype_is_rejected() -> TestResult {
    let Err(err) = graphql_rpcgen::compile_str(&sdl(
        "scalar Weird @scalar(rust: \"String\", typescript: \"string\", rustCopy: true)\n",
    )) else {
        return Err("`rustCopy` without `rustNewtype` must fail".into());
    };

    assert!(err.to_string().contains("rustCopy"), "got: {err}");
    Ok(())
}

/// A project generating only a client should not be forced to emit a server.
#[test]
fn a_target_without_an_output_path_is_not_generated() -> TestResult {
    let config = Config {
        output: Output {
            typescript: Some(PathBuf::from("client.ts")),
            ..Output::default()
        },
        ..Config::default()
    };

    let api = graphql_rpcgen::compile_str(&sdl(PLAIN_SDL))?;
    let files = graphql_rpcgen::generate_all(&api, &config)?;

    assert_eq!(files.len(), 1, "only the configured target is emitted");
    assert_eq!(files[0].path, PathBuf::from("client.ts"));
    Ok(())
}

/// The point of splitting the Rust target: a crate that only calls the API
/// must not be made to compile a web framework.
#[test]
fn the_types_target_carries_no_web_framework() -> TestResult {
    let api = graphql_rpcgen::compile_str(&sdl(PLAIN_SDL))?;
    let types = graphql_rpcgen::generate_rust::generate_types(&api, &Config::default())?;

    assert!(types.contains("pub struct GetOutput"), "types are emitted");
    assert!(!types.contains("axum"), "no web framework: {types}");
    assert!(!types.contains("Router"), "no router in the types target");
    assert!(
        !types.contains("pub trait QueryService"),
        "service traits belong to the server target"
    );
    Ok(())
}

/// With the types in their own module the server imports them; without it the
/// server is self-contained, which is the pre-split layout.
#[test]
fn the_server_emits_types_only_when_they_have_no_module_of_their_own() -> TestResult {
    let api = graphql_rpcgen::compile_str(&sdl(PLAIN_SDL))?;

    let single_file = graphql_rpcgen::generate_rust::generate(&api, &Config::default())?;
    assert!(
        single_file.contains("pub struct GetOutput"),
        "with no types_path the server carries the types itself"
    );

    let mut config = Config::default();
    config.rust.types_path = Some("api_types::generated".to_string());
    let split = graphql_rpcgen::generate_rust::generate(&api, &config)?;

    assert!(
        !split.contains("pub struct GetOutput"),
        "the types are not duplicated into the server"
    );
    // `pub use`, not `use`: a consumer globbing the server module must still
    // see the types, so splitting the crate does not break callers.
    assert!(
        split.contains("pub use api_types::generated::*;"),
        "the server re-exports the types: {split}"
    );
    assert!(split.contains("pub trait QueryService"));
    Ok(())
}

#[test]
fn the_client_emits_a_method_per_operation() -> TestResult {
    let api = graphql_rpcgen::compile_str(&sdl(PLAIN_SDL))?;
    let client = graphql_rpcgen::generate_rust_client::generate(&api, &Config::default())?;

    assert!(client.contains("pub struct ApiClient"));
    // Operations are grouped by service, as in the TypeScript client.
    assert!(client.contains("pub fn query(&self) -> QueryOperations<'_>"));
    assert!(
        client.contains(
            "pub async fn get(&self, input: &GetInput) -> ClientResult<GetOutput, QueryGetCodes>"
        ),
        "{client}"
    );
    assert!(client.contains("self.client.call(\"/get\", input).await"));
    // The client talks HTTP; it must not carry the server's bindings.
    assert!(!client.contains("axum"), "no web framework in the client");
    assert!(!client.contains("pub trait QueryService"));
    Ok(())
}

/// `@throws` codes are the client's branching surface, so they belong in the
/// method's docs rather than only in the SDL.
#[test]
fn client_methods_document_the_codes_they_report() -> TestResult {
    let api = graphql_rpcgen::compile_str(&sdl(PLAIN_SDL))?;
    let client = graphql_rpcgen::generate_rust_client::generate(&api, &Config::default())?;

    assert!(client.contains("/// # Errors"), "{client}");
    assert!(client.contains("/// - `not_found`"), "{client}");
    Ok(())
}

/// The client falls back to the server's `types_path` so a project states the
/// types crate once.
#[test]
fn the_client_imports_types_from_the_shared_path() -> TestResult {
    let api = graphql_rpcgen::compile_str(&sdl(PLAIN_SDL))?;

    let mut config = Config::default();
    config.rust.types_path = Some("api_types::generated".to_string());
    let client = graphql_rpcgen::generate_rust_client::generate(&api, &config)?;

    assert!(client.contains("use api_types::generated::*;"), "{client}");
    assert!(
        !client.contains("pub struct GetOutput"),
        "types come from the shared crate, not a second copy"
    );
    Ok(())
}

/// The envelope is `treat`'s, so the client decodes the type the server encodes
/// rather than a copy that can drift from it.
#[test]
fn the_client_reuses_the_treat_envelope() -> TestResult {
    let api = graphql_rpcgen::compile_str(&sdl(PLAIN_SDL))?;
    let client = graphql_rpcgen::generate_rust_client::generate(&api, &Config::default())?;

    assert!(
        client.contains("pub use treat::{ErrorMessage, ErrorSource};"),
        "{client}"
    );
    assert!(client.contains("= treat::ApiResponse<T,"), "{client}");
    for copy in [
        "pub struct ErrorMessage",
        "pub struct ErrorSource",
        "pub struct ApiResponse",
    ] {
        assert!(
            !client.contains(copy),
            "`{copy}` must come from treat, not a copy"
        );
    }
    Ok(())
}

/// SDL for an operation reading everything from the session.
const NO_INPUT_SDL: &str = r#"
type MeOutput { id: ID! }

type Query @service {
  me: MeOutput! @rpc(path: "/me") @query
}
"#;

/// An operation with no `input` takes no parameter anywhere, but the body it
/// sends is unchanged: the wire format must not depend on this.
#[test]
fn an_operation_without_input_emits_no_parameter() -> TestResult {
    let api = graphql_rpcgen::compile_str(&sdl(NO_INPUT_SDL))?;
    let config = Config::default();

    let server = graphql_rpcgen::generate_rust::generate(&api, &config)?;
    assert!(
        server.contains(
            "async fn me(&self) -> Result<ApiResponse<MeOutput>, ApiError<QueryMeCodes>>;"
        ),
        "the trait method takes no input: {server}"
    );
    // The body is still validated — a malformed one must be `invalid_body`
    // rather than reaching the handler — but nothing is bound from it.
    assert!(
        server.contains("ApiJson(_): ApiJson<serde::de::IgnoredAny>"),
        "the body is validated and discarded: {server}"
    );
    assert!(
        server.contains("match service.me().await"),
        "the trait call passes no input: {server}"
    );

    let client = graphql_rpcgen::generate_rust_client::generate(&api, &config)?;
    assert!(
        client.contains("pub async fn me(&self) -> ClientResult<MeOutput, QueryMeCodes>"),
        "{client}"
    );
    // The empty object still goes on the wire, so the server sees no change.
    assert!(
        client.contains(r#"self.client.call("/me", &serde_json::json!({})).await"#),
        "{client}"
    );

    let ts = graphql_rpcgen::generate_typescript::generate(&api);
    assert!(
        ts.contains("me: (options?: RpcCallOptions): Promise<MeOutput> =>"),
        "{ts}"
    );
    assert!(
        ts.contains("client.call<MeOutput>('/me', {}, options)"),
        "{ts}"
    );

    // The request body is documented as the empty object it actually is.
    let openapi = graphql_rpcgen::generate_openapi::generate(&api, &config)?;
    let doc: serde_json::Value = serde_json::from_str(&openapi)?;
    let schema =
        &doc["paths"]["/me"]["post"]["requestBody"]["content"]["application/json"]["schema"];
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["additionalProperties"], false);
    Ok(())
}

/// An unconstrained `JSON` scalar must still declare what it can be.
///
/// OpenAPI 3.1 reads a typeless schema as "anything", which is correct but
/// leaves Swagger UI rendering the field as a string. Listing the permitted
/// types keeps the meaning and fixes the rendering.
#[test]
fn an_arbitrary_json_scalar_lists_its_permitted_types() -> TestResult {
    let api = graphql_rpcgen::compile_str(&sdl(r#"
input GetInput { id: ID! }
type GetOutput { blob: JSON! }
type Query @service {
  get(input: GetInput!): GetOutput! @rpc(path: "/get") @query
}
"#))?;

    let openapi = graphql_rpcgen::generate_openapi::generate(&api, &Config::default())?;
    let doc: serde_json::Value = serde_json::from_str(&openapi)?;
    let blob = &doc["components"]["schemas"]["GetOutput"]["properties"]["blob"];

    let types = blob["type"].as_array().ok_or("a type list: {blob}")?;
    for expected in ["object", "array", "string", "number", "boolean", "null"] {
        assert!(
            types.iter().any(|t| t == expected),
            "`{expected}` must be permitted: {blob}"
        );
    }
    Ok(())
}

/// A project on another HTTP stack replaces the transport without forking.
#[test]
fn a_custom_client_template_replaces_the_builtin_transport() -> TestResult {
    let dir = scratch()?;
    let method = dir.join("client_method.mustache");
    std::fs::write(
        &method,
        "    pub fn {{{method}}}(&self) -> &'static str {\n        \"{{{path}}}\"\n    }\n",
    )?;

    let mut config = Config::default();
    config.rust_client.method_template = Some(method);

    let api = graphql_rpcgen::compile_str(&sdl(PLAIN_SDL))?;
    let client = graphql_rpcgen::generate_rust_client::generate(&api, &config)?;

    assert!(
        client.contains("pub fn get(&self) -> &'static str {"),
        "{client}"
    );
    assert!(
        !client.contains("self.client.call(\"/get\", input).await"),
        "the built-in method body must be replaced, not supplemented"
    );
    Ok(())
}

#[test]
fn output_paths_come_from_the_config() -> TestResult {
    let config = Config {
        output: Output {
            rust_types: Some(PathBuf::from("src/types.rs")),
            rust_server: Some(PathBuf::from("src/api.rs")),
            rust_client: Some(PathBuf::from("src/client.rs")),
            typescript: Some(PathBuf::from("web/api.ts")),
            typescript_zod: Some(PathBuf::from("web/validation.ts")),
            openapi: Some(PathBuf::from("docs/openapi.json")),
        },
        ..Config::default()
    };

    let api = graphql_rpcgen::compile_str(&sdl(PLAIN_SDL))?;
    let paths: Vec<PathBuf> = graphql_rpcgen::generate_all(&api, &config)?
        .into_iter()
        .map(|f| f.path)
        .collect();

    assert_eq!(
        paths,
        vec![
            PathBuf::from("src/types.rs"),
            PathBuf::from("src/api.rs"),
            PathBuf::from("src/client.rs"),
            PathBuf::from("web/api.ts"),
            PathBuf::from("web/validation.ts"),
            PathBuf::from("docs/openapi.json"),
        ]
    );
    Ok(())
}

#[test]
fn openapi_metadata_comes_from_the_config() -> TestResult {
    let config: Config = toml::from_str(
        r#"
        [openapi]
        title = "Billing API"
        version = "3.2.1"
        description = "Internal billing."
        servers = ["https://api.example.com", "/"]
        "#,
    )?;

    let api = graphql_rpcgen::compile_str(&sdl(PLAIN_SDL))?;
    let doc = graphql_rpcgen::generate_openapi::generate(&api, &config)?;

    assert!(doc.contains("\"title\": \"Billing API\""));
    assert!(doc.contains("\"version\": \"3.2.1\""));
    assert!(doc.contains("\"description\": \"Internal billing.\""));
    assert!(doc.contains("https://api.example.com"));
    Ok(())
}

/// The escape hatch for a project on another web stack.
#[test]
fn a_custom_template_replaces_the_builtin_handler() -> TestResult {
    let dir = scratch()?;
    let handler = dir.join("handler.mustache");
    std::fs::write(
        &handler,
        "\npub async fn {{{handler}}}(input: {{{input}}}) -> HttpResponse {\n    \
         svc.{{{method}}}(input).await.into()\n}\n",
    )?;

    let mut config = Config::default();
    config.rust.handler_template = Some(handler);

    let api = graphql_rpcgen::compile_str(&sdl(PLAIN_SDL))?;
    let rust = graphql_rpcgen::generate_rust::generate(&api, &config)?;

    assert!(rust.contains("pub async fn query_get(input: GetInput) -> HttpResponse {"));
    // The built-in axum handler must be gone, not merely supplemented.
    assert!(!rust.contains("ApiJson(input): ApiJson<GetInput>"));
    Ok(())
}

/// Custom values a project's own template can reference.
#[test]
fn template_vars_are_available_to_a_custom_template() -> TestResult {
    let dir = scratch()?;
    let router = dir.join("router.mustache");
    std::fs::write(&router, "\n// {{{framework}}} router for {{{service}}}\n")?;

    let mut config = Config::default();
    config.rust.router_template = Some(router);
    config
        .rust
        .template_vars
        .insert("framework".to_string(), "actix".to_string());

    let api = graphql_rpcgen::compile_str(&sdl(PLAIN_SDL))?;
    let rust = graphql_rpcgen::generate_rust::generate(&api, &config)?;

    assert!(rust.contains("// actix router for Query"));
    Ok(())
}

/// Mustache renders an unknown key as empty, which would emit broken Rust.
/// The generator refuses instead.
#[test]
fn a_typo_in_a_custom_template_fails_generation() -> TestResult {
    let dir = scratch()?;
    let handler = dir.join("typo.mustache");
    std::fs::write(&handler, "async fn {{{handlr}}}() {}\n")?;

    let mut config = Config::default();
    config.rust.handler_template = Some(handler);

    let api = graphql_rpcgen::compile_str(&sdl(PLAIN_SDL))?;
    let Err(err) = graphql_rpcgen::generate_rust::generate(&api, &config) else {
        return Err("a template naming an unknown variable must fail".into());
    };

    assert!(err.to_string().contains("handlr"), "got: {err}");
    Ok(())
}

/// `{{x}}` HTML-escapes, so `Vec<T>` would emit as `Vec&lt;T&gt;`.
#[test]
fn an_escaping_tag_in_a_custom_template_is_rejected() -> TestResult {
    let dir = scratch()?;
    let handler = dir.join("escaping.mustache");
    std::fs::write(&handler, "async fn f(i: {{input}}) {}\n")?;

    let mut config = Config::default();
    config.rust.handler_template = Some(handler);

    let api = graphql_rpcgen::compile_str(&sdl(PLAIN_SDL))?;
    let Err(err) = graphql_rpcgen::generate_rust::generate(&api, &config) else {
        return Err("an escaping tag must fail".into());
    };

    assert!(err.to_string().contains("HTML-escapes"), "got: {err}");
    Ok(())
}
