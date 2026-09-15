//! The generated oneOf input, compiled and run: the wire shape the SDL claims
//! and the arm constraints it declares.
//!
//! A snapshot test asserts what the emitter writes; this test asserts what the
//! written code *does* — so the externally tagged oneOf shape (a body is
//! exactly one key, carrying the chosen arm's value) and the arm checks are
//! pinned at runtime rather than left to a reader's imagination of the
//! emitted text.
//!
//! The SDL also declares one service, so the emitted error vocabulary is
//! compiled rather than assumed: an `error_set!` with members, and the
//! `TryFrom` / `Undeclared` impls a `narrow` goes through. A set is macro
//! input, so its syntax is only ever checked by compiling it — a snapshot
//! that matches the emitted text says nothing about whether `error_set`
//! accepts it.
//!
//! The generated types get their own scratch cargo project: the project's
//! target directory is stable under this crate's `target/`, so the
//! dependencies compile once and later runs reuse them. The scratch project's
//! lockfile is kept across runs and the build is offline, so nothing reaches
//! the network and nothing drifts from the versions this crate's lockfile
//! actually pins.

/// What a test body answers with: the first failure ends it, carrying the
/// message of whatever actually went wrong rather than a fixed one.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

use std::process::Command;

const SDL: &str = r#"
scalar UUID

enum ErrorCode {
  invalid_body
  internal_error
  bad_request
  not_found
}

"A unix second count, 0 through 10 in the test below."
scalar ZoroTime
  @scalar(rust: "u32", typescript: "number", openapiType: "integer", openapiFormat: "int64", rustNewtype: true, rustCopy: true, rustRange: true)
  @range(min: 0, max: 10)

"A nickname, between one and twelve characters."
scalar Nick @scalar(rust: "String", typescript: "string", openapiType: "string", rustNewtype: true) @length(min: 1, max: 12)

"A slug, inherited: lowercase letters, digits, dashes."
scalar Slug @scalar(rust: "String", typescript: "string", openapiType: "string", rustNewtype: true) @pattern(value: "^[a-z0-9-]+$")

"""
A money amount. The one newtype here over a floating-point representation,
which is the case `Eq`, `Ord` and `Hash` cannot be derived for — and the case
`rustRange` exists for, so the two have to hold at once.
"""
scalar Money
  @scalar(rust: "f64", typescript: "number", openapiType: "number", rustNewtype: true, rustRange: true)
  @range(min: 0)

input CardInput { token: String! }
input CryptoInput { coin: String! }

type CardPayment @variant(tag: "card") { last4: String! }
type CryptoPayment { network: String! }
union PaymentMethod = CardPayment | CryptoPayment

input PaymentInput @oneOf {
  card: CardInput
  crypto: CryptoInput
  nick: Nick
  slug: Slug
  tag: String @pattern(value: "^[a-z]+$")
  exp: ZoroTime
  amount: Money
}

"""
An expression that describes itself. A `@oneOf` input is emitted as an enum,
and an arm naming the enum has to be boxed or the type has no finite size.
"""
input ExprInput @oneOf {
  negate: ExprInput
  literal: Int
}

type Things @service {
  get(input: CardInput!): CardPayment! @query @throws(codes: ["bad_request"])
}
"#;

/// The driver: one wire-shape test and one arm-validation test per kind of
/// constraint the emitter can attach to an arm.
const DRIVER: &str = r##"
/// What a test body answers with: the first failure ends it, carrying the
/// message of whatever actually went wrong rather than a fixed one.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[test]
fn the_one_of_wire_shape_is_one_key_with_the_chosen_arm() -> TestResult {
    let json = serde_json::to_string(&PaymentInput::Card(CardInput {
        token: "tok".into(),
    }))?;
    assert_eq!(json, r#"{"card":{"token":"tok"}}"#);
    Ok(())
}

#[test]
fn a_body_naming_two_arms_is_rejected() -> TestResult {
    let two = r#"{"card":{"token":"t"},"crypto":{"coin":"btc"}}"#;
    assert!(serde_json::from_str::<PaymentInput>(two).is_err());
    Ok(())
}

#[test]
fn a_stray_key_is_rejected() -> TestResult {
    let stray = r#"{"card":{"token":"t"},"bogus":1}"#;
    assert!(serde_json::from_str::<PaymentInput>(stray).is_err());
    Ok(())
}

#[test]
fn an_arm_satisfying_its_scalar_constraints_passes() -> TestResult {
    let ok: PaymentInput = serde_json::from_str(r#"{"nick":"andrew"}"#)?;
    assert!(ok.validate().is_ok());
    Ok(())
}

#[test]
fn an_arm_breaking_its_scalar_constraints_is_rejected() -> TestResult {
    // One char is inside @length(min: 1); the twelve-char maximum is the live
    // bound, so over it is what must fail.
    let long: PaymentInput = serde_json::from_str(r#"{"nick":"a-very-long-nickname"}"#)?;
    let Err(err) = long.validate() else {
        return Err("@length(max: 12) applies to the arm".into());
    };
    assert!(err.0.contains_key("nick"), "the error must point at the arm: {err:?}");
    Ok(())
}

#[test]
fn an_inherited_pattern_on_a_newtype_arm_is_checked() -> TestResult {
    // The pattern is declared on the scalar and inherited by the arm; the arm
    // measures it through the newtype's ValidateRegex adapter.
    let ok: PaymentInput = serde_json::from_str(r#"{"slug":"ok-1"}"#)?;
    assert!(ok.validate().is_ok());
    let bad: PaymentInput = serde_json::from_str(r#"{"slug":"NOpe"}"#)?;
    let Err(err) = bad.validate() else {
        return Err("@pattern applies to the arm".into());
    };
    assert!(err.0.contains_key("slug"), "the error must point at the arm: {err:?}");
    Ok(())
}

#[test]
fn a_field_level_pattern_arm_is_checked_too() -> TestResult {
    let ok: PaymentInput = serde_json::from_str(r#"{"tag":"ab"}"#)?;
    assert!(ok.validate().is_ok());
    let bad: PaymentInput = serde_json::from_str(r#"{"tag":"AB"}"#)?;
    let Err(err) = bad.validate() else {
        return Err("the field's own @pattern applies".into());
    };
    assert!(err.0.contains_key("tag"), "the error must point at the arm: {err:?}");
    Ok(())
}

#[test]
fn a_range_arm_is_checked_against_its_bounds() -> TestResult {
    // Both declared bounds must actually bite: 0 and 10 are inside, 11 is
    // not. A bound that cannot fail (min: 0 on an unsigned type) would prove
    // only that the path compiles.
    for inside in ["0", "10"] {
        let ok: PaymentInput = serde_json::from_str(&format!(r#"{{"exp":{inside}}}"#))?;
        assert!(ok.validate().is_ok(), "{inside} is inside @range(min: 0, max: 10)");
    }
    let out: PaymentInput = serde_json::from_str(r#"{"exp":11}"#)?;
    let Err(err) = out.validate() else {
        return Err("@range(max: 10) applies to the arm".into());
    };
    assert!(err.0.contains_key("exp"), "the error must point at the arm: {err:?}");
    Ok(())
}

#[test]
fn a_nested_arm_validates_itself() -> TestResult {
    let v: PaymentInput = serde_json::from_str(r#"{"card":{"token":"t"}}"#)?;
    assert!(v.validate().is_ok());
    Ok(())
}

#[test]
fn a_recursive_one_of_arm_is_boxed_and_still_round_trips() -> TestResult {
    // The boxing is a Rust concern only: the wire shape is the same one key
    // holding the chosen arm, nested.
    let nested = ExprInput::Negate(Box::new(ExprInput::Literal(3)));
    assert_eq!(serde_json::to_string(&nested)?, r#"{"negate":{"literal":3}}"#);

    let parsed: ExprInput = serde_json::from_str(r#"{"negate":{"negate":{"literal":1}}}"#)?;
    assert!(parsed.validate().is_ok());
    Ok(())
}

#[test]
fn a_float_newtype_arm_is_checked_against_its_bound() -> TestResult {
    let ok: PaymentInput = serde_json::from_str(r#"{"amount":0.5}"#)?;
    assert!(ok.validate().is_ok());

    let below: PaymentInput = serde_json::from_str(r#"{"amount":-1.5}"#)?;
    let Err(err) = below.validate() else {
        return Err("@range(min: 0) applies to the arm".into());
    };
    assert!(err.0.contains_key("amount"), "the error must point at the arm: {err:?}");
    Ok(())
}

#[test]
fn a_declared_code_narrows_into_the_operation_set() -> TestResult {
    let raised: Result<(), Failure> = Err(treat::error(ErrorCode::bad_request));
    let narrowed = raised.narrow::<ThingsGetCodes>();
    let Err(err) = narrowed else {
        return Err("a failure stays a failure".into());
    };
    assert!(matches!(err.code(), ThingsGetCodes::BadRequest));
    Ok(())
}

#[test]
fn a_set_carries_the_runtime_codes_the_operation_never_declared() -> TestResult {
    let raised: Result<(), Failure> = Err(treat::error(ErrorCode::invalid_body));
    let Err(err) = raised.narrow::<ThingsGetCodes>() else {
        return Err("a failure stays a failure".into());
    };
    assert!(matches!(err.code(), ThingsGetCodes::InvalidBody));
    assert!(matches!(
        ThingsGetCodes::UNDECLARED,
        ThingsGetCodes::InternalError
    ));
    Ok(())
}
"##;

fn generated_types() -> TestResult<String> {
    let api = graphql_rpcgen::compile_str(SDL)?;
    Ok(graphql_rpcgen::generate_rust::generate_types(
        &api,
        &graphql_rpcgen::config::Config::default(),
    )?)
}

/// The dependencies the generated types reach for, with this crate's lockfile
/// as the source of truth for their versions. They are dev-dependencies here:
/// the generator needs none of them, but the lockfile has to carry them for
/// this test to resolve a version offline.
const DEPENDENCIES: &[&str] = &[
    "chrono",
    "derive_more",
    "error_set",
    "regex",
    "serde",
    "serde_json",
    "treat",
    "uuid",
    "validator",
];

/// Per-dependency features, exactly as the generated types need them.
fn features_of(dep: &str) -> &'static str {
    match dep {
        "chrono" => r#"{ version = "{v}", features = ["serde"] }"#,
        "derive_more" => r#"{ version = "{v}", features = ["from", "into", "display", "as_ref"] }"#,
        "serde" => r#"{ version = "{v}", features = ["derive"] }"#,
        "uuid" => r#"{ version = "{v}", features = ["serde"] }"#,
        "validator" => r#"{ version = "{v}", features = ["derive"] }"#,
        _ => r#""{v}""#, // a plain version requirement
    }
}

fn manifest_for(root: &std::path::Path) -> TestResult<String> {
    // Pinning to this crate's lockfile is what keeps this build hermetic
    // and in sync: a version bump there lands here on the next run, and a
    // version the lock does not contain is an error instead of a fetch.
    let lock = std::fs::read_to_string(root.join("Cargo.lock"))?;
    let lock: toml::Table = toml::from_str(&lock)?;
    let mut deps = Vec::new();
    for dep in DEPENDENCIES {
        let packages = lock["package"].as_array().ok_or("lockfile has packages")?;
        let version = packages
            .iter()
            .find_map(|p| {
                if p.get("name")?.as_str() == Some(*dep) {
                    p.get("version")?.as_str().map(str::to_string)
                } else {
                    None
                }
            })
            .ok_or_else(|| {
                format!("`{dep}` is in the generated types' dependency set but not in Cargo.lock")
            })?;
        deps.push(format!(
            "{dep} = {}",
            features_of(dep).replacen("{v}", &version, 1)
        ));
    }
    Ok(format!(
        "[package]\nname = \"oneof_wire\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\n{}\n\n[workspace]\n",
        deps.join("\n")
    ))
}

/// Compile the generated types plus the driver in a scratch cargo project and
/// run the driver's tests.
fn compile_and_run() -> TestResult {
    let root = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;

    // Stable locations: the project's lockfile and target directory survive
    // across runs, so the dependency tree resolves once, offline, and later
    // runs reuse it. Only the generated source is rewritten.
    let project = root.join("target").join("oneof-test");
    let project_target = root.join("target").join("oneof-test-target");
    std::fs::create_dir_all(project.join("src"))?;

    let lib = generated_types()? + "\n" + DRIVER;
    std::fs::write(project.join("src").join("lib.rs"), lib)?;
    std::fs::write(project.join("Cargo.toml"), manifest_for(&root)?)?;

    let output = Command::new("cargo")
        .args(["test", "--quiet", "--offline"])
        .current_dir(&project)
        .env("CARGO_TARGET_DIR", &project_target)
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the generated oneOf types must build and pass their wire tests:\n{stdout}\n{stderr}"
    );
    Ok(())
}

#[test]
fn one_of_wire_shape_and_arm_validation() -> TestResult {
    compile_and_run()?;
    Ok(())
}
