//! `@mcp`: which operations an MCP server built over the OpenAPI document
//! exposes as tools.
//!
//! The flag only surfaces in the OpenAPI document (`x-mcp: true`); the other
//! emitters are untouched. These pin the inheritance rule, the emission and the
//! rejections that keep a declaration from promising a call shape an operation
//! cannot honour.

/// What a test body answers with: the first failure ends it, carrying the
/// message of whatever actually went wrong rather than a fixed one.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

use graphql_rpcgen::config::Config;
use graphql_rpcgen::generate_openapi;

const PRELUDE: &str = r#"
scalar UUID

enum ErrorCode {
  invalid_body
  internal_error
}

input GetInput { id: UUID! }
type Thing { id: UUID! }
type Event { n: Int! }
"#;

const SDL: &str = r#"
type Exposed @service @mcp {
  "Inherits the service's rule."
  get(input: GetInput!): Thing! @query @rpc(path: "/get")

  "Carved out of an exposed service."
  hidden(input: GetInput!): Thing! @query @rpc(path: "/hidden") @mcp(expose: false)
}

type Closed @service {
  "Unannotated: not a tool."
  get(input: GetInput!): Thing! @query @rpc(path: "/closed-get")

  "Exposed on its own, against an unannotated service."
  make(input: GetInput!): Thing! @mutation @rpc(path: "/make") @mcp
}
"#;

fn openapi() -> TestResult<serde_json::Value> {
    let api = graphql_rpcgen::compile_str(&format!("{PRELUDE}{SDL}"))?;
    let out = generate_openapi::generate(&api, &Config::default())?;
    Ok(serde_json::from_str(&out)?)
}

fn expect_error(body: &str) -> TestResult<String> {
    match graphql_rpcgen::compile_str(&format!("{PRELUDE}{body}")) {
        Ok(_) => Err("expected a compile error, but compilation succeeded".into()),
        Err(e) => Ok(e.to_string()),
    }
}

/// The service's rule reaches every operation, and an operation's own rule
/// wins whole — in both directions.
#[test]
fn the_openapi_document_carries_the_flag() -> TestResult {
    let doc = openapi()?;

    assert_eq!(doc["paths"]["/get"]["post"]["x-mcp"], true);
    assert_eq!(doc["paths"]["/make"]["post"]["x-mcp"], true);

    // Absence, not `false`: an unexposed operation says nothing at all.
    assert!(doc["paths"]["/hidden"]["post"]
        .as_object()
        .ok_or("operation exists")?
        .get("x-mcp")
        .is_none());
    assert!(doc["paths"]["/closed-get"]["post"]
        .as_object()
        .ok_or("operation exists")?
        .get("x-mcp")
        .is_none());
    Ok(())
}

/// On a plain output type the directive would expose nothing.
#[test]
fn mcp_on_a_plain_type_is_rejected() -> TestResult {
    let err = expect_error("type Plain @mcp { id: UUID! }")?;
    assert!(err.contains("@mcp applies to a @service"), "{err}");
    Ok(())
}

/// A subscription is an open stream, not one request and one response.
#[test]
fn mcp_on_a_subscription_is_rejected() -> TestResult {
    let err = expect_error(
        r#"
type S @service @mcp {
  watch(input: GetInput!): Event! @subscription @rpc(path: "/watch")
}
"#,
    )?;
    assert!(err.contains("@subscription cannot be an MCP tool"), "{err}");
    Ok(())
}

/// A raw body is not the JSON envelope a tool call promises.
#[test]
fn mcp_on_a_raw_operation_is_rejected() -> TestResult {
    let err = expect_error(
        r#"
scalar Binary
  @scalar(rust: "Vec<u8>", typescript: "Blob", openapiType: "string", openapiFormat: "binary")

type R @service @mcp {
  export(input: GetInput!): Binary! @query @rpc(path: "/export") @raw(response: ["text/csv"])
}
"#,
    )?;
    assert!(
        err.contains("@raw operation cannot be an MCP tool"),
        "{err}"
    );
    Ok(())
}

/// An excluded subscription is fine: the rejection is about exposure, not
/// about the directive being present somewhere on the service.
#[test]
fn an_excluded_subscription_compiles() -> TestResult {
    let api = graphql_rpcgen::compile_str(&format!(
        "{PRELUDE}{}",
        r#"
type S @service @mcp {
  get(input: GetInput!): Thing! @query @rpc(path: "/s-get")
  watch(input: GetInput!): Event! @subscription @rpc(path: "/s-watch") @mcp(expose: false)
}
"#
    ))?;
    let service = &api.services[0];
    assert!(service.operations.iter().any(|o| o.name == "get" && o.mcp));
    assert!(service
        .operations
        .iter()
        .any(|o| o.name == "watch" && !o.mcp));
    Ok(())
}

/// A malformed argument is named, not defaulted over.
#[test]
fn a_non_boolean_expose_is_rejected() -> TestResult {
    let err = expect_error(
        r#"
type S @service {
  get(input: GetInput!): Thing! @query @rpc(path: "/s-get") @mcp(expose: "yes")
}
"#,
    )?;
    assert!(err.contains("@mcp(expose:) must be a boolean"), "{err}");
    Ok(())
}
