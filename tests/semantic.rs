//! Semantic validation: every rule from the spec gets a positive or negative case.

/// What a test body answers with: the first failure ends it, carrying the
/// message of whatever actually went wrong rather than a fixed one.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

use graphql_rpcgen::ir::{ApiType, OperationKind, TypeRef};

/// Minimal preamble so each case only states what it is testing.
const PRELUDE: &str = r#"
scalar UUID
scalar DateTime
scalar JSON

enum ErrorCode {
  invalid_body
  internal_error
  same
  x
  not_listed
}
"#;

fn compile(body: &str) -> Result<graphql_rpcgen::ir::Api, String> {
    graphql_rpcgen::compile_str(&format!("{PRELUDE}{body}")).map_err(|e| e.to_string())
}

fn expect_error(body: &str) -> TestResult<String> {
    match compile(body) {
        Ok(_) => Err("expected a compile error, but compilation succeeded".into()),
        Err(e) => Ok(e),
    }
}

/// `@throws` names registry codes, so an unlisted one must be rejected.
#[test]
fn unknown_error_code_in_throws_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
input I { id: UUID! }
type User { id: UUID! }
type Users @service {
  get(input: I!): User! @query @throws(codes: ["never_registered"])
}
"#,
    )?;
    assert!(e.contains("is not listed in `ErrorCode`"), "{e}");
    Ok(())
}

#[test]
fn duplicate_code_in_throws_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
input I { id: UUID! }
type User { id: UUID! }
type Users @service {
  get(input: I!): User! @query @throws(codes: ["same", "same"])
}
"#,
    )?;
    assert!(e.contains("lists `same` twice"), "{e}");
    Ok(())
}

#[test]
fn valid_service_compiles() -> TestResult {
    let api = compile(
        r#"
input GetUserInput { id: UUID! }
type User { id: UUID! }
type Users @service {
  get(input: GetUserInput!): User! @query
}
"#,
    )?;

    assert_eq!(api.services.len(), 1);
    let op = &api.services[0].operations[0];
    assert_eq!(op.path, "/rpc/users/get");
    assert_eq!(op.operation_id, "users_get");
    assert_eq!(op.kind, OperationKind::Query);
    Ok(())
}

#[test]
fn unknown_type_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
type User { id: UUID! missing: NoSuchType! }
"#,
    )?;
    assert!(e.contains("unknown type `NoSuchType`"), "{e}");
    Ok(())
}

#[test]
fn duplicate_type_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
type User { id: UUID! }
type User { id: UUID! }
"#,
    )?;
    assert!(e.contains("duplicate type `User`"), "{e}");
    Ok(())
}

#[test]
fn duplicate_rpc_path_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
input I { id: UUID! }
type User { id: UUID! }
type Users @service {
  get(input: I!): User! @query
  other(input: I!): User! @query @rpc(path: "/rpc/users/get")
}
"#,
    )?;
    assert!(e.contains("duplicate RPC path `/rpc/users/get`"), "{e}");
    Ok(())
}

#[test]
fn malformed_rpc_path_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
input I { id: UUID! }
type User { id: UUID! }
type Users @service {
  get(input: I!): User! @query @rpc(path: "no-leading-slash")
}
"#,
    )?;
    assert!(e.contains("must start with `/`"), "{e}");
    Ok(())
}

/// A service version prefixes every path in it, derived or pinned with `@rpc`.
#[test]
fn service_version_prefixes_every_path() -> TestResult {
    let api = compile(
        r#"
input I { id: UUID! }
type User { id: UUID! }
type Users @service @version(n: 2) {
  get(input: I!): User! @query
  pinned(input: I!): User! @query @rpc(path: "/user_pinned")
}
"#,
    )?;

    let paths: Vec<&str> = api.services[0]
        .operations
        .iter()
        .map(|o| o.path.as_str())
        .collect();
    assert_eq!(paths, ["/v2/rpc/users/get", "/v2/user_pinned"]);
    Ok(())
}

/// The point of a per-operation version: one endpoint moves to `v2` while the
/// rest of its service keeps serving `v1`, and both paths coexist.
#[test]
fn operation_version_overrides_the_service_one() -> TestResult {
    let api = compile(
        r#"
input I { id: UUID! }
type User { id: UUID! }
type Users @service @version(n: 1) {
  get(input: I!): User! @query @rpc(path: "/user_get")
  get_v2(input: I!): User! @query @rpc(path: "/user_get") @version(n: 2)
}
"#,
    )?;

    let paths: Vec<&str> = api.services[0]
        .operations
        .iter()
        .map(|o| o.path.as_str())
        .collect();
    assert_eq!(paths, ["/v1/user_get", "/v2/user_get"]);
    Ok(())
}

/// An SDL that never mentions a version stays unversioned rather than becoming
/// `v1`, so the directive is what moves a route and nothing moves by default.
#[test]
fn a_path_without_a_version_is_left_alone() -> TestResult {
    let api = compile(
        r#"
input I { id: UUID! }
type User { id: UUID! }
type Users @service {
  get(input: I!): User! @query @rpc(path: "/user_get")
}
"#,
    )?;

    assert_eq!(api.services[0].operations[0].path, "/user_get");
    Ok(())
}

#[test]
fn version_below_one_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
input I { id: UUID! }
type User { id: UUID! }
type Users @service {
  get(input: I!): User! @query @version(n: 0)
}
"#,
    )?;
    assert!(e.contains("must be 1 or greater"), "{e}");
    Ok(())
}

/// A version has nothing to prefix on an output type, so it must not look like
/// it had an effect there.
#[test]
fn version_on_a_non_service_object_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
type User @version(n: 2) { id: UUID! }
"#,
    )?;
    assert!(e.contains("@version applies to a @service"), "{e}");
    Ok(())
}

/// A constraint that cannot hold for the type it is attached to is not a
/// stricter rule but no rule at all, so it must not compile.
#[test]
fn length_on_a_number_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
input I { count: Int! @length(min: 2) }
type User { id: UUID! }
type Users @service {
  get(input: I!): User! @query
}
"#,
    )?;
    assert!(e.contains("@length applies to a string or a list"), "{e}");
    Ok(())
}

#[test]
fn range_on_a_string_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
input I { name: String! @range(min: 2) }
type User { id: UUID! }
type Users @service {
  get(input: I!): User! @query
}
"#,
    )?;
    assert!(e.contains("@range applies to a number"), "{e}");
    Ok(())
}

/// `@pattern` on a list would match the array itself, which no target can do.
#[test]
fn pattern_on_a_list_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
input I { names: [String!]! @pattern(value: "^a") }
type User { id: UUID! }
type Users @service {
  get(input: I!): User! @query
}
"#,
    )?;
    assert!(e.contains("@pattern applies to a string"), "{e}");
    Ok(())
}

/// Counting the elements of a list is what `@length` means there, so this is the
/// one case where a non-string may carry it.
#[test]
fn length_on_a_list_counts_elements() -> TestResult {
    let api = compile(
        r#"
input I { names: [String!]! @length(min: 1, max: 5) }
type User { id: UUID! }
type Users @service {
  get(input: I!): User! @query
}
"#,
    )?;

    let ApiType::InputObject(input) = api.find_type("I").ok_or("input exists")? else {
        return Err("`I` is an input object".into());
    };
    assert_eq!(input.fields[0].constraints.min_length, Some(1));
    Ok(())
}

/// A tag no union names would silently do nothing, so it is rejected instead.
#[test]
fn variant_tag_outside_a_union_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
type Alone @variant(tag: "alone") { id: UUID! }
"#,
    )?;
    assert!(e.contains("no union has it as a member"), "{e}");
    Ok(())
}

/// Two members answering to one tag make the wire ambiguous in both directions.
#[test]
fn duplicate_variant_tags_are_rejected() -> TestResult {
    let e = expect_error(
        r#"
type A @variant(tag: "same") { a: String! }
type B @variant(tag: "same") { b: String! }
union U @discriminator(field: "kind") = A | B
type Wrapper { u: U! }
"#,
    )?;
    assert!(e.contains("share the tag `same`"), "{e}");
    Ok(())
}

#[test]
fn variant_tag_reaches_the_union_member() -> TestResult {
    let api = compile(
        r#"
type A @variant(tag: "a-on-the-wire") { a: String! }
type B { b: String! }
union U @discriminator(field: "kind") = A | B
type Wrapper { u: U! }
"#,
    )?;

    let ApiType::Union(u) = api.find_type("U").ok_or("union exists")? else {
        return Err("`U` is a union".into());
    };
    assert_eq!(u.members[0].tag, "a-on-the-wire");
    // Without `@variant` the tag stays the type name, so nothing changes for a
    // member that never asked for one.
    assert_eq!(u.members[1].tag, "B");
    Ok(())
}

#[test]
fn one_of_with_non_null_member_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
input A { a: String! }
input B { b: String! }
input Bad @oneOf { a: A! b: B }
"#,
    )?;
    assert!(e.contains("must be nullable"), "{e}");
    Ok(())
}

#[test]
fn one_of_with_single_variant_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
input A { a: String! }
input Bad @oneOf { a: A }
"#,
    )?;
    assert!(e.contains("at least two variants"), "{e}");
    Ok(())
}

#[test]
fn one_of_with_list_member_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
input A { a: String! }
input Bad @oneOf { a: A list: [String] }
"#,
    )?;
    assert!(e.contains("must not be a list"), "{e}");
    Ok(())
}

/// A numeric newtype has no runtime `ValidateRange`, so `@range` reaching into
/// it is rejected in the SDL — the failure this project is built to move out
/// of rustc. Both directions of the inheritance are covered: the bound on the
/// scalar, and the bound on the field.
#[test]
fn a_range_on_a_newtype_scalar_without_rust_range_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
scalar Money @scalar(rust: "u32", typescript: "number", openapiType: "integer", rustNewtype: true) @range(min: 1)
input I { m: Money }
"#,
    )?;
    assert!(e.contains("rustRange"), "{e}");
    Ok(())
}

#[test]
fn a_field_range_on_a_newtype_field_without_rust_range_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
scalar Money @scalar(rust: "u32", typescript: "number", openapiType: "integer", rustNewtype: true)
input I { m: Money @range(min: 1) }
"#,
    )?;
    assert!(e.contains("rustRange"), "{e}");
    Ok(())
}

/// The mirror of the previous pair: opting into `@range` without a newtype
/// wraps nothing, so it is rejected where it is declared.
#[test]
fn rust_range_without_rust_newtype_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
scalar Bad @scalar(rust: "u32", typescript: "number", openapiType: "integer", rustRange: true)
input I { m: Bad }
"#,
    )?;
    assert!(e.contains("only applies to a newtype"), "{e}");
    Ok(())
}

#[test]
fn a_newtype_with_rust_range_and_a_bound_compiles() -> TestResult {
    let _ = compile(
        r#"
scalar Good @scalar(rust: "u32", typescript: "number", openapiType: "integer", rustNewtype: true, rustCopy: true, rustRange: true) @range(min: 1)
input I { m: Good }
"#,
    )?;
    Ok(())
}

#[test]
fn union_member_field_named_like_its_discriminator_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
type A { kind: String }
type B { kind: String }
union U = A | B
"#,
    )?;
    assert!(e.contains("discriminator"), "{e}");
    Ok(())
}

#[test]
fn custom_discriminator_collision_is_rejected_too() -> TestResult {
    let e = expect_error(
        r#"
type A { provider: String }
type B { provider: String }
union U @discriminator(field: "provider") = A | B
"#,
    )?;
    assert!(e.contains("provider"), "{e}");
    Ok(())
}

#[test]
fn repeated_directive_argument_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
input I { n: Int @length(min: 1) }
type T { x: String }
type S @service {
  op(input: I): T @query @rpc(path: "/a", path: "/b")
}
"#,
    )?;
    assert!(e.contains("argument `path` is repeated"), "{e}");
    Ok(())
}

#[test]
fn union_of_non_objects_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
enum E { A B }
type T { x: String! }
union U = T | E
"#,
    )?;
    assert!(e.contains("must be an object type"), "{e}");
    Ok(())
}

#[test]
fn union_discriminator_defaults_to_kind() -> TestResult {
    let api = compile(
        r#"
type A { a: String! }
type B { b: String! }
union U = A | B
"#,
    )?;

    let ApiType::Union(u) = api.find_type("U").ok_or("union exists")? else {
        return Err("the type is a union".into());
    };
    assert_eq!(u.discriminator, "kind");
    Ok(())
}

#[test]
fn operation_with_positional_arguments_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
type User { id: UUID! }
type Users @service {
  get(id: UUID!, extra: String): User! @query
}
"#,
    )?;
    assert!(e.contains("a single argument named `input`"), "{e}");
    Ok(())
}

/// One argument is allowed, but it has to be the one the wire format expects.
#[test]
fn operation_with_a_misnamed_argument_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
input I { id: UUID! }
type User { id: UUID! }
type Users @service {
  get(payload: I!): User! @query
}
"#,
    )?;
    assert!(e.contains("a single argument named `input`"), "{e}");
    Ok(())
}

/// An operation reading everything from the session declares no argument, so
/// the SDL no longer needs a placeholder input type with a field nobody reads.
#[test]
fn operation_without_arguments_compiles_with_no_input() -> TestResult {
    let api = compile(
        r#"
type User { id: UUID! }
type Users @service {
  me: User! @query
}
"#,
    )?;

    assert!(
        api.services[0].operations[0].input.is_none(),
        "the absence must reach the IR rather than becoming a synthetic type"
    );
    Ok(())
}

#[test]
fn operation_without_kind_directive_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
input I { id: UUID! }
type User { id: UUID! }
type Users @service {
  get(input: I!): User!
}
"#,
    )?;
    assert!(
        e.contains("must be annotated with @query, @mutation or @subscription"),
        "{e}"
    );
    Ok(())
}

#[test]
fn operation_input_must_be_input_object() -> TestResult {
    let e = expect_error(
        r#"
type User { id: UUID! }
type Users @service {
  get(input: User!): User! @query
}
"#,
    )?;
    assert!(e.contains("must be an input object"), "{e}");
    Ok(())
}

#[test]
fn input_referencing_output_object_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
type User { id: UUID! }
input Bad { user: User }
"#,
    )?;
    assert!(e.contains("references output object"), "{e}");
    Ok(())
}

#[test]
fn output_referencing_input_object_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
input In { id: UUID! }
type Bad { field: In }
"#,
    )?;
    assert!(e.contains("references input object"), "{e}");
    Ok(())
}

#[test]
fn unknown_scalar_is_rejected() -> TestResult {
    let e = expect_error("scalar Weird\n")?;
    assert!(e.contains("scalar `Weird` has no target mapping"), "{e}");
    assert!(e.contains("@scalar"), "must name the fix: {e}");
    Ok(())
}

#[test]
fn a_scalar_with_a_mapping_compiles() -> TestResult {
    let api = compile("scalar Money @scalar(rust: \"String\", typescript: \"string\")\n")?;

    let money = api
        .types
        .iter()
        .find_map(|t| match t {
            ApiType::Scalar(s) if s.name == "Money" => Some(s),
            _ => None,
        })
        .ok_or("Money must reach the IR")?;

    assert_eq!(money.rust, "String");
    assert_eq!(money.typescript, "string");
    Ok(())
}

#[test]
fn unknown_directive_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
type User @bogus { id: UUID! }
"#,
    )?;
    assert!(e.contains("unknown directive `@bogus`"), "{e}");
    Ok(())
}

#[test]
fn conflicting_query_and_mutation_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
input I { id: UUID! }
type User { id: UUID! }
type Users @service {
  get(input: I!): User! @query @mutation
}
"#,
    )?;
    assert!(e.contains("both @query and @mutation"), "{e}");
    Ok(())
}

#[test]
fn registry_must_list_the_runtime_codes() -> TestResult {
    let sdl = r#"
scalar UUID
enum ErrorCode { user_not_found }
type UserNotFound @error(code: user_not_found) { id: UUID! }
"#;
    let Err(e) = graphql_rpcgen::compile_str(sdl).map_err(|e| e.to_string()) else {
        return Err("a registry omitting the runtime codes must fail".into());
    };
    assert!(e.contains("must list `invalid_body`"), "{e}");
    Ok(())
}

#[test]
fn errors_are_reported_together() -> TestResult {
    // One run should surface every problem, not stop at the first.
    let e = expect_error(
        r#"
type User { id: UUID! bad: Missing1! }
type Other { x: Missing2! }
"#,
    )?;
    assert!(e.contains("Missing1") && e.contains("Missing2"), "{e}");
    Ok(())
}

#[test]
fn nullability_and_lists_convert_exactly() -> TestResult {
    let api = compile(
        r#"
type Example {
  a: String!
  b: String
  c: [String!]!
  d: [String]
  e: [[Int!]!]!
}
"#,
    )?;

    let ApiType::Object(o) = api.find_type("Example").ok_or("type exists")? else {
        return Err("`Example` is an object".into());
    };
    let field = |name: &str| -> TestResult<&TypeRef> {
        o.fields
            .iter()
            .find(|f| f.name == name)
            .map(|f| &f.ty)
            .ok_or_else(|| format!("`{name}` is a field of `Example`").into())
    };

    assert_eq!(
        field("a")?,
        &TypeRef::Named {
            name: "String".into(),
            nullable: false
        }
    );
    assert_eq!(
        field("b")?,
        &TypeRef::Named {
            name: "String".into(),
            nullable: true
        }
    );
    assert_eq!(
        field("c")?,
        &TypeRef::List {
            inner: Box::new(TypeRef::Named {
                name: "String".into(),
                nullable: false
            }),
            nullable: false,
        }
    );
    assert_eq!(
        field("d")?,
        &TypeRef::List {
            inner: Box::new(TypeRef::Named {
                name: "String".into(),
                nullable: true
            }),
            nullable: true,
        }
    );
    // Nested lists must survive round-tripping through the IR.
    assert_eq!(
        field("e")?,
        &TypeRef::List {
            inner: Box::new(TypeRef::List {
                inner: Box::new(TypeRef::Named {
                    name: "Int".into(),
                    nullable: false
                }),
                nullable: false,
            }),
            nullable: false,
        }
    );
    Ok(())
}

#[test]
fn rpc_path_override_is_honoured() -> TestResult {
    let api = compile(
        r#"
input I { id: UUID! }
type User { id: UUID! }
type Users @service {
  get(input: I!): User! @query @rpc(path: "/internal/users/get")
}
"#,
    )?;

    assert_eq!(api.services[0].operations[0].path, "/internal/users/get");
    Ok(())
}

#[test]
fn services_and_types_are_sorted_for_determinism() -> TestResult {
    let api = compile(
        r#"
input I { id: UUID! }
type Zebra { id: UUID! }
type Apple { id: UUID! }
type Zulu @service { b(input: I!): Zebra! @query a(input: I!): Apple! @query }
type Alpha @service { x(input: I!): Apple! @query }
"#,
    )?;

    let services: Vec<&str> = api.services.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(services, vec!["Alpha", "Zulu"]);

    let zulu_ops: Vec<&str> = api.services[1]
        .operations
        .iter()
        .map(|o| o.name.as_str())
        .collect();
    assert_eq!(zulu_ops, vec!["a", "b"]);

    let mut sorted = api.types.iter().map(|t| t.name()).collect::<Vec<_>>();
    let original = sorted.clone();
    sorted.sort();
    assert_eq!(original, sorted, "types must be emitted in sorted order");
    Ok(())
}

// ---------------------------------------------------------------------------
// Streams and raw bodies
// ---------------------------------------------------------------------------

/// The binary scalar every raw case below carries. Declared per test rather
/// than in the prelude so the cases that must reject one still see it declared.
const BINARY: &str = r#"
scalar Body
  @scalar(
    rust: "Vec<u8>"
    typescript: "Blob"
    openapiType: "string"
    openapiFormat: "binary"
  )
"#;

#[test]
fn subscription_compiles_and_carries_its_kind() -> TestResult {
    let api = compile(
        r#"
input I { workspace_id: UUID! }
type Event { count: Int! }
type Feed @service {
  events(input: I!): Event! @subscription
}
"#,
    )?;

    let op = &api.services[0].operations[0];
    assert_eq!(op.kind, OperationKind::Subscription);
    // Nothing left to put a body in, so the input has to travel in the URL.
    assert!(op.input_in_query());
    Ok(())
}

#[test]
fn two_kind_directives_are_rejected() -> TestResult {
    let e = expect_error(
        r#"
input I { id: UUID! }
type Event { count: Int! }
type Feed @service {
  events(input: I!): Event! @query @subscription
}
"#,
    )?;
    assert!(e.contains("is both @query and @subscription"), "{e}");
    Ok(())
}

#[test]
fn raw_without_a_direction_is_rejected() -> TestResult {
    let e = expect_error(&format!(
        r#"{BINARY}
input I {{ id: UUID! }}
type R {{ ok: Boolean! }}
type Files @service {{
  get(input: I!): R! @query @raw
}}
"#
    ))?;
    assert!(e.contains("must name at least one of"), "{e}");
    Ok(())
}

#[test]
fn raw_on_a_subscription_is_rejected() -> TestResult {
    let e = expect_error(&format!(
        r#"{BINARY}
input I {{ id: UUID! }}
type Event {{ count: Int! }}
type Feed @service {{
  events(input: I!): Event! @subscription @raw(response: ["text/csv"])
}}
"#
    ))?;
    assert!(e.contains("cannot be combined with @subscription"), "{e}");
    Ok(())
}

/// `@raw(response:)` and a binary return type are one statement made twice.
/// Either half alone describes something that cannot be generated.
#[test]
fn raw_response_without_a_binary_return_is_rejected() -> TestResult {
    let e = expect_error(&format!(
        r#"{BINARY}
input I {{ id: UUID! }}
type R {{ ok: Boolean! }}
type Files @service {{
  get(input: I!): R! @query @raw(response: ["text/csv"])
}}
"#
    ))?;
    assert!(
        e.contains("requires the operation to return a binary"),
        "{e}"
    );
    Ok(())
}

#[test]
fn binary_return_without_raw_response_is_rejected() -> TestResult {
    let e = expect_error(&format!(
        r#"{BINARY}
input I {{ id: UUID! }}
type Files @service {{
  get(input: I!): Body! @query
}}
"#
    ))?;
    assert!(e.contains("must declare @raw(response:)"), "{e}");
    Ok(())
}

#[test]
fn raw_request_needs_exactly_one_binary_field() -> TestResult {
    let e = expect_error(&format!(
        r#"{BINARY}
input I {{ id: UUID!, first: Body!, second: Body! }}
type R {{ ok: Boolean! }}
type Files @service {{
  put(input: I!): R! @mutation @raw(request: ["text/csv"])
}}
"#
    ))?;
    assert!(e.contains("requires exactly one binary input field"), "{e}");
    Ok(())
}

#[test]
fn binary_field_without_raw_request_is_rejected() -> TestResult {
    let e = expect_error(&format!(
        r#"{BINARY}
input I {{ id: UUID!, body: Body! }}
type R {{ ok: Boolean! }}
type Files @service {{
  put(input: I!): R! @mutation
}}
"#
    ))?;
    assert!(e.contains("must declare @raw(request:)"), "{e}");
    Ok(())
}

/// Bytes have no JSON encoding, so a binary field cannot sit inside a response
/// object — it has to be the whole body or nothing.
#[test]
fn binary_field_in_an_output_type_is_rejected() -> TestResult {
    let e = expect_error(&format!(
        r#"{BINARY}
input I {{ id: UUID! }}
type R {{ ok: Boolean!, body: Body! }}
type Files @service {{
  get(input: I!): R! @query
}}
"#
    ))?;
    assert!(e.contains("cannot be a field of an output type"), "{e}");
    Ok(())
}

#[test]
fn raw_request_splits_the_body_from_the_query_parameters() -> TestResult {
    let api = compile(&format!(
        r#"{BINARY}
input I {{ workspace_id: UUID!, body: Body! }}
type R {{ ok: Boolean! }}
type Files @service {{
  put(input: I!): R! @mutation @raw(request: ["text/csv"])
}}
"#
    ))?;

    let op = &api.services[0].operations[0];
    assert!(op.input_in_query());
    assert_eq!(api.body_field(op).map(|f| f.name.as_str()), Some("body"));
    let params: Vec<&str> = api
        .query_fields(op)
        .into_iter()
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(params, vec!["workspace_id"]);
    Ok(())
}

/// GraphQL enum values must be identifiers and several wire vocabularies are
/// not, so the tag is what a value actually serialises to.
#[test]
fn enum_variant_tag_becomes_the_wire_name() -> TestResult {
    let api = compile(
        r#"
enum Kind {
  document
  content_rule @variant(tag: "content-rule")
}
type Wrapper { kind: Kind! }
"#,
    )?;

    let ApiType::Enum(e) = api.find_type("Kind").ok_or("enum exists")? else {
        return Err("`Kind` is an enum".into());
    };
    // Untagged values keep their SDL spelling, so a value that never asked for
    // a tag is unaffected by one appearing next to it.
    assert_eq!(e.values[0].wire_name(), "document");
    assert_eq!(e.values[1].wire_name(), "content-rule");
    assert_eq!(e.values[1].name, "content_rule");
    Ok(())
}

/// Two values serialising to one string are ambiguous decoding and lossy
/// encoding, which no target can encode its way out of.
#[test]
fn colliding_enum_wire_names_are_rejected() -> TestResult {
    let e = expect_error(
        r#"
enum Kind {
  a @variant(tag: "same")
  b @variant(tag: "same")
}
type Wrapper { kind: Kind! }
"#,
    )?;
    assert!(e.contains("both serialise to `same`"), "{e}");
    Ok(())
}

/// A tag colliding with an untagged value's own name is the same ambiguity,
/// reached from the other side.
#[test]
fn an_enum_tag_may_not_shadow_another_value() -> TestResult {
    let e = expect_error(
        r#"
enum Kind {
  document
  doc @variant(tag: "document")
}
type Wrapper { kind: Kind! }
"#,
    )?;
    assert!(e.contains("both serialise to `document`"), "{e}");
    Ok(())
}

// --- Names that reach a target as an identifier -------------------------------

/// Overriding a built-in is what `scalar` redeclaration is for, and the
/// duplicate check below must not take it away.
#[test]
fn a_scalar_may_still_replace_a_builtin() -> TestResult {
    let api = compile(
        r#"
scalar Int @scalar(rust: "i64", typescript: "number", openapiType: "integer")
type T { n: Int! }
"#,
    )?;
    assert_eq!(api.find_scalar("Int").map(|s| s.rust.as_str()), Some("i64"));
    Ok(())
}

/// The overwrite that replaced a built-in also replaced anything else of the
/// same name, so an object type could be swapped for a scalar with no word
/// said about it.
#[test]
fn a_scalar_may_not_take_the_name_of_an_object_type() -> TestResult {
    let e = expect_error(
        r#"
type Foo { a: String! }
scalar Foo @scalar(rust: "String", typescript: "string", openapiType: "string")
"#,
    )?;
    assert!(e.contains("duplicate type `Foo`"), "{e}");
    Ok(())
}

#[test]
fn a_scalar_declared_twice_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
scalar Foo @scalar(rust: "String", typescript: "string", openapiType: "string")
scalar Foo @scalar(rust: "i64", typescript: "number", openapiType: "integer")
"#,
    )?;
    assert!(e.contains("duplicate type `Foo`"), "{e}");
    Ok(())
}

/// Field names are normalized to snake_case, so two spellings can arrive as one
/// JSON key — and as one Rust field declared twice.
#[test]
fn two_fields_normalising_to_one_wire_name_are_rejected() -> TestResult {
    let e = expect_error("type User { userId: String! user_id: String! }")?;
    assert!(e.contains("both become `user_id`"), "{e}");
    Ok(())
}

#[test]
fn the_same_collision_in_an_input_is_rejected() -> TestResult {
    let e = expect_error("input User { userId: String! user_id: String! }")?;
    assert!(e.contains("both become `user_id`"), "{e}");
    Ok(())
}

/// Most keywords survive as `r#name`; these three do not exist in that form.
#[test]
fn a_field_named_after_an_unescapable_keyword_is_rejected() -> TestResult {
    for name in ["self", "crate", "super"] {
        let e = expect_error(&format!("type T {{ {name}: String! }}"))?;
        assert!(e.contains("no Rust spelling"), "{name}: {e}");
    }
    Ok(())
}

/// The escape does work for the rest, including one reserved after the list
/// was first written.
#[test]
fn a_field_named_after_an_escapable_keyword_compiles() -> TestResult {
    compile("type T { try: String! type: String! match: String! }")?;
    Ok(())
}

/// The trait method, the handler and the client method are all the snake_case
/// of the operation name, and the path check cannot see this: the two spellings
/// below are different routes.
#[test]
fn two_operations_normalising_to_one_method_are_rejected() -> TestResult {
    let e = expect_error(
        r#"
type Users @service {
  getById: String! @query
  get_by_id: String! @query
}
"#,
    )?;
    assert!(e.contains("both become `get_by_id`"), "{e}");
    Ok(())
}

#[test]
fn an_operation_named_after_an_unescapable_keyword_is_rejected() -> TestResult {
    let e = expect_error("type Users @service { self: String! @query }")?;
    assert!(e.contains("no Rust spelling"), "{e}");
    Ok(())
}

/// The id lowercases both halves, so two operations can share one while their
/// paths differ — and a generated OpenAPI client would keep only one of them.
#[test]
fn two_operations_sharing_an_operation_id_are_rejected() -> TestResult {
    let e = expect_error(
        r#"
type Users @service {
  getById: String! @query @rpc(path: "/a")
  getbyid: String! @query @rpc(path: "/b")
}
"#,
    )?;
    assert!(e.contains("duplicate operationId `users_getbyid`"), "{e}");
    Ok(())
}

/// The discriminator is the one wire name that arrives as a directive argument
/// rather than as a GraphQL name, and TypeScript spells it as a bare key.
#[test]
fn a_discriminator_that_is_not_an_identifier_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
type CardPay { last4: String! }
type CashPay { note: String! }
union Pay @discriminator(field: "kind-of") = CardPay | CashPay
"#,
    )?;
    assert!(e.contains("must be an identifier"), "{e}");
    Ok(())
}

/// The path is spelled into a Rust string literal by the built-in templates,
/// which do not escape it.
#[test]
fn a_path_carrying_a_quote_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
type Users @service {
  get: String! @query @rpc(path: "/a\"b")
}
"#,
    )?;
    assert!(e.contains("unreserved URL characters"), "{e}");
    // The older rule still has to be stated, since it is the common mistake.
    assert!(e.contains("must start with `/`"), "{e}");
    Ok(())
}

// --- Bounds that nothing can satisfy ------------------------------------------

#[test]
fn a_length_with_min_above_max_is_rejected() -> TestResult {
    let e = expect_error("input I { name: String! @length(min: 10, max: 5) }")?;
    assert!(e.contains("nothing satisfies it"), "{e}");
    Ok(())
}

#[test]
fn a_range_with_min_above_max_is_rejected() -> TestResult {
    let e = expect_error("input I { n: Int! @range(min: 10, max: 1) }")?;
    assert!(e.contains("nothing satisfies it"), "{e}");
    Ok(())
}

/// A length counts things, and `validator` measures against a `u64`: the
/// generated `length(min = -5)` does not compile.
#[test]
fn a_negative_length_bound_is_rejected() -> TestResult {
    let e = expect_error("input I { name: String! @length(min: -5) }")?;
    assert!(e.contains("must not be negative"), "{e}");
    Ok(())
}

/// Neither declaration is inverted on its own; the pair only exists once the
/// field's rule and the scalar's are resolved together.
#[test]
fn a_field_bound_inverting_its_scalars_is_rejected() -> TestResult {
    let e = expect_error(
        r#"
scalar Nick
  @scalar(rust: "String", typescript: "string", openapiType: "string")
  @length(max: 5)
input I { nick: Nick! @length(min: 10) }
"#,
    )?;
    assert!(e.contains("resolves to an empty range"), "{e}");
    Ok(())
}

// --- Patterns that must run in two languages ----------------------------------

#[test]
fn an_empty_pattern_is_rejected() -> TestResult {
    let e = expect_error(r#"input I { s: String! @pattern(value: "") }"#)?;
    assert!(e.contains("@pattern is empty"), "{e}");
    Ok(())
}

/// The zod target emits the pattern as a `/.../` literal carrying no flags, so
/// an inline flag group is a syntax error in the browser.
#[test]
fn an_inline_flag_group_is_rejected() -> TestResult {
    let e = expect_error(r#"input I { s: String! @pattern(value: "(?i)^a+$") }"#)?;
    assert!(e.contains("the browser cannot read"), "{e}");
    assert!(e.contains("(?i"), "must name the construct: {e}");
    Ok(())
}

#[test]
fn rust_only_regex_constructs_are_rejected() -> TestResult {
    for (pattern, construct) in [
        (r"^\\p{L}+$", r"\p{"),
        (r"^(?P<a>x)$", "(?P<"),
        (r"^[[:alpha:]]+$", "[[:"),
        (r"\\Aabc", r"\A"),
    ] {
        let e = expect_error(&format!(
            r#"input I {{ s: String! @pattern(value: "{pattern}") }}"#
        ))?;
        assert!(e.contains(construct), "{pattern}: {e}");
    }
    Ok(())
}

/// The scan has to tell a group opener from a literal paren, or it would
/// refuse the most ordinary pattern there is.
#[test]
fn a_portable_pattern_with_escaped_parens_compiles() -> TestResult {
    compile(r#"input I { s: String! @pattern(value: "^\\(?[0-9]+\\)?$") }"#)?;
    Ok(())
}

/// The two group forms JavaScript shares with the Rust crate stay allowed.
#[test]
fn portable_group_forms_compile() -> TestResult {
    compile(r#"input I { s: String! @pattern(value: "^(?:ab)+(?<tail>c)$") }"#)?;
    Ok(())
}
