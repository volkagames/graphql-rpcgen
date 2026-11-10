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

A `@raw(request:, stream: true)` operation adds one more to `rust_server`: the
**`io` feature** on `futures-util`, which `RawRequest::into_reader` needs. An API
without a streamed request does not emit `RawRequest` and needs no such feature.

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

`code` is the `ErrorCode` value's wire name: its SDL spelling, or its
`@variant(tag:)` when the wire carries something that is not a GraphQL
identifier. `portal_db__clan_not_found @variant(tag: "portal_db::clan_not_found")`
stays `portal_db__clan_not_found` in Rust and in `@throws`, and every target —
the Rust server's envelope, the Rust and TypeScript clients, OpenAPI — writes and
reads `portal_db::clan_not_found`.

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

Some unions are selected by a field their holder declares anyway — a template
field whose `type` decides the shape of its `settings`. A tag inside the member
would repeat that field and could contradict it, so `@discriminator(sibling:)`
puts the tag beside the member instead, as a key of the holding object:

```graphql
enum SettingKind { flag number }
type FlagSetting @variant(tag: "flag") { tribool: Boolean }
type NumberSetting @variant(tag: "number") { min: Int }
union Setting @discriminator(sibling: "kind") = FlagSetting | NumberSetting

type Param { label: String! kind: SettingKind! settings: Setting! }
```
```json
{ "label": "cap", "kind": "number", "settings": { "min": 1 } }
```

Rust gets `#[serde(tag = "kind", content = "settings")] enum Setting` flattened
into `Param`, which has no separate `kind` field — a `kind` that disagrees with
`settings` cannot be built. TypeScript gets the holder as
`{ label: string } & ({ kind: 'flag'; settings: FlagSetting } | …)`, which
narrows `settings` on `kind`. OpenAPI gets the holder's other fields plus a
`oneOf` of `{kind, settings}` pairs.

The holder must declare the tag as a non-null enum whose values are exactly the
member tags, and hold the union in one non-null, non-list field. Every holder
names that field the same, one holder holds at most one such union, and the
union cannot be an operation's output: all of these leave the tag without one
key to sit in.

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
| `@raw(request:, response:, stream:)` | FIELD | a body that is bytes rather than the JSON envelope; `stream: true` hands the request body to the service unread |
| `@auth(require:, role:, permission:, unguarded:, explicit:)` | OBJECT/FIELD | what the caller must prove; `session` passes a context; `role` assigns the operation to a role; `permission` names the permission it needs; `unguarded: true` drops its service's check; `explicit: true` on a service has each operation state its own |
| `@roles` | ENUM | the enum's values are the API's roles |
| `@inherits(roles:)` | ENUM_VALUE | on a role: the roles it inherits the permissions of |
| `@permissions(codes:)` | ENUM | the enum's values are the API's permissions; `codes` are what the permission check answers with |
| `@mcp(expose:)` | OBJECT/FIELD | the operation is an MCP tool; surfaces as `x-mcp` in OpenAPI |
| `@throws(codes:)` | FIELD | error codes the operation may return |
| `@length(min:, max:)` | FIELD/INPUT | `minLength` / `maxLength` |
| `@range(min:, max:)` | FIELD/INPUT | `minimum` / `maximum` |
| `@pattern(value:)` | FIELD/INPUT | `pattern` |
| `@oneOf` | INPUT_OBJECT | exactly one field must be present |
| `@discriminator(field:)` | UNION | wire tag field inside the member, default `kind` |
| `@discriminator(sibling:)` | UNION | wire tag field beside the member, in the holding object |
| `@variant(tag:)` | OBJECT | the tag value selecting this union member |
| `@scalar(rust:, typescript:, …)` | SCALAR | the scalar's representation in each target |

Directives are pure metadata; no user code is ever executed. There is no plugin
system and nothing in an SDL document can run code at generation time.

An unsupported SDL construct is a compile error naming the offending type or
field, not a silently dropped declaration. That includes an argument a directive
does not read: `@auth(require: session, permisson: users_read)` is refused, since
dropping the misspelt argument would leave the operation open to every session.

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
checked for what it becomes. `@discriminator(field:)` and `(sibling:)` are emitted as a bare key
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
    @auth(require: session, role: content_admin)
    @throws(codes: ["role_required"])

  export(input: ExportInput!): Binary!
    @query @rpc(path: "/export") @raw(response: ["application/json"])
    @auth(require: token)
}
```

Declared on the service and inherited by each operation; an operation restates
it to differ. A restatement replaces the rule whole rather than merging, so a
narrower spelling cannot inherit a role or a permission it dropped.

Dropping one is said in so many words. In a service whose `@auth` names a role
or a permission, an operation restating `@auth` without naming one of its own is
refused, whatever its `require:`: a copied `@auth(require: session)` would
otherwise open the operation to every session, a `require: public` to everyone,
and nothing would tell either from a deliberate one. An operation that needs
neither says `unguarded: true`:

```graphql
type Users @service @auth(require: session, permission: users_read) {
  "Any session reads its own account."
  me: Me! @query @rpc(path: "/users/me") @auth(require: session, unguarded: true)

  "Anyone, signed in or not."
  status: Status! @query @rpc(path: "/users/status") @auth(require: public, unguarded: true)
}
```

`unguarded` is refused where it would say nothing: as `false`, beside a `role:`
or a `permission:`, on a service, or in a service with no check to drop.

A service whose operations need different checks — a read and a write, or
several sections of an administration — is better off without a default: one
would hand a new operation whatever check it names, a write inheriting the read.
`explicit: true` says the service has none, and that each operation states its
own `@auth` with a role, a permission or `unguarded: true`:

```graphql
type AdminUsers @service @auth(require: session, explicit: true) {
  list(input: UserListInput!): UserListResponse!
    @query @rpc(path: "/admin/users/list")
    @auth(permission: users_read) @throws(codes: ["role_forbidden", "scope_required"])

  delete(input: UserDeleteInput!): UserDeleteResponse!
    @mutation @rpc(path: "/admin/users/delete")
    @auth(permission: users_write) @throws(codes: ["role_forbidden", "scope_required"])
}
```

An operation added without `@auth`, or with one naming no check, is a compile
error rather than an operation open to every session. `explicit` takes
`require: session` and no `role:` or `permission:`, and applies to a service
only.

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

`role:` names a value of the `@roles` enum — see [Roles](#roles-roles-and-inherits)
— and is enforced by the **generated** handler, before the service is reached:

```rust
async fn documents_revert<S: DocumentsService>(
    State(service): State<S>,
    Ctx(ctx): Ctx<RequestContext>,
    request: axum::extract::Request,
) -> Response {
    let access = ctx.require_role(Role::ContentAdmin);
    if let Err(response) = access {
        return response;
    }
    let body = <ApiJson<RevertInput> as axum::extract::FromRequest<()>>::from_request(request, &()).await;
    ...
```

A declaration the implementation could forget to apply would be a comment, so
the check is not left to it. It runs before the request's body is read, in
every handler form — a JSON body, a raw one, a query string: a caller without
the role learns that rather than what its body also got wrong, and nothing it
sent is parsed for it. The compiler requires `role_required` in
`@throws`: the check answers with that code, and an operation's declared codes
are what its clients are typed against — a refusal missing from them would be
one no client expects.

`permission:` names a value of the `@permissions` enum instead — see
[Permissions](#permissions-permissions) — and is enforced the same way, awaited:

```rust
    let access = ctx.require_permission(Permission::UsersWrite).await;
    if let Err(response) = access {
        return response;
    }
```

An operation names a role or a permission, never both.

The TypeScript client carries the same declaration, so a UI that says who may
do what reads it rather than restating it by hand. Each operation in
`createApi(client)` holds its role beside its call:

```ts
documents: {
  revert: {
    role: 'content_admin' as const,
    call: (input: RevertInput, options?: RpcCallOptions): Promise<RevertResponse> =>
      client.call<RevertResponse>('/revert', input, options),
    // queryKey, mutationOptions
  },
},
```

Only operations with a role carry the field, a service's role included. The
field says what the server asks for; the server still decides. The value is the
name a session carries for the role, its `@variant(tag:)` when it has one.

**Why the role sits on the entry, and there is no `operationRoles` map.** 0.11
emitted the roles as a separate map keyed `'Service.operation'`. A control then
named its operation twice — once to call it, once as a string to look its role
up — and the two names could be edited apart. On the entry, the role is read
from the same object the call goes through, so there is nothing to keep in step.

The map is removed rather than kept beside the field. Both would be generated
from one declaration and could not disagree, but two shapes for one fact leave
every consumer to choose between them, and the codebase to drift toward both.
What the map also served — the list of roles — is [`roles`](#roles-roles-and-inherits),
which lists the whole set rather than only the roles some operation names. A
lookup by operation name has no replacement; a consumer that needs one builds it
from `createApi`.

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

`FromRequestContext` is generated beside the extractor. It asks for the caller,
and, in an API with a `@roles` enum, for the role check, and in one with a
`@permissions` enum, for the permission check:

```rust
pub trait FromRequestContext: Sized + Send + Sync + 'static {
    fn from_parts(parts: &mut Parts)
        -> impl Future<Output = Result<Self, Response>> + Send;
    fn require_role(&self, role: Role) -> Result<(), Response>;
    fn require_permission(&self, permission: Permission)
        -> impl Future<Output = Result<(), Response>> + Send;
}
```

Without a `@roles` enum there is no role type to check against and no operation
asking for one, so `require_role` is not part of the trait; likewise
`require_permission` without a `@permissions` enum.

What a session *is* — the cookie, the exchange with the identity provider, the
user lookup, what a denial says — is the server's, and no schema can describe
it. The SDL says which operations need one; the implementation says what one is.
An API with no session-guarded operation compiles none of this.

### Roles: `@roles` and `@inherits`

Access control follows **ANSI INCITS 359-2012, Role Based Access Control**:
Core RBAC together with General Hierarchical RBAC. Static and dynamic separation
of duty (SSD, DSD) are not implemented. The terms below are the standard's, and
an API documenting its own policy should use them in the same sense.

An API states its policy with `@roles`, or with `@permissions` below — which
one is [Roles or permissions](#roles-or-permissions).

```graphql
"Who may do what."
enum Role @roles {
  content_read
  content_translate
  "Writes whole records, so it writes their translations too."
  content_edit @inherits(roles: [content_translate])
  content_admin
}
```

Where each element of the model lives:

| Standard | In the SDL | Generated | Left to the implementation |
| --- | --- | --- | --- |
| USERS | — | — | who a session names |
| ROLES | the values of the `@roles` enum | the enum, `from_name`, `as_str`; TS: the literal union and `roles` | — |
| RH, role hierarchy | `@inherits(roles:)` on a value | `Role::authorizes`; TS: `roleAuthorizes` | — |
| OPS, operations | an operation of a `@service` | its handler and client entry | — |
| OBS, objects | — | — | whatever the operation acts on |
| PRMS, permissions | an operation on every object it takes | — | a permission narrowed to some objects — a field, a protected branch — checked in the method, since `@auth` cannot see the request |
| PA, permission assignment | `@auth(role:)` | the role guard; `Role::OPERATIONS`; `role` on the `createApi` entry | — |
| UA, user assignment | — | — | whoever issues sessions |
| SESSIONS | `require: session` | the `Ctx` extractor | which of the user's roles are active |
| CheckAccess | — | the guard calling `require_role` | `require_role` |

#### The names, and why

- **`@roles`** marks the enum whose values are ROLES. A directive rather than a
  reserved type name, as `ErrorCode` is, because `Role` is a common name for
  data — a game's `enum Role { tank healer }` — and a reserved name would turn
  such an enum into an access-control set without being asked. An API has one
  role set: `role:` names a bare value, which means something against one enum
  only.
- **`@inherits(roles:)`** is the hierarchy. The standard defines it as
  inheritance: a senior role acquires the permissions of its juniors. The senior
  declares it, the way a type names what it extends, so a role's definition
  shows everything it grants. Inheritance is transitive; the compiler closes over
  it once, and both targets read that closure.
- **`role:`** on `@auth` is a permission assignment: the operation, which is the
  permission, is assigned to this role. It is **one** role. The standard lets a
  permission be assigned to several, but under a hierarchy assigning it to the
  most junior role that should hold it already reaches every role above. A list
  would add a second way to say the same thing, and an "any of" or "all of"
  question with it. A permission that seems to need two roles is either a
  missing role or a condition on the request, and the second belongs in the
  method.
- **`require_role`** is CheckAccess narrowed to what `@auth` can see: the
  permission assignment is already compiled into the handler, so what remains is
  whether a role active in the session is `role` or inherits it. It keeps its
  0.11 name; the parameter is now the role type, and the contract names the
  hierarchy instead of leaving it to the implementation to remember.
- **`authorizes`**: the standard calls the roles a user holds through
  assignment and inheritance together their *authorized roles*. `a.authorizes(b)`
  reads as that: holding `a` authorizes `b`.
- **`role_required`** is the code the check answers with, so an operation with a
  role must list it in `@throws`.

#### How to use them

- Assign each operation to the most junior role that should reach it, and let
  `@inherits` carry it upward. An operation assigned to a senior role is out of
  every junior's reach, and opening it to one later means moving the assignment
  rather than adding a grant.
- Read the names a session carries with `Role::from_name`. `None` is a name
  outside the set — usually another application's role — and is skipped, not an
  error.
- Implement `require_role` over the session's active roles:
  `active.iter().any(|held| held.authorizes(role))`, answering `role_required`
  when none does.
- In the browser, gate a control on the operation it calls:
  `roleAuthorizes(held, api.documents.revert.role)`. `roles` lists the whole set,
  in declaration order.

The compiler refuses: a second `@roles` enum; `@roles` on `ErrorCode`; a `role:`
that is quoted, missing from the set, or used without a set; `@inherits` outside
the role set, empty, naming a value the enum lacks, the role itself or one role
twice; and a cycle, since the standard's hierarchy is a partial order and roles
that inherit each other are one role under two names.

### Permissions: `@permissions`

`@roles` compiles the permission assignment into the API: an operation is
assigned to a role in the SDL, and the role hierarchy decides the rest. That is
the wrong shape for an application whose roles are data — made by its
administrators, different per tenant, built on one another at run time. Such an
application decides access by permission: an operation needs a permission, and
which roles hold it is a table the application keeps.

```graphql
"What a session may be allowed to do."
enum Permission @permissions(codes: ["role_forbidden", "scope_required"]) {
  users_read @variant(tag: "admin.users.read")
  users_write @variant(tag: "admin.users.write")
}

type Users @service @auth(require: session, permission: users_read) {
  list(input: UserListInput!): UserListResponse!
    @query @rpc(path: "/users/list")
    @throws(codes: ["role_forbidden", "scope_required"])

  delete(input: UserDeleteInput!): UserDeleteResponse!
    @mutation @rpc(path: "/users/delete")
    @auth(require: session, permission: users_write)
    @throws(codes: ["role_forbidden", "scope_required"])
}
```

In the standard's terms, where the two models differ:

| Standard | With `@roles` | With `@permissions` |
| --- | --- | --- |
| PRMS, permissions | the operation itself | the values of the `@permissions` enum, each covering the operations that name it |
| PA, permission assignment | `@auth(role:)`, in the SDL | the implementation's |
| CheckAccess | `require_role`, over the session's roles and the compiled hierarchy | `require_permission`, over whatever the implementation keeps |
| What the SDL says of an operation | which role it is assigned to | which permission it needs |

Where they agree is OBS. The standard's permission is an operation on an object,
and `@auth` sees no object: a permission here covers its operations on every
object they take. A permission narrowed to some objects — one tenant's
accounts, one drive, one folder — depends on the request, and is checked in the
method, under either model.

#### Roles or permissions

The choice is who changes the permission assignment, and when. If which roles
may run an operation changes with a release — a developer edits the SDL — use
`@roles`. If it changes at run time — an administrator edits data — use
`@permissions`.

`@roles` fits when:

- the role set is fixed and the same in every installation and tenant;
- roles arrive from the identity provider ready-made, and the application only
  reads them;
- who may do what is a product decision that goes through code review;
- the hierarchy is known in advance and can be written once in `@inherits`.

It costs nothing to run: no storage or administration screen for the
assignment, a synchronous `require_role`, one fixed refusal code, and a browser
that decides what to show on its own with `roleAuthorizes`. Moving an operation
to another role takes a release.

`@permissions` fits when:

- roles are data — administrators create them, each tenant has its own, custom
  roles are built on system ones;
- a role's permissions change without a release;
- a credential narrows access by a list of its own, such as a token limited to
  some permissions, so the check has more than one refusal to give;
- one permission covers many operations, such as every read of one section of
  an administration.

The implementation keeps the assignment, writes an async `require_permission`,
provides the screen where an administrator hands permissions out, and decides
which permissions imply which (`write` covering `read`) — the SDL describes
none of that. The browser cannot decide on its own: the server has to tell it
which permissions the session holds.

| Question | If yes |
| --- | --- |
| Can an administrator create a role, or change what it may do, without a release? | `@permissions` |
| Do tenants have different role sets? | `@permissions` |
| Do tokens or keys narrow access by a list of their own? | `@permissions` |
| Do roles come from the identity provider, the same everywhere? | `@roles` |
| Are there few roles, with who may do what settled in review? | `@roles` |
| Does access depend on the object — this drive, this folder, one's own document? | neither: the method checks it |

Two more rules:

- **Mixed needs are one dynamic model.** An API declares one set, and one whose
  operations seem to need both static and dynamic roles has a dynamic model in which some roles are system rows — a
  super administrator, a default member — seeded with the data.
- **`@roles` can become `@permissions` later.** Each role turns into a set of
  permissions seeded into the assignment, and each `@auth(role: x)` into
  `@auth(permission: …)`. The way back is harder: an assignment administrators
  have edited has no single SDL spelling. If run-time roles are a question for
  the coming year, start with `@permissions`.

#### The names, and why

- **`@permissions`** marks the enum whose values are PRMS, a directive for the
  reason `@roles` is one. An API has one permission set, it is not the role set
  — a role is who acts and a permission is what is done — and it is not the
  error registry.
- **One model per API.** An API declares `@roles` or `@permissions`, not both.
  Under `@permissions` the roles are the implementation's data, so a `@roles`
  enum beside it would be a second, static set of roles next to the dynamic
  one, and an operation's check would depend on which of the two it named.
- **`permission:`** on `@auth` names the one permission the operation needs. Not
  `op:` or `operation:`: here an operation is a field of a `@service`, and a
  permission routinely covers several of them.
- **`require_permission`** follows `require_role`. It is async where
  `require_role` is not: a role is read off the session, while which roles hold
  a permission is the implementation's data, which it may have to fetch.
- **`codes:`** on `@permissions` lists what the check answers with, and every
  operation with a permission must list them in `@throws`. They are the
  project's rather than a fixed `permission_required`, because whether a
  credential that does not reach a permission and a user who lacks it are one
  refusal or two is the implementation's to say — a fixed code would describe a
  refusal the server may never send. The list is required, so there is one way
  to write it.
- **No hierarchy.** Which permissions imply which (`write` covering `read`) is
  part of the assignment the implementation keeps, so the set carries no
  `@inherits` and there is no `authorizes`.

#### What is generated

- Rust: `impl Permission { from_name, as_str }` over the wire names — the
  `@variant(tag:)` when a value has one, since permission ids such as
  `admin.users.read` are seldom identifiers — and the awaited guard.
- Rust: `Permission::OPERATIONS`, every operation of the API by its
  `operationId` with the permission it needs (`Role::OPERATIONS` likewise for
  roles). The guard sits in the handler, so a caller reaching a service other
  than through its router — an MCP tool calling the method, a job runner —
  passes none. This is where such a caller reads the check, rather than
  restating which operation needs what:

  ```rust
  let required = Permission::OPERATIONS
      .iter()
      .find(|(id, _)| *id == tool.operation_id)
      .map(|(_, permission)| *permission);
  match required {
      None => return Err(unknown_tool()),        // no such operation: refuse
      Some(None) => {}                           // needs no permission
      Some(Some(permission)) => ctx.require_permission(permission).await?,
  }
  ```

  An id the API lacks has no row, so a misspelt one is refused rather than
  read as an operation that needs nothing. `None` means no check beyond
  `require:` — which for a `token` or `public` operation is no session at all.
  The caller still maps its own names to operation ids: an MCP tool written by
  hand, named otherwise or calling several methods, keeps that map; what the
  table removes is the check column beside it. The table checks nothing by
  itself: a caller that skips it runs the method unchecked.
- TypeScript: `permissions`, the whole set in declaration order, and a
  `permission` field on each `createApi` entry that needs one. It says what the
  server asks for, not who passes: there is no client-side check to mirror.
- OpenAPI: `x-permission` on the operation, its wire name; a role-checked one
  carries `x-role` the same way.

The compiler refuses: a second `@permissions` enum; `@permissions` on
`ErrorCode`, on the `@roles` enum or in an API that has one; `codes:` missing, empty, repeating a code or not a
list of strings, or naming a code `ErrorCode` lacks; `@inherits` on its values; a `permission:` that is quoted,
missing from the set, used without a set or outside `require: session`; `role:`
and `permission:` on one operation; and an operation with a permission whose
`@throws` misses one of the set's codes.

**Upgrading from 0.12.** An argument a directive does not read is now refused
rather than ignored. In the library, `Operation::role` is `Operation::access`, an
`Option<Access>` holding a role or a permission. A custom handler template names
the gate `{{{access_guard}}}` instead of `{{{role_guard}}}`, and a custom context
template carries `{{{require_permission}}}` beside `{{{require_role}}}`. The
JSON-bodied handlers — `handler`, `handler_no_input`, `handler_raw_response` and
`handler_raw_response_no_input` — now take `request: axum::extract::Request`
and read the body with `ApiJson` after `{{{access_guard}}}`, so a custom one
that still extracts `ApiJson` in its parameters parses the body before the
check. In a service with `role:` or `permission:`, an operation restating
`@auth` without a check of its own — `require: session`, `token` or `public` —
is refused; add `unguarded: true` where it needs none. An API declaring both a
`@roles` and a `@permissions` enum is refused; keep one.

**Upgrading from 0.11.** Declare the role set as a `@roles` enum and write each
`role:` bare (`role: content_admin`). `require_role` takes the role type.
`operationRoles` is gone: read the role from the `createApi` entry and the list
from `roles`. An API with a session and no roles drops `require_role` from its
`FromRequestContext` impl.

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

### `@raw(stream:)`

A `@raw(request:)` body is buffered before the service sees it, and bounded by
`RAW_BODY_LIMIT` (8 MiB) because buffering is what spends the memory.
`stream: true` hands it over unread instead, for an upload the server should
copy somewhere rather than hold:

```graphql
input UploadInput { id: UUID!, body: Binary! }

type Files @service {
  upload(input: UploadInput!): UploadResult!
    @mutation
    @raw(request: ["application/octet-stream"], stream: true)
}
```

```rust
/// The request body arrives unread as `body`; `input.body` is always empty.
/// No size limit is applied to it: this method must bound it — see [`RawRequest`].
async fn upload(&self, input: UploadInput, body: RawRequest)
    -> Result<ApiResponse<UploadResult>, ApiError<FilesUploadCodes>>;
```

The input is read from the query string and validated exactly as for a
buffered request, and the service then gets the rest of the request as a
`RawRequest`: `headers()`, `extensions()` (`ConnectInfo`, when the router is
served with it), `content_length()`, `content_type()`, and the body through
`into_body()` or `into_reader()`, a `futures_util::io::AsyncRead`.

**No size limit is applied — the service method owns it.** Nothing in the
generated router bounds a streamed body: `RAW_BODY_LIMIT` covers buffered ones
only. Skipping the buffer saves memory, but an unbounded body still fills a disk
and holds a connection, so the implementation must set the bound: refuse on
`content_length()` before reading, and count what it reads, since a chunked
request declares no length at all. `@length` on the binary field would measure
the empty value the server leaves there, so it is a compile error on a streamed
operation, inherited from the scalar or not.

Only the Rust server changes. The input type keeps its binary field, and the
TypeScript and Rust clients, zod and OpenAPI describe the operation exactly as
a buffered raw request — on the wire the two are the same thing.

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
| `[rust]` | handler | `handler`, `trait_name`, `input`, `output`, `method`, `path`, `query_fields`, `body_field`, `context_extractor`, `context_arg`, `access_guard` |
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
just lint                  # cargo deny, fmt-check, clippy, cargo sort, machete
```

237 tests, none of which need a network:

- **semantic** (96) — every validation rule, the error-code registry, path
  versioning, union tags, which constraint may sit on which type, plus
  nullability/list conversion
- **reuse** (21) — the generator is project-agnostic: a default config
  generates, SDL scalars reach all three targets and can override a built-in,
  unmapped scalars fail, outputs are selectable, the types target carries no web
  framework, the server re-exports the types rather than duplicating them, the
  client emits a method per operation, custom templates replace the built-ins,
  and a typo or an escaping tag in a template fails generation
- **unit** (34) — config defaults and TOML rejection, the `rpcgen_version`
  requirement and every way it can fail, template variable validation,
  identifier casing
- **auth** (28) — `@auth` on the trait signature, the handler's extractors, the
  role guard ahead of the body, the client's per-operation `role` field, the
  `@roles` set, its `@inherits` closure on both sides and its `OPERATIONS`
  table, and the rejections
  that keep a declaration from being decorative
- **permissions** (18) — `@permissions` and `@auth(permission:)`: the awaited
  guard ahead of the request in every handler form, its inheritance, the context trait's check, the set's Rust and
  TypeScript halves and its `OPERATIONS` table, `x-permission` / `x-role` in
  OpenAPI, a misspelt `@auth` argument refused, `unguarded`, `explicit`, one set per API,
  and every other rejection
- **mcp** (6) — `@mcp` inheritance, the `x-mcp` flag in the OpenAPI document,
  and the rejections of shapes a tool call cannot honour
- **streaming** (10) — `@subscription` and `@raw` on both sides: the signature,
  the route verb, and where the input is read from
- **snapshots** (7) — Rust, TypeScript, zod and OpenAPI output, determinism, and
  a pass over the whole OpenAPI document for the breaks a per-path assertion
  cannot see: a dangling `$ref`, a duplicated `operationId`, an operation with
  no response
- **cli** (15) — the binary itself: `generate` writes what `[output]` names and
  rewrites nothing unchanged, `check` exits non-zero on drift or a missing file
  and names it without repairing it, `--root` is what output paths resolve
  against, `--version` prints the package version, and a bad command or a
  `--config` that does not exist fails instead of falling back
- **oneof_wire** (1) — the generated `@oneOf` types compiled and run in a
  scratch cargo project, pinning the wire shape and the arm constraints at
  runtime rather than in emitted text
- **raw_wire** (1) — all three Rust targets compiled for an SDL reaching every
  `@raw` form, a `@subscription`, a role reached through `@inherits`, a
  `@variant(tag:)` error code, and for a second API a permission held and one
  refused in every handler form, which is the only thing that
  checks the emitted handlers, client methods and streaming runtime against
  rustc rather than against a `contains`; the router is then served on a local
  port and a streamed body larger than `RAW_BODY_LIMIT` is sent through it, and
  the tagged code is checked to reach the envelope by its tag

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
