//! `@permissions` and `@auth(permission:)`: an operation names the permission it
//! needs, and which roles hold it is the implementation's.
//!
//! These pin the guard each target emits, the context trait's check, the
//! permission set's runtime half in Rust and TypeScript, the OpenAPI marker, and
//! every rejection that keeps a declaration from being decorative — a misspelt
//! argument above all, which would otherwise leave an operation open.

/// What a test body answers with: the first failure ends it, carrying the
/// message of whatever actually went wrong rather than a fixed one.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

use graphql_rpcgen::config::Config;
use graphql_rpcgen::{generate_openapi, generate_rust, generate_typescript};

/// Everything but the permission set.
const BASE: &str = r#"
scalar UUID

enum ErrorCode {
  invalid_body
  internal_error
  role_required
  role_forbidden
  scope_required
}

input GetInput { id: UUID! }
type Thing { id: UUID! }
"#;

/// Two refusals, as an implementation telling a credential that does not reach
/// the permission from a user who lacks it would declare them; ids that are not
/// GraphQL identifiers, as permission ids seldom are.
const PERMISSION_SET: &str = r#"
enum Permission @permissions(codes: ["role_forbidden", "scope_required"]) {
  users_read @variant(tag: "admin.users.read")
  users_write @variant(tag: "admin.users.write")
}
"#;

/// A service whose rule names a permission, and every way an operation departs
/// from it.
const SDL: &str = r#"
type Users @service @auth(require: session, permission: users_read) {
  "Inherits the service's permission."
  list(input: GetInput!): Thing!
    @query
    @rpc(path: "/users/list")
    @throws(codes: ["role_forbidden", "scope_required"])

  "Names a permission of its own."
  delete(input: GetInput!): Thing!
    @mutation
    @rpc(path: "/users/delete")
    @auth(require: session, permission: users_write)
    @throws(codes: ["role_forbidden", "scope_required"])

  "Restates the rule without a permission, so it needs none."
  me(input: GetInput!): Thing! @query @rpc(path: "/users/me") @auth(require: session)

  "Opens the operation, overriding the service's session and permission alike."
  ping(input: GetInput!): Thing! @query @rpc(path: "/users/ping") @auth(require: public)
}
"#;

fn api() -> TestResult<graphql_rpcgen::ir::Api> {
    Ok(graphql_rpcgen::compile_str(&format!(
        "{BASE}{PERMISSION_SET}{SDL}"
    ))?)
}

/// The compile error `body` must produce, with the base declarations in front.
fn expect_error(body: &str) -> TestResult<String> {
    match graphql_rpcgen::compile_str(&format!("{BASE}{body}")) {
        Ok(_) => Err("expected a compile error, but compilation succeeded".into()),
        Err(e) => Ok(e.to_string()),
    }
}

/// The handler body for one operation, from its signature to the closing brace.
fn handler<'a>(out: &'a str, name: &str) -> TestResult<&'a str> {
    out.split(&format!("async fn {name}<"))
        .nth(1)
        .and_then(|s| s.split("\n}").next())
        .ok_or_else(|| format!("the {name} handler must be emitted").into())
}

/// One operation's entry in `createApi`, from its key to its closing brace.
fn api_entry<'a>(out: &'a str, service: &str, op: &str) -> TestResult<&'a str> {
    out.split("export function createApi(")
        .nth(1)
        .and_then(|s| s.split(&format!("\n    {service}: {{\n")).nth(1))
        .and_then(|s| s.split("\n    },\n").next())
        .and_then(|s| s.split(&format!("      {op}: {{\n")).nth(1))
        .and_then(|s| s.split("\n      },").next())
        .ok_or_else(|| format!("createApi must carry {service}.{op}").into())
}

/// The permission is checked before the body is read and the method runs,
/// awaited since the implementation may read the assignment from its data; a
/// service's permission reaches each operation that does not restate the rule,
/// and one that does carries no check.
#[test]
fn a_permission_is_checked_before_the_method_runs() -> TestResult {
    let out = generate_rust::generate(&api()?, &Config::default())?;

    for (name, variant) in [("users_list", "UsersRead"), ("users_delete", "UsersWrite")] {
        let body = handler(&out, name)?;
        let guard = format!(
            "    let access = ctx.require_permission(Permission::{variant}).await;\n    \
             if let Err(response) = access {{\n        \
             return response;\n    }}\n"
        );
        let guard_at = body
            .find(&guard)
            .ok_or(format!("{name} must check: {body}"))?;
        let validate_at = body
            .find("FromRequest<()>>::from_request(request")
            .ok_or("no body read")?;
        let call_at = body.find("service.").ok_or("no call")?;
        assert!(
            guard_at < validate_at && validate_at < call_at,
            "{name}: {body}"
        );
    }
    for name in ["users_me", "users_ping"] {
        let body = handler(&out, name)?;
        assert!(
            !body.contains("require_permission"),
            "{name} must not check: {body}"
        );
    }
    Ok(())
}

/// The context trait asks for the permission check, async and typed by the
/// permission set, and naming the codes it answers with; an API without a set
/// has no such method to implement, and one without roles no role check.
#[test]
fn the_permission_check_is_part_of_the_context_trait_only_with_a_set() -> TestResult {
    let out = generate_rust::generate(&api()?, &Config::default())?;
    assert!(
        out.contains(
            "    fn require_permission(\n        &self,\n        permission: Permission,\n    \
             ) -> impl std::future::Future<Output = Result<(), Response>> + Send;\n"
        ),
        "{out}"
    );
    assert!(
        out.contains("(`role_forbidden`, `scope_required`)"),
        "{out}"
    );
    assert!(!out.contains("fn require_role"), "{out}");

    let open = graphql_rpcgen::compile_str(&format!(
        r#"{BASE}
type Open @service @auth(require: session) {{
  get(input: GetInput!): Thing! @query @rpc(path: "/get")
}}
"#
    ))?;
    let out = generate_rust::generate(&open, &Config::default())?;
    assert!(out.contains("pub trait FromRequestContext"), "{out}");
    assert!(!out.contains("require_permission"), "{out}");
    Ok(())
}

/// The Rust set reads a permission from the id an implementation stores and
/// spells it back, by the tag rather than the GraphQL name.
#[test]
fn the_rust_permission_set_reads_and_spells_ids() -> TestResult {
    let out = generate_rust::generate_types(&api()?, &Config::default())?;
    assert!(
        out.contains("            \"admin.users.read\" => Some(Self::UsersRead),\n"),
        "{out}"
    );
    assert!(out.contains("            _ => None,\n"), "{out}");
    assert!(
        out.contains("            Self::UsersWrite => \"admin.users.write\",\n"),
        "{out}"
    );
    assert!(
        !out.contains("fn authorizes"),
        "a permission set has no hierarchy: {out}"
    );
    Ok(())
}

/// The browser reads each operation's permission from the entry it calls
/// through, and the whole set from `permissions`; whether a session holds one
/// is the server's, so there is no check to mirror.
#[test]
fn the_typescript_api_carries_each_operations_permission() -> TestResult {
    let out = generate_typescript::generate(&api()?);

    for (op, id) in [
        ("list", "admin.users.read"),
        ("delete", "admin.users.write"),
    ] {
        let entry = api_entry(&out, "users", op)?;
        let field = format!("        permission: '{id}' as const,\n");
        assert!(entry.contains(&field), "users.{op}: {entry}");
    }
    for op in ["me", "ping"] {
        let entry = api_entry(&out, "users", op)?;
        assert!(!entry.contains("permission:"), "users.{op}: {entry}");
    }
    assert!(
        out.contains(
            "export const permissions: readonly Permission[] = ['admin.users.read', \
             'admin.users.write']\n"
        ),
        "{out}"
    );
    Ok(())
}

/// The OpenAPI document marks each operation with the permission or the role
/// `@auth` checks, by its wire name, and leaves the others unmarked.
#[test]
fn the_openapi_document_marks_each_operations_check() -> TestResult {
    let roles = r#"
enum Role @roles { auditor @variant(tag: "audit-reader") }
type Audit @service @auth(require: session, role: auditor) {
  read(input: GetInput!): Thing! @query @rpc(path: "/audit/read") @throws(codes: ["role_required"])
}
"#;
    let api = graphql_rpcgen::compile_str(&format!("{BASE}{PERMISSION_SET}{SDL}{roles}"))?;
    let doc: serde_json::Value =
        serde_json::from_str(&generate_openapi::generate(&api, &Config::default())?)?;
    let op = |path: &str| &doc["paths"][path]["post"];

    assert_eq!(op("/users/list")["x-permission"], "admin.users.read");
    assert_eq!(op("/users/delete")["x-permission"], "admin.users.write");
    assert_eq!(op("/audit/read")["x-role"], "audit-reader");
    for path in ["/users/me", "/users/ping", "/audit/read"] {
        assert!(
            op(path).get("x-permission").is_none(),
            "{path}: {}",
            op(path)
        );
    }
    assert!(op("/users/list").get("x-role").is_none());
    Ok(())
}

/// A misspelt `@auth` argument is refused rather than dropped: dropped, it would
/// leave the operation open to every session.
#[test]
fn an_unknown_auth_argument_is_rejected() -> TestResult {
    let e = expect_error(&format!(
        r#"{PERMISSION_SET}
type S @service {{
  g(input: GetInput!): Thing! @query @rpc(path: "/g") @auth(require: session, permisson: users_read)
}}
"#
    ))?;
    assert!(
        e.contains(
            "directive `@auth` has no argument `permisson`; it takes `require`, `role`, `permission`"
        ),
        "{e}"
    );
    Ok(())
}

/// Every operation with a permission declares the codes the check answers with,
/// so its typed outcomes include the refusal it can get.
#[test]
fn a_permission_must_declare_the_codes_its_check_answers_with() -> TestResult {
    let e = expect_error(&format!(
        r#"{PERMISSION_SET}
type S @service {{
  g(input: GetInput!): Thing! @query @rpc(path: "/g")
    @auth(require: session, permission: users_read) @throws(codes: ["role_forbidden"])
}}
"#
    ))?;
    assert!(
        e.contains("`S.g`: @auth(permission:) is checked before the handler, so @throws(codes:) must list `scope_required`"),
        "{e}"
    );
    assert!(!e.contains("must list `role_forbidden`"), "{e}");
    Ok(())
}

/// The set's codes are required, non-empty and listed once: a default would
/// describe a refusal the implementation may never send.
#[test]
fn the_permission_codes_are_declared_once_each() -> TestResult {
    for (codes, problem) in [
        ("", "is missing"),
        ("(codes: [])", "lists no code"),
        (
            r#"(codes: ["scope_required", "scope_required"])"#,
            "lists a code twice",
        ),
        ("(codes: scope_required)", "must be a list of strings"),
    ] {
        let e = expect_error(&format!(
            "enum Permission @permissions{codes} {{ users_read }}\n"
        ))?;
        assert!(
            e.contains(&format!("@permissions(codes:) {problem}")),
            "{codes}: {e}"
        );
    }
    Ok(())
}

/// Each of the set's codes is a code of the registry, whether or not an
/// operation names a permission yet: otherwise a misspelt code would surface
/// only once the first operation lists it.
#[test]
fn the_permission_codes_are_registry_codes() -> TestResult {
    let e =
        expect_error("enum Permission @permissions(codes: [\"role_forbiden\"]) { users_read }\n")?;
    assert!(
        e.contains(
            "enum `Permission`: @permissions(codes:) names `role_forbiden`, which is not listed in `ErrorCode`"
        ),
        "{e}"
    );
    Ok(())
}

/// A permission is a value of the set, written bare, and only on a session: a
/// quoted one, one outside the set, one without a set, and one on a token or a
/// public operation are each refused.
#[test]
fn a_permission_outside_a_session_set_is_rejected() -> TestResult {
    let op = |auth: &str| {
        format!(
            r#"type S @service {{
  g(input: GetInput!): Thing! @query @rpc(path: "/g") @auth({auth})
    @throws(codes: ["role_forbidden", "scope_required"])
}}
"#
        )
    };
    for (set, auth, expected) in [
        (
            PERMISSION_SET,
            r#"require: session, permission: "users_read""#,
            "@auth(permission:) names a value of the @permissions enum, written bare",
        ),
        (
            PERMISSION_SET,
            "require: session, permission: users_admin",
            "@auth(permission: users_admin) is not a value of the @permissions enum `Permission`",
        ),
        (
            "",
            "require: session, permission: users_read",
            "@auth(permission: users_read) needs an enum marked @permissions listing it",
        ),
        (
            PERMISSION_SET,
            "require: token, permission: users_read",
            "@auth(permission:) needs `require: session`",
        ),
        (
            PERMISSION_SET,
            "require: public, permission: users_read",
            "@auth(permission:) needs `require: session`",
        ),
    ] {
        let e = expect_error(&format!("{set}{}", op(auth)))?;
        assert!(e.contains(expected), "{auth}: {e}");
    }
    Ok(())
}

/// A role and a permission are two answers to one question, so an operation
/// names one of them.
#[test]
fn a_role_and_a_permission_together_are_rejected() -> TestResult {
    let e = expect_error(&format!(
        r#"{PERMISSION_SET}
enum Role @roles {{ admin }}
type S @service {{
  g(input: GetInput!): Thing! @query @rpc(path: "/g")
    @auth(require: session, role: admin, permission: users_read)
    @throws(codes: ["role_required", "role_forbidden", "scope_required"])
}}
"#
    ))?;
    assert!(
        e.contains("@auth takes `role:` or `permission:`, not both"),
        "{e}"
    );
    Ok(())
}

/// An API has one permission set; it is neither the error registry nor the role
/// set, and it has no hierarchy.
#[test]
fn a_malformed_permission_set_is_rejected() -> TestResult {
    for (sdl, expected) in [
        (
            format!(
                "{PERMISSION_SET}enum More @permissions(codes: [\"scope_required\"]) {{ x }}\n"
            ),
            "enum `More`: @permissions is already on `Permission`; an API has one permission set",
        ),
        (
            "enum Both @roles @permissions(codes: [\"scope_required\"]) { x }\n".to_string(),
            "enum `Both`: @roles and @permissions are two sets; mark one",
        ),
        (
            "enum Permission @permissions(codes: [\"scope_required\"]) { read write @inherits(roles: [read]) }\n"
                .to_string(),
            "enum `Permission`: @inherits applies to a value of the @roles enum",
        ),
    ] {
        let e = expect_error(&sdl)?;
        assert!(e.contains(expected), "{sdl}: {e}");
    }

    let registry = graphql_rpcgen::compile_str(
        "enum ErrorCode @permissions(codes: [\"internal_error\"]) { invalid_body internal_error }\n",
    );
    let e = match registry {
        Ok(_) => return Err("the registry cannot be the permission set".into()),
        Err(e) => e.to_string(),
    };
    assert!(
        e.contains("`ErrorCode` is the error code registry; it cannot be @permissions"),
        "{e}"
    );
    Ok(())
}
