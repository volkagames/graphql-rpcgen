//! `@auth`: who may call an operation, and what its handler hands the service.
//!
//! A requirement changes the trait signature, the handler's extractors and, for
//! a role, what runs before the method is reached. These pin all three, plus the
//! rejections that keep a declaration from being decorative.

/// What a test body answers with: the first failure ends it, carrying the
/// message of whatever actually went wrong rather than a fixed one.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

use graphql_rpcgen::config::Config;
use graphql_rpcgen::generate_rust;

const PRELUDE: &str = r#"
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
    @auth(require: session, role: "content_manager")
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
    let api = graphql_rpcgen::compile_str(&format!("{PRELUDE}{SDL}"))?;
    Ok(generate_rust::generate(&api, &Config::default())?)
}

fn expect_error(body: &str) -> TestResult<String> {
    match graphql_rpcgen::compile_str(&format!("{PRELUDE}{body}")) {
        Ok(_) => Err("expected a compile error, but compilation succeeded".into()),
        Err(e) => Ok(e.to_string()),
    }
}

/// The context leads the parameter list: it is the caller rather than the
/// request, and it is what every session-guarded operation has in common.
#[test]
fn a_session_operation_receives_the_context() -> TestResult {
    let out = server()?;

    assert!(
        out.contains(
            "async fn get(&self, ctx: &Self::Ctx, input: GetInput) \
             -> Result<ApiResponse<Thing>, ApiError<GuardedGetCodes>>;"
        ),
        "{out}"
    );
    // Extracted after the state and before the body, which is the only order
    // axum accepts: a body extractor has to be last.
    assert!(
        out.contains(
            "async fn guarded_get<S: GuardedService>(\n    \
             State(service): State<S>,\n    \
             Ctx(ctx): Ctx<S::Ctx>,\n    \
             ApiJson(input): ApiJson<GetInput>,\n"
        ),
        "{out}"
    );
    assert!(
        out.contains("match service.get(&ctx, input).await {"),
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
        out.contains(
            "async fn me(&self, ctx: &Self::Ctx) \
             -> Result<ApiResponse<Thing>, ApiError<GuardedMeCodes>>;"
        ),
        "{out}"
    );
    assert!(out.contains("match service.me(&ctx).await {"), "{out}");
    Ok(())
}

/// The role is enforced by the generated handler, not by the implementation: a
/// rule the service could forget to apply would be a comment, not a guard.
#[test]
fn a_role_is_checked_before_the_method_runs() -> TestResult {
    let out = server()?;

    assert!(
        out.contains(
            "    if let Err(response) = ctx.require_role(\"content_manager\") {\n        \
             return response;\n    }\n"
        ),
        "{out}"
    );

    // Before the body is even validated, so a caller without the role learns
    // that rather than which field it also got wrong.
    let handler = out
        .split("async fn guarded_start")
        .nth(1)
        .ok_or("the start handler must be emitted")?;
    let guard = handler
        .find("require_role")
        .ok_or("guard must be present")?;
    let validate = handler
        .find("Validate::validate")
        .ok_or("the validation call must be emitted")?;
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

    assert!(
        out.contains(
            "async fn watch(&self, ctx: &Self::Ctx, input: GetInput) \
             -> Result<EventStream<Event>, ApiError<GuardedWatchCodes>>;"
        ),
        "{out}"
    );
    assert!(
        out.contains(
            "async fn guarded_watch<S: GuardedService>(\n    \
             State(service): State<S>,\n    \
             Ctx(ctx): Ctx<S::Ctx>,\n    \
             RawQuery(query): RawQuery,\n"
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
            out.contains(&format!(
                "async fn {method}(&self, input: GetInput) \
                 -> Result<ApiResponse<Thing>, ApiError<{codes}>>;"
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
    assert!(open.contains("match service.open(input).await {"), "{open}");
    Ok(())
}

/// An SDL that says nothing about authentication must generate what it did
/// before `@auth` existed, so adding the directive breaks no existing project.
#[test]
fn an_unannotated_service_is_unchanged() -> TestResult {
    let api = graphql_rpcgen::compile_str(&format!(
        "{PRELUDE}
type Plain @service {{
  get(input: GetInput!): Thing! @query @rpc(path: \"/plain\")
}}
"
    ))?;

    // No context configured, because nothing asks for one.
    let out = generate_rust::generate(&api, &Config::default())?;

    assert!(
        out.contains(
            "async fn get(&self, input: GetInput) \
             -> Result<ApiResponse<Thing>, ApiError<PlainGetCodes>>;"
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
    // The contract the project satisfies, emitted beside the extractor.
    assert!(
        out.contains("pub trait FromRequestContext: Sized + Send + Sync + 'static {"),
        "{out}"
    );
    assert!(out.contains("pub struct Ctx<T>(pub T);"), "{out}");
    Ok(())
}

#[test]
fn a_role_without_a_session_is_rejected() -> TestResult {
    for require in ["token", "public"] {
        let e = expect_error(&format!(
            "type S @service {{
  g(input: GetInput!): Thing! @query @rpc(path: \"/g\") \
   @auth(require: {require}, role: \"admin\") @throws(codes: [\"role_required\"])
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
  g(input: GetInput!): Thing! @query @rpc(path: "/g") @auth(role: "admin")
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

#[test]
fn an_empty_role_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
type S @service {
  g(input: GetInput!): Thing! @query @rpc(path: "/g") @auth(role: "")
}
"#,
    )?;
    assert!(e.contains("must not be empty"), "{e}");
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
