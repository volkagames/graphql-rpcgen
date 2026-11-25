//! The generated oneOf input, compiled and run: the wire shape the SDL claims
//! and the arm constraints it declares.
//!
//! A snapshot test asserts what the emitter writes; this test asserts what the
//! written code *does* — so the externally tagged oneOf shape (a body is
//! exactly one key, carrying the chosen arm's value) and the arm checks are
//! pinned at runtime rather than left to a reader's imagination of the
//! emitted text.
//!
//! A union tagged beside its holder rides along for the same reason: serde's
//! flattened adjacent tagging is behaviour, and only running it shows that the
//! tag and the member land as two keys of the holder.
//!
//! The SDL also declares one service, so the emitted error vocabulary is
//! compiled rather than assumed: an `error_set!` with members deriving
//! `ApiErrorCode` and `WireEnum`, which `treat::rpc::Narrow` reads a flat
//! failure into the operation's set through. A set is macro input, so its
//! syntax is only ever checked by compiling it — a snapshot that matches the
//! emitted text says nothing about whether `error_set` accepts it.
//!
//! Every enum's `ALL`, `from_name` and `as_str` (`treat::rpc::WireEnum`, as
//! the generated module re-exports it) are run too: that `as_str` is exactly
//! what serde writes, a `@variant(tag:)` included, can only be shown by
//! serialising.
//!
//! The generated types get their own scratch cargo project: the project's
//! target directory is stable under this crate's `target/`, so the
//! dependencies compile once and later runs reuse them. The scratch project
//! starts from this crate's lockfile on every run and the build is offline,
//! so nothing reaches the network and nothing drifts from the versions this
//! crate's lockfile actually pins.

#![allow(
    clippy::panic_in_result_fn,
    reason = "a test asserts, and returns `Result` only to use `?` on its setup"
)]

/// What a test body answers with: the first failure ends it, carrying the
/// message of whatever actually went wrong rather than a fixed one.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

mod common;

use common::Dependency;

/// Every arm constraint, both union taggings and one service; `golden.rs`
/// keeps what each target emits for it.
const SDL: &str = include_str!("fixtures/oneof_wire.graphql");

/// The driver: one wire-shape test and one arm-validation test per kind of
/// constraint the emitter can attach to an arm.
const DRIVER: &str = r##"
//! The test driver. Not generated code, so the lints the generated module is
//! held to are not this module's.
#![allow(clippy::all, dead_code, unused_imports, reason = "a test driver, not generated code")]

use crate::types::*;
use validator::Validate;

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
fn a_sibling_tag_is_a_key_of_the_holder_not_of_the_member() -> TestResult {
    let param = Param {
        label: "cap".into(),
        settings: Setting::NumberSetting(NumberSetting { min: Some(1) }),
    };
    let json = serde_json::to_string(&param)?;
    assert_eq!(json, r#"{"label":"cap","kind":"number","settings":{"min":1}}"#);
    assert_eq!(serde_json::from_str::<Param>(&json)?, param);
    Ok(())
}

#[test]
fn the_holder_tag_selects_the_member() -> TestResult {
    // Both members would accept an empty object, so only the tag can decide.
    let flag: Param = serde_json::from_str(r#"{"label":"l","kind":"flag","settings":{}}"#)?;
    assert!(matches!(flag.settings, Setting::FlagSetting(_)));
    let number: Param = serde_json::from_str(r#"{"label":"l","kind":"number","settings":{}}"#)?;
    assert!(matches!(number.settings, Setting::NumberSetting(_)));
    Ok(())
}

#[test]
fn a_holder_with_an_unknown_or_missing_tag_is_rejected() -> TestResult {
    for body in [
        r#"{"label":"l","kind":"bogus","settings":{}}"#,
        r#"{"label":"l","settings":{}}"#,
        r#"{"label":"l","kind":"flag"}"#,
    ] {
        assert!(serde_json::from_str::<Param>(body).is_err(), "{body}");
    }
    Ok(())
}

#[test]
fn every_enum_lists_its_values_and_spells_them_as_serde_does() -> TestResult {
    assert_eq!(Visibility::ALL, &[Visibility::Private, Visibility::PublicLink]);
    for v in Visibility::ALL.iter().copied() {
        assert_eq!(serde_json::to_value(v)?, serde_json::json!(v.as_str()));
        assert_eq!(Visibility::from_name(v.as_str()), Some(v));
    }
    assert_eq!(Visibility::PublicLink.as_str(), "public-link");
    // The identifier is not a wire name.
    assert_eq!(Visibility::from_name("public_link"), None);

    assert_eq!(SettingKind::ALL, &[SettingKind::Flag, SettingKind::Number]);
    for v in SettingKind::ALL.iter().copied() {
        assert_eq!(serde_json::to_value(v)?, serde_json::json!(v.as_str()));
    }

    // The registry is an enum like any other: listed in SDL order, spelt as
    // the envelope carries it.
    assert_eq!(ErrorCode::ALL.first(), Some(&ErrorCode::invalid_body));
    assert_eq!(ErrorCode::ALL.len(), 4);
    for code in ErrorCode::ALL.iter().copied() {
        assert_eq!(serde_json::to_value(code)?, serde_json::json!(code.as_str()));
        assert_eq!(code.to_string(), code.as_str());
        assert_eq!(ErrorCode::from_name(code.as_str()), Some(code));
    }

    // `Hash`, so a value keys a set.
    let set: std::collections::HashSet<Visibility> = Visibility::ALL.iter().copied().collect();
    assert_eq!(set.len(), Visibility::ALL.len());
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
/// The scratch project, and the dependencies the generated types reach for:
/// `treat` with `validator` for the `@pattern` checks, and no `regex` of the
/// project's own — the patterns compile through `treat::rpc::validate`.
/// They are dev-dependencies here: the generator needs none of them, but the
/// lockfile has to carry them for this test to resolve a version offline.
///
/// No `--lib`: this project runs its doctests too.
const PROJECT: common::ScratchProject = common::ScratchProject {
    name: "oneof_wire",
    dir: "oneof-test",
    dependencies: &[
        Dependency::with("chrono", &["serde"]),
        Dependency::with("derive_more", &["from", "into", "display", "as_ref"]),
        Dependency::plain("error_set"),
        Dependency::with("serde", &["derive"]),
        Dependency::plain("serde_json"),
        Dependency::with("treat", &["validator"]),
        Dependency::with("uuid", &["serde"]),
        Dependency::with("validator", &["derive"]),
    ],
    test_args: &[],
    target: None,
    edition: "2021",
};

/// Compile the generated types plus the driver in a scratch cargo project and
/// run the driver's tests.
#[test]
fn one_of_wire_shape_and_arm_validation() -> TestResult {
    let types = generated_types()?;
    PROJECT.check(
        &[
            ("types.rs", &types),
            ("driver.rs", DRIVER),
            ("lib.rs", "mod driver;\npub mod types;\n"),
        ],
        &["types.rs"],
    )
}
