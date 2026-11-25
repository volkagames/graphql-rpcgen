//! Snapshot tests for the three emitters.
//!
//! Assertions target the constructs that are easy to get wrong (nullability,
//! unions, oneOf, recursion, constraints) rather than whole-file byte equality,
//! which would break on every cosmetic change. The whole files are kept too, by
//! `golden.rs`, where a cosmetic change is a reviewed diff rather than a
//! failure to puzzle over.

#![allow(
    clippy::panic_in_result_fn,
    reason = "a test asserts, and returns `Result` only to use `?` on its setup"
)]

/// What a test body answers with: the first failure ends it, carrying the
/// message of whatever actually went wrong rather than a fixed one.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

mod common;

use graphql_rpcgen::config::Config;
use graphql_rpcgen::{generate_openapi, generate_rust, generate_typescript, generate_zod};

/// Emitter settings; the fixture SDL carries its own scalar mappings.
fn config() -> Config {
    Config::default()
}

const SDL: &str = include_str!("fixtures/snapshots.graphql");

fn api() -> TestResult<graphql_rpcgen::ir::Api> {
    Ok(graphql_rpcgen::compile_str(SDL)?)
}

#[test]
fn rust_snapshot() -> TestResult {
    let out = generate_rust::generate(&api()?, &config())?;
    let flat = common::flat(&out);

    assert!(out.starts_with("// @generated"), "missing generated header");
    assert!(out.contains("DO NOT EDIT MANUALLY."));

    // Nullability and lists.
    assert!(out.contains("pub a: String,"));
    assert!(out.contains("pub b: Option<String>,"));
    assert!(out.contains("pub c: Vec<String>,"));
    assert!(out.contains("pub d: Option<Vec<Option<String>>>,"));
    assert!(out.contains("pub when: chrono::DateTime<chrono::Utc>,"));
    assert!(out.contains("pub blob: Option<serde_json::Value>,"));

    // Recursion must be boxed, but a Vec already provides indirection.
    assert!(out.contains("pub parent: Option<Box<Node>>,"));
    assert!(out.contains("pub children: Vec<Node>,"));

    // A newtype scalar becomes a distinct Rust type that fields refer to by
    // name, while `transparent` keeps the wire format that of the inner type.
    assert!(out.contains("#[serde(transparent)]"));
    assert!(out.contains("pub struct ClanId(pub uuid::Uuid);"));
    assert!(out.contains("pub id: ClanId,"));
    // Conversions are derived, not hand-written.
    assert!(flat.contains(
        "::derive_more::AsRef, ::derive_more::Display, ::derive_more::From, ::derive_more::Into)]"
    ));
    // By their paths: the generated code imports nothing an SDL type could
    // be spelt like.
    assert!(!out.contains("use derive_more"), "{out}");
    // Ord is required for use as a BTreeMap key.
    assert!(flat.contains("PartialOrd, Ord, Hash,"));

    // `Deref` would auto-deref the inner type's whole API onto the wrapper
    // (`clan_id.as_u128()`), dissolving the barrier the newtype exists to be.
    assert!(
        !out.contains("std::ops::Deref"),
        "a scalar newtype must not deref to its inner type"
    );
    assert!(!out.contains("Deref,"), "Deref must not be derived either");

    // Union: internally tagged, and the tag is the value the wire carries rather
    // than the name of the Rust variant holding it.
    assert!(out.contains(r#"#[serde(tag = "kind")]"#));
    assert!(out.contains("pub enum PaymentMethod {"));
    assert!(out.contains("    #[serde(rename = \"card\")]\n    CardPayment(CardPayment),"));
    // A member with no @variant keeps its type name, so nothing changes for it.
    assert!(out.contains("    CryptoPayment(CryptoPayment),"));

    // oneOf input becomes an enum.
    assert!(out.contains("pub enum PaymentInput {"));
    assert!(out.contains("Card(CardInput),"));

    // Constraints are enforced, not merely documented: an input derives
    // `Validate` and each field carries the rules the SDL declared for it.
    assert!(out.contains(
        "::serde::Serialize, ::serde::Deserialize, ::validator::Validate)]\npub struct CreateInput {"
    ));
    assert!(out.contains(
        r#"#[validate(length(min = 1, max = 100, message = "length must be between 1 and 100"))]"#
    ));
    assert!(out.contains(
        r#"#[validate(range(min = 18, max = 150, message = "must be between 18 and 150"))]"#
    ));
    // On a list the same directive counts elements, and says so in the message.
    assert!(out.contains(
        r#"#[validate(length(min = 1, max = 5, message = "element count must be between 1 and 5"))]"#
    ));
    // A nested input validates through its parent, so a pointer reaches into it.
    assert!(out.contains("#[validate(nested)]"));
    assert!(out.contains("pub nested: Option<CardInput>,"));
    // A scalar's own bound applies wherever it is used, without repeating it.
    assert!(out.contains(
        r#"#[validate(range(min = 0, message = "must be at least 0"))]
    pub seen: u32,"#
    ));
    // ...and a field may tighten it.
    assert!(out.contains(
        r#"#[validate(range(min = 50, max = 100, message = "must be between 50 and 100"))]
    pub bounded: u32,"#
    ));

    // A pattern is compiled once rather than per request, and kept as an
    // `Option` so one that does not compile rejects rather than panicking.
    // Its source is a `const` of its own, which the failure message quotes.
    assert!(out.contains(r#"const FIELD_CREATE_INPUT_EMAIL_SOURCE: &str = "^[^@]+@[^@]+$";"#));
    assert!(flat.contains(
        "static FIELD_CREATE_INPUT_EMAIL_PATTERN: ::std::sync::LazyLock<Option<::treat::rpc::validate::regex::Regex>> = \
         ::std::sync::LazyLock::new(|| ::treat::rpc::validate::regex::Regex::new(FIELD_CREATE_INPUT_EMAIL_SOURCE).ok());"
    ));
    assert!(flat.contains(
        "fn check_field_create_input_email_pattern<T>(value: &T) -> \
         Result<(), ::validator::ValidationError> where T: ::validator::ValidateRegex + ?Sized,"
    ));
    // The check binds its outcome before branching on it.
    assert!(flat.contains(
        "let matched = ::treat::rpc::validate::matches_pattern(&FIELD_CREATE_INPUT_EMAIL_PATTERN, value); if matched {"
    ));
    assert!(out.contains("#[validate(custom(function = check_field_create_input_email_pattern))]"));

    // An `@oneOf` input is an enum, which the derive cannot reach, so its impl
    // is written out: a nested arm delegates, and a scalar arm is measured with
    // the same traits and messages a `#[validate(...)]` attribute would use,
    // under the arm's own key.
    assert!(out.contains("impl ::validator::Validate for PaymentInput {"));
    assert!(out.contains("Self::Card(value) => ::validator::Validate::validate(value),"));
    assert!(out.contains("Self::Nick(value) => {"));
    assert!(flat.contains(
        "let length_ok = ::validator::ValidateLength::validate_length(value, Some(1), None, None); \
         if !length_ok {"
    ));
    assert!(flat.contains(
        r##"let mut error = ::validator::ValidationError::new("length"); error.message = Some("length must be at least 1".into()); errors.add("nick", error);"##
    ));
    // A pattern is borrowed, not moved: the static is a LazyLock, and moving a
    // static out is not legal Rust. Inherited patterns take the scalar's
    // static, field-level ones the owner's.
    assert!(out.contains("Self::Slug(value) => {"));
    assert!(out.contains("matches_pattern(&SCALAR_SLUG_PATTERN, value);"));
    assert!(out.contains("Self::Tag(value) => {"));
    assert!(out.contains("matches_pattern(&FIELD_PAYMENT_INPUT_TAG_PATTERN, value)"));
    assert!(out.contains("Self::Seen(value) => {"));
    // The arm inherits the scalar's own bound, the same as an ordinary field.
    assert!(flat.contains(
        "let range_ok = ::validator::ValidateRange::validate_range(value, Some(0), None, None, None); \
         if !range_ok {"
    ));

    // A constraint on a newtype-typed field measures the wire value inside it.
    assert!(out.contains("impl ::validator::ValidateLength<u64> for PlayerId {"));
    assert!(out.contains("::validator::ValidateLength::length(&self.0)"));
    assert!(out.contains("impl ::validator::ValidateRegex for ClanId {"));
    assert!(out.contains("::validator::ValidateRegex::validate_regex(&self.to_string(), regex)"));

    // A body that parses but breaks a rule is rejected before the trait is
    // called, as the registry code for a body that did not match the input:
    // `json_input` reads and validates, and `RuntimeCodes` names the code.
    assert!(out.contains("    let input = Rt::json_input(request).await?;\n"));
    assert!(out.contains(
        "impl ::treat::rpc::RuntimeCodes for ErrorCode {\n    \
         const INVALID_BODY: Self = Self::invalid_body;\n    \
         const INTERNAL_ERROR: Self = Self::internal_error;\n}"
    ));
    assert!(out.contains("type Rt = ::treat::rpc::Runtime<ErrorCode>;"));

    // Enum variants keep their wire spelling.
    assert!(out.contains(r#"#[serde(rename = "ACTIVE")]"#));
    assert!(out.contains("    Active,"));
    // Every enum lists its values in SDL order and spells them both ways —
    // `WireEnum`, reading the renames — so a consumer storing one as text
    // writes no array or `match` of its own; and it is `Hash`, so it keys a
    // set or a map. The trait is re-exported, so a glob import reaches
    // `Status::from_name`.
    assert!(out.contains(
        "#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ::serde::Serialize, ::serde::Deserialize, ::treat::rpc::WireEnum)]\n\
         pub enum Status {"
    ));
    assert!(out.contains("    #[serde(rename = \"DISABLED\")]\n    Disabled,\n"));
    assert!(
        out.contains("pub use ::treat::rpc::{Narrow, UNDECLARED_CODE_META, Undeclared, WireEnum};")
    );
    assert!(
        !out.contains("pub const ALL: &[Self]"),
        "the trait lists the values: {out}"
    );

    // Service trait and Axum bindings.
    assert!(out.contains("pub trait ThingsService: Clone + Send + Sync + 'static {"));
    // The success half is the envelope; the failure half names the codes this
    // operation is allowed to use, so an undeclared one does not type-check.
    assert!(flat.contains(
        "async fn get(&self, input: CreateInput) -> Result<::treat::ApiResponse<Example>, ::treat::ApiError<ThingsGetCodes>>;"
    ));
    assert!(out.contains("pub fn things_router<S: ThingsService>(service: S) -> ::axum::Router {"));
    assert!(out.contains(r#".route("/rpc/things/get", ::axum::routing::post(things_get::<S>))"#));

    // The error model is codes and nothing else: one flat registry naming every
    // code the API can return, with no per-code payload struct.
    assert!(out.contains("pub enum ErrorCode {"));
    assert!(flat.contains(
        "::serde::Deserialize, ::treat::ApiErrorCode, ::treat::rpc::WireEnum)] pub enum ErrorCode {"
    ));
    assert!(out.contains("#[allow(non_camel_case_types, reason = "));
    assert!(out.contains(r#"    #[message("not found")]"#));
    assert!(out.contains("    not_found,"));

    // Every operation's set is a variant list in one `error_set!` declaration:
    // one block, so the macro sees which sets are subsets of which and derives
    // the conversions a shared check needs.
    assert!(out.contains("::error_set::error_set! {"));
    // `ApiErrorCode` writes the code by its `#[code]`, which is also what
    // `WireEnum` narrows by; `error_set`'s own `Display` is skipped for it.
    assert!(out.contains(
        "    #[skip(Display)]\n    #[derive(Clone, Copy, ::treat::ApiErrorCode, ::treat::rpc::WireEnum)]\n    ThingsGetCodes := {"
    ));
    assert!(out.contains(r#"        #[code("not_found")] NotFound,"#));
    assert!(out.contains(r#"        #[undeclared] #[code("internal_error")] InternalError,"#));

    // A tagged code keeps its identifier as the Rust name and carries the tag
    // everywhere a string is produced: `Display` (what `treat` writes into the
    // envelope), serde (what a client reads back) and the operation's set.
    assert!(out.contains(
        "    #[message(\"gone\")]\n    #[code(\"things::gone\")]\n    #[serde(rename = \"things::gone\")]\n    things__gone,"
    ));
    assert!(out.contains(r#"        #[code("things::gone")] ThingsGone,"#));
    assert!(!out.contains(r#"#[code("things__gone")]"#));
    // No conversion per set: `Narrow` reads a code into a set by its wire name.
    assert!(!out.contains("impl TryFrom<ErrorCode>"), "{out}");

    // A set names what the operation declared, then what the runtime can emit
    // on its own. Anything else fails to compile at the handler, which is what
    // makes `@throws` binding.
    assert!(out.contains("    ThingsGetCodes := {"));
    // An operation declaring no `@throws` still gets the runtime codes, and
    // only those.
    assert!(out.contains("    ThingsMakeCodes := {"));
    assert!(!out.contains("statuses"), "no status plumbing may survive");
    assert!(!out.contains("STATUS"), "no per-error status constant");

    // The handler translates nothing: the runtime answers with the envelope
    // the service returned, or the failure's.
    assert!(
        out.contains("    let result = service.get(input).await;\n    Ok(Rt::respond(result))\n")
    );
    Ok(())
}

/// A union tagged beside the field holding it: the tag and the member are two
/// keys of the holder, `{"kind": "flag", "settings": {…}}`, and every target
/// has to pair them rather than emit two independent fields.
#[test]
fn sibling_tagged_union_snapshot() -> TestResult {
    let api = api()?;

    let rust = generate_rust::generate(&api, &config())?;
    assert!(rust.contains("#[serde(tag = \"kind\", content = \"settings\")]\npub enum Setting {"));
    let param = rust
        .split("pub struct Param {")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .ok_or("the holder is emitted as a struct")?;
    assert!(
        param.contains("    #[serde(flatten)]\n    pub settings: Setting,"),
        "{param}"
    );
    // The enum writes the tag, so a field of its own would claim the key twice.
    assert!(!param.contains("pub kind:"), "{param}");

    let ts = generate_typescript::generate(&api);
    assert!(ts.contains("export type Setting =\n  | FlagSetting\n  | NumberSetting\n"));
    assert!(ts.contains("export type Param = {\n  label: string\n} & (\n"));
    assert!(ts.contains("  | { kind: 'flag'; settings: FlagSetting }\n"));
    assert!(ts.contains("  | { kind: 'number'; settings: NumberSetting }\n"));
    assert!(ts.contains("  /** Which shape `settings` takes. */\n"));
    assert!(!ts.contains("export interface Param "));

    let doc: serde_json::Value =
        serde_json::from_str(&generate_openapi::generate(&api, &config())?)?;
    let schemas = &doc["components"]["schemas"];
    assert!(schemas["Setting"]["discriminator"].is_null());
    assert_eq!(
        schemas["Setting"]["oneOf"][0]["$ref"],
        "#/components/schemas/FlagSetting"
    );
    assert!(schemas["FlagSetting_Tagged"].is_null());

    let base = &schemas["Param"]["allOf"][0];
    assert!(base["properties"]["label"].is_object());
    assert!(base["properties"]["kind"].is_null());
    assert!(base["properties"]["settings"].is_null());

    let flag = &schemas["Param"]["allOf"][1]["oneOf"][0];
    assert_eq!(flag["properties"]["kind"]["const"], "flag");
    assert_eq!(
        flag["properties"]["kind"]["description"],
        "Which shape `settings` takes."
    );
    assert_eq!(
        flag["properties"]["settings"]["$ref"],
        "#/components/schemas/FlagSetting"
    );
    assert_eq!(flag["required"], serde_json::json!(["kind", "settings"]));

    // A holder with nothing but the pair is the arms alone, not an empty
    // object intersected with them.
    assert!(ts.contains("export type BareParam =\n  | { kind: 'flag'; settings: FlagSetting }\n"));
    assert!(schemas["BareParam"]["allOf"].is_null());
    assert_eq!(
        schemas["BareParam"]["oneOf"][1]["properties"]["kind"]["const"],
        "number"
    );
    Ok(())
}

#[test]
fn typescript_snapshot() -> TestResult {
    let out = generate_typescript::generate(&api()?);

    assert!(out.starts_with("// @generated"), "missing generated header");

    // Nullability and lists.
    assert!(out.contains("  a: string"));
    assert!(out.contains("  b?: string | null"));
    assert!(out.contains("  c: string[]"));
    assert!(out.contains("  d?: (string | null)[] | null"));
    assert!(out.contains("  blob?: unknown | null"));

    // The Rust newtype does not reach TypeScript: the id stays a plain string.
    assert!(out.contains("  id: string"));
    assert!(!out.contains("ClanId"));

    // Enum as a literal union.
    assert!(out.contains("export type Status = 'ACTIVE' | 'DISABLED'"));

    // Discriminated output union.
    assert!(out.contains("export type PaymentMethod ="));
    // The tag is the wire value from `@variant`; the type it selects keeps its
    // own name, so the arm reads `{ kind: 'card' } & CardPayment`.
    assert!(out.contains("({ kind: 'card' } & CardPayment)"));
    assert!(out.contains("({ kind: 'CryptoPayment' } & CryptoPayment)"));

    // Exclusive oneOf input.
    assert!(out.contains("export type PaymentInput ="));
    assert!(out.contains("crypto?: never"));

    // The client unwraps the envelope, so callers get the payload directly
    // while `errors` — not the HTTP status — decides success.
    assert!(out.contains("export interface RpcResponse<Output = unknown> {"));
    assert!(out.contains("return body.data as Output"));
    assert!(out.contains("if (body?.errors?.length) {"));

    // Client and TanStack helpers.
    assert!(out.contains("export class RpcClient {"));
    assert!(out.contains("export class RpcError"));
    assert!(out.contains("queryOptions: (input: CreateInput)"));
    assert!(out.contains("mutationOptions: ()"));
    assert!(out.contains("queryKey('things', 'get', input)"));
    // `createApi` is built on the per-service object: the request is spelled
    // once, there, and the entry's call and helpers go through it, the query
    // key built once and reused by `queryOptions`.
    assert_eq!(
        out.matches("client.call<Example>('/rpc/things/get'")
            .count(),
        1
    );
    assert!(out.contains("    things: createThingsOperations(client),\n"));
    assert!(out.contains("        call: ops.things.get,\n        queryKey: keys.things.get,\n"));
    assert!(out.contains("          queryKey: keys.things.get(input),\n"));
    assert!(out.contains("          queryFn: ({ signal }) => ops.things.get(input, { signal }),\n"));
    // The prefix every key of an operation starts with, without a client in
    // reach, so invalidating all of its queries names it typed.
    assert!(out.contains("export const operationKeys = {\n  things: {\n"));
    assert!(out.contains("    get: ['things', 'get'] as const,\n"));
    // The client reads treat's envelope: payload under `meta`, locator under
    // `source`, and the outcome decided by `errors[]` rather than the status.
    assert!(out.contains("export type RpcErrorCode = ErrorCode"));
    assert!(out.contains("'things::gone'"));
    assert!(out.contains("export interface RpcErrorItem {"));
    // `meta` is an object, not an arbitrary value: indexing it must not need
    // a cast at the call site.
    assert!(out.contains("meta?: globalThis.Record<string, unknown>"));
    assert!(out
        .contains("metaFor(code: RpcErrorCode): globalThis.Record<string, unknown> | undefined {"));
    assert!(out.contains("export interface RpcErrorSource {"));
    assert!(out.contains("get pointer(): string | undefined {"));
    assert!(out.contains("export type RpcRuntimeErrorCode = 'invalid_body' | 'internal_error'"));
    Ok(())
}

#[test]
fn openapi_snapshot() -> TestResult {
    let out = generate_openapi::generate(&api()?, &config())?;
    let doc: serde_json::Value = serde_json::from_str(&out)?;

    assert_eq!(doc["openapi"], "3.1.0");

    let get = &doc["paths"]["/rpc/things/get"]["post"];
    assert_eq!(get["operationId"], "things_get");
    assert_eq!(get["tags"][0], "Things");
    assert_eq!(get["x-rpc-kind"], "query");
    assert_eq!(
        doc["paths"]["/rpc/things/make"]["post"]["x-rpc-kind"],
        "mutation"
    );

    // Every outcome is a 200: success and failure are two alternatives of the
    // same response, and no per-error status response exists.
    let responses = get["responses"].as_object().ok_or("responses")?;
    assert_eq!(
        responses.keys().collect::<Vec<_>>(),
        vec!["200"],
        "the status line carries no outcome"
    );

    let ok = &get["responses"]["200"]["content"]["application/json"]["schema"];
    let success = &ok["oneOf"][0];
    assert_eq!(success["title"], "Success");
    assert_eq!(
        success["properties"]["data"]["$ref"],
        "#/components/schemas/Example"
    );
    assert_eq!(success["required"][0], "data");

    // A failure narrows `code` to what this operation declares, plus the
    // codes the runtime can always emit.
    let failure = &ok["oneOf"][1];
    assert_eq!(failure["title"], "Failure");
    let codes = failure["properties"]["errors"]["items"]["properties"]["code"]["enum"]
        .as_array()
        .ok_or("code enum")?;
    assert!(codes.iter().any(|c| c == "not_found"));
    assert!(codes.iter().any(|c| c == "invalid_body"));
    // Codes appear by their wire name, as the registry enum lists them.
    assert!(codes.iter().any(|c| c == "things::gone"));
    assert!(!codes.iter().any(|c| c == "things__gone"));

    // `meta` is an object on both halves of the envelope. A schema carrying
    // only a description has no type at all, and Swagger UI renders such a
    // field as a string — which is what this pins down.
    assert_eq!(success["properties"]["meta"]["type"], "object");
    // A failure's items `$ref` the shared entry, which is where `meta` lives;
    // only `code` is narrowed per operation.
    let error_meta = &doc["components"]["schemas"]["RpcErrorItem"]["properties"]["meta"];
    assert_eq!(error_meta["type"], "object");
    // An object already permits any key, so `additionalProperties: true` adds
    // nothing — and it makes Swagger UI invent `additionalProp1` in examples.
    assert!(
        error_meta.get("additionalProperties").is_none(),
        "must stay absent: {error_meta}"
    );
    assert!(
        success["properties"]["meta"]
            .get("additionalProperties")
            .is_none(),
        "must stay absent"
    );

    // The out-of-band signal is documented as a response header. Every operation
    // answers with the same one, so it is defined once and referenced.
    assert_eq!(
        get["responses"]["200"]["headers"]["x-rpc-status"]["$ref"],
        "#/components/headers/x-rpc-status"
    );
    assert_eq!(
        doc["components"]["headers"]["x-rpc-status"]["schema"]["enum"][0],
        "ok"
    );

    let schemas = &doc["components"]["schemas"];

    // Enum.
    assert_eq!(schemas["Status"]["type"], "string");
    assert_eq!(schemas["Status"]["enum"][0], "ACTIVE");

    // Object nullability: 3.1 uses a type array, and required lists only
    // non-null fields.
    let example = &schemas["Example"];
    assert_eq!(example["properties"]["a"]["type"], "string");
    assert_eq!(example["properties"]["b"]["type"][0], "string");
    assert_eq!(example["properties"]["b"]["type"][1], "null");
    assert_eq!(example["properties"]["d"]["items"]["type"][1], "null");
    let required: Vec<&str> = example["required"]
        .as_array()
        .ok_or("required array")?
        .iter()
        .map(|v| {
            v.as_str()
                .ok_or_else(|| "every required entry is a string".into())
        })
        .collect::<TestResult<Vec<&str>>>()?;
    assert!(required.contains(&"a") && !required.contains(&"b"));

    // Recursion via $ref rather than an inlined tree.
    assert_eq!(
        schemas["Node"]["properties"]["parent"]["oneOf"][0]["$ref"],
        "#/components/schemas/Node"
    );
    assert_eq!(
        schemas["Node"]["properties"]["children"]["items"]["$ref"],
        "#/components/schemas/Node"
    );

    // Union: oneOf plus a discriminator.
    assert_eq!(
        schemas["PaymentMethod"]["discriminator"]["propertyName"],
        "kind"
    );
    assert!(schemas["PaymentMethod"]["oneOf"].is_array());
    // Both the tagged schema's `const` and the mapping key are the wire tag, so
    // a client matching on `kind` finds the variant it was told to expect.
    assert_eq!(
        schemas["CardPayment_Tagged"]["allOf"][1]["properties"]["kind"]["const"],
        "card"
    );
    assert_eq!(
        schemas["PaymentMethod"]["discriminator"]["mapping"]["card"],
        "#/components/schemas/CardPayment_Tagged"
    );
    assert_eq!(
        schemas["CryptoPayment_Tagged"]["allOf"][1]["properties"]["kind"]["const"],
        "CryptoPayment"
    );

    // @oneOf input: each variant requires exactly its own key.
    let variants = schemas["PaymentInput"]["oneOf"].as_array().ok_or("oneOf")?;
    assert_eq!(variants.len(), 6);
    assert_eq!(variants[0]["required"][0], "card");
    assert_eq!(variants[0]["additionalProperties"], false);

    // Constraints.
    let create = &schemas["CreateInput"]["properties"];
    assert_eq!(create["name"]["minLength"], 1);
    assert_eq!(create["name"]["maxLength"], 100);
    assert_eq!(create["age"]["minimum"], 18.0);
    assert_eq!(create["age"]["maximum"], 150.0);
    assert_eq!(create["email"]["pattern"], "^[^@]+@[^@]+$");

    // On a list, `@length` bounds the number of elements. JSON Schema applies
    // `minLength` to strings only, so spelling it that way here would leave the
    // constraint documented and unenforced by every validator that reads this.
    assert_eq!(create["tags"]["type"], "array");
    assert_eq!(create["tags"]["minItems"], 1);
    assert_eq!(create["tags"]["maxItems"], 5);
    assert!(create["tags"]["minLength"].is_null());

    // A scalar's own constraint and description reach every field using it,
    // so the rule is stated once rather than at each use site.
    assert_eq!(create["seen"]["minimum"], 0.0);
    assert_eq!(create["seen"]["type"], "integer");
    assert!(create["seen"]["description"]
        .as_str()
        .ok_or("scalar description")?
        .contains("unix epoch"));

    // A field's own description wins, while the scalar's constraint survives.
    assert_eq!(
        create["touched"]["description"],
        "A field description overrides the scalar's."
    );
    assert_eq!(create["touched"]["minimum"], 0.0);
    // Nullability still applies on top of the scalar's schema.
    assert_eq!(create["touched"]["type"][1], "null");
    // A field narrowing the scalar's rule wins outright, rather than the two
    // being intersected.
    assert_eq!(create["bounded"]["minimum"], 50.0);
    assert_eq!(create["bounded"]["maximum"], 100.0);

    // A Rust newtype is invisible here: the scalar keeps its wire schema and
    // gains no named component of its own.
    assert_eq!(schemas["Node"]["properties"]["id"]["type"], "string");
    assert_eq!(schemas["Node"]["properties"]["id"]["format"], "uuid");
    assert!(schemas.get("ClanId").is_none());

    // One shared error item, matching `treat::ErrorMessage`; `source` carries
    // the JSON Pointer for a rejected body.
    let item = &schemas["RpcErrorItem"]["properties"];
    assert!(item["meta"].is_object());
    assert!(item["source"]["properties"]["pointer"].is_object());
    assert!(
        !schemas
            .as_object()
            .ok_or("schemas")?
            .keys()
            .any(|k| k.starts_with("ErrorResponse_")),
        "no per-code response component may remain"
    );

    // The registry itself reaches the document as an enum of codes.
    assert!(schemas["ErrorCode"]["enum"]
        .as_array()
        .ok_or("registry")?
        .iter()
        .any(|v| v == "not_found"));
    Ok(())
}

/// Every `$ref` in `value`, in document order.
fn collect_refs(value: &serde_json::Value, found: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                match (key.as_str(), child.as_str()) {
                    ("$ref", Some(target)) => found.push(target.to_string()),
                    _ => collect_refs(child, found),
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_refs(item, found);
            }
        }
        _ => {}
    }
}

/// Whether the document holds together, as opposed to what it says.
///
/// `openapi_snapshot` pins values at paths it names, so it can only catch a
/// construct it already thought of. A `$ref` to a component that was renamed or
/// never emitted, or two operations sharing an `operationId`, leave every one of
/// those assertions passing and still hand the consumer a document their
/// generator rejects. These checks are over the whole document instead, so a
/// future emitter change cannot introduce that class of break unnoticed.
#[test]
fn the_openapi_document_is_internally_consistent() -> TestResult {
    let doc: serde_json::Value =
        serde_json::from_str(&generate_openapi::generate(&api()?, &config())?)?;

    let mut refs = Vec::new();
    collect_refs(&doc, &mut refs);
    assert!(!refs.is_empty(), "the fixture must exercise $ref at all");

    for target in &refs {
        // A local pointer is the only form this generator emits: anything else
        // would make the document depend on a file the consumer does not have.
        let pointer = target
            .strip_prefix("#")
            .ok_or_else(|| format!("`{target}` is not a local $ref"))?;
        assert!(
            doc.pointer(pointer).is_some(),
            "$ref `{target}` resolves to nothing"
        );
    }

    let paths = doc["paths"].as_object().ok_or("paths")?;
    assert!(!paths.is_empty(), "the fixture must emit operations");

    let mut operation_ids = std::collections::BTreeSet::new();
    for (path, methods) in paths {
        let methods = methods
            .as_object()
            .ok_or_else(|| format!("`{path}` is not an object"))?;
        for (method, op) in methods {
            let id = op["operationId"]
                .as_str()
                .ok_or_else(|| format!("{method} {path} has no operationId"))?;
            // Client generators key their method names off this, so a
            // collision silently drops an operation rather than failing.
            assert!(
                operation_ids.insert(id.to_string()),
                "duplicate operationId `{id}` at {method} {path}"
            );

            let responses = op["responses"]
                .as_object()
                .ok_or_else(|| format!("{method} {path} has no responses"))?;
            assert!(
                !responses.is_empty(),
                "{method} {path} declares no response"
            );

            // A body is optional — an operation taking no input has none — but
            // one that is present has to describe what it accepts.
            if let Some(body) = op.get("requestBody") {
                let content = body["content"]
                    .as_object()
                    .ok_or_else(|| format!("{method} {path} has a requestBody with no content"))?;
                assert!(
                    !content.is_empty(),
                    "{method} {path} accepts a body of no media type"
                );
            }
        }
    }

    Ok(())
}

#[test]
fn generation_is_deterministic() -> TestResult {
    let api = api()?;
    for _ in 0..3 {
        assert_eq!(
            generate_rust::generate(&api, &config())?,
            generate_rust::generate(&api, &config())?
        );
        assert_eq!(
            generate_typescript::generate(&api),
            generate_typescript::generate(&api)
        );
        assert_eq!(
            generate_openapi::generate(&api, &config())?,
            generate_openapi::generate(&api, &config())?
        );
        assert_eq!(generate_zod::generate(&api), generate_zod::generate(&api));
    }
    Ok(())
}

/// The zod target restates the same constraints the Rust server enforces, so
/// these pin the mapping from each directive onto its zod refinement.
#[test]
fn zod_snapshot() -> TestResult {
    let out = generate_zod::generate(&api()?);

    assert!(out.starts_with("// @generated"), "missing generated header");
    assert!(out.contains("import { z } from 'zod'"));

    // A domain scalar gets one named schema, so a rule it declares is written
    // once and inherited by every field of that type.
    assert!(out.contains("export const ZoroTimeSchema = z.number().int().min(0)"));
    // A declared format is itself a constraint worth checking.
    assert!(out.contains("export const ClanIdSchema = z.guid()"));
    assert!(out.contains("export const DateTimeSchema = z.iso.datetime()"));
    // The Rust newtype does not reach here either: PlayerId is a plain string.
    assert!(out.contains("export const PlayerIdSchema = z.string()"));

    assert!(out.contains("export const StatusSchema = z.enum(['ACTIVE', 'DISABLED'])"));

    // Field rules: strings and numbers by bound, lists by element count.
    assert!(out.contains("name: z.string().min(1).max(100),"));
    assert!(out.contains("age: z.number().int().min(18).max(150).nullish(),"));
    assert!(out.contains("tags: z.array(z.string()).min(1).max(5),"));
    assert!(out.contains("email: z.string().regex(/^[^@]+@[^@]+$/),"));
    // A field's own bound replaces the scalar's rather than adding to it.
    assert!(out.contains("seen: ZoroTimeSchema,"));
    assert!(out.contains("bounded: z.number().int().min(50).max(100),"));

    // `z.lazy` frees the file from declaration order, so a nested — or
    // recursive — input needs no forward reference.
    assert!(out.contains("nested: z.lazy(() => CardInputSchema).nullish(),"));
    // A schema defined in terms of itself cannot have its type inferred from
    // its own initializer, so the recursive one — and only it — states the type
    // rather than leaving TypeScript to report an implicit `any`.
    // A schema defined in terms of itself cannot have its type inferred from
    // its own initializer, so the recursive one states the type rather than
    // leaving TypeScript to report an implicit `any`. The reference runs
    // through a list, which `z.array` does nothing to break.
    assert!(out.contains("export const TreeInputSchema: z.ZodTypeAny = z.object({"));
    // A type that is merely referred to by others is not recursive, and keeps
    // the inferred type that makes `z.infer` worth having.
    assert!(out.contains("export const CardInputSchema = z.object({"));

    // Only inputs get a schema; an output is what the server already sent.
    assert!(!out.contains("ExampleSchema"));
    assert!(!out.contains("PaymentMethodSchema"));

    // `@oneOf`: one arm per variant, each admitting its own key alone.
    assert!(out.contains("export const PaymentInputSchema = z.union(["));
    assert!(out.contains("z.strictObject({ card: z.lazy(() => CardInputSchema) }),"));

    // Keyed by path, which is what the client has at the call site.
    assert!(out.contains("'/rpc/things/get': CreateInputSchema,"));
    assert!(out.contains("export function validateInput(route: string, input: unknown): void {"));
    assert!(out.contains("inputSchemas[route]?.parse(input)"));
    Ok(())
}

/// An `@http` operation in the targets nothing compiles: the TypeScript client
/// and the OpenAPI document. `http_wire` covers the Rust ones on a socket.
const HTTP_SDL: &str = include_str!("fixtures/snapshots_http.graphql");

#[test]
fn http_typescript_snapshot() -> TestResult {
    let api = graphql_rpcgen::compile_str(HTTP_SDL)?;
    let ts = generate_typescript::generate(&api);
    for expected in [
        "async http<Output>(",
        "export interface RpcHttpSpec {",
        "client.http<User>('/api/user/{user_id}', input, { method: 'GET', pathFields: ['user_id'], body: 'none', envelope: false }, options)",
        "client.http<Info>('/api/info', input, { method: 'POST', pathFields: [], body: 'form', envelope: false }, options)",
        "client.http<User>('/api/user/{user_id}', input, { method: 'DELETE', pathFields: ['user_id'], body: 'none', envelope: true }, options)",
    ] {
        assert!(ts.contains(expected), "missing `{expected}` in:\n{ts}");
    }
    assert!(
        ts.contains("throw new globalThis.TypeError(`path parameter \\`${name}\\` is missing`)")
    );
    // An object rides as JSON text, a list's items included, and only a list
    // repeats its key: the table the server reads with says which field is
    // which, so it rides with the spec wherever it changes the spelling.
    for expected in [
        "client.http<User>('/search', input, { method: 'GET', pathFields: [], body: 'none', envelope: true, fields: { filters: 'json[]', one: 'json' } }, options)",
        "client.http<User>('/find', input, { method: 'POST', pathFields: [], body: 'form', envelope: true, fields: { filters: 'json[]', one: 'json' } }, options)",
    ] {
        assert!(ts.contains(expected), "missing `{expected}` in:\n{ts}");
    }
    // A JSON body beside a plain path parameter needs no table: every field
    // it carries is JSON already.
    assert!(
        ts.contains("client.http<User>('/items/{id}', input, { method: 'PUT', pathFields: ['id'], body: 'json', envelope: true }, options)"),
        "{ts}"
    );
    assert!(
        ts.contains("for (const text of paramTexts(key, value, table)) params.append(key, text)")
    );
    assert!(ts.contains(
        "throw new globalThis.TypeError(`list field \\`${key}\\` has a \\`null\\` item, which no query parameter spells`)"
    ));
    assert!(
        ts.contains("this.validate?.(`${spec.method} ${path}`, input ?? {})"),
        "{ts}"
    );

    // Two operations on one path are two keys, the one the client passes.
    let zod = generate_zod::generate(&api);
    for expected in [
        "'GET /api/user/{user_id}': UserInputSchema,",
        "'DELETE /api/user/{user_id}': UserInputSchema,",
        "'POST /api/info': InfoInputSchema,",
    ] {
        assert!(zod.contains(expected), "missing `{expected}` in:\n{zod}");
    }
    Ok(())
}

#[test]
fn http_openapi_snapshot() -> TestResult {
    let api = graphql_rpcgen::compile_str(HTTP_SDL)?;
    let doc: serde_json::Value =
        serde_json::from_str(&generate_openapi::generate(&api, &config())?)?;

    let user = doc
        .pointer("/paths/~1api~1user~1{user_id}/get")
        .ok_or("GET /api/user/{user_id}")?;
    assert_eq!(
        user.pointer("/parameters/0/in"),
        Some(&serde_json::json!("path")),
        "{user}"
    );
    assert_eq!(
        user.pointer("/parameters/0/required"),
        Some(&serde_json::json!(true)),
        "{user}"
    );
    assert!(
        user.get("requestBody").is_none(),
        "a GET has no body: {user}"
    );
    assert_eq!(
        user.pointer("/responses/200/content/application~1json/schema"),
        Some(&serde_json::json!({ "$ref": "#/components/schemas/User" })),
        "a bare success is the value: {user}"
    );
    assert!(
        user.pointer("/responses/default").is_some(),
        "the failure envelope: {user}"
    );

    let drop = doc
        .pointer("/paths/~1api~1user~1{user_id}/delete")
        .ok_or("DELETE beside GET on one path")?;
    assert!(
        drop.pointer("/responses/200/content/application~1json/schema/oneOf")
            .is_some(),
        "an enveloped success keeps the envelope: {drop}"
    );
    assert!(
        drop.pointer("/responses/default").is_some(),
        "an enveloped @http failure the runtime builds has a status: {drop}"
    );

    // The path parameter is read from the path, so the body does not list it.
    let rename = doc
        .pointer("/paths/~1items~1{id}/put/requestBody/content/application~1json/schema")
        .ok_or("PUT /items/{id} body")?;
    assert_eq!(
        rename.pointer("/required"),
        Some(&serde_json::json!(["name"])),
        "{rename}"
    );
    assert!(rename.pointer("/properties/id").is_none(), "{rename}");

    // An object read as JSON text is described as a string holding JSON, in
    // the query string and in a form, one per item of a list.
    let search = doc
        .pointer("/paths/~1search/get/parameters")
        .ok_or("GET /search parameters")?;
    assert_eq!(
        search.pointer("/0/schema/items/contentMediaType"),
        Some(&serde_json::json!("application/json")),
        "{search}"
    );
    assert_eq!(
        search.pointer("/1/schema/contentMediaType"),
        Some(&serde_json::json!("application/json")),
        "{search}"
    );
    let find = doc
        .pointer("/paths/~1find/post/requestBody/content/application~1x-www-form-urlencoded/schema/properties/one")
        .ok_or("POST /find form field")?;
    assert_eq!(
        find.get("type"),
        Some(&serde_json::json!("string")),
        "{find}"
    );

    // No input, no body: the handler reads none.
    let logout = doc.pointer("/paths/~1logout/post").ok_or("POST /logout")?;
    assert!(logout.get("requestBody").is_none(), "{logout}");

    let info = doc
        .pointer("/paths/~1api~1info/post")
        .ok_or("ANY described under POST")?;
    assert_eq!(
        info.get("x-rpc-any-method"),
        Some(&serde_json::json!(true)),
        "{info}"
    );
    assert!(
        info.pointer(
            "/requestBody/content/application~1x-www-form-urlencoded/schema/properties/session_id"
        )
        .is_some(),
        "{info}"
    );
    Ok(())
}

#[test]
fn explicit_nulls_snapshot() -> TestResult {
    let api = graphql_rpcgen::compile_str(
        r#"
enum ErrorCode { invalid_body internal_error }
type User @explicitNulls { id: String! nick: String }
type Plain { id: String! nick: String }
type Users @service { me: User! @query  plain: Plain! @query }
"#,
    )?;
    let rust = generate_rust::generate(&api, &config())?;
    let user = rust
        .split("pub struct User {")
        .nth(1)
        .ok_or("struct User")?;
    let user = user.split('}').next().ok_or("User body")?;
    assert!(
        user.contains("#[serde(default)]\n    pub nick: Option<String>"),
        "{user}"
    );
    assert!(!user.contains("skip_serializing_if"), "{user}");

    let ts = generate_typescript::generate(&api);
    assert!(ts.contains("  nick: string | null\n"), "{ts}");
    assert!(
        ts.contains("  nick?: string | null\n"),
        "Plain keeps the optional key: {ts}"
    );

    let doc: serde_json::Value =
        serde_json::from_str(&generate_openapi::generate(&api, &config())?)?;
    assert_eq!(
        doc.pointer("/components/schemas/User/required"),
        Some(&serde_json::json!(["id", "nick"])),
        "{doc}"
    );
    assert_eq!(
        doc.pointer("/components/schemas/Plain/required"),
        Some(&serde_json::json!(["id"]))
    );
    Ok(())
}

/// Every way an input field is spelled as text, on every operation form that
/// sends one; shared with `ts_check`'s runtime test.
const FIELD_KINDS_SDL: &str = include_str!("ts/field_kinds.graphql");

/// One field-table entry as both targets spell it: name, kind (`Str`, `Int`,
/// `Float`, `Bool`, `Json`) and whether it is a list.
type Entry = (String, String, bool);

/// The TypeScript client's field tables are the Rust server's.
///
/// The two generators each derive a field's kind from its type, so this holds
/// them to one answer: wherever the TypeScript spec carries a table, it lists
/// the entries of the `static` the server reads with, in order; where it
/// carries none, every entry it would have counted is a plain one — not JSON,
/// not a list — which the TypeScript runtime writes the same way with or
/// without a table.
#[test]
fn the_typescript_field_tables_are_the_servers() -> TestResult {
    let mut sdls: Vec<(&str, &str)> = common::fixtures::FIXTURES
        .iter()
        .map(|fixture| (fixture.name, fixture.sdl))
        .collect();
    sdls.push(("field_kinds", FIELD_KINDS_SDL));
    let mut tables = 0usize;
    let mut omitted = 0usize;
    for (name, sdl) in sdls {
        let api = graphql_rpcgen::compile_str(sdl)?;
        let ts = generate_typescript::generate(&api);
        let rust = common::flat(&generate_rust::generate_types(&api, &config())?);
        for service in &api.services {
            for op in &service.operations {
                let at = format!("{name}: {}.{}", service.name, op.name);
                let rows = rust_table(&rust, &service.name, &op.name);
                let table = ts_table(&ts, &service.name, &op.name).ok_or(at.clone())?;
                match (table, rows) {
                    (Some(entries), rows) => {
                        assert_eq!(Some(entries), rows, "{at}");
                        tables = tables.saturating_add(1);
                    }
                    (None, None) => {}
                    (None, Some(rows)) => {
                        // A JSON body carries every field but the path's as
                        // JSON whatever the table says.
                        let counted = |row: &&Entry| match &op.http {
                            Some(http) if http.body == graphql_rpcgen::ir::HttpBody::Json => {
                                http.path_params.contains(&row.0)
                            }
                            _ => true,
                        };
                        let plain = rows
                            .iter()
                            .filter(counted)
                            .all(|(_, kind, list)| kind != "Json" && !list);
                        assert!(plain, "{at}: no table, but {rows:?}");
                        omitted = omitted.saturating_add(1);
                    }
                }
            }
        }
    }
    assert!(tables >= 8, "only {tables} tables compared");
    assert!(omitted >= 8, "only {omitted} omitted tables checked");
    Ok(())
}

/// The entries of an operation's field table in generated Rust (flattened):
/// its `_FIELDS` static, else its `_HTTP` spec's `.fields(..)`; `None` without
/// either. Each entry is a `QueryField` constructor — `QueryField::int("n")`,
/// `.list()` after it for a list — read back as the kind's variant name.
fn rust_table(rust: &str, service: &str, operation: &str) -> Option<Vec<Entry>> {
    let name = format!(
        "{}_{}",
        graphql_rpcgen::naming::snake_case_raw(service),
        graphql_rpcgen::naming::snake_case_raw(operation)
    )
    .to_uppercase();
    let fields = format!("pub static {name}_FIELDS: &[QueryField] = &[");
    let http = format!("pub static {name}_HTTP: HttpSpec = ");
    let segment = match rust.split_once(&fields) {
        Some((_, rest)) => rest.split_once("];").map(|(table, _)| table),
        None => {
            let (_, spec) = rust.split_once(&http)?;
            let (spec, _) = spec.split_once(';')?;
            Some(
                spec.split_once(".fields(&[")
                    .and_then(|(_, rest)| rest.split_once("])"))
                    .map_or("", |(table, _)| table),
            )
        }
    }?;
    let entries = segment
        .split("QueryField::")
        .skip(1)
        .filter_map(|entry| {
            let (constructor, rest) = entry.split_once("(\"")?;
            let (field, rest) = rest.split_once("\")")?;
            let list = rest.starts_with(".list()");
            let mut kind = constructor.chars();
            let first = kind.next()?.to_ascii_uppercase();
            Some((field.to_string(), format!("{first}{}", kind.as_str()), list))
        })
        .collect();
    Some(entries)
}

/// The entries of the table an operation's TypeScript call passes, from its
/// `create<Service>Operations` object: `Some(None)` when it passes none, and
/// `None` when the call itself is not found.
fn ts_table(ts: &str, service: &str, operation: &str) -> Option<Option<Vec<Entry>>> {
    let start = format!("export function create{service}Operations(client: RpcClient) {{\n");
    let (_, object) = ts.split_once(&start)?;
    let (object, _) = object.split_once("\n}\n")?;
    let (_, call) = object.split_once(&format!("\n    {operation}: ("))?;
    let call = call.lines().nth(1)?;
    let literal = match call.split_once("fields: { ") {
        Some((_, rest)) => rest,
        None => match call.split_once("handlers, { ") {
            Some((_, rest)) => rest,
            None => return Some(None),
        },
    };
    let (literal, _) = literal.split_once(" }")?;
    let entries = literal
        .split(", ")
        .filter_map(|entry| {
            let (field, kind) = entry.split_once(": ")?;
            let kind = kind.trim_matches('\'');
            let (kind, list) = match kind.strip_suffix("[]") {
                Some(kind) => (kind, true),
                None => (kind, false),
            };
            let kind = match kind {
                "str" => "Str",
                "int" => "Int",
                "float" => "Float",
                "bool" => "Bool",
                "json" => "Json",
                _ => return None,
            };
            Some((field.to_string(), kind.to_string(), list))
        })
        .collect();
    Some(Some(entries))
}
