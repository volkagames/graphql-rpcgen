//! Snapshot tests for the three emitters.
//!
//! Assertions target the constructs that are easy to get wrong (nullability,
//! unions, oneOf, recursion, constraints) rather than whole-file byte equality,
//! which would break on every cosmetic change.

/// What a test body answers with: the first failure ends it, carrying the
/// message of whatever actually went wrong rather than a fixed one.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

use graphql_rpcgen::config::Config;
use graphql_rpcgen::{generate_openapi, generate_rust, generate_typescript, generate_zod};

/// Emitter settings; the fixture SDL carries its own scalar mappings.
fn config() -> Config {
    Config::default()
}

const SDL: &str = r#"
scalar UUID
scalar DateTime
scalar JSON

"A point in time as whole seconds since the unix epoch."
scalar ZoroTime
  @scalar(rust: "u32", typescript: "number", openapiType: "integer", openapiFormat: "int64")
  @range(min: 0)

scalar PlayerId
  @scalar(rust: "String", typescript: "string", openapiType: "string", rustNewtype: true)

"Identifies a clan. A UUID on the wire."
scalar ClanId
  @scalar(
    rust: "uuid::Uuid"
    typescript: "string"
    openapiType: "string"
    openapiFormat: "uuid"
    rustNewtype: true
    rustCopy: true
  )

"A slug: the pattern lives on the scalar, and every use inherits it."
scalar Slug
  @scalar(rust: "String", typescript: "string", openapiType: "string", rustNewtype: true)
  @pattern(value: "^[a-z0-9-]+$")

"A documented enum."
enum Status { ACTIVE DISABLED }

type Node {
  id: ClanId!
  "Recursive reference."
  parent: Node
  children: [Node!]!
}

type Example {
  a: String!
  b: String
  c: [String!]!
  d: [String]
  when: DateTime!
  blob: JSON
  status: Status!
}

"The wire tag is not the type name, so the SDL states it."
type CardPayment @variant(tag: "card") { last4: String! }
type CryptoPayment { network: String! }
union PaymentMethod @discriminator(field: "kind") = CardPayment | CryptoPayment

enum SettingKind { flag number }
type FlagSetting @variant(tag: "flag") { tribool: Boolean }
type NumberSetting @variant(tag: "number") { min: Int }
"Selected by the holder's own `kind`, so the member carries no copy of the tag."
union Setting @discriminator(sibling: "kind") = FlagSetting | NumberSetting
type Param {
  label: String!
  "Which shape `settings` takes."
  kind: SettingKind!
  settings: Setting!
}
"Nothing but the pair."
type BareParam { kind: SettingKind! settings: Setting! }

input CardInput { token: String! }

"A tree holding itself through a list, which is what makes its schema recursive."
input TreeInput {
  id: String!
  children: [TreeInput!]
}
input CryptoInput { address: String! }
input PaymentInput @oneOf { card: CardInput crypto: CryptoInput nick: String @length(min: 1) slug: Slug tag: String @pattern(value: "^[a-z]+$") seen: ZoroTime }

input CreateInput {
  name: String! @length(min: 1, max: 100)
  age: Int @range(min: 18, max: 150)
  email: String! @pattern(value: "^[^@]+@[^@]+$")
  seen: ZoroTime!
  "A field description overrides the scalar's."
  touched: ZoroTime
  bounded: ZoroTime! @range(min: 50, max: 100)
  "On a list, @length counts elements rather than characters."
  tags: [String!]! @length(min: 1, max: 5)
  nested: CardInput
}

enum ErrorCode {
  invalid_body
  internal_error
  not_found
}

type Things @service {
  "Fetch one thing."
  get(input: CreateInput!): Example! @query @throws(codes: ["not_found"])
  make(input: PaymentInput!): PaymentMethod! @mutation
}
"#;

fn api() -> TestResult<graphql_rpcgen::ir::Api> {
    Ok(graphql_rpcgen::compile_str(SDL)?)
}

#[test]
fn rust_snapshot() -> TestResult {
    let out = generate_rust::generate(&api()?, &config())?;

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
    assert!(out.contains("AsRef, Display, From, Into,"));
    assert!(out.contains("use derive_more::{AsRef, Display, From, Into};"));
    // Ord is required for use as a BTreeMap key.
    assert!(out.contains("PartialOrd, Ord, Hash,"));

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
    assert!(out.contains("Serialize, Deserialize, Validate)]\npub struct CreateInput {"));
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
    assert!(out.contains("static FIELD_CREATE_INPUT_EMAIL_PATTERN: LazyLock<Option<Regex>> ="));
    assert!(out.contains(r#"Regex::new("^[^@]+@[^@]+$").ok()"#));
    assert!(out.contains(
        "fn check_field_create_input_email_pattern<T: validator::ValidateRegex + ?Sized>("
    ));
    assert!(out.contains("#[validate(custom(function = check_field_create_input_email_pattern))]"));

    // An `@oneOf` input is an enum, which the derive cannot reach, so its impl
    // is written out: a nested arm delegates, and a scalar arm is measured with
    // the same traits and messages a `#[validate(...)]` attribute would use,
    // under the arm's own key.
    assert!(out.contains("impl Validate for PaymentInput {"));
    assert!(out.contains("Self::Card(value) => value.validate(),"));
    assert!(out.contains("Self::Nick(value) => {"));
    assert!(out
        .contains(r"if !validator::ValidateLength::validate_length(value, Some(1), None, None) {"));
    assert!(out.contains(
        r##"errors.add("nick", validator::ValidationError::new("length").with_message("length must be at least 1".into()));"##
    ));
    // A pattern is borrowed, not moved: the static is a LazyLock, and moving a
    // static out is not legal Rust. Inherited patterns take the scalar's
    // static, field-level ones the owner's.
    assert!(out.contains("Self::Slug(value) => {"));
    assert!(out.contains("matches_pattern(&SCALAR_SLUG_PATTERN, value)"));
    assert!(out.contains("Self::Tag(value) => {"));
    assert!(out.contains("matches_pattern(&FIELD_PAYMENT_INPUT_TAG_PATTERN, value)"));
    assert!(out.contains("Self::Seen(value) => {"));
    // The arm inherits the scalar's own bound, the same as an ordinary field.
    assert!(out.contains(
        r"if !validator::ValidateRange::validate_range(value, Some(0), None, None, None) {"
    ));

    // A constraint on a newtype-typed field measures the wire value inside it.
    assert!(out.contains("impl validator::ValidateLength<u64> for PlayerId {"));
    assert!(out.contains("validator::ValidateLength::length(&self.0)"));
    assert!(out.contains("impl validator::ValidateRegex for ClanId {"));
    assert!(out.contains("validator::ValidateRegex::validate_regex(&self.to_string(), regex)"));

    // A body that parses but breaks a rule is rejected before the trait is
    // called, as the registry code for a body that did not match the input.
    assert!(out.contains("if let Err(violations) = validator::Validate::validate(&input) {"));
    assert!(out.contains("return invalid_input(&violations);"));
    assert!(out.contains("message.code = ErrorCode::invalid_body.to_string();"));

    // Enum variants keep their wire spelling.
    assert!(out.contains(r#"#[serde(rename = "ACTIVE")]"#));
    assert!(out.contains("    Active,"));

    // Service trait and Axum bindings.
    assert!(out.contains("pub trait ThingsService: Clone + Send + Sync + 'static {"));
    // The success half is the envelope; the failure half names the codes this
    // operation is allowed to use, so an undeclared one does not type-check.
    assert!(out.contains(
        "async fn get(&self, input: CreateInput) -> Result<ApiResponse<Example>, ApiError<ThingsGetCodes>>;"
    ));
    assert!(out.contains("pub fn things_router<S: ThingsService>(service: S) -> Router {"));
    assert!(out.contains(r#".route("/rpc/things/get", post(things_get::<S>))"#));

    // The error model is codes and nothing else: one flat registry naming every
    // code the API can return, with no per-code payload struct.
    assert!(out.contains("pub enum ErrorCode {"));
    assert!(out.contains("Serialize, Deserialize, ApiErrorCode)]"));
    assert!(out.contains("#[allow(non_camel_case_types)]"));
    assert!(out.contains(r#"    #[message("not found")]"#));
    assert!(out.contains("    not_found,"));

    // Every operation's set is a variant list in one `error_set!` declaration:
    // one block, so the macro sees which sets are subsets of which and derives
    // the conversions a shared check needs.
    assert!(out.contains("error_set! {"));
    assert!(out.contains(r#"        #[display("not_found")] NotFound,"#));

    // A set names what the operation declared, then what the runtime can emit
    // on its own. Anything else fails to compile at the handler, which is what
    // makes `@throws` binding.
    assert!(out.contains("    ThingsGetCodes := {"));
    // An operation declaring no `@throws` still gets the runtime codes, and
    // only those.
    assert!(out.contains("    ThingsMakeCodes := {"));
    assert!(!out.contains("statuses"), "no status plumbing may survive");
    assert!(!out.contains("STATUS"), "no per-error status constant");

    // The body is parsed by treat's extractor, which reports `invalid_body`
    // with a JSON Pointer instead of a bare 400.
    assert!(out.contains("use treat::extract_axum::ApiJson;"));
    assert!(out.contains("ApiJson(input): ApiJson<CreateInput>"));
    // The handler translates nothing: the envelope is a response, and so is
    // `ApiError`.
    assert!(out.contains("match service.get(input).await"));
    assert!(out.contains("Err(error) => error.into_response(),"));
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
    // The client reads treat's envelope: payload under `meta`, locator under
    // `source`, and the outcome decided by `errors[]` rather than the status.
    assert!(out.contains("export type RpcErrorCode = ErrorCode"));
    assert!(out.contains("export interface RpcErrorItem {"));
    // `meta` is an object, not an arbitrary value: indexing it must not need
    // a cast at the call site.
    assert!(out.contains("meta?: Record<string, unknown>"));
    assert!(out.contains("metaFor(code: RpcErrorCode): Record<string, unknown> | undefined {"));
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
    assert!(out.contains("export function validateInput(path: string, input: unknown): void {"));
    assert!(out.contains("inputSchemas[path]?.parse(input)"));
    Ok(())
}
