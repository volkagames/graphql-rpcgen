//! The generated `@http` server and client, compiled and talking to each other.
//!
//! What `@http` promises is a wire format fixed elsewhere: a method other than
//! `POST`, parameters in the path, a query string, a form body, a bare JSON
//! answer. None of that shows in what the templates write, only in what goes
//! over a socket, so this builds the server and the client from one SDL in a
//! scratch cargo project — the arrangement `raw_wire` uses, offline and pinned
//! to this crate's lockfile — serves the router on a local port, and sends it
//! both hand-written requests (the format) and the generated client's (the two
//! sides agreeing).

#![allow(
    clippy::panic_in_result_fn,
    reason = "a test asserts, and returns `Result` only to use `?` on its setup"
)]

/// What a test body answers with: the first failure ends it, carrying the
/// message of whatever actually went wrong rather than a fixed one.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

mod common;

use common::Dependency;

/// One operation per form the server and the clients branch on: a path
/// parameter, a query string, `ANY` reading a form body over the query string,
/// no input at all, a JSON body beside a path parameter with the envelope kept,
/// and a second method on a path already taken.
const SDL: &str = include_str!("fixtures/http_wire.graphql");

const DRIVER: &str = r##"
//! The test driver: an implementation of the generated trait and the wire
//! tests. Not generated code, so the lints the generated modules are held to
//! are not this module's.
#![allow(clippy::all, dead_code, unused_imports, reason = "a test driver, not generated code")]

use crate::{client, server, types};

#[derive(Clone)]
pub struct Users;

/// The caller named by `x-user`. Both refusals are the envelope under `200`,
/// as an RPC project writes them: the `@http` handler is what gives them a
/// status.
pub struct Caller(String);

fn refusal(code: &str) -> server::Refusal {
    axum::response::IntoResponse::into_response(axum::Json(serde_json::json!({
        "errors": [{ "code": code, "message": code }]
    })))
    .into()
}

impl server::ResolveContext for Caller {
    async fn from_parts(parts: &mut axum::http::request::Parts) -> Result<Self, server::Refusal> {
        match parts.headers.get("x-user").and_then(|v| v.to_str().ok()) {
            Some(user) => Ok(Caller(user.to_string())),
            None => Err(refusal("unauthenticated")),
        }
    }
}

impl server::FromRequestContext for Caller {
    fn require_role(&self, _role: server::Role) -> Result<(), server::Refusal> {
        match self.0.as_str() {
            "admin" => Ok(()),
            _ => Err(refusal("forbidden")),
        }
    }
}

#[async_trait::async_trait]
impl server::UsersService for Users {
    type Ctx = Caller;

    async fn user(
        &self,
        input: server::UserInput,
    ) -> Result<server::User, server::ApiError<server::UsersUserCodes>> {
        if input.user_id == "missing" {
            return Err(server::error(server::UsersUserCodes::NotFound).with_status(404));
        }
        if input.user_id == "unstated" {
            return Err(server::error(server::UsersUserCodes::NotFound));
        }
        Ok(server::User {
            email: format!("{}@example.com", input.user_id),
            id: input.user_id,
            limit: None,
        })
    }

    async fn by_email(
        &self,
        input: server::FindInput,
    ) -> Result<server::User, server::ApiError<server::UsersByEmailCodes>> {
        Ok(server::User {
            id: "u1".to_string(),
            email: input.email,
            limit: input.limit,
        })
    }

    async fn info(
        &self,
        input: server::InfoInput,
    ) -> Result<server::Info, server::ApiError<server::UsersInfoCodes>> {
        Ok(server::Info {
            session_id: input.session_id.or(input.session_cookie),
            verbose: input.verbose.unwrap_or(false),
        })
    }

    async fn me(&self) -> Result<server::Me, server::ApiError<server::UsersMeCodes>> {
        Ok(server::Me {
            user_id: "u1".to_string(),
        })
    }

    async fn rename(
        &self,
        input: server::RenameInput,
    ) -> Result<server::ApiResponse<server::Renamed>, server::ApiError<server::UsersRenameCodes>> {
        Ok(server::success(server::Renamed {
            id: input.id,
            name: input.name,
        }))
    }

    async fn drop(
        &self,
        input: server::UserInput,
    ) -> Result<server::Me, server::ApiError<server::UsersDropCodes>> {
        Ok(server::Me {
            user_id: input.user_id,
        })
    }

    async fn blank(&self) -> Result<server::Me, server::ApiError<server::UsersBlankCodes>> {
        Ok(server::Me {
            user_id: "new".to_string(),
        })
    }

    async fn whoami(
        &self,
        ctx: &Caller,
    ) -> Result<server::Me, server::ApiError<server::UsersWhoamiCodes>> {
        Ok(server::Me {
            user_id: ctx.0.clone(),
        })
    }
}

#[cfg(test)]
mod wire {
    use super::{client, server, types};

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    const ID: &str = "7b1e3c2a-0000-4000-8000-000000000001";
    const OTHER_ID: &str = "7b1e3c2a-0000-4000-8000-000000000002";

    async fn serve() -> TestResult<String> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let router = server::users_router(super::Users);
        // The table the lookups below search is one they can search.
        assert_eq!(treat::rpc::route::check_table(types::operations::ALL), Ok(()));
        tokio::spawn(async move { axum::serve(listener, router).await });
        Ok(base)
    }

    fn client(base: &str) -> client::ApiClient {
        client::ApiClient::new(base)
    }

    /// The path parameter is the input; the answer is the value, not `{data}`.
    #[tokio::test]
    async fn a_path_parameter_answered_bare() -> TestResult {
        let base = serve().await?;
        let response = reqwest::get(format!("{base}/api/user/u1")).await?;
        assert_eq!(response.status(), 200);
        let json: serde_json::Value = response.json().await?;
        assert_eq!(json, serde_json::json!({ "id": "u1", "email": "u1@example.com", "limit": null }), "@explicitNulls writes the null");

        // A segment needing escapes survives the client's encoding and the router's decoding.
        let user = client(&base)
            .users()
            .user(&types::UserInput { user_id: "a b/c".to_string() })
            .await?;
        assert_eq!(user.id, "a b/c");

        // A dot segment would be dropped from the URL, an empty one leaves no
        // route: the client refuses them before sending rather than calling
        // another endpoint.
        for user_id in ["..", ".", ""] {
            let call = client(&base)
                .users()
                .user(&types::UserInput { user_id: user_id.to_string() })
                .await;
            assert!(matches!(call, Err(client::ClientError::Encode(_))), "{user_id:?}: {call:?}");
        }
        Ok(())
    }

    /// A failure keeps the envelope and the status the service gave it.
    #[tokio::test]
    async fn a_failure_is_the_envelope_under_its_status() -> TestResult {
        let base = serve().await?;
        let response = reqwest::get(format!("{base}/api/user/missing")).await?;
        assert_eq!(response.status(), 404);
        let json: serde_json::Value = response.json().await?;
        assert_eq!(json.pointer("/errors/0/code"), Some(&serde_json::json!("not_found")), "{json}");

        let call = client(&base)
            .users()
            .user(&types::UserInput { user_id: "missing".to_string() })
            .await;
        match call {
            Err(client::ClientError::Api(errors)) => {
                assert!(errors.is(types::UsersUserCodes::NotFound));
            }
            other => return Err(format!("expected the not_found envelope, got {other:?}").into()),
        }

        // Left at `200`, a bare operation's failure would read as its success.
        let response = reqwest::get(format!("{base}/api/user/unstated")).await?;
        assert_eq!(response.status(), 500);
        let call = client(&base)
            .users()
            .user(&types::UserInput { user_id: "unstated".to_string() })
            .await;
        match call {
            Err(client::ClientError::Api(errors)) => {
                assert!(errors.is(types::UsersUserCodes::NotFound));
            }
            other => return Err(format!("expected the not_found envelope, got {other:?}").into()),
        }
        Ok(())
    }

    /// A refused session answers `401` and a denied role `403`, though the
    /// context wrote both under `200`; an admitted caller is answered bare.
    #[tokio::test]
    async fn session_and_role_refusals_carry_a_status() -> TestResult {
        let base = serve().await?;
        let http = reqwest::Client::new();
        let anonymous = http.get(format!("{base}/api/whoami")).send().await?;
        assert_eq!(anonymous.status(), 401);
        let json: serde_json::Value = anonymous.json().await?;
        assert_eq!(json.pointer("/errors/0/code"), Some(&serde_json::json!("unauthenticated")), "{json}");

        let denied = http.get(format!("{base}/api/whoami")).header("x-user", "guest").send().await?;
        assert_eq!(denied.status(), 403);

        let admitted = http.get(format!("{base}/api/whoami")).header("x-user", "admin").send().await?;
        assert_eq!(admitted.status(), 200);
        let json: serde_json::Value = admitted.json().await?;
        assert_eq!(json, serde_json::json!({ "user_id": "admin" }));
        Ok(())
    }

    /// Query parameters take their declared types; a wrong one is `invalid_body`.
    #[tokio::test]
    async fn the_query_string_is_the_input() -> TestResult {
        let base = serve().await?;
        let json: serde_json::Value =
            reqwest::get(format!("{base}/api/user_by_email?email=a%40b.c&limit=3")).await?.json().await?;
        assert_eq!(json, serde_json::json!({ "id": "u1", "email": "a@b.c", "limit": 3 }));

        let response = reqwest::get(format!("{base}/api/user_by_email?email=a%40b.c&limit=lots")).await?;
        assert_eq!(response.status(), 400, "an input that does not parse says so in the status line");
        let json: serde_json::Value = response.json().await?;
        assert_eq!(json.pointer("/errors/0/code"), Some(&serde_json::json!("invalid_body")), "{json}");

        let user = client(&base)
            .users()
            .by_email(&types::FindInput { email: "x@y.z".to_string(), limit: Some(2) })
            .await?;
        assert_eq!((user.email.as_str(), user.limit), ("x@y.z", Some(2)));
        Ok(())
    }

    /// `ANY`: a GET reads the query string, a POST a form body over it, and a
    /// body that is not a form is not read.
    #[tokio::test]
    async fn any_method_reads_a_form_over_the_query_string() -> TestResult {
        let base = serve().await?;
        let http = reqwest::Client::new();

        let get: serde_json::Value = http.get(format!("{base}/api/info?session_id=q")).send().await?.json().await?;
        assert_eq!(get, serde_json::json!({ "session_id": "q", "verbose": false }));

        let form: serde_json::Value = http
            .post(format!("{base}/api/info?session_id=q&verbose=true"))
            .header("content-type", "application/x-www-form-urlencoded")
            .body("session_id=f")
            .send()
            .await?
            .json()
            .await?;
        assert_eq!(form, serde_json::json!({ "session_id": "f", "verbose": true }), "the body wins per field");

        // A media type is case-insensitive.
        let cased: serde_json::Value = http
            .post(format!("{base}/api/info?session_id=q"))
            .header("content-type", "Application/X-WWW-Form-Urlencoded; charset=UTF-8")
            .body("session_id=c")
            .send()
            .await?
            .json()
            .await?;
        assert_eq!(cased, serde_json::json!({ "session_id": "c", "verbose": false }));

        let json_body: serde_json::Value = http
            .post(format!("{base}/api/info?session_id=q"))
            .header("content-type", "application/json")
            .body(r#"{"session_id":"j"}"#)
            .send()
            .await?
            .json()
            .await?;
        assert_eq!(json_body, serde_json::json!({ "session_id": "q", "verbose": false }));

        let patched = http.patch(format!("{base}/api/info?session_cookie=c")).send().await?;
        assert_eq!(patched.status(), 200);

        let info = client(&base)
            .users()
            .info(&types::InfoInput {
                session_id: None,
                session_cookie: Some("c".to_string()),
                verbose: Some(true),
            })
            .await?;
        assert_eq!((info.session_id.as_deref(), info.verbose), (Some("c"), true));
        Ok(())
    }

    /// No input: any method reaches it, and nothing is read from the request.
    #[tokio::test]
    async fn no_input_answers_every_method() -> TestResult {
        let base = serve().await?;
        let http = reqwest::Client::new();
        for method in [reqwest::Method::GET, reqwest::Method::POST, reqwest::Method::DELETE] {
            let response = http.request(method.clone(), format!("{base}/api/me")).body("{ not json").send().await?;
            assert_eq!(response.status(), 200, "{method}");
            let json: serde_json::Value = response.json().await?;
            assert_eq!(json, serde_json::json!({ "user_id": "u1" }), "{method}");
        }
        let me = client(&base).users().me().await?;
        assert_eq!(me.user_id, "u1");
        Ok(())
    }

    /// A JSON body beside a path parameter: the path wins, the envelope stays.
    #[tokio::test]
    async fn a_json_body_and_the_path_with_the_envelope() -> TestResult {
        let base = serve().await?;
        let response = reqwest::Client::new()
            .put(format!("{base}/items/{ID}"))
            .json(&serde_json::json!({ "id": OTHER_ID, "name": "renamed" }))
            .send()
            .await?;
        assert_eq!(response.status(), 200);
        let json: serde_json::Value = response.json().await?;
        assert_eq!(json, serde_json::json!({ "data": { "id": ID, "name": "renamed" } }));

        let renamed = client(&base)
            .users()
            .rename(&types::RenameInput { id: ID.parse()?, name: "again".to_string() })
            .await?;
        assert_eq!((renamed.id.to_string().as_str(), renamed.name.as_str()), (ID, "again"));
        Ok(())
    }

    /// Two methods on one path reach two operations, in the router and in the table.
    #[tokio::test]
    async fn two_methods_share_a_path() -> TestResult {
        let base = serve().await?;
        let response = reqwest::Client::new().delete(format!("{base}/api/user/u9")).send().await?;
        let json: serde_json::Value = response.json().await?;
        assert_eq!(json, serde_json::json!({ "user_id": "u9" }));
        let info: serde_json::Value = reqwest::get(format!("{base}/api/info")).await?.json().await?;
        assert_eq!(info, serde_json::json!({ "verbose": false }), "without @explicitNulls an absent value is left out");
        let dropped = client(&base)
            .users()
            .drop(&types::UserInput { user_id: "u8".to_string() })
            .await?;
        assert_eq!(dropped.user_id, "u8");

        use types::operations::by_route;
        let id = |method: &str, path: &str| by_route(method, path).map(|op| op.id);
        assert_eq!(id("GET", "/api/user/{user_id}"), Some("users_user"));
        assert_eq!(id("DELETE", "/api/user/{user_id}"), Some("users_drop"));
        assert_eq!(id("POST", "/api/user/{user_id}"), None);
        assert_eq!(id("PATCH", "/api/info"), Some("users_info"), "ANY answers every method");
        // The request's own path, as a gate reads it off the URI.
        assert_eq!(id("GET", "/api/user/u1"), Some("users_user"));
        assert_eq!(id("DELETE", "/api/user/u1"), Some("users_drop"));
        assert_eq!(id("POST", "/api/user/u1"), None);
        assert_eq!(id("GET", "/api/user/"), None, "a parameter takes a non-empty segment");
        assert_eq!(id("GET", "/api/user/a/b"), None, "and one segment only");
        assert_eq!(id("GET", "/api/whoami"), Some("users_whoami"));

        // A fixed segment wins over a parameter, in the router and in the table,
        // and a method the fixed route lacks is not handed to the parameter's.
        assert_eq!(id("GET", "/api/user/new"), Some("users_blank"));
        assert_eq!(id("DELETE", "/api/user/new"), None);
        let fixed: serde_json::Value = reqwest::get(format!("{base}/api/user/new")).await?.json().await?;
        assert_eq!(fixed, serde_json::json!({ "user_id": "new" }));
        let other = reqwest::Client::new().delete(format!("{base}/api/user/new")).send().await?;
        assert_eq!(other.status(), 405, "the router does not fall back to the parameter's route either");
        Ok(())
    }
}
"##;

/// The three Rust targets, emitted into one crate.
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

/// The scratch project, and what the generated code reaches for: the runtime
/// is `treat`'s, an `@http` API needs its `rpc-server` and `rpc-client` and no
/// `rpc-stream`. `reqwest` and `tokio` are the driver's, for requests the
/// generated client would never send.
const PROJECT: common::ScratchProject = common::ScratchProject {
    name: "http_wire",
    dir: "http-wire-test",
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
        Dependency::with("treat", &["rpc-client", "rpc-server", "rpc-status-header"]),
        Dependency::with("uuid", &["serde"]),
        Dependency::with("validator", &["derive"]),
    ],
    test_args: &["--lib"],
    target: None,
    edition: "2021",
};

#[test]
fn the_generated_http_server_and_client_compile_and_agree() -> TestResult {
    let (types, server, client) = generated()?;
    let lib = "pub mod client;\nmod driver;\npub mod server;\npub mod types;\n";
    let generated = ["types.rs", "server.rs", "client.rs"];
    PROJECT.check(
        &[
            ("types.rs", &types),
            ("server.rs", &server),
            ("client.rs", &client),
            ("driver.rs", DRIVER),
            ("lib.rs", lib),
        ],
        &generated,
    )
}
