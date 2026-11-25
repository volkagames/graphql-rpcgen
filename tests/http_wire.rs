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

/// What a test body answers with: the first failure ends it, carrying the
/// message of whatever actually went wrong rather than a fixed one.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

use std::process::Command;

/// One operation per form the server and the clients branch on: a path
/// parameter, a query string, `ANY` reading a form body over the query string,
/// no input at all, a JSON body beside a path parameter with the envelope kept,
/// and a second method on a path already taken.
const SDL: &str = r#"
scalar UUID

enum ErrorCode {
  invalid_body
  internal_error
  not_found
  "Declared by no operation, so every generated set keeps a reachable fallback."
  database_error
}

input UserInput { user_id: String! }
input FindInput { email: String! limit: Int }
input InfoInput { session_id: String session_cookie: String verbose: Boolean }
input RenameInput { id: UUID! name: String! }

"Every key present, an absent value as null: the format the clients already read."
type User @explicitNulls { id: String! email: String! limit: Int }
type Info { session_id: String verbose: Boolean! }
type Renamed { id: UUID! name: String! }
type Me { user_id: String! }

type Users @service {
  "A path parameter, answered bare."
  user(input: UserInput!): User!
    @query
    @http(method: GET, path: "/api/user/{user_id}", envelope: false)
    @throws(codes: ["not_found"])

  "Everything in the query string."
  byEmail(input: FindInput!): User!
    @query
    @http(method: GET, path: "/api/user_by_email", envelope: false)

  "Any method; a form body over the query string."
  info(input: InfoInput!): Info!
    @query
    @http(method: ANY, path: "/api/info", body: FORM, envelope: false)

  "Any method, nothing to read."
  me: Me!
    @query
    @http(method: ANY, path: "/api/me", envelope: false)

  "A JSON body beside a path parameter, the envelope kept."
  rename(input: RenameInput!): Renamed!
    @mutation
    @http(method: PUT, path: "/items/{id}")

  "A second method on a path `user` already answers."
  drop(input: UserInput!): Me!
    @mutation
    @http(method: DELETE, path: "/api/user/{user_id}", envelope: false)
}
"#;

const DRIVER: &str = r##"
// --- driver ------------------------------------------------------------------

#[derive(Clone)]
pub struct Users;

#[async_trait::async_trait]
impl server::UsersService for Users {
    async fn user(
        &self,
        input: server::UserInput,
    ) -> Result<server::User, server::ApiError<server::UsersUserCodes>> {
        if input.user_id == "missing" {
            return Err(server::error(server::UsersUserCodes::NotFound).with_status(404));
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
}

#[cfg(test)]
mod wire {
    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    const ID: &str = "7b1e3c2a-0000-4000-8000-000000000001";
    const OTHER_ID: &str = "7b1e3c2a-0000-4000-8000-000000000002";

    async fn serve() -> TestResult<String> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let router = crate::server::users_router(super::Users);
        tokio::spawn(async move { axum::serve(listener, router).await });
        Ok(base)
    }

    fn client(base: &str) -> crate::client::ApiClient {
        crate::client::ApiClient::new(base)
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
            .user(&crate::types::UserInput { user_id: "a b/c".to_string() })
            .await?;
        assert_eq!(user.id, "a b/c");
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
            .user(&crate::types::UserInput { user_id: "missing".to_string() })
            .await;
        match call {
            Err(crate::client::ClientError::Api(errors)) => {
                assert!(errors.is(crate::types::UsersUserCodes::NotFound));
            }
            other => return Err(format!("expected the not_found envelope, got {other:?}").into()),
        }
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
        let json: serde_json::Value = response.json().await?;
        assert_eq!(json.pointer("/errors/0/code"), Some(&serde_json::json!("invalid_body")), "{json}");

        let user = client(&base)
            .users()
            .by_email(&crate::types::FindInput { email: "x@y.z".to_string(), limit: Some(2) })
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
            .info(&crate::types::InfoInput {
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
            .rename(&crate::types::RenameInput { id: ID.parse()?, name: "again".to_string() })
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
            .drop(&crate::types::UserInput { user_id: "u8".to_string() })
            .await?;
        assert_eq!(dropped.user_id, "u8");

        use crate::server::operations::by_route;
        let id = |method: &str, path: &str| by_route(method, path).map(|op| op.id);
        assert_eq!(id("GET", "/api/user/{user_id}"), Some("users_user"));
        assert_eq!(id("DELETE", "/api/user/{user_id}"), Some("users_drop"));
        assert_eq!(id("POST", "/api/user/{user_id}"), None);
        assert_eq!(id("PATCH", "/api/info"), Some("users_info"), "ANY answers every method");
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

/// What the generated code reaches for. `form_urlencoded` is the `@http`
/// runtime's: it decodes query strings and form bodies. No `futures-*` and no
/// `reqwest` `stream` feature: an `@http` API compiles neither.
const DEPENDENCIES: &[&str] = &[
    "async-trait",
    "axum",
    "chrono",
    "derive_more",
    "error_set",
    "form_urlencoded",
    "regex",
    "reqwest",
    "serde",
    "serde_json",
    "tokio",
    "treat",
    "uuid",
    "validator",
];

fn features_of(dep: &str) -> &'static str {
    match dep {
        "chrono" => r#"{ version = "{v}", features = ["serde"] }"#,
        "derive_more" => r#"{ version = "{v}", features = ["from", "into", "display", "as_ref"] }"#,
        "reqwest" => {
            r#"{ version = "{v}", default-features = false, features = ["json", "rustls"] }"#
        }
        "serde" => r#"{ version = "{v}", features = ["derive"] }"#,
        "tokio" => r#"{ version = "{v}", features = ["macros", "net", "rt"] }"#,
        "treat" => {
            r#"{ version = "{v}", features = ["axum", "validator-extract", "rpc-status-header"] }"#
        }
        "uuid" => r#"{ version = "{v}", features = ["serde"] }"#,
        "validator" => r#"{ version = "{v}", features = ["derive"] }"#,
        _ => r#""{v}""#,
    }
}

/// A manifest pinned to this crate's lockfile, as `raw_wire` builds one.
fn manifest_for(root: &std::path::Path) -> TestResult<String> {
    let lock = std::fs::read_to_string(root.join("Cargo.lock"))?;
    let lock: toml::Table = toml::from_str(&lock)?;
    let packages = lock
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or("lockfile has no [[package]] array")?;
    let mut deps = Vec::new();
    for dep in DEPENDENCIES {
        let versions: Vec<&str> = packages
            .iter()
            .filter(|p| p.get("name").and_then(toml::Value::as_str) == Some(*dep))
            .filter_map(|p| p.get("version").and_then(toml::Value::as_str))
            .collect();
        let [version] = versions.as_slice() else {
            return Err(format!(
                "Cargo.lock must carry exactly one version of `{dep}`, found {versions:?}"
            )
            .into());
        };
        deps.push(format!(
            "{dep} = {}",
            features_of(dep).replacen("{v}", version, 1)
        ));
    }
    Ok(format!(
        "[package]\nname = \"http_wire\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\n{}\n\n[workspace]\n",
        deps.join("\n")
    ))
}

#[test]
fn the_generated_http_server_and_client_compile_and_agree() -> TestResult {
    let root = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;
    let project = root.join("target").join("http-wire-test");
    let project_target = root.join("target").join("http-wire-test-target");
    let src = project.join("src");
    std::fs::create_dir_all(&src)?;
    let (types, server, client) = generated()?;
    std::fs::write(src.join("types.rs"), types)?;
    std::fs::write(src.join("server.rs"), server)?;
    std::fs::write(src.join("client.rs"), client)?;
    std::fs::write(
        src.join("lib.rs"),
        format!("pub mod client;\npub mod server;\npub mod types;\n{DRIVER}"),
    )?;
    std::fs::write(project.join("Cargo.toml"), manifest_for(&root)?)?;
    let output = Command::new("cargo")
        .args(["test", "--lib", "--quiet", "--offline"])
        .current_dir(&project)
        .env("CARGO_TARGET_DIR", &project_target)
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the generated @http server and client must compile and pass the wire tests:\n{stdout}\n{stderr}"
    );
    Ok(())
}
