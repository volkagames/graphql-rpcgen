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

  "Restates the rule, saying it needs no permission."
  me(input: GetInput!): Thing! @query @rpc(path: "/users/me")
    @auth(require: session, unguarded: true)

  "Opens the operation, overriding the service's session and permission alike."
  ping(input: GetInput!): Thing! @query @rpc(path: "/users/ping") @auth(require: public, unguarded: true)
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

/// One operation per handler form, each needing a permission, in a service
/// whose operations each state their own: every template carries the guard,
/// and a template that lost it would open its form to every session.
const EVERY_FORM: &str = r#"
scalar UUID
scalar Binary
  @scalar(rust: "Vec<u8>", typescript: "Blob", openapiType: "string", openapiFormat: "binary")

enum ErrorCode {
  invalid_body
  internal_error
  permission_required
}

enum Permission @permissions(codes: ["permission_required"]) { files_wipe }

type Event { line: String! }
input WatchInput { id: UUID! }
input IngestInput { id: UUID! body: Binary! }
type IngestResult { written: Int! }

type Admin @service @auth(require: session, explicit: true) {
  json(input: WatchInput!): IngestResult!
    @mutation @rpc(path: "/json")
    @auth(permission: files_wipe) @throws(codes: ["permission_required"])
  no_input: IngestResult!
    @mutation @rpc(path: "/no_input")
    @auth(permission: files_wipe) @throws(codes: ["permission_required"])
  watch(input: WatchInput!): Event!
    @subscription @rpc(path: "/watch")
    @auth(permission: files_wipe) @throws(codes: ["permission_required"])
  watch_all: Event!
    @subscription @rpc(path: "/watch_all")
    @auth(permission: files_wipe) @throws(codes: ["permission_required"])
  report(input: WatchInput!): Binary!
    @query @rpc(path: "/report") @raw(response: ["text/csv"])
    @auth(permission: files_wipe) @throws(codes: ["permission_required"])
  manifest: Binary!
    @query @rpc(path: "/manifest") @raw(response: ["text/csv"])
    @auth(permission: files_wipe) @throws(codes: ["permission_required"])
  ingest(input: IngestInput!): IngestResult!
    @mutation @rpc(path: "/ingest") @raw(request: ["text/csv"])
    @auth(permission: files_wipe) @throws(codes: ["permission_required"])
  convert(input: IngestInput!): Binary!
    @mutation @rpc(path: "/convert") @raw(request: ["text/csv"], response: ["text/csv"])
    @auth(permission: files_wipe) @throws(codes: ["permission_required"])
  upload(input: IngestInput!): IngestResult!
    @mutation @rpc(path: "/upload") @raw(request: ["application/octet-stream"], stream: true)
    @auth(permission: files_wipe) @throws(codes: ["permission_required"])
}
"#;

/// In every handler form the guard comes first: before the body or the query
/// string is read, and before the method is called.
#[test]
fn every_handler_form_checks_before_reading_the_request() -> TestResult {
    let api = graphql_rpcgen::compile_str(EVERY_FORM)?;
    let out = generate_rust::generate(&api, &Config::default())?;
    let guard = "    let access = ctx.require_permission(Permission::FilesWipe).await;\n    \
                 if let Err(response) = access {\n        return response;\n    }\n";
    // What a handler does with the request once the guard has let it through.
    let reads = [
        "from_request(",
        "input_from_query(",
        "into_parts()",
        "raw_body(",
        "service.",
    ];

    let forms = [
        "json",
        "no_input",
        "watch",
        "watch_all",
        "report",
        "manifest",
        "ingest",
        "convert",
        "upload",
    ];
    for form in forms {
        let name = format!("admin_{form}");
        let body = handler(&out, &name)?;
        let guard_at = body
            .find(guard)
            .ok_or(format!("{name} must check the permission: {body}"))?;
        let first_read = reads
            .iter()
            .filter_map(|read| body.find(read))
            .min()
            .ok_or(format!(
                "{name} must read the request or call the service: {body}"
            ))?;
        assert!(guard_at < first_read, "{name} must check first: {body}");
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

/// A caller reaching a service other than through its router — an MCP tool
/// calling the method — passes no generated guard, so the set lists what each
/// operation needs by its `operationId`: every operation, `None` for one that
/// needs nothing, so a misspelt id is a missing row rather than an open one.
#[test]
fn the_permission_set_lists_each_operations_permission() -> TestResult {
    let out = generate_rust::generate_types(&api()?, &Config::default())?;
    let table = out
        .split("pub const OPERATIONS: &'static [(&'static str, Option<Self>)] = &[\n")
        .nth(1)
        .and_then(|s| s.split("    ];\n").next())
        .ok_or_else(|| format!("the permission set must list its operations: {out}"))?;
    assert_eq!(
        table,
        "        (\"users_delete\", Some(Self::UsersWrite)),\n        \
         (\"users_list\", Some(Self::UsersRead)),\n        \
         (\"users_me\", None),\n        \
         (\"users_ping\", None),\n"
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
    let document = |sdl: String| -> TestResult<serde_json::Value> {
        let api = graphql_rpcgen::compile_str(&sdl)?;
        Ok(serde_json::from_str(&generate_openapi::generate(
            &api,
            &Config::default(),
        )?)?)
    };

    let doc = document(format!("{BASE}{PERMISSION_SET}{SDL}"))?;
    let op = |path: &str| doc["paths"][path]["post"].clone();
    assert_eq!(op("/users/list")["x-permission"], "admin.users.read");
    assert_eq!(op("/users/delete")["x-permission"], "admin.users.write");
    for path in ["/users/me", "/users/ping"] {
        assert!(
            op(path).get("x-permission").is_none(),
            "{path}: {}",
            op(path)
        );
    }
    assert!(op("/users/list").get("x-role").is_none());

    let doc = document(format!("{BASE}{roles}"))?;
    let audit = &doc["paths"]["/audit/read"]["post"];
    assert_eq!(audit["x-role"], "audit-reader");
    assert!(audit.get("x-permission").is_none(), "{audit}");
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
            "directive `@auth` has no argument `permisson`; it takes `require`, `role`, `permission`, `unguarded`, `explicit`"
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

/// A restatement that leaves out its service's check must say so: a copied
/// `@auth(require: session)` would otherwise open the operation to every
/// session without a word, and the compiler cannot tell it from a deliberate
/// one. `require: public` opens it wider still, so it says so too.
#[test]
fn a_restatement_drops_a_check_only_by_saying_so() -> TestResult {
    for (rule, restated, expected) in [
        (
            "permission: users_read",
            "require: session",
            "`S.g`: @auth restates the rule without its service's `permission: users_read`; \
             name a role or a permission, or write `unguarded: true` if it needs neither",
        ),
        (
            "permission: users_read",
            "require: public",
            "`S.g`: @auth restates the rule without its service's `permission: users_read`",
        ),
        (
            "permission: users_read",
            "require: token",
            "`S.g`: @auth restates the rule without its service's `permission: users_read`",
        ),
        (
            "role: admin",
            "require: session",
            "`S.g`: @auth restates the rule without its service's `role: admin`",
        ),
    ] {
        let set = if rule.starts_with("role:") {
            "enum Role @roles { admin }\n"
        } else {
            PERMISSION_SET
        };
        let e = expect_error(&format!(
            r#"{set}
type S @service @auth(require: session, {rule}) {{
  g(input: GetInput!): Thing! @query @rpc(path: "/g") @auth({restated})
    @throws(codes: ["role_required", "role_forbidden", "scope_required"])
}}
"#
        ))?;
        assert!(e.contains(expected), "{rule} / {restated}: {e}");
    }
    Ok(())
}

/// One operation of `S`, under the service rule and the operation rule given;
/// an empty operation rule leaves the operation without `@auth` of its own.
fn one_operation(service_rule: &str, op_rule: &str) -> String {
    let op_auth = match op_rule {
        "" => String::new(),
        rule => format!("@auth({rule})"),
    };
    format!(
        r#"{PERMISSION_SET}
type S @service {service_rule} {{
  g(input: GetInput!): Thing! @query @rpc(path: "/g") {op_auth}
    @throws(codes: ["role_forbidden", "scope_required"])
}}
"#
    )
}

/// `unguarded` is written only to drop a check: as `true`, naming no check,
/// in a service that has one to drop. Any `require:` may drop it.
#[test]
fn unguarded_is_refused_where_it_says_nothing() -> TestResult {
    let guarded = "@auth(require: session, permission: users_read)";
    for (sdl, expected) in [
        (
            one_operation(guarded, "require: session, unguarded: false"),
            "`S.g`: @auth(unguarded:) `false` is the default; drop the argument",
        ),
        (
            one_operation(guarded, r#"require: session, unguarded: "yes""#),
            "`S.g`: @auth(unguarded:) must be `true`",
        ),
        (
            one_operation(
                guarded,
                "require: session, permission: users_write, unguarded: true",
            ),
            "`S.g`: @auth(unguarded: true) says the operation needs no role or permission, so it names neither",
        ),
        (
            one_operation("@auth(require: session)", "require: session, unguarded: true"),
            "`S.g`: @auth(unguarded: true) has nothing to drop",
        ),
        (
            one_operation(
                "@auth(require: session, unguarded: true)",
                "require: session, permission: users_read",
            ),
            "service `S`: @auth(unguarded:) applies to an operation",
        ),
    ] {
        let e = expect_error(&sdl)?;
        assert!(e.contains(expected), "{sdl}: {e}");
    }

    for op_rule in [
        "require: session, unguarded: true",
        "require: public, unguarded: true",
        "require: token, unguarded: true",
    ] {
        graphql_rpcgen::compile_str(&format!("{BASE}{}", one_operation(guarded, op_rule)))?;
    }
    Ok(())
}

/// An `explicit` service has no default: each operation states its own check,
/// so a forgotten one is a compile error rather than an operation open to every
/// session, and a write cannot inherit a read.
#[test]
fn an_explicit_service_has_each_operation_state_its_check() -> TestResult {
    let explicit = "@auth(require: session, explicit: true)";
    for (sdl, expected) in [
        (
            one_operation(explicit, ""),
            "`S.g`: its service is @auth(explicit: true), so it states its own @auth: a role, \
             a permission, or `unguarded: true`",
        ),
        (
            one_operation(explicit, "require: session"),
            "`S.g`: @auth restates the rule without a check its `explicit` service asks for",
        ),
        (
            one_operation(explicit, "require: public"),
            "`S.g`: @auth restates the rule without a check its `explicit` service asks for",
        ),
        (
            one_operation(
                "@auth(require: session)",
                "require: session, explicit: true",
            ),
            "`S.g`: @auth(explicit:) applies to a service",
        ),
        (
            one_operation(
                "@auth(require: session, permission: users_read, explicit: true)",
                "require: session, permission: users_read",
            ),
            "`S`: @auth(explicit: true) has each operation name its own check, so it takes \
             `require: session` and no `role:` or `permission:`",
        ),
        (
            one_operation(
                "@auth(require: token, explicit: true)",
                "require: session, permission: users_read",
            ),
            "`S`: @auth(explicit: true) has each operation name its own check",
        ),
        (
            one_operation(
                "@auth(require: session, explicit: false)",
                "require: session, permission: users_read",
            ),
            "`S`: @auth(explicit:) `false` is the default; drop the argument",
        ),
    ] {
        let e = expect_error(&sdl)?;
        assert!(e.contains(expected), "{sdl}: {e}");
    }

    for op_rule in [
        "require: session, permission: users_write",
        "require: session, unguarded: true",
        "require: public, unguarded: true",
    ] {
        let api =
            graphql_rpcgen::compile_str(&format!("{BASE}{}", one_operation(explicit, op_rule)))?;
        let op = &api.services[0].operations[0];
        assert_eq!(
            op.access.is_some(),
            op_rule.contains("permission:"),
            "{op_rule}"
        );
    }
    Ok(())
}

/// An API assigns permissions to roles in the SDL or leaves that to the
/// implementation, not both: under `@permissions` the roles are data, so a
/// `@roles` enum beside it would be a second set of roles.
#[test]
fn a_role_set_and_a_permission_set_are_not_declared_together() -> TestResult {
    let e = expect_error(&format!("{PERMISSION_SET}enum Role @roles {{ admin }}\n"))?;
    assert!(
        e.contains(
            "enum `Role` is @roles and enum `Permission` is @permissions: an API assigns \
             permissions to roles in the SDL or leaves the assignment to the implementation; \
             declare one set"
        ),
        "{e}"
    );
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
