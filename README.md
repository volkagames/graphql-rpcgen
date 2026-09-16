# graphql-rpcgen

A GraphQL SDL document is the single source of truth; `graphql-rpcgen` turns it
into a typed Axum server, a typed Rust client, an OpenAPI 3.1 document, a typed
TypeScript client with TanStack Query helpers, and the zod schemas that check an
input against the same rules the server enforces.

GraphQL is used **only as the schema language**. There is no GraphQL runtime, no
resolvers and no query execution — the transport is plain RPC over HTTP.

```text
api/**/*.graphql
       │
       ▼
 graphql-rpcgen
       │
       ├── Rust wire types            ─┬─►  service traits + Axum bindings
       │                               └─►  Rust client (reqwest)
       ├── OpenAPI 3.1  ──►  Swagger UI
       ├── TypeScript client + TanStack Query helpers
       └── zod schemas for the inputs
```

The wire types are their own target so the client does not compile a web
framework and the server does not compile an HTTP client; both name the same
types, so a value the client returns is the type the handler produced.

## Install

```bash
cargo install graphql-rpcgen
```

The package and the binary are both `graphql-rpcgen`; the library is `graphql_rpcgen`.

As a build-time or dev dependency:

```toml
[dependencies]
graphql-rpcgen = "0.7"
```

## CLI

```bash
graphql-rpcgen generate                                  # write artifacts
graphql-rpcgen check                                     # fail if checked-in output is stale
graphql-rpcgen generate --api api --root . --config rpcgen.toml
```

| Option | Default | Meaning |
| --- | --- | --- |
| `--api <dir>` | `api` | directory scanned for `*.graphql` |
| `--root <dir>` | `.` | project root that `[output]` paths are relative to |
| `--config <file>` | `<root>/rpcgen.toml` | settings file; optional |

`check` exits non-zero when generated files differ from what the current SDL
produces, which makes it usable as a CI gate.

A run that would write nothing is a failure rather than a quiet success. The
settings file is optional, but the defaults name no target, so a project
without one — or with a `--root` pointing a directory too high — configures no
output at all. Left as a warning that case made `check` report "up to date"
after reading nothing, which is the one answer a CI gate must never give
wrongly.

Generation is deterministic: the same SDL always yields byte-identical output,
with types, services, operations and OpenAPI components sorted stably. No
timestamps, and no network access at generation time.

## Configuration

Nothing about any particular API is compiled into `graphql-rpcgen`. The SDL
describes the API — scalars included — and `rpcgen.toml` covers only what is not
the API: where artifacts land and what the OpenAPI document calls itself.

A config file must declare which generator it was written against, and this is
the one required key:

```toml
rpcgen_version = "0.1.0"
```

Generated code is a contract between an SDL and one generator, so a project
states which one rather than accepting whatever happens to be installed. The
rule is Cargo's caret semantics: the **major must match exactly**, and the rest
must be at least what was asked for.

| Required | Installed | Result |
| --- | --- | --- |
| `0.1.0` | `0.1.0` | ✅ |
| `0.1.0` | `0.2.0` | ✅ newer within the major |
| `0.2.0` | `0.1.0` | ⚠️ fails — older than required |
| `1.0.0` | `2.0.0` | ⚠️ fails — different major, even though it is newer |
| absent | any | ⚠️ fails — the key is required |

A different major is refused in both directions: a newer generator may have
dropped what the project relies on, and an older one cannot know what the
project was written against. Either way `graphql-rpcgen` exits non-zero without
writing anything.

The requirement is a plain `major.minor.patch` triple — not a range expression.
A partial version like `0.1` is an error rather than a silent `0.1.0`, since the
difference between those is what the check exists to catch.

Note that within major zero every version shares the major, so only the floor
applies: `0.1.0` accepts `0.2.0`. That differs from Cargo's treatment of `^0.1`,
and it is the range `graphql-rpcgen` itself is currently in.

The check runs when the file is read, so it also guards a library caller going
through `Config::from_toml_file`. A `Config` built in Rust carries no
requirement and is not checked — cargo already pinned the version there.

Beyond that key, everything is optional: the built-in treat + axum templates
apply and nothing is written until an `[output]` path says so.

```toml
rpcgen_version = "0.1.0"

[output]                              # a target with no path is not generated
rust_types = "crates/api-types/src/generated.rs"
rust_server = "crates/api/src/generated.rs"
rust_client = "crates/api-client/src/generated.rs"
typescript = "web/src/api.ts"
typescript_zod = "web/src/validation.ts"
openapi = "docs/openapi.json"

[openapi]
title = "Billing API"
version = "3.2.1"
servers = ["https://api.example.com"]
```

### The three Rust targets

The wire types, the server bindings and the client are separate targets because
their dependencies are: types need only `serde`, the server adds a web
framework, the client adds an HTTP stack. A service consuming an API it does not
serve generates `rust_types` + `rust_client` and never compiles `axum`:

```toml
[output]
rust_types = "src/types.rs"
rust_client = "src/client.rs"         # no rust_server: this project only calls

[rust_client]
types_path = "crate::types"           # where the client imports its types from
```

`types_path` is the Rust path of the types module. The server target re-exports
it (`pub use`), so `use my_api::generated::*` still brings both the types and
the traits into scope — splitting the crate is not a breaking change for
callers.

Omitting `types_path` keeps everything in one file: the server target then emits
the types itself, which is the single-file layout.

#### What the generated Rust needs on the other side

The generator itself depends on almost nothing, but the code it writes does, and
a missing crate surfaces as a compile error in the consumer rather than here.
`rust_server` and `rust_client` each pull in the `rust_types` row as well; they
do not pull in each other, so a client-only consumer needs no web framework:

| Target | Crates |
| --- | --- |
| `rust_types` | `serde`, `serde_json`, `derive_more`, `treat`, `regex`, `validator`, `error_set`, plus whatever each `@scalar(rust:)` names — typically `uuid` and `chrono` |
| `rust_server` | `axum`, `async-trait`, and `treat` with its `axum` and `validator-extract` features |
| `rust_client` | `reqwest` with its `json` feature; `treat` for the envelope and its error entries, no features needed |

Those are the features the generated code needs to *compile*. The OpenAPI
document additionally describes an `x-rpc-status` header on every response,
which no generated handler writes: it comes from `treat`'s middleware, so a
server meant to honour that document also needs `treat`'s `rpc-status-header`
feature.

Span traces are the other feature nothing here needs to compile, and the one
easiest to forget. Without them an internal error logs as `connection lost`
with no trace of which request or which record it came from. A server should
enable `treat`'s `spantrace` feature **and** install `tracing_error::ErrorLayer`
in its subscriber; with the feature alone the trace is printed empty, and
nothing warns about it:

```toml
treat = { version = "0.23", features = ["axum", "validator-extract", "rpc-status-header", "spantrace"] }
tracing-error = "0.2"
```

```rust
use tracing_subscriber::prelude::*;

tracing_subscriber::registry()
    .with(tracing_subscriber::fmt::layer())
    .with(tracing_error::ErrorLayer::default())
    .init();
```

The details, including why a trace can go missing, are in `treat`'s
[span traces](https://github.com/volkagames/treat/blob/main/docs/errors.md#span-traces)
section.

An API declaring a `@subscription` or a `@raw` body emits a streaming runtime on
top of that, which a plain JSON API never compiles:

| Target | Additional crates |
| --- | --- |
| `rust_server` | `futures-core`, `futures-util`, `form_urlencoded` |
| `rust_client` | `futures-core`, `futures-util`, `bytes`, and the **`stream` feature** on `reqwest` |

`form_urlencoded` and the `stream` feature are easy to miss because nothing
needs them until the first `@raw` or `@subscription` operation: the runtime
reads a raw request's parameters out of the query string, its one body having
been spent on the payload, and the client reads a raw response back as a byte
stream.

The generated client groups operations by service, matching the TypeScript one:

```rust
let api = ApiClient::new("https://api.example.com");
match api.clans().create(&input).await {
    Ok(response) => { /* ... */ }
    // The outcome travels in the envelope, so a rejected call is an `Api`
    // error carrying codes — not a transport failure.
    Err(ClientError::Api(errors)) if errors.is(ClansCreateError::clan_already_exist) => { /* ... */ }
    Err(e) => return Err(e.into()),
}
```

`ClientError` separates what the server said (`Api`) from what never reached it
(`Transport`, `Decode`, `Status`), because only the first carries a code worth
branching on. `ApiClient::with_http` takes a configured `reqwest::Client`, which
is where timeouts, proxies and default headers go.

### Library use

`rpcgen.toml` deserializes into `config::Config`, which is also the public Rust
API — a project embedding the generator builds one directly and skips the file:

```rust
let api = graphql_rpcgen::compile(Path::new("api"))?;                     // scalars come from the SDL
let config = Config::from_toml_file(Path::new("rpcgen.toml"))?;   // or built by hand
for file in graphql_rpcgen::generate_all(&api, &config)? { /* write file.path */ }
```

`compile_str` takes SDL as a string, for a caller that does not have it on disk.

## Wire protocol

Every operation is `POST /rpc/{service}/{operation}` with a JSON body, except a
`@subscription` (a `GET` returning SSE) and a `@raw` body. `@query` and
`@mutation` describe TanStack semantics only; they do not change the verb.

`@rpc(path:)` overrides the derived path; `@version(n:)` mounts it under a
`/v{n}` prefix.

Responses carry a `{"data": …}` / `{"errors": […]}` envelope. Errors are
`{code, message, source}` where `source.pointer` is an RFC 6901 pointer to the
offending field. All errors in one response share one HTTP status.

## Architecture

The pipeline has a deliberate seam in the middle:

```text
GraphQL SDL → AST → semantic validation → IR → generators
```

Generators never see the GraphQL AST — they read only `ir::Api`, which knows
nothing about GraphQL, Axum, OpenAPI or TypeScript. Adding a target means
adding one emitter, not touching the front end.

```text
src/parser.rs                SDL discovery + parsing (graphql-parser)
src/ir.rs                    semantic IR — the model every generator reads
src/semantic.rs              AST -> IR, all validation
src/config.rs                settings: outputs, OpenAPI metadata, templates
src/version.rs               the required `rpcgen_version` and how it is compared
src/template.rs              mustache rendering + variable validation
templates/rust/              built-in treat + axum + reqwest templates
src/generate_rust.rs         Rust wire types, traits, Axum bindings
src/generate_rust_client.rs  Rust client over reqwest
src/generate_typescript.rs   TS types, fetch client, TanStack helpers
src/generate_zod.rs          zod schemas for the input types
src/generate_openapi.rs      OpenAPI 3.1
src/main.rs                  CLI (generate / check)
```

Every generated file carries a `@generated` / `DO NOT EDIT MANUALLY.` header.

## Type mapping

Built in, needing no `@scalar`:

| SDL | Rust | TypeScript | OpenAPI |
| --- | --- | --- | --- |
| `String` | `String` | `string` | `string` |
| `Int` | `i32` | `number` | `integer/int32` |
| `Float` | `f64` | `number` | `number/double` |
| `Boolean` | `bool` | `boolean` | `boolean` |
| `ID` | `String` | `string` | `string` |
| `UUID` | `uuid::Uuid` | `string` | `string/uuid` |
| `DateTime` | `chrono::DateTime<Utc>` | `string` | `string/date-time` |
| `JSON` | `serde_json::Value` | `unknown` | unconstrained |

`scalar UUID` alone is enough, which is what keeps an SDL self-contained.

### Nullability

GraphQL nullability is preserved exactly:

| SDL | Rust | TypeScript |
| --- | --- | --- |
| `a: String!` | `String` | `a: string` |
| `b: String` | `Option<String>` | `b?: string \| null` |
| `c: [String!]!` | `Vec<String>` | `c: string[]` |
| `d: [String]` | `Option<Vec<Option<String>>>` | `d?: (string \| null)[] \| null` |

Recursive references are boxed in Rust (`Option<Box<User>>`) and emitted as
`$ref` cycles in OpenAPI.

### Scalars

A custom scalar states its own mapping in the SDL, beside the `scalar` keyword,
so it is declared once:

```graphql
"Identifies an account."
scalar AccountId
  @scalar(
    rust: "u64"
    typescript: "number"
    openapiType: "integer"
    openapiFormat: "int64"
    rustNewtype: true                 # distinct Rust type, transparent on the wire
  )
  @range(min: 0)                      # inherited by every field of this type
```

Naming a built-in in `@scalar` retargets it:

```graphql
scalar DateTime @scalar(rust: "time::OffsetDateTime", typescript: "string", openapiType: "string")
```

A scalar with no mapping is a compile error reported at the declaration, rather
than a broken identifier appearing later in generated Rust.

`rustNewtype: true` produces a `#[serde(transparent)]` newtype — a distinct type
to the compiler, a plain scalar on the wire and in TypeScript — deriving its
conversions through `derive_more`:

```rust
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
    Serialize, Deserialize,
    AsRef, Display, From, Into,
)]
#[serde(transparent)]
pub struct ClanId(pub uuid::Uuid);
```

`Deref` is deliberately absent. It would auto-deref the inner type's entire API
onto the wrapper — `clan_id.as_u128()`, `clan_id.get_version()` — so the newtype
would stop being the barrier it exists to be. `AsRef` gives the same access
explicitly, and a snapshot test asserts `Deref` never comes back.

`rustCopy` and `rustRange` opt a newtype into `Copy` and into carrying a
`@range`; a newtype a `@range` reaches without `rustRange: true` is a compile
error in the SDL rather than an rustc error in the generated file.

### Unions and `@oneOf`

An output union becomes a discriminated object on the wire:

```json
{ "kind": "CardPayment", "last4": "4242" }
```

Rust gets `#[serde(tag = "kind")] enum PaymentMethod`, TypeScript gets
`({ kind: 'CardPayment' } & CardPayment) | …`, OpenAPI gets `oneOf` plus a
`discriminator`.

The tag defaults to the type name, which is what each target's own tagging
produces — and rarely what the wire carries. `@variant` states the real value, so
the type keeps a name that reads in Rust while the wire keeps the one the service
actually sends:

```graphql
type GooglePlayPurchase @variant(tag: "googleplay") { … }
union BillingInfo @discriminator(field: "billing_provider") = GooglePlayPurchase
```
```json
{ "billing_provider": "googleplay", "order_id": "GPA.1" }
```

A tag on a type no union names is rejected, as are two members of one union
claiming the same tag: the first would do nothing and the second would make the
wire ambiguous in both directions.

An `@oneOf` input becomes a Rust enum and an *exclusive* TypeScript union, so
supplying two variants is a compile error:

```ts
type PaymentInput =
  | { card: CardPaymentInput; crypto?: never }
  | { card?: never; crypto: CryptoPaymentInput }
```

## Directives

| Directive | Position | Meaning |
| --- | --- | --- |
| `@service` | OBJECT | the type is an RPC service; each field is an operation |
| `@query` | FIELD | read-only; generates a TanStack query helper |
| `@mutation` | FIELD | state-changing; generates a mutation helper |
| `@rpc(path:)` | FIELD | override the derived path |
| `@version(n:)` | OBJECT/FIELD | mount under a `/v{n}` prefix |
| `@subscription` | FIELD | a stream of events over SSE; the field's type is one event |
| `@raw(request:, response:)` | FIELD | a body that is bytes rather than the JSON envelope |
| `@auth(require:, role:)` | OBJECT/FIELD | what the caller must prove; `session` passes a context |
| `@mcp(expose:)` | OBJECT/FIELD | the operation is an MCP tool; surfaces as `x-mcp` in OpenAPI |
| `@throws(codes:)` | FIELD | error codes the operation may return |
| `@length(min:, max:)` | FIELD/INPUT | `minLength` / `maxLength` |
| `@range(min:, max:)` | FIELD/INPUT | `minimum` / `maximum` |
| `@pattern(value:)` | FIELD/INPUT | `pattern` |
| `@oneOf` | INPUT_OBJECT | exactly one field must be present |
| `@discriminator(field:)` | UNION | wire tag field, default `kind` |
| `@variant(tag:)` | OBJECT | the tag value selecting this union member |
| `@scalar(rust:, typescript:, …)` | SCALAR | the scalar's representation in each target |

Directives are pure metadata; no user code is ever executed. There is no plugin
system and nothing in an SDL document can run code at generation time.

An unsupported SDL construct is a compile error naming the offending type or
field, not a silently dropped declaration.

### Names the targets have to be able to spell

Field names travel to the wire as `snake_case`, so two spellings of one name are
one JSON key and one Rust field declared twice: `userId` beside `user_id` is
refused. Operation names collide the same way through two channels that do not
overlap — the Rust method is their `snake_case` and the `operationId` is their
lowercase — so `getById` beside `get_by_id` and `getById` beside `getbyid` are
both refused, the second even when `@rpc(path:)` gives them different routes.

Most Rust keywords survive as `r#type`; `self`, `crate` and `super` do not exist
in that form, so a field or operation named after one is refused rather than
renamed behind the author's back.

Two strings arrive as SDL arguments rather than as GraphQL names, and each is
checked for what it becomes. `@discriminator(field:)` is emitted as a bare key
in the TypeScript union, so it must be an identifier. `@rpc(path:)` is spelled
into Rust and TypeScript string literals by templates that do not escape it, so
it must start with `/` and carry only unreserved URL characters.

A `scalar` declaration may replace one of the built-ins — that is how an SDL
retargets `DateTime` — but nothing else. A second declaration of one name, or a
name an object type already took, is a duplicate rather than an overwrite.

### `@auth`

Who may call an operation, and what its handler hands the service.

```graphql
type Documents @service @auth(require: session) {
  list(input: DocumentListInput!): DocumentListResponse! @query @rpc(path: "/list")

  revert(input: RevertInput!): RevertResponse!
    @mutation @rpc(path: "/revert")
    @auth(require: session, role: "content_admin")
    @throws(codes: ["role_required"])

  export(input: ExportInput!): Binary!
    @query @rpc(path: "/export") @raw(response: ["application/json"])
    @auth(require: token)
}
```

Declared on the service and inherited by each operation; an operation restates
it to differ. A restatement replaces the rule whole rather than merging, so a
narrower spelling cannot inherit a role it dropped.

`require:` is `session`, `token` or `public`. Only `session` produces a
parameter, because the context carries the caller and only a session names one:

```rust
async fn list(&self, ctx: &RequestContext, input: DocumentListInput)
    -> Result<ApiResponse<DocumentListResponse>, ApiError<DocumentsListCodes>>;

// token and public are unchanged — a token authenticates a program, not a person
async fn export(&self, input: ExportInput) -> Result<RawBody, ApiError<DocumentsExportCodes>>;
```

The extractor runs after `State` and before the body, which is the only order
axum accepts, and the same context reaches every form — a subscription and a raw
request included.

`role:` is enforced by the **generated** handler, before the service is reached:

```rust
async fn documents_revert<S: DocumentsService>(
    State(service): State<S>,
    Ctx(ctx): Ctx<RequestContext>,
    ApiJson(input): ApiJson<RevertInput>,
) -> Response {
    if let Err(response) = ctx.require_role("content_admin") {
        return response;
    }
    ...
```

A declaration the implementation could forget to apply would be a comment, so
the check is not left to it. It runs before the body is validated, so a caller
without the role learns that rather than which field it also got wrong. The
compiler requires `role_required` in `@throws`: the check answers with that
code, and an operation may only answer with what it declares — undeclared, it
would narrow to `internal_error` and a denial would read as a server fault.

**Absence of `@auth` means `public`.** Silence is not a claim about who may
call, so protection is opt-in and an SDL predating the directive generates
exactly what it did before. `@auth` bare means `require: session`.

The context type is the service trait's own associated `Ctx`, so the generated
crate never names a type that lives above it — which is what keeps it from
depending on the application that implements it:

```rust
pub trait DocumentsService: Clone + Send + Sync + 'static {
    /// The caller, resolved from the request before a guarded operation runs.
    type Ctx: FromRequestContext;

    async fn list(&self, ctx: &Self::Ctx, input: DocumentListInput)
        -> Result<ApiResponse<DocumentListResponse>, ApiError<DocumentsListCodes>>;
}
```

`FromRequestContext` is generated beside the extractor and asks for two things:

```rust
pub trait FromRequestContext: Sized + Send + Sync + 'static {
    fn from_parts(parts: &mut Parts)
        -> impl Future<Output = Result<Self, Response>> + Send;
    fn require_role(&self, role: &str) -> Result<(), Response>;
}
```

What a session *is* — the cookie, the exchange with the identity provider, the
user lookup, what a denial says — is the server's, and no schema can describe
it. The SDL says which operations need one; the implementation says what one is.
An API with no session-guarded operation compiles none of this.

### `@mcp`

Which operations an MCP server built over the OpenAPI document exposes as
tools. On a service the directive exposes every operation; an operation
restates it to carve itself out (`@mcp(expose: false)`) or to expose itself
against an unannotated service. Absence means not exposed, so an SDL that never
mentions MCP generates exactly what it did before the directive existed.

The flag surfaces only in the OpenAPI document, as `x-mcp: true` on the
operation; the Rust, TypeScript and zod targets are untouched. An MCP server
derives its tool list by filtering the document on the flag — the operation's
`operationId`, description and input schema are already there, so the SDL stays
the one place deciding what an agent may call.

A `@subscription` or `@raw` operation cannot be a tool — a tool call is one
JSON request and one JSON response — and declaring one is a compile error
rather than a tool that cannot honour its shape.

## Constraints

`@length`, `@range` and `@pattern` are **enforced**, not documented. One
declaration in the SDL becomes three things: the OpenAPI schema, a `validator`
rule on the Rust input type, and a zod schema for the browser.

```graphql
input PlayerInfoListInput {
  "Player ids to look up. Must be non-empty and shorter than 100 entries."
  list: [PlayerId!]! @length(min: 1, max: 99)
}
```

```rust
#[validate(length(min = 1, max = 99, message = "element count must be between 1 and 99"))]
pub list: Vec<PlayerId>,
```
```ts
list: z.array(PlayerIdSchema).min(1).max(99),
```
```json
{ "type": "array", "minItems": 1, "maxItems": 99 }
```

A violated rule is rejected in the generated handler before the service trait is
called, as `invalid_body` — a value outside its declared bounds is a body that
did not match the operation's input, which is what that code names. Every code
on the wire therefore stays a registry value rather than leaking `validator`'s
own rule names, and the rule that was broken survives in `message`:

```json
{
  "errors": [
    { "code": "invalid_body", "message": "element count must be between 1 and 99",
      "source": { "pointer": "/list" } }
  ]
}
```

Every violation in one body is reported together, each with an RFC 6901 pointer
— `/device_info/display_density`, `/list/3/reason` — so a form learns all of its
problems in one round trip.

### What a rule may say

`@length` counts what the value has: characters for a string, elements for a
list. That is one directive for both because it is one idea, and because it is
how zod's `min`/`max` already read — but JSON Schema spells them `minLength` and
`minItems`, and applies each only to its own type, so the emitter picks the right
one. Spelling an array's bound `minLength` leaves it documented and unenforced by
every validator that reads the document.

A constraint the type cannot satisfy is rejected rather than dropped: `@length`
on a number, `@range` on a string, `@pattern` on a list. A silently ignored rule
reads like enforcement that is not happening.

So is a rule no value can satisfy. A `min` above its `max` is rejected, whether
both are written on one directive or the field's `min` only meets the scalar's
`max` once the two are resolved; a negative `@length` bound is rejected too,
since a length counts things and `validator` measures against a `u64`.

`@pattern` has to hold in two languages. It is compiled by the Rust `regex`
crate on the server and emitted as a bare `/.../` literal in the zod schema, so
what an SDL may declare is the subset both read. Inline flag groups (`(?i)`),
`(?P<name>)`, `\p{…}`, POSIX classes (`[[:alpha:]]`), `\A` and `\z` are refused
with the portable spelling named; `(?:…)` and `(?<name>…)` are fine. An empty
pattern is refused as well: it constrains nothing, and the `.regex(//)` it
emits opens a JavaScript comment rather than an empty regex.

A rule stated on a scalar applies to every field of that type, so it is written
once:

```graphql
scalar Decimal
  @scalar(rust: "String", typescript: "string", openapiType: "string")
  @pattern(value: "^-?[0-9]+(\\.[0-9]+)?$")
```

A field may override any part of that; each rule resolves independently, the
field's winning over the scalar's. Both the Rust and zod emitters resolve it
through the same `Api::resolved_constraints`, which is what keeps the browser's
check and the server's check the same check.

Two limits are worth knowing. Constraints on a scalar used as a **list item** are
enforced by zod and OpenAPI but not by the Rust side, which validates fields
rather than elements — so `[PlayerId!]!` checks its element count, not each id.
And a constraint written directly on an `@oneOf` variant field is not enforced in
Rust either, since the derive works on structs and an `@oneOf` input is an enum;
put the rule on the variant's own input type or its scalar, where every use of it
inherits the rule.

### zod in the browser

The zod target is its own file because it is the one artifact with a runtime
dependency; a project that does not want `zod` in its bundle drops the line from
`rpcgen.toml` and keeps a client that imports nothing. The client takes the check
as a hook:

```ts
import { validateInput } from './validation'

export const client = new RpcClient({ baseUrl: '', validate: validateInput })
```

Bad input then throws a `ZodError` naming the field before the request is built.
It cannot disagree with the server: both sides are generated from the same
declarations.

## Retargeting the Rust output

The preamble, router and handler come from mustache templates. The built-ins
target `treat` + `axum` for the server and `reqwest` for the client, and are
compiled into the binary, so the default path needs no template files on disk.
Point at your own to generate for another stack:

```toml
[rust]
handler_template = "templates/handler.mustache"
template_vars = { framework = "actix" }

[rust_client]
preamble_template = "templates/client.mustache"   # e.g. hyper, ureq, a mock
```

Template paths resolve relative to the config file that names them.

| Section | Template | Variables |
|---|---|---|
| `[rust]` | preamble | `error_code_enum`, `types_import` |
| `[rust]` | router | `service`, `module`, `trait_name`, `routes` |
| `[rust]` | handler | `handler`, `trait_name`, `input`, `output`, `method`, `path`, `query_fields`, `body_field`, `context_extractor`, `context_arg`, `role_guard` |
| `[rust]` | handler_no_input | same, with `input` empty |
| `[rust_client]` | preamble | `error_code_enum`, `types_import`, `client_name` |
| `[rust_client]` | method | `method`, `input`, `output`, `path`, `operation_id` |
| `[rust_client]` | method_no_input | same, with `input` empty |

The `_no_input` forms serve operations declaring no `input` argument. They are
separate templates rather than a conditional, because a flat mustache context
has nothing to branch on.

Anything in `template_vars` is added to every template in its section.

Two mustache behaviours matter when generating code, and `graphql-rpcgen` guards
both:

- `{{x}}` HTML-escapes, so `Vec<Option<T>>` would emit as
  `Vec&lt;Option&lt;T&gt;&gt;`. Use the triple-stache `{{{x}}}`; the escaping
  form is rejected with a message naming the fix.
- An unknown key renders as the empty string, so `{{{handlr}}}` would silently
  emit `async fn <S: …>`. Every referenced variable is checked against the
  declared set, so a typo fails generation instead.

## Tests

```bash
just test                  # whole suite
just test-one registry     # by test name
just ci                    # fmt-check, lints as errors, tests
```

156 tests, none of which need a network:

- **semantic** (60) — every validation rule, the error-code registry, path
  versioning, union tags, which constraint may sit on which type, plus
  nullability/list conversion
- **reuse** (20) — the generator is project-agnostic: a default config
  generates, SDL scalars reach all three targets and can override a built-in,
  unmapped scalars fail, outputs are selectable, the types target carries no web
  framework, the server re-exports the types rather than duplicating them, the
  client emits a method per operation, custom templates replace the built-ins,
  and a typo or an escaping tag in a template fails generation
- **unit** (32) — config defaults and TOML rejection, the `rpcgen_version`
  requirement and every way it can fail, template variable validation,
  identifier casing
- **auth** (12) — `@auth` on the trait signature, the handler's extractors, the
  role guard ahead of the body, and the rejections that keep a declaration from
  being decorative
- **mcp** (6) — `@mcp` inheritance, the `x-mcp` flag in the OpenAPI document,
  and the rejections of shapes a tool call cannot honour
- **streaming** (7) — `@subscription` and `@raw` on both sides: the signature,
  the route verb, and where the input is read from
- **snapshots** (6) — Rust, TypeScript, zod and OpenAPI output, determinism, and
  a pass over the whole OpenAPI document for the breaks a per-path assertion
  cannot see: a dangling `$ref`, a duplicated `operationId`, an operation with
  no response
- **cli** (11) — the binary itself: `generate` writes what `[output]` names and
  rewrites nothing unchanged, `check` exits non-zero on drift or a missing file
  and names it without repairing it, `--root` is what output paths resolve
  against, and a bad command or a `--config` that does not exist fails instead
  of falling back
- **oneof_wire** (1) — the generated `@oneOf` types compiled and run in a
  scratch cargo project, pinning the wire shape and the arm constraints at
  runtime rather than in emitted text
- **raw_wire** (1) — all three Rust targets compiled for an SDL reaching every
  `@raw` form and a `@subscription` beside them, which is the only thing that
  checks the emitted handlers, client methods and streaming runtime against
  rustc rather than against a `contains`

`oneof_wire` and `raw_wire` build generated code against the versions in this
repo's `Cargo.lock`, offline. Those crates (`treat`, `axum`, `reqwest`,
`form_urlencoded`, …) are dev-dependencies here for exactly that reason: the
generator itself needs none of them, but the lockfile has to carry them for the
tests to resolve a version without reaching the network.

That is also what keeps the dependency table above honest. A crate the emitted
code reaches for is easy to leave undocumented, because nothing notices until a
consumer writes their first `@raw` operation — `form_urlencoded` and `reqwest`'s
`stream` feature were both exactly that. Dropping either from `raw_wire`'s
dependency set now fails the test with the same error the consumer would have
seen.

## Out of scope

No GraphQL runtime or resolvers. No `utoipa` or Rust-to-OpenAPI inference —
OpenAPI comes from the IR. No hand-written OpenAPI. No Node in the compile path.
No executable decorators or plugin system. No websockets, no schema registry, no
compatibility checker, no binary protocols, no generic REST/GET mappings, no SDL
imports.

## License

MIT — see [LICENSE](LICENSE).
