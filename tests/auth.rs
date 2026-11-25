//! `@auth`: who may call an operation, and what its handler hands the service.
//!
//! A requirement changes the trait signature, the handler's extractors and, for
//! a role, what runs before the method is reached. These pin all three, plus the
//! rejections that keep a declaration from being decorative.

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
use graphql_rpcgen::generate_rust;

/// Everything but the role set, for the tests that need an API without one.
const BASE: &str = r#"
scalar UUID

enum ErrorCode {
  invalid_body
  internal_error
  role_required
}

input GetInput { id: UUID! }
type Thing { id: UUID! }
type Event { n: Int! }
"#;

/// A hierarchy two levels deep, so a test can tell the closure from the direct
/// edges, and a role whose session name is not a GraphQL identifier.
const ROLE_SET: &str = r#"
enum Role @roles {
  content_translate
  content_edit @inherits(roles: [content_translate])
  content_manager @inherits(roles: [content_edit])
  content_release
  content_ops @variant(tag: "content-ops")
}
"#;

/// One service exercising every requirement against every handler form.
const SDL: &str = r#"
type Guarded @service @auth(require: session) {
  "Inherits the service's rule."
  get(input: GetInput!): Thing! @query @rpc(path: "/get")

  "A session and nothing to send: the context is the whole input."
  me: Thing! @query @rpc(path: "/me")

  "A role, checked before the method runs."
  start(input: GetInput!): Thing!
    @mutation
    @rpc(path: "/start")
    @auth(require: session, role: content_manager)
    @throws(codes: ["role_required"])

  "A stream still gets its caller."
  watch(input: GetInput!): Event! @subscription @rpc(path: "/watch")

  "Restates the rule as open, overriding the service."
  open(input: GetInput!): Thing! @query @rpc(path: "/open") @auth(require: public)

  "A caller with no browser, so no context: a token names no user."
  export(input: GetInput!): Thing! @query @rpc(path: "/export") @auth(require: token)
}
"#;

fn server() -> TestResult<String> {
    let api = graphql_rpcgen::compile_str(&format!("{BASE}{ROLE_SET}{SDL}"))?;
    Ok(generate_rust::generate(&api, &Config::default())?)
}

fn expect_error(body: &str) -> TestResult<String> {
    expect_error_in(&format!("{ROLE_SET}{body}"))
}

/// Like [`expect_error`], without the role set: for the tests about declaring
/// one.
fn expect_error_in(body: &str) -> TestResult<String> {
    match graphql_rpcgen::compile_str(&format!("{BASE}{body}")) {
        Ok(_) => Err("expected a compile error, but compilation succeeded".into()),
        Err(e) => Ok(e.to_string()),
    }
}

/// The context leads the parameter list: it is the caller rather than the
/// request, and it is what every session-guarded operation has in common.
#[test]
fn a_session_operation_receives_the_context() -> TestResult {
    let out = server()?;
    let flat = flat(&out);

    assert!(
        flat.contains(
            "async fn get(&self, ctx: &Self::Ctx, input: GetInput) \
             -> Result<::treat::ApiResponse<Thing>, ::treat::ApiError<GuardedGetCodes>>;"
        ),
        "{out}"
    );
    // Extracted after the state and before the request, which is the only
    // order axum accepts: the request has to be last. Its body is read in the
    // handler, after any access check.
    assert!(
        flat.contains(
            "async fn guarded_get<S: GuardedService>(::axum::extract::State(service): ::axum::extract::State<S>, \
             ::treat::rpc::server::Ctx(ctx): ::treat::rpc::server::Ctx<S::Ctx>, request: ::axum::extract::Request) -> Result<::axum::response::Response, ::treat::rpc::server::Refusal> {"
        ),
        "{out}"
    );
    assert!(
        out.contains("    let result = service.get(&ctx, input).await;\n"),
        "{out}"
    );
    Ok(())
}

/// With nothing to send, the context is the only parameter — and the call must
/// not trail a comma where no argument follows.
#[test]
fn a_session_operation_without_input_takes_only_the_context() -> TestResult {
    let out = server()?;

    assert!(
        flat(&out).contains(
            "async fn me(&self, ctx: &Self::Ctx) \
             -> Result<::treat::ApiResponse<Thing>, ::treat::ApiError<GuardedMeCodes>>;"
        ),
        "{out}"
    );
    assert!(
        out.contains("    let result = service.me(&ctx).await;\n"),
        "{out}"
    );
    Ok(())
}

/// The role is enforced by the generated handler, not by the implementation: a
/// rule the service could forget to apply would be a comment, not a guard.
#[test]
fn a_role_is_checked_before_the_method_runs() -> TestResult {
    let out = server()?;

    // A refusal is the context's own, answered as it is: `?` on the
    // `Result<(), Refusal>` the check returns.
    assert!(
        out.contains("    ctx.require_role(Role::ContentManager)?;\n"),
        "{out}"
    );

    // Before the body is even read, so a caller without the role learns that
    // rather than what its body also got wrong.
    let handler = out
        .split("async fn guarded_start")
        .nth(1)
        .ok_or("the start handler must be emitted")?;
    let guard = handler
        .find("require_role")
        .ok_or("guard must be present")?;
    let validate = handler
        .find("Rt::json_input(request)")
        .ok_or("the body must be read in the handler")?;
    assert!(
        guard < validate,
        "the role must be checked first: {handler}"
    );
    Ok(())
}

/// A stream is opened by a caller like anything else, so it gets the same
/// context — the extractor still precedes the query-string decode.
#[test]
fn a_subscription_receives_the_context() -> TestResult {
    let out = server()?;
    let flat = flat(&out);

    assert!(
        flat.contains(
            "async fn watch(&self, ctx: &Self::Ctx, input: GetInput) \
             -> Result<::treat::rpc::server::EventStream<Event>, ::treat::ApiError<GuardedWatchCodes>>;"
        ),
        "{out}"
    );
    assert!(
        flat.contains(
            "async fn guarded_watch<S: GuardedService>(::axum::extract::State(service): ::axum::extract::State<S>, \
             ::treat::rpc::server::Ctx(ctx): ::treat::rpc::server::Ctx<S::Ctx>, ::axum::extract::RawQuery(query): ::axum::extract::RawQuery)"
        ),
        "{out}"
    );
    Ok(())
}

/// Neither a public operation nor a token-guarded one names a user, so neither
/// has a context to be handed.
#[test]
fn an_operation_needing_no_session_gets_no_context() -> TestResult {
    let out = server()?;

    for (method, codes) in [
        ("open", "GuardedOpenCodes"),
        ("export", "GuardedExportCodes"),
    ] {
        assert!(
            flat(&out).contains(&format!(
                "async fn {method}(&self, input: GetInput) \
                 -> Result<::treat::ApiResponse<Thing>, ::treat::ApiError<{codes}>>;"
            )),
            "{method} must take no context: {out}"
        );
    }

    let open = out
        .split("async fn guarded_open")
        .nth(1)
        .and_then(|s| s.split("\n}").next())
        .ok_or("the open handler must be emitted")?;
    assert!(!open.contains("Ctx"), "{open}");
    assert!(
        open.contains("let result = service.open(input).await;"),
        "{open}"
    );
    Ok(())
}

/// An SDL that says nothing about authentication must generate what it did
/// before `@auth` existed, so adding the directive breaks no existing project.
#[test]
fn an_unannotated_service_is_unchanged() -> TestResult {
    let api = graphql_rpcgen::compile_str(&format!(
        "{BASE}
type Plain @service {{
  get(input: GetInput!): Thing! @query @rpc(path: \"/plain\")
}}
"
    ))?;

    // No context configured, because nothing asks for one.
    let out = generate_rust::generate(&api, &Config::default())?;

    assert!(
        flat(&out).contains(
            "async fn get(&self, input: GetInput) \
             -> Result<::treat::ApiResponse<Thing>, ::treat::ApiError<PlainGetCodes>>;"
        ),
        "{out}"
    );
    assert!(!out.contains("Ctx"), "{out}");
    assert!(!out.contains("require_role"), "{out}");
    Ok(())
}

/// The context is an associated type on the service trait, so the generated
/// crate never names one from above it — which is what keeps `generated-api`
/// from depending on the application that implements it.
#[test]
fn the_context_is_an_associated_type_on_the_service() -> TestResult {
    let out = server()?;

    assert!(out.contains("    type Ctx: FromRequestContext;"), "{out}");
    // The contract the project satisfies: `treat`'s `ResolveContext`, which
    // the extractor reads the caller with, plus the API's own access checks.
    assert!(
        out.contains("pub trait FromRequestContext: ::treat::rpc::server::ResolveContext {"),
        "{out}"
    );
    let reexports = flat(&out)
        .split("pub use ::treat::rpc::server::{")
        .nth(1)
        .and_then(|rest| rest.split('}').next())
        .map(str::to_string)
        .ok_or("the server must re-export the runtime it names")?;
    // What an implementation names; the extractor `Ctx` is the handlers',
    // which name it by its path.
    for item in ["ResolveContext", "Refusal"] {
        assert!(
            reexports.split(", ").any(|name| name == item),
            "{item}: {reexports}"
        );
    }
    Ok(())
}

#[test]
fn a_role_without_a_session_is_rejected() -> TestResult {
    for require in ["token", "public"] {
        let e = expect_error(&format!(
            "type S @service {{
  g(input: GetInput!): Thing! @query @rpc(path: \"/g\") \
   @auth(require: {require}, role: content_release) @throws(codes: [\"role_required\"])
}}
"
        ))?;
        assert!(e.contains("needs `require: session`"), "{e}");
    }
    Ok(())
}

/// The generated check answers with `role_required`, and an operation may only
/// answer with what it declares — undeclared, the code would narrow to
/// `internal_error` and a denial would read as a server fault.
#[test]
fn a_role_must_declare_the_code_it_answers_with() -> TestResult {
    let e = expect_error(
        r#"
type S @service {
  g(input: GetInput!): Thing! @query @rpc(path: "/g") @auth(role: content_release)
}
"#,
    )?;
    assert!(e.contains("must list `role_required`"), "{e}");
    Ok(())
}

#[test]
fn an_unknown_requirement_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
type S @service {
  g(input: GetInput!): Thing! @query @rpc(path: "/g") @auth(require: bogus)
}
"#,
    )?;
    assert!(e.contains("must be `session`, `token` or `public`"), "{e}");
    Ok(())
}

/// A role is a value of the `@roles` enum, and an enum-typed argument takes a
/// bare value in GraphQL; a string would be a second spelling of it.
#[test]
fn a_quoted_role_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
type S @service {
  g(input: GetInput!): Thing! @query @rpc(path: "/g") @auth(role: "content_release")
    @throws(codes: ["role_required"])
}
"#,
    )?;
    assert!(e.contains("written bare"), "{e}");
    Ok(())
}

/// A role outside the set would reach the server as a name no session
/// carries, refusing everyone; the compiler refuses it instead.
#[test]
fn a_role_outside_the_set_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
type S @service {
  g(input: GetInput!): Thing! @query @rpc(path: "/g") @auth(role: content_admin)
    @throws(codes: ["role_required"])
}
"#,
    )?;
    assert!(
        e.contains("@auth(role: content_admin) is not a value of the @roles enum `Role`"),
        "{e}"
    );
    Ok(())
}

#[test]
fn a_role_without_a_role_set_is_rejected() -> TestResult {
    let e = expect_error_in(
        r#"
type S @service {
  g(input: GetInput!): Thing! @query @rpc(path: "/g") @auth(role: content_release)
    @throws(codes: ["role_required"])
}
"#,
    )?;
    assert!(e.contains("needs an enum marked @roles"), "{e}");
    Ok(())
}

/// A bare value means something against one set of roles only.
#[test]
fn a_second_role_set_is_rejected() -> TestResult {
    let e = expect_error("enum Other @roles { a }\n")?;
    assert!(e.contains("an API has one role set"), "{e}");
    Ok(())
}

/// The registry names what can go wrong, not who may act. Compiled without
/// [`BASE`], which declares the registry without `@roles`.
#[test]
fn the_error_registry_cannot_be_the_role_set() -> TestResult {
    let e = match graphql_rpcgen::compile_str(
        "enum ErrorCode @roles { invalid_body internal_error }\n",
    ) {
        Ok(_) => return Err("expected a compile error, but compilation succeeded".into()),
        Err(e) => e.to_string(),
    };
    assert!(e.contains("cannot be @roles"), "{e}");
    Ok(())
}

/// The hierarchy is between roles; on any other enum it would read as a grant
/// that is not made.
#[test]
fn inherits_outside_the_role_set_is_rejected() -> TestResult {
    let e = expect_error("enum Color { red green @inherits(roles: [red]) }\n")?;
    assert!(
        e.contains("@inherits applies to a value of the @roles enum"),
        "{e}"
    );
    Ok(())
}

#[test]
fn a_malformed_hierarchy_is_rejected() -> TestResult {
    for (set, message) in [
        (
            "a b @inherits(roles: [c])",
            "`b` inherits `c`, which is not one of its values",
        ),
        (
            "a @inherits(roles: [a])",
            "`a` inherits `a`, which is itself",
        ),
        ("a b @inherits(roles: [a, a])", "`b` inherits `a`, twice"),
        ("a b @inherits(roles: [])", "inherits nothing"),
        ("a b @inherits(roles: [\"a\"])", "malformed @inherits"),
    ] {
        let e = expect_error_in(&format!("enum Role @roles {{ {set} }}\n"))?;
        assert!(e.contains(message), "{set}: {e}");
    }
    Ok(())
}

/// The standard's role hierarchy is a partial order: roles inheriting each
/// other grant the same permissions, so they are one role under two names.
#[test]
fn a_cycle_in_the_hierarchy_is_rejected() -> TestResult {
    let e = expect_error_in(
        "enum Role @roles {\n  a @inherits(roles: [c])\n  b @inherits(roles: [a])\n  \
         c @inherits(roles: [b])\n  d @inherits(roles: [a])\n}\n",
    )?;
    assert!(
        e.contains("@inherits forms a cycle through `a`, `b`, `c`;"),
        "{e}"
    );
    Ok(())
}

/// Authentication guards operations. On a plain output type there is nothing to
/// guard, so a rule written there would read as protection that is not
/// happening.
#[test]
fn auth_on_a_non_service_object_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
type Out @auth { id: UUID! }
type S @service {
  g(input: GetInput!): Thing! @query @rpc(path: "/g")
}
"#,
    )?;
    assert!(e.contains("@auth applies to a @service"), "{e}");
    Ok(())
}

/// A service that sets a role, declared ahead of `Guarded` and with its
/// operations out of name order, so a test can tell the declaration order from
/// the sorted one.
const RELEASES: &str = r#"
type Releases @service @auth(require: session, role: content_release) {
  "Inherits the service's role."
  start(input: GetInput!): Thing!
    @mutation
    @rpc(path: "/releases/start")
    @throws(codes: ["role_required"])

  "Inherits it too, declared after `start`."
  approve(input: GetInput!): Thing!
    @mutation
    @rpc(path: "/releases/approve")
    @throws(codes: ["role_required"])

  "Restates the rule, saying it needs no role."
  list(input: GetInput!): Thing! @query @rpc(path: "/releases/list")
    @auth(require: session, unguarded: true)

  "Opens the operation, overriding the service's session and role alike."
  feed(input: GetInput!): Thing! @query @rpc(path: "/releases/feed")
    @auth(require: public, unguarded: true)
}
"#;

fn releases_api() -> TestResult<graphql_rpcgen::ir::Api> {
    Ok(graphql_rpcgen::compile_str(&format!(
        "{BASE}{ROLE_SET}{RELEASES}{SDL}"
    ))?)
}

/// The handler body for one operation, from its signature to the closing brace.
fn handler<'a>(out: &'a str, name: &str) -> TestResult<&'a str> {
    out.split(&format!("async fn {name}<"))
        .nth(1)
        .and_then(|s| s.split("\n}").next())
        .ok_or_else(|| format!("the {name} handler must be emitted").into())
}

/// A role set on the service is a requirement of each operation that does not
/// restate the rule, so the server checks it there too; an operation that does
/// restate it — a session marked `unguarded`, or opened to the public —
/// carries no role.
#[test]
fn a_service_role_is_checked_on_each_operation_that_inherits_it() -> TestResult {
    let out = generate_rust::generate(&releases_api()?, &Config::default())?;
    let guard = "ctx.require_role(Role::ContentRelease)";

    for name in ["releases_start", "releases_approve"] {
        let body = handler(&out, name)?;
        assert!(body.contains(guard), "{name} must check the role: {body}");
    }
    for name in ["releases_list", "releases_feed"] {
        let body = handler(&out, name)?;
        assert!(!body.contains("require_role"), "{name} must not: {body}");
    }
    Ok(())
}

/// One operation's entry in `createApi`, from its key to its closing brace.
///
/// Read from the object `createApi` returns, past the `ops` and `keys` it
/// builds first, whose per-service blocks open the same way.
fn api_entry<'a>(out: &'a str, service: &str, op: &str) -> TestResult<&'a str> {
    out.split("export function createApi(")
        .nth(1)
        .and_then(|s| s.split("\n  return {").nth(1))
        .and_then(|s| s.split(&format!("\n    {service}: {{\n")).nth(1))
        .and_then(|s| s.split("\n    },\n").next())
        .and_then(|s| s.split(&format!("      {op}: {{\n")).nth(1))
        .and_then(|s| s.split("\n      },").next())
        .ok_or_else(|| format!("createApi must carry {service}.{op}").into())
}

/// The browser mirrors the server's role checks, so it reads the role from the
/// same declaration the generated handler enforces: a role the service sets is
/// inherited, an operation's own rule wins whole, and one without a role
/// carries no `role` field.
#[test]
fn the_typescript_api_carries_each_operations_role() -> TestResult {
    let out = graphql_rpcgen::generate_typescript::generate(&releases_api()?);

    for (service, op, role) in [
        ("guarded", "start", "content_manager"),
        ("releases", "start", "content_release"),
        ("releases", "approve", "content_release"),
    ] {
        let entry = api_entry(&out, service, op)?;
        let field = format!("        role: '{role}' as const,\n");
        assert!(entry.contains(&field), "{service}.{op}: {entry}");
    }
    for op in ["list", "feed"] {
        let entry = api_entry(&out, "releases", op)?;
        assert!(!entry.contains("role:"), "releases.{op}: {entry}");
    }
    Ok(())
}

/// An API that names no role gives no operation a `role` field, so its
/// generated client stays as it was.
#[test]
fn an_api_without_roles_emits_no_role_field() -> TestResult {
    let api = graphql_rpcgen::compile_str(&format!(
        r#"{BASE}
type Open @service @auth(require: session) {{
  get(input: GetInput!): Thing! @query @rpc(path: "/get")
}}
"#
    ))?;
    let out = graphql_rpcgen::generate_typescript::generate(&api);

    assert!(!out.contains("role:"), "{out}");
    Ok(())
}

/// A role whose session name is not an identifier is checked by its Rust
/// variant and carried to the browser by the name the session holds.
#[test]
fn a_tagged_role_travels_by_its_wire_name() -> TestResult {
    let api = graphql_rpcgen::compile_str(&format!(
        r#"{BASE}{ROLE_SET}
type Ops @service @auth(require: session, role: content_ops) {{
  run(input: GetInput!): Thing! @mutation @rpc(path: "/ops/run") @throws(codes: ["role_required"])
}}
"#
    ))?;

    let rust = generate_rust::generate(&api, &Config::default())?;
    assert!(
        rust.contains("ctx.require_role(Role::ContentOps)"),
        "{rust}"
    );
    // `WireEnum` reads the wire name from the rename, so `from_name` and
    // `as_str` spell it as the session does.
    assert!(
        rust.contains("    #[serde(rename = \"content-ops\")]\n    ContentOps,\n"),
        "{rust}"
    );

    let ts = graphql_rpcgen::generate_typescript::generate(&api);
    let entry = api_entry(&ts, "ops", "run")?;
    assert!(
        entry.contains("        role: 'content-ops' as const,\n"),
        "{entry}"
    );
    Ok(())
}

/// The server reads a role from the name a session carries, and checks it
/// against the hierarchy the compiler closed over: each arm lists the role and
/// everything it inherits, two levels down included.
#[test]
fn the_rust_role_set_reads_names_and_follows_the_hierarchy() -> TestResult {
    let out = server()?;

    assert!(out.contains("\nimpl Role {\n"), "{out}");
    // Names both ways come from `WireEnum`, by the renames serde reads.
    assert!(
        out.contains(
            "#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ::serde::Serialize, ::serde::Deserialize, ::treat::rpc::WireEnum)]\n\
             pub enum Role {"
        ),
        "{out}"
    );
    assert!(
        out.contains("    #[serde(rename = \"content_edit\")]\n    ContentEdit,\n"),
        "{out}"
    );
    let out = flat(&out);
    for arm in [
        "Self::ContentTranslate => matches!(required, Self::ContentTranslate),",
        "Self::ContentEdit => matches!(required, Self::ContentEdit | Self::ContentTranslate),",
        "Self::ContentManager => matches!(required, \
         Self::ContentManager | Self::ContentTranslate | Self::ContentEdit),",
        "Self::ContentRelease => matches!(required, Self::ContentRelease),",
    ] {
        assert!(out.contains(arm), "missing `{arm}`: {out}");
    }
    Ok(())
}

/// The role check takes the role type, so the implementation cannot be asked
/// about a name outside the set.
#[test]
fn the_role_check_takes_the_role_type() -> TestResult {
    let out = server()?;
    assert!(
        out.contains("    fn require_role(&self, role: Role) -> Result<(), ::treat::rpc::server::Refusal>;\n"),
        "{out}"
    );
    Ok(())
}

/// The operation table carries each operation's role — the permission
/// assignment the SDL declares — for a caller that reaches a service other than
/// through its router and so passes no generated guard; an operation assigned to
/// none, a public one included, says `None`.
#[test]
fn the_operation_table_carries_each_operations_role() -> TestResult {
    let out = generate_rust::generate(&releases_api()?, &Config::default())?;
    for (id, role) in [
        ("releases_start", "Some(super::Role::ContentRelease)"),
        ("releases_approve", "Some(super::Role::ContentRelease)"),
        ("releases_list", "None"),
        ("releases_feed", "None"),
    ] {
        let row = operation_row(&out, id).ok_or_else(|| format!("no row for {id}: {out}"))?;
        assert!(row.ends_with(&format!(", role: {role} }}")), "{id}: {row}");
        assert!(
            !row.contains("permission:"),
            "no permission set, no column: {row}"
        );
    }
    assert!(
        !out.contains("pub const OPERATIONS"),
        "one table, not two: {out}"
    );
    Ok(())
}

/// With no role set there is no role type to check against and no operation
/// asking for one, so the context trait has no role check to implement.
#[test]
fn a_session_api_without_roles_has_no_role_check() -> TestResult {
    let api = graphql_rpcgen::compile_str(&format!(
        r#"{BASE}
type Open @service @auth(require: session) {{
  get(input: GetInput!): Thing! @query @rpc(path: "/get")
}}
"#
    ))?;
    let out = generate_rust::generate(&api, &Config::default())?;

    assert!(out.contains("pub trait FromRequestContext"), "{out}");
    assert!(!out.contains("require_role"), "{out}");
    Ok(())
}

/// The browser runs the server's hierarchy check, not a copy written by hand.
#[test]
fn the_typescript_role_set_lists_roles_and_follows_the_hierarchy() -> TestResult {
    let out = graphql_rpcgen::generate_typescript::generate(&releases_api()?);

    assert!(
        out.contains(
            "export const roles: readonly Role[] = ['content_translate', 'content_edit', \
             'content_manager', 'content_release', 'content-ops']\n"
        ),
        "{out}"
    );
    assert!(
        out.contains("  'content_manager': ['content_translate', 'content_edit'],\n"),
        "{out}"
    );
    assert!(out.contains("  'content-ops': [],\n"), "{out}");
    assert!(
        out.contains(
            "export function roleAuthorizes(held: Role, required: Role): boolean {\n  \
             const inherited: readonly Role[] = roleInherits[held]\n  \
             return held === required || inherited.includes(required)\n}"
        ),
        "{out}"
    );
    Ok(())
}
