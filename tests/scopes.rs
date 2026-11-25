//! `@scopes` and `scope:` on `@query`, `@mutation` and `@subscription`: what a
//! delegated credential — an API token — must carry to call an operation, and
//! the operation table every target's consumers can look it up in.
//!
//! These pin the resolution — a query's default, a mutation's mandatory choice,
//! the operations no scope applies to — the table's shape and its lookups'
//! inputs, the OpenAPI and TypeScript markers, and every rejection that keeps a
//! scope from being decorative. A mutation that forgets its scope failing the
//! build above all: defaulted, it would be reachable by every token that writes.

#![allow(
    clippy::panic_in_result_fn,
    reason = "a test asserts, and returns `Result` only to use `?` on its setup"
)]

/// What a test body answers with: the first failure ends it, carrying the
/// message of whatever actually went wrong rather than a fixed one.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

mod common;

use common::{flat, operation_row};
use graphql_rpcgen::config::Config;
use graphql_rpcgen::{generate_openapi, generate_rust, generate_typescript};

/// Everything but the scope set.
const BASE: &str = r#"
scalar UUID

enum ErrorCode {
  invalid_body
  internal_error
  role_forbidden
}

input GetInput { id: UUID! }
type Thing { id: UUID! }

enum Permission @permissions(codes: ["role_forbidden"]) {
  files_admin @variant(tag: "files.admin")
}
"#;

/// A default for queries, and one scope whose wire name is not its identifier.
const SCOPE_SET: &str = r#"
"What a token may do on its owner's behalf."
enum TokenScope @scopes(query: read) {
  read
  write
  purge @variant(tag: "files.purge")
  secrets
}
"#;

/// One operation of each kind a scope resolves differently for.
const SDL: &str = r#"
type Files @service @auth(require: session) @version(n: 1) {
  "A query: the set's default."
  list(input: GetInput!): Thing! @query @rpc(path: "/files/list")

  "A query that reveals more than a read, saying so."
  keys(input: GetInput!): Thing! @query(scope: secrets) @rpc(path: "/files/keys")

  "A stream reads: the default too."
  watch(input: GetInput!): Thing! @subscription @rpc(path: "/files/watch")

  "A mutation: its own judgement."
  rename(input: GetInput!): Thing! @mutation(scope: write) @rpc(path: "/files/rename")
  purge(input: GetInput!): Thing! @mutation(scope: purge) @rpc(path: "/files/purge")

  "No credential, so no scope."
  ping(input: GetInput!): Thing! @query @rpc(path: "/files/ping") @auth(require: public)

  "Its permission decides what a credential reaches."
  wipe(input: GetInput!): Thing!
    @mutation
    @rpc(path: "/files/wipe")
    @auth(require: session, permission: files_admin)
    @throws(codes: ["role_forbidden"])
}

type Sync @service @auth(require: token) {
  "A token operation is scoped like a session one."
  push(input: GetInput!): Thing! @mutation(scope: write) @rpc(path: "/sync/push")
}
"#;

fn api() -> TestResult<graphql_rpcgen::ir::Api> {
    Ok(graphql_rpcgen::compile_str(&format!(
        "{BASE}{SCOPE_SET}{SDL}"
    ))?)
}

/// The compile error `body` must produce, with the base declarations in front.
fn expect_error(body: &str) -> TestResult<String> {
    match graphql_rpcgen::compile_str(&format!("{BASE}{body}")) {
        Ok(_) => Err("expected a compile error, but compilation succeeded".into()),
        Err(e) => Ok(e.to_string()),
    }
}

/// The table's rows, from the line after its head to the line before its end.
/// The rows of `operations::ALL`, flattened: `Operation { .. }, Operation { .. }`.
fn table(out: &str) -> TestResult<String> {
    flat(out)
        .split("pub const ALL: &[Operation] = &[")
        .nth(1)
        .and_then(|s| s.split("];").next())
        .map(str::to_string)
        .ok_or_else(|| format!("the operation table must be emitted: {out}").into())
}

/// Every operation's resolved scope: a query's default filled in, a stated one
/// kept, and none where no scope applies.
#[test]
fn each_operation_resolves_its_scope() -> TestResult {
    let api = api()?;
    let scope = |name: &str| -> TestResult<Option<String>> {
        let op = api
            .services
            .iter()
            .flat_map(|s| &s.operations)
            .find(|op| op.name == name)
            .ok_or_else(|| format!("no operation {name}"))?;
        Ok(op.scope.clone())
    };
    for (name, expected) in [
        ("list", Some("read")),
        ("keys", Some("secrets")),
        ("watch", Some("read")),
        ("rename", Some("write")),
        ("purge", Some("purge")),
        ("push", Some("write")),
        ("ping", None),
        ("wipe", None),
    ] {
        assert_eq!(scope(name)?.as_deref(), expected, "{name}");
    }
    Ok(())
}

/// One const table, sorted by path, with every column the API has a set for:
/// the method and path as the router mounts them, the version prefix included.
#[test]
fn the_operation_table_lists_every_operation_as_mounted() -> TestResult {
    let out = generate_rust::generate_types(&api()?, &Config::default())?;
    let row = |id: &str,
               method: &str,
               path: &str,
               kind: &str,
               auth: &str,
               permission: &str,
               scope: &str| {
        format!(
            "Operation {{ id: \"{id}\", method: \"{method}\", path: \"{path}\", kind: Kind::{kind}, \
             auth: Auth::{auth}, permission: {permission}, scope: {scope} }}"
        )
    };
    let expected = [
        // Byte order: `/sync` sorts before `/v1`.
        row(
            "sync_push",
            "POST",
            "/sync/push",
            "Mutation",
            "Token",
            "None",
            "Some(super::TokenScope::Write)",
        ),
        row(
            "files_keys",
            "POST",
            "/v1/files/keys",
            "Query",
            "Session",
            "None",
            "Some(super::TokenScope::Secrets)",
        ),
        row(
            "files_list",
            "POST",
            "/v1/files/list",
            "Query",
            "Session",
            "None",
            "Some(super::TokenScope::Read)",
        ),
        row(
            "files_ping",
            "POST",
            "/v1/files/ping",
            "Query",
            "Public",
            "None",
            "None",
        ),
        row(
            "files_purge",
            "POST",
            "/v1/files/purge",
            "Mutation",
            "Session",
            "None",
            "Some(super::TokenScope::Purge)",
        ),
        row(
            "files_rename",
            "POST",
            "/v1/files/rename",
            "Mutation",
            "Session",
            "None",
            "Some(super::TokenScope::Write)",
        ),
        row(
            "files_watch",
            "GET",
            "/v1/files/watch",
            "Subscription",
            "Session",
            "None",
            "Some(super::TokenScope::Read)",
        ),
        row(
            "files_wipe",
            "POST",
            "/v1/files/wipe",
            "Mutation",
            "Session",
            "Some(super::Permission::FilesAdmin)",
            "None",
        ),
    ]
    .join(", ");
    assert_eq!(table(&out)?, expected);

    // The lookups the table is for, by route and by id; a role column only
    // where a role set exists.
    assert!(
        out.contains("pub fn by_route(method: &str, path: &str) -> Option<&'static Operation> {")
    );
    assert!(out.contains("pub fn by_id(id: &str) -> Option<&'static Operation> {"));
    assert!(!out.contains("pub role:"), "{out}");
    Ok(())
}

/// The table is part of the wire types, which compile no web framework, and the
/// single-file server carries it once.
#[test]
fn the_operation_table_lives_with_the_types() -> TestResult {
    let api = api()?;
    let types = generate_rust::generate_types(&api, &Config::default())?;
    assert!(types.contains("pub mod operations {"));
    assert!(
        !types.contains("axum"),
        "the types name no framework: {types}"
    );

    let single = generate_rust::generate(&api, &Config::default())?;
    assert_eq!(single.matches("pub mod operations {").count(), 1);
    Ok(())
}

/// An API without a scope set has no scope column, and its mutations state none.
#[test]
fn without_a_scope_set_nothing_asks_for_one() -> TestResult {
    let api = graphql_rpcgen::compile_str(&format!(
        r#"{BASE}
type Open @service @auth(require: session) {{
  make(input: GetInput!): Thing! @mutation @rpc(path: "/make")
}}
"#
    ))?;
    let out = generate_rust::generate_types(&api, &Config::default())?;
    assert!(!out.contains("pub scope:"), "{out}");
    assert!(
        table(&out)?.ends_with("auth: Auth::Session, permission: None }"),
        "{out}"
    );

    let doc = generate_openapi::generate(&api, &Config::default())?;
    assert!(!doc.contains("x-scope"), "{doc}");
    let ts = generate_typescript::generate(&api);
    assert!(!ts.contains("export const scopes"), "{ts}");
    assert!(!ts.contains("scope:"), "{ts}");
    Ok(())
}

/// The set's runtime half reads and spells a credential's scope names, by wire
/// name.
#[test]
fn the_rust_scope_set_reads_and_spells_names() -> TestResult {
    let out = generate_rust::generate_types(&api()?, &Config::default())?;
    // `WireEnum` reads the name from the rename serde writes.
    assert!(
        out.contains("    #[serde(rename = \"files.purge\")]\n    Purge,\n"),
        "{out}"
    );
    assert!(
        out.contains("::serde::Serialize, ::serde::Deserialize, ::treat::rpc::WireEnum)]\npub enum TokenScope {"),
        "{out}"
    );
    Ok(())
}

/// OpenAPI marks each scoped operation by its scope's wire name, a query's
/// default included, and leaves an unscoped one unmarked.
#[test]
fn the_openapi_document_marks_each_operations_scope() -> TestResult {
    let doc: serde_json::Value =
        serde_json::from_str(&generate_openapi::generate(&api()?, &Config::default())?)?;
    let op = |method: &str, path: &str| doc["paths"][path][method].clone();
    assert_eq!(op("post", "/v1/files/list")["x-scope"], "read");
    assert_eq!(op("get", "/v1/files/watch")["x-scope"], "read");
    assert_eq!(op("post", "/v1/files/purge")["x-scope"], "files.purge");
    assert_eq!(op("post", "/sync/push")["x-scope"], "write");
    for path in ["/v1/files/ping", "/v1/files/wipe"] {
        assert!(
            op("post", path).get("x-scope").is_none(),
            "{path}: {}",
            op("post", path)
        );
    }
    Ok(())
}

/// The browser gets the set, for a screen that mints tokens, and each entry the
/// scope its call needs.
#[test]
fn the_typescript_api_carries_each_operations_scope() -> TestResult {
    let out = generate_typescript::generate(&api()?);
    assert!(
        out.contains(
            "export const scopes: readonly TokenScope[] = ['read', 'write', 'files.purge', 'secrets']\n"
        ),
        "{out}"
    );
    let entry = |op: &str| -> TestResult<&str> {
        // Past the `ops` and `keys` locals, whose per-service blocks would
        // otherwise be found first.
        out.split("export function createApi(")
            .nth(1)
            .and_then(|s| s.split("\n  return {").nth(1))
            .and_then(|s| s.split(&format!("      {op}: {{\n")).nth(1))
            .and_then(|s| s.split("\n      },").next())
            .ok_or_else(|| format!("createApi must carry {op}").into())
    };
    assert!(entry("purge")?.contains("        scope: 'files.purge' as const,\n"));
    assert!(entry("list")?.contains("        scope: 'read' as const,\n"));
    assert!(!entry("ping")?.contains("scope:"));
    Ok(())
}

/// One service operation under `rule`, declared as `kind`, with the set.
fn one_operation(kind: &str, rule: &str) -> String {
    format!(
        r#"{SCOPE_SET}
type S @service @auth(require: session) {{
  g(input: GetInput!): Thing! {kind} @rpc(path: "/g") {rule}
}}
"#
    )
}

/// The judgement a mutation makes cannot be skipped: forgetting it fails the
/// build rather than defaulting to the widest scope a token commonly holds.
#[test]
fn a_mutation_without_a_scope_is_rejected() -> TestResult {
    let e = expect_error(&one_operation("@mutation", ""))?;
    // Reported where the operation is declared, not as a bare message.
    let line = e
        .lines()
        .find(|line| line.contains("a @mutation states the scope"))
        .ok_or_else(|| format!("no such error: {e}"))?;
    assert!(
        line.trim_start().starts_with("- <memory>:26:3: `S.g`"),
        "{line}"
    );
    assert!(
        e.contains(
            "`S.g`: a @mutation states the scope a credential needs to call it, one of read, \
             write, purge, secrets: `@mutation(scope: …)`"
        ),
        "{e}"
    );
    // A token operation is held to it too.
    let e = expect_error(&format!(
        r#"{SCOPE_SET}
type T @service @auth(require: token) {{
  g(input: GetInput!): Thing! @mutation @rpc(path: "/g")
}}
"#
    ))?;
    assert!(e.contains("`T.g`: a @mutation states the scope"), "{e}");
    Ok(())
}

/// A set with no `query:` default asks every operation to state its scope.
#[test]
fn without_a_query_default_a_query_states_its_scope() -> TestResult {
    let e = expect_error(
        r#"
enum Scope @scopes { read write }
type S @service @auth(require: session) {
  g(input: GetInput!): Thing! @query @rpc(path: "/g")
  h(input: GetInput!): Thing! @query(scope: read) @rpc(path: "/h")
}
"#,
    )?;
    assert!(
        e.contains(
            "`S.g`: @query needs `scope:` — one of read, write — since the @scopes enum `Scope` \
             gives queries no `query:` default"
        ),
        "{e}"
    );
    assert!(!e.contains("`S.h`"), "{e}");
    Ok(())
}

/// A scope where no credential is narrowed by it would read as a gate that is
/// not there.
#[test]
fn a_scope_where_none_applies_is_rejected() -> TestResult {
    let e = expect_error(&one_operation(
        "@query(scope: secrets)",
        "@auth(require: public)",
    ))?;
    assert!(
        e.contains(
            "`S.g`: @query(scope: secrets) applies to no credential: a public operation is called \
             with none, so there is nothing for a scope to narrow; drop it"
        ),
        "{e}"
    );
    Ok(())
}

/// Beside a permission a scope is optional: an implementation whose permission
/// check judges only the user states one, and one whose check judges the
/// credential too need not. Neither kind gets the query default, since the
/// permission stands in for it.
#[test]
fn a_scope_beside_a_permission_is_optional() -> TestResult {
    let api = graphql_rpcgen::compile_str(&format!(
        r#"{BASE}{SCOPE_SET}
type Admin @service @auth(require: session, permission: files_admin) {{
  wipe(input: GetInput!): Thing! @mutation(scope: purge) @rpc(path: "/wipe") @throws(codes: ["role_forbidden"])
  audit(input: GetInput!): Thing! @query(scope: read) @rpc(path: "/audit") @throws(codes: ["role_forbidden"])
  stats(input: GetInput!): Thing! @query @rpc(path: "/stats") @throws(codes: ["role_forbidden"])
  reset(input: GetInput!): Thing! @mutation @rpc(path: "/reset") @throws(codes: ["role_forbidden"])
}}
"#
    ))?;
    let out = generate_rust::generate_types(&api, &Config::default())?;
    let row = |id: &str| -> TestResult<String> {
        Ok(operation_row(&out, id).ok_or_else(|| format!("no row for {id}"))?)
    };
    let permission = "permission: Some(super::Permission::FilesAdmin)";
    assert!(row("admin_wipe")?.ends_with(&format!(
        "{permission}, scope: Some(super::TokenScope::Purge) }}"
    )));
    assert!(row("admin_audit")?.ends_with(&format!(
        "{permission}, scope: Some(super::TokenScope::Read) }}"
    )));
    assert!(row("admin_stats")?.ends_with(&format!("{permission}, scope: None }}")));
    assert!(row("admin_reset")?.ends_with(&format!("{permission}, scope: None }}")));
    // The duty that comes with leaving it out is said where the column is.
    assert!(
        out.contains("`require_permission` must refuse a credential not allowed it"),
        "{out}"
    );

    let doc: serde_json::Value =
        serde_json::from_str(&generate_openapi::generate(&api, &Config::default())?)?;
    assert_eq!(doc["paths"]["/wipe"]["post"]["x-scope"], "files.purge");
    assert!(doc["paths"]["/stats"]["post"].get("x-scope").is_none());
    Ok(())
}

/// A query that takes a raw body in reads bytes from the caller, so it states
/// its scope rather than taking the read default.
#[test]
fn a_query_with_a_raw_request_states_its_scope() -> TestResult {
    let binary = r#"scalar Binary @scalar(rust: "Vec<u8>", typescript: "Blob", openapiType: "string", openapiFormat: "binary")
input BodyInput { id: UUID!, body: Binary! }
"#;
    let e = expect_error(&format!(
        r#"{binary}{SCOPE_SET}
type S @service @auth(require: session) {{
  probe(input: BodyInput!): Thing! @query @rpc(path: "/probe") @raw(request: ["text/csv"])
}}
"#
    ))?;
    assert!(
        e.contains(
            "`S.probe`: a @query with @raw(request:) takes a body in, so it states its scope rather \
             than taking the read default: one of read, write, purge, secrets"
        ),
        "{e}"
    );
    let api = graphql_rpcgen::compile_str(&format!(
        r#"{BASE}{binary}{SCOPE_SET}
type S @service @auth(require: session) {{
  probe(input: BodyInput!): Thing! @query(scope: read) @rpc(path: "/probe") @raw(request: ["text/csv"])
}}
"#
    ))?;
    let scope = api
        .services
        .iter()
        .flat_map(|s| &s.operations)
        .find_map(|op| op.scope.clone());
    assert_eq!(scope.as_deref(), Some("read"));
    Ok(())
}

/// A scope is a value of the set, written bare, and restating the default is
/// refused so there is one way to write each operation.
#[test]
fn a_malformed_scope_is_rejected() -> TestResult {
    for (kind, message) in [
        (
            "@mutation(scope: wrote)",
            "`S.g`: @mutation(scope: wrote) is not a value of the @scopes enum `TokenScope`",
        ),
        (
            r#"@mutation(scope: "write")"#,
            "`S.g`: @mutation(scope:) names a value of the @scopes enum, written bare: `scope: write`",
        ),
        (
            "@query(scope: read)",
            "`S.g`: @query(scope: read) is the default @scopes(query:) gives every query; drop the \
             argument",
        ),
        (
            "@subscription(scope: read)",
            "`S.g`: @subscription(scope: read) is the default @scopes(query:) gives every \
             subscription; drop the argument",
        ),
        ("@query(scop: read)", "directive `@query` has no argument `scop`; it takes `scope`"),
    ] {
        let e = expect_error(&one_operation(kind, ""))?;
        assert!(e.contains(message), "{kind}: {e}");
    }
    Ok(())
}

/// A scope needs a set to be a value of.
#[test]
fn a_scope_without_a_set_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
type S @service @auth(require: session) {
  g(input: GetInput!): Thing! @mutation(scope: write) @rpc(path: "/g")
}
"#,
    )?;
    assert!(
        e.contains("`S.g`: @mutation(scope: write) needs an enum marked @scopes listing it"),
        "{e}"
    );
    Ok(())
}

/// One set per API, apart from the roles, the permissions and the registry,
/// with a default that is one of its values.
#[test]
fn a_malformed_scope_set_is_rejected() -> TestResult {
    for (sdl, message) in [
        (
            format!("{SCOPE_SET}enum More @scopes {{ x }}\n"),
            "enum `More`: @scopes is already on `TokenScope`; an API has one scope set".to_string(),
        ),
        (
            "enum Both @permissions(codes: [\"role_forbidden\"]) @scopes { x }\n".to_string(),
            "enum `Both`: @scopes is the set a credential carries, apart from the roles and the \
             permissions; mark one"
                .to_string(),
        ),
        (
            "enum Scope @scopes(query: reed) { read }\n".to_string(),
            "enum `Scope`: @scopes(query: reed) is not one of its values".to_string(),
        ),
        (
            "enum Scope @scopes(query: \"read\") { read }\n".to_string(),
            "enum `Scope`: @scopes(query:) names one of its values, written bare: `query: read`"
                .to_string(),
        ),
        (
            "enum Scope @scopes { read write @inherits(roles: [read]) }\n".to_string(),
            "enum `Scope`: @inherits applies to a value of the @roles enum".to_string(),
        ),
    ] {
        let e = expect_error(&sdl)?;
        assert!(e.contains(&message), "{sdl}: {e}");
    }

    // The registry is checked against `BASE`'s own, so this one stands alone.
    let e =
        match graphql_rpcgen::compile_str("enum ErrorCode @scopes { invalid_body internal_error }")
        {
            Ok(_) => return Err("expected a compile error".into()),
            Err(e) => e.to_string(),
        };
    assert!(
        e.contains("`ErrorCode` is the error code registry; it cannot be @scopes"),
        "{e}"
    );
    Ok(())
}
