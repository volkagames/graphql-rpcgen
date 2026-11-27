//! Error groups, compiled and run: the failure types `@errorGroup` generates,
//! carried the way a layered service carries them.
//!
//! `golden` pins what is emitted for `tests/fixtures/error_groups.graphql`;
//! this test pins what that code does once rustc has it. Business code names
//! its group's code where the failure happens — `.wrap_api_code(FolderCode::x)?`
//! into an `erris::Result<_, FolderFailure>` — a failure of an included group
//! converts into the including one keeping everything but its code, a group's
//! failure joins the flat registry and narrows back into a group, and the
//! service trait's edge takes a group's failure with a plain `?` exactly where
//! the operation's `@throws` covers the group. The last is also pinned from
//! the other side: an edge whose operation does not cover the group is a
//! build that fails.
//!
//! A failure type is an `erris::tracked::TrackedError`, so the project builds
//! `treat` with its `tracked` feature, which turns on erris's nightly
//! `tracked` mode: this test needs a nightly toolchain, as the generated code
//! of an API with error groups does. The feature changes what `treat` and
//! `erris` compile to, so the two projects here build in a target directory
//! of their own rather than the one the other wire tests share.

#![allow(
    clippy::panic_in_result_fn,
    reason = "a test asserts, and returns `Result` only to use `?` on its setup"
)]

/// What a test body answers with: the first failure ends it, carrying the
/// message of whatever actually went wrong rather than a fixed one.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

mod common;

use common::{Dependency, ScratchProject};
use graphql_rpcgen::config::Config;
use graphql_rpcgen::generate_rust;

/// Groups, an include of two groups, `@message`, a tagged group code, and
/// operations covering a group by naming it, by listing its codes, and not
/// at all.
const SDL: &str = include_str!("fixtures/error_groups.graphql");

/// The folder area's code, written as a service below the edge writes it:
/// each failure named where it happens, each layer a `?`.
const AREA: &str = r##"
//! Business code of the folder area. Not generated code, so the lints the
//! generated module is held to are not this module's.
#![allow(clippy::all, dead_code, unused_imports, reason = "a test driver, not generated code")]

use crate::types::*;
use treat::erris::prelude::*;
use treat::prelude::*;

/// A storage read that fails: the cause every refusal below keeps.
pub fn disk() -> std::result::Result<(), std::io::Error> {
    std::result::Result::Err(std::io::Error::other("disk gone"))
}

/// Who may reach `path`: `secret` is refused.
pub fn check_access(path: &str) -> treat::erris::Result<(), AccessFailure> {
    if path == "secret" {
        disk().wrap_api_code(AccessCode::forbidden)?;
    }
    Ok(())
}

/// A layer above the check, so a refusal passes one `?` on its way up.
pub fn reach(path: &str) -> treat::erris::Result<(), AccessFailure> {
    check_access(path)?;
    Ok(())
}

/// Making a folder: its own codes, raised where the failure happens.
pub fn make(path: &str) -> treat::erris::Result<Folder, FolderFailure> {
    match path {
        "deep" => disk().wrap_api_code(FolderCode::path_too_deep)?,
        "bad" => disk().wrap_api_code(FolderCode::name_refused)?,
        "file" => return Err(FolderCode::kind_conflict.into()),
        _ => {}
    }
    Ok(Folder { path: path.to_string() })
}

/// Taking a lock: `held` is locked.
pub fn take_lock(path: &str) -> treat::erris::Result<(), LockFailure> {
    if path == "held" {
        disk().wrap_api_code(LockCode::locked)?;
    }
    Ok(())
}
"##;

/// The edge: the service trait, each operation converting its area's failure
/// once, with a `?`.
const EDGE: &str = r##"
//! The service trait's implementation. Not generated code.
#![allow(clippy::all, dead_code, unused_imports, reason = "a test driver, not generated code")]

use crate::area;
use crate::server::*;

#[derive(Clone)]
pub struct Service;

#[async_trait::async_trait]
impl FoldersService for Service {
    async fn lock(&self, input: PathInput) -> Result<ApiResponse<Folder>, ApiError<FoldersLockCodes>> {
        area::reach(&input.path)?;
        area::take_lock(&input.path)?;
        Ok(success(Folder { path: input.path }))
    }

    async fn mkdir(&self, input: PathInput) -> Result<ApiResponse<Folder>, ApiError<FoldersMkdirCodes>> {
        area::reach(&input.path)?;
        Ok(success(area::make(&input.path)?))
    }

    async fn stat(&self, input: PathInput) -> Result<ApiResponse<Folder>, ApiError<FoldersStatCodes>> {
        Ok(success(Folder { path: input.path }))
    }
}
"##;

/// The tests, over the area and the edge.
const DRIVER: &str = r##"
//! The test driver. Not generated code.
#![allow(clippy::all, dead_code, unused_imports, reason = "a test driver, not generated code")]

use crate::area;
use crate::edge::Service;
use crate::server::*;

type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn cause(error: &dyn std::error::Error) -> String {
    let mut chain = Vec::new();
    let mut link = error.source();
    while let Some(error) = link {
        chain.push(error.to_string());
        link = error.source();
    }
    chain.join(": ")
}

#[test]
fn a_code_raised_in_the_area_is_the_groups_with_its_message_and_cause() -> TestResult {
    let Err(failure) = area::make("bad").into_std() else {
        return Err("`bad` is refused".into());
    };
    assert_eq!(failure.code(), FolderCode::name_refused);
    assert_eq!(
        failure.error().message().map(|m| m.as_ref()),
        Some("the name is not allowed here"),
        "@message(text:) is the code's message"
    );
    assert!(cause(&failure).contains("disk gone"), "{}", cause(&failure));

    let Err(failure) = area::make("deep").into_std() else {
        return Err("`deep` is refused".into());
    };
    assert_eq!(failure.code(), FolderCode::path_too_deep);
    assert_eq!(failure.error().message().map(|m| m.as_ref()), Some("path too deep"));

    let Err(failure) = area::make("file").into_std() else {
        return Err("`file` is refused".into());
    };
    assert_eq!(failure.code(), FolderCode::kind_conflict);
    assert_eq!(failure.error().message().map(|m| m.as_ref()), Some("kind conflict"));
    assert!(area::make("ok").into_std().is_ok());
    Ok(())
}

#[test]
fn an_included_failure_keeps_everything_but_its_code() -> TestResult {
    let Err(access) = area::reach("secret").into_std() else {
        return Err("`secret` is refused".into());
    };
    let access = AccessFailure(
        access
            .into_error()
            .with_status(403)
            .with_meta(serde_json::json!({ "path": "secret" })),
    );
    let hops = access.error().hops().len();
    assert!(hops >= 1, "the `?` in `reach` records a hop");
    let location = treat::ApiErrorHandler::location(access.error()).to_string();

    let folder = FolderFailure::from(access);
    assert_eq!(folder.code(), FolderCode::forbidden);
    assert_eq!(folder.error().message().map(|m| m.as_ref()), Some("forbidden"));
    assert_eq!(folder.error().status(), 403);
    assert_eq!(
        folder.error().meta().and_then(|m| m.get("path")),
        Some(&serde_json::json!("secret"))
    );
    assert_eq!(folder.error().hops().len(), hops, "the hops survive");
    assert_eq!(
        treat::ApiErrorHandler::location(folder.error()).to_string(),
        location,
        "the raise site survives"
    );
    assert!(cause(&folder).contains("disk gone"), "{}", cause(&folder));

    let Err(lock) = area::take_lock("held").into_std() else {
        return Err("`held` is locked".into());
    };
    assert_eq!(FolderFailure::from(lock).code(), FolderCode::locked);
    Ok(())
}

#[test]
fn a_group_failure_joins_the_registry_and_narrows_back() -> TestResult {
    let Err(failure) = area::make("deep").into_std() else {
        return Err("`deep` is refused".into());
    };
    let flat: Failure = failure.into();
    assert_eq!(*flat.code(), ErrorCode::path_too_deep);
    assert_eq!(flat.code().as_str(), "folder.path-too-deep");
    assert_eq!(flat.message().map(|m| m.as_ref()), Some("path too deep"));

    let back = std::result::Result::<(), Failure>::Err(flat).narrow::<FolderCode>();
    let Err(back) = back else {
        return Err("a failure stays a failure".into());
    };
    assert_eq!(*back.code(), FolderCode::path_too_deep);

    // A registry code no group holds narrows to the group's undeclared value.
    let outside = std::result::Result::<(), Failure>::Err(ErrorCode::not_found.into());
    let Err(outside) = outside.narrow::<FolderCode>() else {
        return Err("a failure stays a failure".into());
    };
    assert_eq!(*outside.code(), FolderCode::internal_error);
    assert_eq!(
        outside.meta().and_then(|m| m.get(UNDECLARED_CODE_META)),
        Some(&serde_json::json!("not_found"))
    );
    assert_eq!(
        ApiError::from(ErrorCode::not_found).message().map(|m| m.as_ref()),
        Some("no such folder"),
        "@message(text:) on a registry code"
    );
    assert_eq!(serde_json::to_string(&FolderCode::path_too_deep)?, "\"folder.path-too-deep\"");
    Ok(())
}

#[tokio::test]
async fn the_edge_answers_a_group_failure_in_the_operations_set() -> TestResult {
    let Err(err) = Service.mkdir(PathInput { path: "deep".into() }).await else {
        return Err("`deep` is refused".into());
    };
    assert!(matches!(err.code(), FoldersMkdirCodes::PathTooDeep));
    assert_eq!(err.message().map(|m| m.as_ref()), Some("path too deep"));
    assert!(cause(&err).contains("disk gone"), "{}", cause(&err));

    let Err(err) = Service.mkdir(PathInput { path: "secret".into() }).await else {
        return Err("`secret` is refused".into());
    };
    assert!(matches!(err.code(), FoldersMkdirCodes::Forbidden));

    // `lock` names the access group and lists `locked`, which covers the
    // lock group without naming it.
    let Err(err) = Service.lock(PathInput { path: "held".into() }).await else {
        return Err("`held` is locked".into());
    };
    assert!(matches!(err.code(), FoldersLockCodes::Locked));

    let Ok(made) = Service.mkdir(PathInput { path: "ok".into() }).await else {
        return Err("`ok` is made".into());
    };
    assert_eq!(serde_json::to_value(&made)?["data"]["path"], "ok");
    Ok(())
}
"##;

/// `lock` written over the folder area: its `@throws` does not cover the
/// folder group, so the edge's `?` has no conversion to take.
const EDGE_UNCOVERED: &str = r##"
//! An edge whose operation does not cover the group it is written over.
#![allow(clippy::all, dead_code, unused_imports, reason = "a test driver, not generated code")]

use crate::area;
use crate::server::*;

pub async fn lock(input: PathInput) -> Result<ApiResponse<Folder>, ApiError<FoldersLockCodes>> {
    Ok(success(area::make(&input.path)?))
}
"##;

/// What the generated code reaches for, `treat` with `tracked`, and `tokio`
/// to run the service trait's async methods.
const DEPENDENCIES: &[Dependency] = &[
    Dependency::plain("async-trait"),
    Dependency::plain("axum"),
    Dependency::plain("error_set"),
    Dependency::with("serde", &["derive"]),
    Dependency::plain("serde_json"),
    Dependency::with("tokio", &["macros", "rt"]),
    Dependency::with("treat", &["rpc-server", "tracked"]),
    Dependency::with("validator", &["derive"]),
];

fn project(name: &'static str, dir: &'static str) -> ScratchProject {
    ScratchProject {
        name,
        dir,
        dependencies: DEPENDENCIES,
        test_args: &["--lib"],
        target: Some("error-groups-test-target"),
        edition: "2021",
    }
}

/// The fixture's types and server, the server reaching for `crate::types`.
fn generated() -> TestResult<(String, String)> {
    let api = graphql_rpcgen::compile_str(SDL)?;
    let mut config = Config::default();
    config.rust.types_path = Some("crate::types".to_string());
    let types = generate_rust::generate_types(&api, &config)?;
    let server = generate_rust::generate(&api, &config)?;
    Ok((types, server))
}

/// The generated code formatted and warning-free, and the driver's tests
/// passing over it.
#[test]
fn group_failures_compose_from_the_area_to_the_edge() -> TestResult {
    let (types, server) = generated()?;
    let lib = "pub mod area;\npub mod edge;\npub mod server;\npub mod types;\n\
               #[cfg(test)]\nmod driver;\n";
    project("error_groups", "error-groups-test").check(
        &[
            ("types.rs", &types),
            ("server.rs", &server),
            ("area.rs", AREA),
            ("edge.rs", EDGE),
            ("driver.rs", DRIVER),
            ("lib.rs", lib),
        ],
        &["types.rs", "server.rs"],
    )
}

/// An operation whose `@throws` does not cover a group gets no conversion
/// from the group's failure: its edge's `?` is a compile error.
#[test]
fn an_edge_outside_the_groups_codes_does_not_build() -> TestResult {
    let (types, server) = generated()?;
    let lib = "pub mod area;\npub mod edge;\npub mod server;\npub mod types;\n";
    project("error_groups_refused", "error-groups-refused-test").refuses(
        &[
            ("types.rs", &types),
            ("server.rs", &server),
            ("area.rs", AREA),
            ("edge.rs", EDGE_UNCOVERED),
            ("lib.rs", lib),
        ],
        &["types.rs", "server.rs"],
        &[
            "E0277",
            "From<types::FolderFailure>",
            "ApiError<types::FoldersLockCodes>",
        ],
    )
}
