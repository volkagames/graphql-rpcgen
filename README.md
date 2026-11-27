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
       │                               └─►  Rust client (treat::rpc RpcClient)
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
graphql-rpcgen = "0.16"
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

Both commands pass every Rust target through `rustfmt` before writing or
comparing it: the emitters write plain code — one statement per line, a
signature or a derive list on one line however long — and the layout is
`rustfmt`'s. It is the `rustfmt` named by `$RUSTFMT`, else the one on `PATH`,
run from the directory each file is written to (or the nearest one that
exists), so the project's `rustfmt.toml` or `.rustfmt.toml` applies exactly as
it does to `cargo fmt`. Keep one in the project — an empty file will do —
since without it `rustfmt` falls back to the developer's own
`~/.config/rustfmt/rustfmt.toml`, and two machines would write two layouts.

`--edition` is the one `cargo fmt` passes for the file: `[rust] edition` when
the settings name one, else the `package.edition` of the nearest `Cargo.toml`
above the file (`workspace.package.edition` when the package says
`edition.workspace = true`), else 2021. It decides more than parsing:
`rustfmt` derives its style edition from it unless `rustfmt.toml` sets
`style_edition`, and the 2024 style sorts `use` lists and breaks lines
differently, so a 2024 crate gets files its own `cargo fmt --check` keeps.
Name the edition in the settings when the files land outside the crate that
compiles them.

A `rustfmt` that is missing or fails stops the run, naming the target — a
rustup toolchain without the `rustfmt` component is named as such, with the
`rustup component add rustfmt` that fixes it; `[rust] format = false` writes
the plain output instead (see
[What the generated Rust needs](#what-the-generated-rust-needs-on-the-other-side)).

Both commands compare text, not line endings: a file that differs from the
output only in `\r\n` against `\n` is up to date, and `generate` leaves it
alone. A checkout with `core.autocrlf` (Git for Windows' default) rewrites
every line ending, and without this every file would be stale there. The
generator writes `\n`, except where a `rustfmt.toml` sets `newline_style`
(`"Windows"`, or `"Native"` on Windows), which applies to the Rust targets
only.

A run that would write nothing is a failure rather than a quiet success. The
settings file is optional, but the defaults name no target, so a project
without one — or with a `--root` pointing a directory too high — configures no
output at all. Left as a warning that case made `check` report "up to date"
after reading nothing, which is the one answer a CI gate must never give
wrongly.

Generation is deterministic: the output is byte-identical for the same SDL,
generator version and `rustfmt` (its version and the configuration it finds),
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

**Raise it when you upgrade.** The check refuses only a generator older than
the requirement, never a newer one, so it protects the output only if the
requirement moves with it. Set `rpcgen_version` to the version you regenerated
with, in the same change: left at the old one, a teammate or a CI job still on
the old generator runs `generate` and writes the old output over the new, or
runs `check` and calls the new output stale, instead of refusing the settings.

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

[rust]
format = true                         # pass the Rust targets through rustfmt (see CLI)
edition = "2024"                      # rustfmt's --edition; default: the crate's own
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

What each target holds:

- **`rust_types`** — the SDL's types with their `validator` rules, the
  `ErrorCode` registry and one code set per operation, the
  [`operations`](#the-operation-table) table, and `wire`: the `static`s both
  ends read an operation off — a subscription's or a raw request's query table
  (`<OP>_FIELDS`), an `@http` operation's `treat::rpc::HttpSpec`
  (`<OP>_HTTP`), a raw request's accepted media types (`<OP>_TYPES`). It
  re-exports `treat::rpc::{WireEnum, Narrow, Undeclared, UNDECLARED_CODE_META}`,
  so a glob import of it reaches `Role::from_name` and `.narrow()`.
- **`rust_server`** — a service trait and an axum router per service, a handler
  per operation (`type Rt = ::treat::rpc::Runtime<ErrorCode>;` and a few
  `Rt::…` calls each), and `FromRequestContext`, the access checks a session's
  context adds to `treat`'s `ResolveContext`. It re-exports what an
  implementation names: the envelope builders, `Refusal`, `ResolveContext`,
  `EventStream`, `event_stream`, `RawBody`, `RawRequest`, `RAW_BODY_LIMIT`, as
  the API uses them.
- **`rust_client`** — `ApiClient`, a newtype over `treat::rpc::client::RpcClient`
  with an accessor per service, each method one call.

The generated code itself imports no name beside the API's types: it writes
`::treat::rpc::server::Refusal`, `::axum::extract::Request`,
`::serde::Serialize` and the rest by their absolute paths, so an SDL type may be
spelt like any of them — an `input Request`, a `type Response`, an
`enum WireEnum` — and is the only item of that name in the module. A re-export
is left out where the API declares a type of the same name; reach that item by
its `treat` path. The few names the generator declares itself, and the prelude
names it writes unqualified, are refused as type names (see
[Names the targets have to be able to spell](#names-the-targets-have-to-be-able-to-spell)).

Omitting `types_path` keeps everything in one file: the server target then emits
the types itself, which is the single-file layout — the types preamble, then
the server's, exactly what the two files of the split layout start with.

#### What the generated Rust needs on the other side

The generator itself depends on almost nothing, but the code it writes does, and
a missing crate surfaces as a compile error in the consumer rather than here.
None of it is a runtime pasted into the output: everything that does not depend
on the schema — reading an input, refusing a request, answering with the
envelope, bytes or Server-Sent Events, sending a call and reading its answer —
is [`treat::rpc`](https://github.com/volkagames/treat/blob/main/docs/rpc.md),
which the generated files call. `rust_server` and `rust_client` each pull in the
`rust_types` row as well; they do not pull in each other, so a client-only
consumer needs no web framework:

| Target | Crates |
| --- | --- |
| `rust_types` | `serde`, `serde_json`, `treat` (default features; its `validator` feature with any `@pattern`; its `tracked` feature, and so a nightly toolchain, with any `@errorGroup`), `validator`, `error_set`; `derive_more` with any newtype `@scalar`; plus whatever each `@scalar(rust:)` names — typically `uuid` and `chrono` |
| `rust_server` | `axum`, `async-trait`, and `treat` with its `rpc-server` feature |
| `rust_client` | `treat` with its `rpc-client` feature, which brings `reqwest` — without TLS: see below |

An API declaring a `@subscription` or a `@raw` body needs `treat`'s
**`rpc-stream`** feature as well, on the server and on the client: it adds
Server-Sent Events and raw byte bodies to whichever of the two is on, and the
generated code then names `EventStream`, `RawBody` and `RawRequest`. A plain
JSON or `@http` API compiles none of it. No `futures-*`, `bytes` or `regex`
dependency of the project's own is needed: `treat::rpc` re-exports what a
service implementation reaches for (`treat::rpc::futures_util` to read a
`RawRequest`, say).

`treat` builds `reqwest` without its default features — TLS, HTTP/2 and system
proxies among them — so an `https` base URL compiles and then fails at the
first request, as `ClientError::Transport`, unless TLS comes from somewhere.
Turn on `treat`'s **`rpc-client-rustls`**, or depend on `reqwest` yourself with
the TLS feature you want — plus `http2` or `system-proxy` if you relied on
`reqwest`'s defaults — which Cargo unifies with `treat`'s, or hand
`ApiClient::with_http` a client configured your way. A project that depended
on `reqwest` for the generated client up to 0.15 had its defaults, TLS
included: keep that dependency, with its features, rather than dropping it.

```toml
# A server, and a client of another service over https.
treat = { version = "0.27", features = ["rpc-server", "rpc-client-rustls", "rpc-stream", "rpc-status-header"] }
```

Those are the features the generated code needs to *compile*. The OpenAPI
document additionally describes an `x-rpc-status` header on every response,
which no generated handler writes: it comes from `treat`'s `rpc-status-header`
feature, which a server meant to honour that document needs. With it a client
also tells a raw or streamed answer from a refusal by the header.

Span traces are the other feature nothing here needs to compile, and the one
easiest to forget. Without them an internal error logs as `connection lost`
with no trace of which request or which record it came from. A server should
enable `treat`'s `spantrace` feature **and** install `tracing_error::ErrorLayer`
in its subscriber; with the feature alone the trace is printed empty, and
nothing warns about it:

```toml
treat = { version = "0.27", features = ["rpc-server", "rpc-status-header", "spantrace"] }
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

The generator needs one more tool where it runs: `rustfmt`, which lays out the
Rust targets as they are generated (see [CLI](#cli)). The files are then what
formatting the project's tree writes, under its own `rustfmt.toml`, and carry no
blanket `allow`: a project that formats its tree, checks it in CI, or denies
clippy's warnings does not have to exclude them. On a machine without
`rustfmt`, turn the step off and get the plain output, which compiles the same:

```toml
[rust]
format = false      # write the Rust targets unformatted; needs no rustfmt
```

A project that runs `cargo fmt --check` then excludes the generated module
from it, since `rustfmt` would rewrite it:

```rust
#[rustfmt::skip]
mod api;
```

The generated client groups operations by service, matching the TypeScript one:

```rust
let api = ApiClient::new("https://api.example.com");
match api.clans().create(&input).await {
    Ok(response) => { /* ... */ }
    // The outcome travels in the envelope, so a rejected call is an `Api`
    // error carrying codes — not a transport failure.
    // `ClansCreateCodes` is the set `Clans.create` may answer with.
    Err(ClientError::Api(errors)) if errors.is(ClansCreateCodes::ClanAlreadyExist) => { /* ... */ }
    Err(e) => return Err(e.into()),
}
```

`ClientError` separates what the server said (`Api`) from what never reached it
(`Encode`, nothing sent; `Transport`; `Decode`; `Status`), because only the
first carries a code worth branching on. It is `treat`'s, re-exported from the
client module with `ApiErrors`, `ClientResult`, `ApiResponse`, `ErrorMessage`,
`ErrorSource` and, with `rpc-stream`, `EventStream` and `StreamError`, and it is
`#[non_exhaustive]`: a `match` ends in a catch-all. `errors.code::<ErrorCode>()`
reads the first code against the registry. `ApiClient::with_http` takes a configured `reqwest::Client`, which
is where timeouts, proxies and default headers go.

`api.rpc()` is the `treat::rpc::client::RpcClient` underneath (`into_inner()`
by value), for a call no generated method makes:
`api.rpc().call::<_, Output, Codes>("/path", &input)`. `subscribe`,
`raw_request` and `raw_request_json` take the operation's query table after the
path (`&wire::<OP>_FIELDS`), and `call_http` an `@http` operation's
`&wire::<OP>_HTTP`.

### Library use

`rpcgen.toml` deserializes into `config::Config`, which is also the public Rust
API — a project embedding the generator builds one directly and skips the file:

```rust
let api = graphql_rpcgen::compile(Path::new("api"))?;                     // scalars come from the SDL
let config = Config::from_toml_file(Path::new("rpcgen.toml"))?;   // or built by hand
let mut files = graphql_rpcgen::generate_all(&api, &config)?;      // pure: the plain output
graphql_rpcgen::format_outputs(&mut files, Path::new("."), &config)?;   // rustfmt, as the CLI does
for file in files { /* write file.path, relative to that root */ }
```

`generate_all` is a pure function of the IR and the settings, and returns the
Rust targets plain — what the golden tests pin. `format_outputs` is the CLI's
formatting step: it passes each Rust target (`file.target.is_rust()`) through
`rustfmt` from the directory under the given root the file is headed for, for
the edition `format::edition_for` finds there, and does nothing when
`[rust] format = false`. A caller that formats its own way, or not at all,
skips it; `format::rustfmt(source, dir, edition)` formats one string.

`compile_str` takes SDL as a string, for a caller that does not have it on disk.

## Wire protocol

Every operation is `POST /rpc/{service}/{operation}` with a JSON body, except a
`@subscription` (a `GET` returning SSE), a `@raw` body and an `@http` binding.
`@query` and `@mutation` describe TanStack semantics only; they do not change
the verb.

The JSON body must be declared JSON: `Content-Type: application/json` (or a
`+json` type), else the server refuses it at `415` with `invalid_body` before
reading it. A browser posts a form or plain text across origins without a CORS
preflight, so a server that read JSON under any type would run such a request
with the visitor's cookies. The generated clients send the header; a caller of
its own — a script, a probe, a test — must too:

```bash
curl -X POST https://api.example.com/rpc/users/get \
  -H 'Content-Type: application/json' \
  -d '{"id": "u1"}'
```

A body that is declared JSON and does not parse is `invalid_body` at `422`,
with `source.pointer` at the offending field. A `@raw(request:)` body is read
only under one of the types its directive lists, and is refused at `415`
otherwise and at `413` past `RAW_BODY_LIMIT`.

`@rpc(path:)` overrides the derived path; `@version(n:)` mounts it under a
`/v{n}` prefix.

### `@http`

For an endpoint whose wire format was fixed before its schema was written — a
service being replaced behind clients that cannot change at the same time —
`@http` replaces the derived route with the one the clients already call:

```graphql
type Users @service @auth(require: token) {
  user(input: UserInput!): User!
    @query
    @http(method: GET, path: "/api/user/{user_id}", envelope: false)
    @throws(codes: ["not_found"])

  info(input: InfoInput!): Info!
    @query
    @http(method: ANY, path: "/api/info", body: FORM, envelope: false)
}
```

| Argument | Meaning |
| --- | --- |
| `method:` | `GET`, `POST`, `PUT`, `PATCH`, `DELETE`, or `ANY` for an endpoint that never checked its method |
| `path:` | the route; a `{name}` segment is filled by the required scalar or enum input field of that name |
| `body:` | where the other fields travel: `NONE` (the query string; the default for `GET`, `DELETE` and `ANY`), `JSON` (the input object; the default for `POST`, `PUT`, `PATCH`) or `FORM` (an `application/x-www-form-urlencoded` body read over the query string, the body winning per field); a `JSON` route answering `GET`, `DELETE` or `ANY` that is called without a body reads the query string instead |
| `envelope:` | `false` answers a success with the output value itself rather than `{"data": …}`; default `true` |

A path parameter always wins over a field of the same name in the query string
or the body: it is what the route matched. A failure is the envelope whatever
`envelope:` says, under the status the error carries (`ApiError::with_status`),
so a caller reads failures one way. An input the runtime cannot read
answers `400` — and a body it cannot read at all the status of that refusal,
`413` for one past the body limit — rather than the `200` the envelope uses
elsewhere: a caller of a route fixed elsewhere reads the status line. For the
same reason a refused session answers `401` and a denied role or permission
`403` — when the context left its refusal at a `2xx` status, so a `ResolveContext::from_parts` failing
for a reason not the caller's (an unreachable session store) gives it its own
status, `503` say, or the caller reads it as a sign-out — and with `envelope: false` a service failure left at any `2xx` status answers
`500`, since a bare success is told from a failure by its status alone. `ANY` reads the same input for every method
— a `GET` simply has no body — and clients send it as `GET`, or as `POST` with
`body: FORM`; OpenAPI describes it under that method with `x-rpc-any-method`.
Two operations may share a path under different methods; `ANY` shares it with
none. Paths that put a parameter in the same position name it the same, since
the router keeps one name per position. The zod `inputSchemas` key an `@http`
operation by `METHOD path`.

An existing format often writes every key, an absent value as `null`, where
this generator leaves the key out; `@explicitNulls` on an output type keeps
that: Rust drops `skip_serializing_if`, TypeScript makes the key required with
a `| null` value, and OpenAPI lists every field as required.

A service's `@version` does not prefix an `@http` path, which is the URL
clients already call, and `@version` on the operation itself is refused: a
version belongs in the path `@http` spells out.

`@http` does not combine with `@rpc` (both name the route), `@raw` or
`@subscription` (both already decide how the operation sits on HTTP), or `@mcp`
(a tool call is the JSON envelope). Fields in a query string or a form body are
scalars, enums and lists of them; an object travels there as JSON text, as it
does for a subscription.

Both clients write a field in a URL or a form from the table the server reads
it with — Rust from the types module's `wire` statics, TypeScript from the
`fields` its spec carries (`{ tags: 'str[]', filter: 'json' }`, left out when
every field is written as plain text anyway): a field of an object or `JSON`
type is JSON text, a string quoted; only a list repeats its key, and an array
in any other field is one JSON value; a list that is not JSON-typed cannot carry
`null`, so one is refused before sending (`Encode` in Rust, a `TypeError` in
TypeScript).

Responses carry a `{"data": …}` / `{"errors": […]}` envelope. Errors are
`{code, message, source}` where `source.pointer` is an RFC 6901 pointer to the
offending field. All errors in one response share one HTTP status.

An API with any operation declares `enum ErrorCode`, the registry every code
comes from, listing at least `invalid_body` and `internal_error`: the runtime
answers those for any operation, `@throws` or not, and the generated server
and client are typed over the registry. Only an SDL of types alone may leave it
out. A code may also be declared by an [error group](#error-groups-errorgroup),
whose codes join the registry in every target.

`code` is the `ErrorCode` value's wire name: its SDL spelling, or its
`@variant(tag:)` when the wire carries something that is not a GraphQL
identifier. `portal_db__clan_not_found @variant(tag: "portal_db::clan_not_found")`
stays `portal_db__clan_not_found` in Rust and in `@throws`, and every target —
the Rust server's envelope, the Rust and TypeScript clients, OpenAPI — writes and
reads `portal_db::clan_not_found`.

## Architecture

The pipeline has a deliberate seam in the middle:

```text
GraphQL SDL → AST → semantic validation → IR → generators → rustfmt (Rust targets)
```

Generators never see the GraphQL AST — they read only `ir::Api`, which knows
nothing about GraphQL, Axum, OpenAPI or TypeScript. Adding a target means
adding one emitter, not touching the front end.

```text
src/parser.rs                SDL discovery + parsing (graphql-parser)
src/ir.rs                    semantic IR — the model every generator reads
src/semantic.rs              AST -> IR, all validation
src/config.rs                settings: outputs, OpenAPI metadata, templates, formatting
src/version.rs               the required `rpcgen_version` and how it is compared
src/template.rs              mustache rendering + variable validation
src/naming.rs                identifier casing shared by the generators
src/error.rs                 compile diagnostics, each naming its SDL file and position
src/emit_common.rs           helpers every emitter shares: header, doc comments, type names, re-exports
src/format.rs                the formatting step: each Rust target through rustfmt, from where it is written
templates/rust/              built-in templates over treat::rpc + axum: preambles, router, handlers, client methods
templates/typescript/        the static fetch client every api.ts carries
src/generate_rust.rs         Rust wire types, traits, Axum bindings
src/generate_rust_client.rs  Rust client over treat::rpc's RpcClient (reqwest underneath)
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
    ::serde::Serialize, ::serde::Deserialize,
    ::derive_more::AsRef, ::derive_more::Display, ::derive_more::From, ::derive_more::Into,
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

### Enums

Every enum, the `ErrorCode` registry included, reaches Rust with its whole set
and its wire names in both directions:

```graphql
enum Visibility { private public_link @variant(tag: "public-link") }
```
```rust
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash,
    ::serde::Serialize, ::serde::Deserialize, ::treat::rpc::WireEnum,
)]
pub enum Visibility {
    #[serde(rename = "private")]
    Private,
    #[serde(rename = "public-link")]
    PublicLink,
}

// From `treat::rpc::WireEnum`, which the types module re-exports:
Visibility::ALL;                           // &[Private, PublicLink], in SDL order
Visibility::from_name("public-link");      // Some(Visibility::PublicLink)
Visibility::PublicLink.as_str();           // "public-link"
```

`ALL`, `from_name` and `as_str` are the items of `treat::rpc::WireEnum`, which
reads the wire names from the renames: a caller needs the trait in scope, and a
glob import of the generated types module brings it. `as_str` is exactly what serde writes — the `@variant(tag:)` when a value has
one — and `from_name` reads only that, so the GraphQL identifier
`public_link` is `None`. A project storing a value as text, filtering a query
by it or listing the choices on a screen reads them here instead of keeping an
array of variants and a `match` of spellings beside the generated type, which
drift from the SDL without a word: a new value missing from the array, an
edited tag leaving the old string behind. `Hash` lets a value key a set or a
map.

The registry's variants keep their SDL spelling, and its derive also writes
`#[code("…")]` for a tagged value, which is what `Display` — and so the
envelope — spells it by. A code named `ALL`, `from_name` or `as_str` is
refused.

**Upgrading from 0.15.** The three are trait items now: import
`treat::rpc::WireEnum` (or glob-import the types module) where they are called.
An inherent `ALL`, `from_name` or `as_str` of a project's own — which 0.15
refused as a duplicate — compiles again, and shadows the trait's item wherever
both are in reach: keep one only if it is meant to differ.

**Upgrading from 0.14.** A project's own `impl Hash` for a generated enum
collides with the derived one: delete it. Its own inherent `as_str`,
`from_name` or `ALL` does not collide — the generated ones are trait items —
but is what a call resolves to, ahead of the trait's: delete it too, unless it
spells a value differently from the wire on purpose, in which case give it
another name so the two vocabularies cannot be mistaken for each other.

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
| `@query(scope:)` | FIELD | read-only; generates a TanStack query helper; `scope` overrides the `@scopes` default |
| `@mutation(scope:)` | FIELD | state-changing; generates a mutation helper; `scope` names what a token needs, required under `@scopes` |
| `@rpc(path:)` | FIELD | override the derived path |
| `@explicitNulls` | OBJECT | an absent optional value of this output type is written as `null` instead of being left out |
| `@http(method:, path:, body:, envelope:)` | FIELD | bind the operation to a method, a path with `{parameters}`, a query-string, JSON or form input, and an enveloped or bare success — see [`@http`](#http) |
| `@version(n:)` | OBJECT/FIELD | mount under a `/v{n}` prefix |
| `@subscription(scope:)` | FIELD | a stream of events over SSE; the field's type is one event |
| `@raw(request:, response:, stream:)` | FIELD | a body that is bytes rather than the JSON envelope; `stream: true` hands the request body to the service unread |
| `@auth(require:, role:, permission:, unguarded:, explicit:)` | OBJECT/FIELD | what the caller must prove; `session` passes a context; `role` assigns the operation to a role; `permission` names the permission it needs; `unguarded: true` drops its service's check; `explicit: true` on a service has each operation state its own |
| `@roles` | ENUM | the enum's values are the API's roles |
| `@inherits(roles:)` | ENUM_VALUE | on a role: the roles it inherits the permissions of |
| `@permissions(codes:)` | ENUM | the enum's values are the API's permissions; `codes` are what the permission check answers with |
| `@scopes(query:)` | ENUM | the enum's values are the scopes a token carries; `query` is the one a query needs by default |
| `@mcp(expose:)` | OBJECT/FIELD | the operation is an MCP tool; surfaces as `x-mcp` in OpenAPI |
| `@throws(codes:, groups:)` | FIELD | error codes the operation may return: those listed, and every code of each [error group](#error-groups-errorgroup) named |
| `@errorGroup(includes:)` | ENUM | the enum's values are one area's error codes, joined by the codes of the groups it includes — see [Error groups](#error-groups-errorgroup) |
| `@message(text:)` | ENUM_VALUE | on an error code, of `ErrorCode` or of a group: the message it carries |
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

Across services the recased names meet too. `Users.getAll` and `UsersGet.all`
are two routes and two `operationId`s, but one server handler
`users_get_all`, one set of `wire::USERS_GET_ALL_*` statics and one
`UsersGetAllCodes`; `Users` and `USERS` are one Rust module and client accessor
`users` and one key of TypeScript's `createApi`. Either pair is refused where
the second is declared, naming the first and every name the two share.

A type is a Rust item beside the ones the generator declares itself, so it may
not be spelt like one of those: `Rt`, `Failure`, `FromRequestContext`,
`ApiClient`, the `operations` and `wire` modules, a service's `<S>Service`
trait and `<S>Operations` client group, an operation's `<S><Op>Codes` set, an
error group's `<Area>Failure`. Nor
like a prelude name the generated code writes unqualified — `Option`, `Some`,
`None`, `Result`, `Ok`, `Err`, `Vec`, `Box`, `String`, `Clone`, `Send`, `Sync`,
`Sized`, `From`, `Into` — which it would take the place of (`From` because
`treat`'s `ApiErrorCode` derive and `error_set!` write it unqualified too). A scalar mapped to a Rust type
declares no item and is exempt; a `rustNewtype` scalar is not.

Free to reuse, by target:

- **Rust** — the runtime's items (`Request`, `Response`, `Refusal`,
  `RpcClient`, `WireEnum`, …): the generated code names them by their absolute
  paths.
- **TypeScript** — the globals the client runtime in `api.ts` uses
  (`Response`, `Headers`, `Promise`, `Record`, `Readonly`, `AbortSignal`,
  `URLSearchParams`, `Uint8Array`, `Error`, …): it writes each as
  `globalThis.X`, so a type the API declares in that module does not take its
  place. The names `api.ts` declares itself are not free: `RpcClient`,
  `RpcError`, `RpcResponse`, `RpcErrorItem`, `RpcErrorCode`, the other `Rpc…`
  types, `QueryOptionsResult`, `MutationOptionsResult`, `Api` and a service's
  `<S>Operations`. The compiler does not refuse them, since an SDL generated
  for Rust alone may use them; an SDL with a TypeScript target must not — an
  interface merges with the class or interface of the same name, and a type
  alias collides with it.

Most Rust keywords survive as `r#type` — `gen`, reserved by the 2024 edition,
as `r#gen`, which every edition accepts; `self`, `crate` and `super` do not
exist in that form, so a field or operation named after one is refused rather
than renamed behind the author's back.

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
    ::axum::extract::State(service): ::axum::extract::State<S>,
    ::treat::rpc::server::Ctx(ctx): ::treat::rpc::server::Ctx<S::Ctx>,
    request: ::axum::extract::Request,
) -> Result<::axum::response::Response, ::treat::rpc::server::Refusal> {
    ctx.require_role(Role::ContentAdmin)?;
    let input = Rt::json_input(request).await?;
    let result = service.revert(&ctx, input).await;
    Ok(Rt::respond(result))
}
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
    ctx.require_permission(Permission::UsersWrite).await?;
```

An `@http` handler gives either refusal the status its callers read, `403`
unless it carries its own: `Rt::http_access(access)?`.

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

Resolving the caller is `treat`'s contract, `treat::rpc::server::ResolveContext`,
which the `Ctx` extractor reads it with. `FromRequestContext` is generated on
top of it with the checks the schema asks for: in an API with a `@roles` enum,
the role check, and in one with a `@permissions` enum, the permission check.
Every refusal is a `Refusal` — a whole response, built from an `ApiError` or a
`Response` with `.into()`:

```rust
// treat's
pub trait ResolveContext: Sized + Send + Sync + 'static {
    fn from_parts(parts: &mut Parts)
        -> impl Future<Output = Result<Self, Refusal>> + Send;
}

// generated, naming `treat`'s items by their paths
pub trait FromRequestContext: ::treat::rpc::server::ResolveContext {
    fn require_role(&self, role: Role) -> Result<(), ::treat::rpc::server::Refusal>;
    fn require_permission(
        &self,
        permission: Permission,
    ) -> impl ::std::future::Future<Output = Result<(), ::treat::rpc::server::Refusal>> + Send;
}
```

A project implements both for its context type:

```rust
impl ResolveContext for RequestContext {
    async fn from_parts(parts: &mut Parts) -> Result<Self, Refusal> {
        let session = read_session(parts).await;
        session.ok_or_else(|| error(ErrorCode::unauthenticated).into())
    }
}

impl FromRequestContext for RequestContext {
    fn require_role(&self, role: Role) -> Result<(), Refusal> {
        match self.roles.iter().any(|held| held.authorizes(role)) {
            true => Ok(()),
            false => Err(error(ErrorCode::role_required).into()),
        }
    }
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
| ROLES | the values of the `@roles` enum | the enum, with [what every enum gets](#enums); TS: the literal union and `roles` | — |
| RH, role hierarchy | `@inherits(roles:)` on a value | `Role::authorizes`; TS: `roleAuthorizes` | — |
| OPS, operations | an operation of a `@service` | its handler and client entry | — |
| OBS, objects | — | — | whatever the operation acts on |
| PRMS, permissions | an operation on every object it takes | — | a permission narrowed to some objects — a field, a protected branch — checked in the method, since `@auth` cannot see the request |
| PA, permission assignment | `@auth(role:)` | the role guard; `role` in [`operations`](#the-operation-table); `role` on the `createApi` entry | — |
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

- Rust: the awaited guard. The enum's `from_name` and `as_str` read and spell
  the wire names, as [every enum's](#enums) do — the `@variant(tag:)` when a
  value has one, since permission ids such as `admin.users.read` are seldom
  identifiers.
- Rust: the `permission` column of [`operations`](#the-operation-table), for a
  caller that reaches a service other than through its router — an MCP tool
  calling the method, a job runner — and so passes no generated guard.
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

**Upgrading from 0.13.** `Permission::OPERATIONS` and `Role::OPERATIONS` are
gone: the same fact is the `permission` or `role` column of
[`operations`](#the-operation-table), and two shapes for one fact leave every
consumer to choose between them. `Permission::OPERATIONS.iter().find(|(id, _)|
*id == x).map(|(_, p)| *p)` is `operations::by_id(x).map(|op| op.permission)`.
`@query`, `@mutation` and `@subscription` now read a `scope:` argument, which an
API without a `@scopes` enum never needs. Every API gains two generated items
whatever it declares: `pub mod operations` in the Rust types and
`operationKeys` in TypeScript — a project with its own item of either name at
the same level renames one. In the library, `Operation` gains `scope` and `Api`
gains `scopes`.

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

### Scopes: `@scopes`

A role or a permission says what the **user** may do. A credential that acts
for a user without being one — an API token, a key minted for a sync client, an
agent's connection — is usually narrowed further: a token that may read must
not delete, whatever its owner may. That second question is the token's
*scope*, and which operation needs which is as much a fact of the API as its
path:

```graphql
"What an API token may do on its owner's behalf."
enum TokenScope @scopes(query: read) {
  read
  write
  delete
}

type Files @service @auth(require: session) {
  list(input: ListInput!): Listing! @query @rpc(path: "/files/list")
  rename(input: RenameInput!): Node! @mutation(scope: write) @rpc(path: "/files/rename")
  purge(input: PurgeInput!): Purged! @mutation(scope: delete) @rpc(path: "/files/purge")
}
```

The rules, and why:

- **The vocabulary is the SDL's**, an enum marked `@scopes` as `@permissions`
  marks the permission set, so no scope word is compiled into the generator and
  the set reaches every target as a type: a credential's scope names are read
  with `TokenScope::from_name`, and a screen minting tokens lists `scopes`. Not
  `rpcgen.toml`: the scopes are part of the contract a client is written against,
  and the config holds only what is not the API.
- **A query reads.** `query:` names the scope a `@query` or a `@subscription`
  needs when it states none; one that needs more says so, `@query(scope: …)`.
  Restating the default is refused, so there is one way to write each
  operation. Without `query:`, every operation states its own. A query taking a
  `@raw(request:)` body gets no default either: it takes bytes in, and whether
  that reads or writes is the same judgement a mutation makes.
- **A mutation always states its scope.** Whether it writes or permanently
  removes — whether a token that may edit may also destroy — is a judgement each
  mutation makes, and a default would make it for every mutation added later
  without a word: the one that forgot would be reachable by every token that
  writes. So a `@mutation` with no `scope:` fails the build, the way a missing
  row of a hand-written scope table fails closed.
- **On the kind directive, not on `@auth`.** Which of the three an operation is
  decides whether it needs a scope and what it defaults to, so the scope sits
  where the kind is declared. On `@auth` it would force restating the rule, and
  a restated `@auth` replaces the service's whole — a scope would cost the
  service's permission or require.
- **No scope on a public operation.** It takes no credential, so nothing is
  narrowed, and a `scope:` there is refused.
- **Beside a permission, a scope is optional.** `@permissions` already lets the
  permission check answer for the credential — its `codes:` may tell a token
  outside its list from a user who lacks the permission — so an operation with
  `@auth(permission:)` needs no scope, gets no default, and may still state one.
  **Leaving it out is a duty on the implementation:** with no `scope`, the
  permission check is all that stands between a delegated credential and the
  operation, so `require_permission` must refuse a credential not allowed the
  permission, not only a user who lacks it. An implementation whose check reads
  only the user's roles states a `scope:` on every such operation instead.
- **A role is the user's.** `role:` says nothing of the credential, so a
  role-checked operation states its scope as any other.
- **No hierarchy.** Whether `write` implies `read`, or what a token holding none
  may do, is the implementation's policy over the credentials it issues.

Nothing checks a scope for you: the generated handler cannot see which
credential the request carried. The scope is read where the credential is —
the context extractor, a middleware in front of the router — from
[`operations`](#the-operation-table):

```rust
let Some(op) = operations::by_route(request.method().as_str(), request.uri().path()) else {
    return Err(unmapped());                            // not an operation: refuse
};
match (op.scope, op.permission) {
    (Some(scope), _) => token.require(scope)?,         // the token must carry it
    (None, Some(_)) => {}                              // `require_permission` judges the token
    (None, None) if op.auth == Auth::Public => {}      // public: no credential
    (None, None) => return Err(unmapped()),            // anything else: refuse
}
```

The last arm cannot be reached through a table the compiler accepted, and is
there so the gate stays closed if the table and the gate ever disagree.

Each target carries the resolved scope — a query's default filled in:

- Rust: the `scope` column of `operations`; the scope enum reads and spells its
  wire names with `from_name` / `as_str`, as [every enum](#enums) does.
- OpenAPI: `x-scope` on the operation, its wire name.
- TypeScript: `scopes`, the whole set in declaration order, and a `scope` field
  on each `createApi` entry that needs one.

The compiler refuses: a second `@scopes` enum; `@scopes` on `ErrorCode`, the
`@roles` or the `@permissions` enum; a `query:` that is quoted or not one of
its values; `@inherits` on its values; a `scope:` that is quoted, not a value
of the set, or used without one; a `scope:` on a public operation; a query
restating the default; and a mutation, a query taking a raw request body, or —
without a default — any operation, that a credential can reach, has no
permission and names no scope. Each is reported at the operation's declaration.

### Error groups: `@errorGroup`

The flat registry says what the API can answer; the code below a handler that
answers the registry's `Failure` may raise any code of it, and only the
operation's edge narrows it to what `@throws` declares. An error group is one
area's own small table of codes, with a failure type of its own: code in that
area can only raise the group's codes, and an operation's edge takes the
group's failure exactly when its `@throws` covers the group — a mismatch is a
compile error, not a request answered `internal_error`.

```graphql
"""Reaching a path: who may, and whether its storage answers."""
enum AccessCode @errorGroup {
  forbidden
  read_only_storage
  storage_unavailable
}

enum LockCode @errorGroup { locked }

"""Making and changing folders."""
enum FolderCode @errorGroup(includes: [AccessCode, LockCode]) {
  "A file holds the name."
  kind_conflict
  name_refused @message(text: "the name is not allowed here")
  symlink_refused
  not_supported
}

type Folders @service {
  mkdir(input: MkdirInput!): Folder! @mutation @throws(groups: [FolderCode])
}
```

| Rule | |
| --- | --- |
| A group's codes | its own values, the codes of every group it includes (transitively, each once), and `internal_error` |
| The registry | `ErrorCode`'s own values and every group's own values: a code lives in `ErrorCode` or in exactly one group, so a project moves codes into groups one at a time |
| `invalid_body`, `internal_error` | stay in `ErrorCode`: the runtime answers them for any operation, and every group holds `internal_error` already |
| `@message(text:)` | on a code of `ErrorCode` or of a group, the message the code carries; without it, the message is the name with spaces (`kind_conflict` → `kind conflict`) |
| `@throws(codes:, groups:)` | the operation answers the runtime codes, the codes listed and every code of each group named; either argument may stand alone |
| The name | `<Area>Code`; the area names the failure type, `<Area>Failure` |

The types target generates, for `FolderCode`:

| Item | What it is for |
| --- | --- |
| `enum FolderCode` | the group's codes, flattened, spelt as in the SDL — the registry's derives and attributes, `#[message]` on each, and `internal_error` as its `#[undeclared]` value |
| `struct FolderFailure(pub ApiError<FolderCode>)` | the error of an `erris::Result` in the folder area: an `erris::tracked::TrackedError` (each `?` records its hop), a std error showing the `ApiError` underneath; `code()`, `error()`, `into_error()` |
| `From<ApiError<FolderCode>>` and `From<FolderCode>` for `FolderFailure` | `.wrap_api_code(FolderCode::kind_conflict)?` and `return Err(FolderCode::kind_conflict.into())` |
| `From<AccessCode> for FolderCode`, `From<AccessFailure> for FolderFailure` | for every group included, directly or not; the failure keeps its message, `meta`, status, source, raise site and hops — only the code changes |
| `From<FolderCode> for ErrorCode`, `From<FolderFailure> for ApiError<ErrorCode>` | a group's failure as the flat `Failure`; `.narrow::<FolderCode>()` reads a `Failure` back, a code the group lacks becoming `internal_error` |
| `From<FolderFailure> for ApiError<FoldersMkdirCodes>` | for every operation whose codes hold all of the group's, named in `@throws(groups:)` or not |

```rust
use treat::erris::prelude::*;
use treat::prelude::*;

fn make(path: &str) -> treat::erris::Result<Folder, FolderFailure> {
    check_name(path).wrap_api_code(FolderCode::name_refused)?;
    Ok(Folder { path: path.to_string() })
}

// The edge: the service trait's method, a std `Result` over the operation's set.
async fn mkdir(&self, input: MkdirInput) -> Result<ApiResponse<Folder>, ApiError<FoldersMkdirCodes>> {
    Ok(success(make(&input.path)?))
}
```

Which `?` compiles:

| `?` on | in a function returning | |
| --- | --- | --- |
| `Result<T, ApiError<FolderCode>>` (`wrap_api_code`, `ok_or_api_code`) | `erris::Result<_, FolderFailure>` | compiles |
| `erris::Result<T, FolderFailure>` | `Result<_, ApiError<FoldersMkdirCodes>>`, the operation's edge | compiles when the operation's codes hold the group's; a compile error otherwise |
| `erris::Result<T, FolderFailure>` | `Result<_, Failure>`, or `erris::Result<_>` over a `Report` | compiles |
| `erris::Result<T, AccessFailure>` | `erris::Result<_, FolderFailure>` | not yet: erris converts a tracked error only into the same type; the `From` it will use is generated |
| `erris::Result<T, FolderFailure>` | `erris::Result<_, Failure>` | not yet, for the same reason |

A failure type is an `erris::tracked::TrackedError`, so an API declaring a group
needs `treat`'s **`tracked`** feature wherever the types target compiles, and
with it a nightly toolchain. An API without one compiles as before.

The other targets hold the group as they hold any enum — a TypeScript union of
its wire names, a zod enum, an OpenAPI schema — its codes flattened as in Rust;
`ErrorCode` lists the groups' codes everywhere, and an operation's failure
`code` enum in OpenAPI lists its groups' codes. GraphQL names every directive
argument, so the message is `@message(text: "…")` rather than a bare string.

The compiler refuses, each at its declaration: a code declared by two of
`ErrorCode` and the groups; `invalid_body` or `internal_error` in a group;
`includes:` naming something that is not a group, or quoted, or twice; groups
including each other in a cycle, reported once with the way round; a group
not named `<Area>Code`, or marked `@roles`, `@permissions` or `@scopes`;
`@errorGroup` on `ErrorCode`, or a group in an API without it; a type spelt
like a group's `<Area>Failure`; `@throws(groups:)` naming something that is not
a group, quoted, or twice; `@message` on a value of any other enum, or without
a string `text:`; and a group's tagged code spelt on the wire like another code.

### The operation table

`operations`, in the types target, is every operation of the API as its router
mounts it, with what the SDL says of each. It is for code that decides something
per operation outside the generated handler — a gate reading `(method, path)`
off the request, an MCP tool or a job runner calling a service past its router —
so that none of it restates the SDL in a table of its own:

```rust
pub mod operations {
    pub enum Kind { Query, Mutation, Subscription }
    pub enum Auth { Session, Token, Public }

    pub struct Operation {
        pub id: &'static str,       // the operationId
        pub method: &'static str,   // "POST", or an `@http` operation's method
        pub path: &'static str,     // as mounted, `@version` prefix included
        pub kind: Kind,
        pub auth: Auth,             // `@auth(require:)`, inherited or not
        pub permission: Option<super::Permission>,  // with @permissions
        pub scope: Option<super::TokenScope>,       // with @scopes
    }

    pub const ALL: &[Operation] = &[ /* sorted by path, then method */ ];
    impl treat::rpc::route::RouteRow for Operation { /* method, path */ }
    pub fn by_route(method: &str, path: &str) -> Option<&'static Operation>;  // treat::rpc::route::by_route
    pub fn by_id(id: &str) -> Option<&'static Operation>;
}
```

A column exists only where the API declares its set — `role` with `@roles`,
`permission` with `@permissions`, `scope` with `@scopes` — so a column is never
a type the API does not have. A module of its own, so `Operation`, `Kind` and
`Auth` cannot collide with the API's types, which it names through `super::`.

Both lookups answer `None` for what the API lacks, so a caller refusing on
`None` fails closed: a misspelt id or a route nothing mounts never reads as an
operation that needs nothing. `by_route` matches the method as spelled: a
router may answer `HEAD` on a `GET` route — axum's `get()` does — but the table
has no `HEAD` row, so a gate refusing on `None` refuses it rather than reading it
as the `GET`. `path` is the request's (`/api/user/u1`) or the route's
(`/api/user/{user_id}`, as axum's `MatchedPath` gives it): a `{name}` segment
takes any one segment, and the route is picked as the router picks it, a fixed
segment before a parameter, before the method is matched. A path some row
spells exactly is a binary search over `ALL`; only one no row spells scans the
rows with a parameter. The search is `treat::rpc::route::by_route`, which trusts
the table's order; `treat::rpc::route::check_table(operations::ALL)` checks it,
once, in a test. `by_id` is a scan. The table checks nothing by itself: a caller that skips it runs
unchecked.

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
/// No size limit is applied to it: this method must bound it — see
/// [`treat::rpc::server::RawRequest`].
async fn upload(&self, input: UploadInput, body: RawRequest)
    -> Result<ApiResponse<UploadResult>, ApiError<FilesUploadCodes>>;
```

The input is read from the query string and validated exactly as for a
buffered request, and the service then gets the rest of the request as a
`RawRequest`: `headers()`, `extensions()` (`ConnectInfo`, when the router is
served with it), `content_length()`, `content_type()`, and the body through
`into_body()` or `into_reader()`, a `futures_util::io::AsyncRead` (re-exported
as `treat::rpc::futures_util`). The handler checks the `Content-Type` against
the `@raw(request:)` list before the service gets the request (`415`
otherwise), as it does for a buffered body.

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

## TanStack keys

Every key an operation builds starts with `[service, operation]`: its
`queryKey(input)` appends the input, its `mutationKey` is the pair alone.
Invalidating every query of one operation, whatever its input, matches on that
prefix, and `operationKeys` holds it per operation as a module-level constant —
the place that invalidates is often one with no client in reach:

```ts
import { operationKeys } from './api'

await queries.invalidateQueries({ queryKey: operationKeys.tags.list })
```

Spelled by hand, `['tags', 'list']` is an untyped literal that a renamed
operation silently stops matching; through `operationKeys` it is a compile error.

## Retargeting the Rust output

The preambles, router, handlers and client methods come from mustache
templates. The built-ins target `treat::rpc` + `axum` for the server and
`treat::rpc`'s `RpcClient` for the client, and are compiled into the binary, so
the default path needs no template files on disk. Point at your own to generate
for another stack:

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
| `[rust]` | `types_preamble` | `error_code_enum`, `reexports`, `registry` |
| `[rust]` | `preamble` (the server's) | `error_code_enum`, `types_import`, `reexports`, `operations`, `context_trait` |
| `[rust]` | `router` | `service`, `module`, `trait_name`, `routes` |
| `[rust]` | `handler` | `handler`, `trait_name`, `input`, `output`, `method`, `path`, `fields`, `spec`, `types`, `body_field`, `context`, `call_args`, `params`, `access_guard`, `read_input`, `read_body`, `respond` |
| `[rust_client]` | `preamble` | `error_code_enum`, `types_import`, `client_name`, `reexports` |
| `[rust_client]` | `method` | `method`, `input`, `output`, `path`, `operation_id`, `error`, `body_field`, `fields`, `spec`, `params`, `success`, `client_call`, `call_args` |

One template serves an operation with an input and one without: `input` is
empty for the second, and a variable doubles as a flag — `{{#input}}…{{/input}}`
renders when it is set, `{{^input}}…{{/input}}` when it is not. A section's name
is checked like any other variable. `handler` serves every form but a buffered
`@raw(request:)` (`handler_raw_request`, built in), and `method` every form but
a `@raw(request:)` (`client_method_raw_request`, built in); the variables say
which form it is (`fields`, `spec`, `types` are empty where the form has none).

A template writes plain text and need not lay anything out: the Rust targets
go through `rustfmt` after rendering (see [CLI](#cli)), so a signature may sit
on one line however long. What differs between the forms is computed for the
template as plain text, one statement per line: the handler's `params` (its
extractors, comma-separated, in the order axum takes them), `access_guard`,
`read_input` and `read_body` (statements, each line indented and ending in a
newline) and `respond` (the expression answering with `result`); the client
method's `params` (`&self` first), `success` (what its `ClientResult` holds),
`client_call` (the `RpcClient` method) and `call_args`; the `reexports` lines,
which leave out a name the API declares a type of; the router's `routes`, one
`.route(..)` per line. The built-in templates name every other item of `axum`
and `treat` by its absolute path, as a project's own should, so that no SDL
type can shadow it.

Anything in `template_vars` is added to every template in its section.

Two mustache behaviours matter when generating code, and `graphql-rpcgen` guards
both:

- `{{x}}` HTML-escapes, so `Vec<Option<T>>` would emit as
  `Vec&lt;Option&lt;T&gt;&gt;`. Use the triple-stache `{{{x}}}`; the escaping
  form is rejected with a message naming the fix.
- An unknown key renders as the empty string, so `{{{handlr}}}` would silently
  emit `async fn <S: …>`. Every referenced variable is checked against the
  declared set, so a typo fails generation instead.

**Upgrading from 0.15.** The template set follows the runtime's move into
`treat::rpc`: a handler is a few `Rt::…` calls and a client method one
`RpcClient` call, and the variables now spell those calls' arguments rather
than code a template laid out. From 0.15.0's variables to these:

- `handler_no_input_template` and `method_no_input_template` are gone, and a
  config naming one is refused: the one template is rendered with `input`
  empty.
- `handler` — 0.15: `handler`, `trait_name`, `input`, `output`, `method`,
  `path`, `query_fields`, `body_field`, `context_extractor`, `context_arg`,
  `access_guard`. The first six, `body_field` and `access_guard` remain.
  `query_fields`, a `[QueryField; N]` literal, became `fields`, the name of
  the operation's `wire` static. `context_extractor` and `context_arg` became
  `context` (set for a session-guarded operation), `params` (every extractor,
  comma-separated) and `call_args` (what the service call takes). New:
  `spec`, `types`, `read_input`, `read_body` and `respond`. The per-form
  built-ins — `handler_subscription`, `handler_raw_response`,
  `handler_raw_request_stream` and the rest — are this one template now,
  except a buffered `@raw(request:)`'s, which stays built in.
- `preamble` — 0.15: `error_code_enum`, `invalid_body_code`, `types_import`.
  `invalid_body_code` is gone: the registry implements
  `treat::rpc::RuntimeCodes`, which is where the runtime finds the code. New:
  `reexports`, `operations`, `context_trait`. `preamble_template` replaces the
  server preamble only, in both layouts; the types preamble is
  `types_preamble_template` (`error_code_enum`, `reexports`, `registry`), and
  the single-file layout renders both.
- `router` — unchanged: `service`, `module`, `trait_name`, `routes`.
- Client `preamble` — 0.15: `error_code_enum`, `types_import`, `client_name`;
  `reexports` is new. Client `method` — 0.15: `method`, `input`, `output`,
  `path`, `operation_id`, `error`, `body_field`; `fields`, `spec`, `params`,
  `success`, `client_call` and `call_args` are new. It serves every form but
  a raw request, whose template stays built in.
- The runtime 0.15's templates sat beside — the built-in `rpc_runtime`,
  `context_runtime`, `stream_runtime` and `client_runtime` — is
  `treat::rpc`; a template names its items, and `axum`'s, by their absolute
  paths.

## Tests

```bash
just test                  # whole suite
just test-one registry     # by test name
just lint                  # cargo deny, fmt-check, clippy, cargo sort, machete
```

343 tests. None needs a network; `ts_check`, the two `#[ignore]`d, needs the
TypeScript compiler `just check-ts` installs from npm (and `node`) and runs
only with `-- --ignored`:

- **semantic** (143) — every validation rule, the error-code registry (and that
  an API with operations declares one), error groups (their codes flattened
  into the group, the registry and each operation, and every rejection),
  path versioning, union tags, which
  constraint may sit on which type, two services or operations whose generated
  names collide (refused where the second is declared), a type spelt like an
  item the generated Rust declares or a prelude name it writes (and one spelt
  like a runtime item, which is fine), the positions of the errors found over
  the IR, plus nullability/list conversion
- **reuse** (21) — the generator is project-agnostic: a default config
  generates, SDL scalars reach all three targets and can override a built-in,
  unmapped scalars fail, outputs are selectable, the types target carries no web
  framework, the server re-exports the types rather than duplicating them, the
  client emits a method per operation, custom templates replace the built-ins,
  and a typo or an escaping tag in a template fails generation
- **unit** (46) — config defaults and TOML rejection (a key removed after 0.15
  named with where its setting went, `[rust] format` on unless turned off,
  `[rust] edition` one `rustfmt` knows), the edition read off a package's or
  its workspace's `Cargo.toml`,
  the `rpcgen_version` requirement and every way it can fail, template
  variable and section validation (the implicit iterator `{{{.}}}` allowed),
  identifier casing (`gen` escaped as a 2024 keyword), and the TypeScript
  runtime naming its placeholder once
- **auth** (28) — `@auth` on the trait signature, the handler's extractors, the
  role guard ahead of the body, the client's per-operation `role` field, the
  `@roles` set, its `@inherits` closure on both sides and its column of the
  operation table, and the rejections
  that keep a declaration from being decorative
- **permissions** (18) — `@permissions` and `@auth(permission:)`: the awaited
  guard ahead of the request in every handler form, its inheritance, the context trait's check, the set's Rust and
  TypeScript halves and its column of the operation table, `x-permission` / `x-role` in
  OpenAPI, a misspelt `@auth` argument refused, `unguarded`, `explicit`, one set per API,
  and every other rejection
- **scopes** (15) — `@scopes` and `scope:`: each operation's resolved scope, an
  optional scope beside a permission, a raw-request query stating its own,
  the operation table's rows, columns and lookups, `x-scope`, the TypeScript
  set and entries, a mutation that forgets its scope failing the build, and
  every other rejection
- **mcp** (6) — `@mcp` inheritance, the `x-mcp` flag in the OpenAPI document,
  and the rejections of shapes a tool call cannot honour
- **streaming** (10) — `@subscription` and `@raw` on both sides: the signature,
  the route verb, and where the input is read from
- **snapshots** (11) — Rust, TypeScript (its TanStack keys included), zod and OpenAPI output, determinism, the
  TypeScript field tables against the Rust server's `wire` tables, and
  a pass over the whole OpenAPI document for the breaks a per-path assertion
  cannot see: a dangling `$ref`, a duplicated `operationId`, an operation with
  no response
- **golden** (2) — one test runs every fixture under `tests/fixtures/` (eight,
  `mcp_version` with its OpenAPI metadata among them) through every target:
  Rust types, server and client in the split layout the wire tests compile,
  the server once more as a single file with its types inline, TypeScript, zod
  and OpenAPI. Each output is kept whole in `tests/snapshots/`, so any change
  to what is generated shows as a diff; the Rust is the plain output
  `generate_all` returns, before `rustfmt`, so the files depend on the
  generator alone. The other checks that
  `tests/common/fixtures.rs` lists exactly the SDL files in that directory,
  each with its own file's text. `INSTA_UPDATE=always cargo test --test golden`
  rewrites the snapshots when the change is meant; `INSTA_FORCE_PASS=1` lists
  every mismatching file instead of stopping at the first
- **cli** (26) — the binary itself: `generate` writes what `[output]` names and
  rewrites nothing unchanged, `check` exits non-zero on drift or a missing file
  and names it without repairing it, `--root` is what output paths resolve
  against, `--version` prints the package version, and a bad command or a
  `--config` that does not exist fails instead of falling back; and the
  formatting step: `generate` writes each Rust target as `rustfmt` formats the
  plain output, `check` compares the formatted files (the plain output in
  their place is stale), `[rust] format = false` writes the plain output with
  no `rustfmt` at all, a missing `$RUSTFMT` fails naming the target and the
  setting, a rustup toolchain without the component is named as such, a
  project `rustfmt.toml` (`hard_tabs`, `max_width`) applies, a 2024 package
  (its own edition or its workspace's) gets 2024-style files that pass
  `rustfmt --edition 2024 --check` and fail the 2021 one, `[rust] edition`
  overrides the package's, and a file with `\r\n` line endings is up to date
- **oneof_wire** (1) — the generated `@oneOf` types compiled and run in a
  scratch cargo project, pinning the wire shape and the arm constraints at
  runtime rather than in emitted text, every enum's `ALL`, `from_name` and
  `as_str` against what serde writes, and a failure narrowed into an
  operation's set
- **raw_wire** (1) — all three Rust targets compiled for an SDL reaching every
  `@raw` form, a `@subscription`, a role reached through `@inherits`, a
  `@variant(tag:)` error code, a scope set and the operation table's lookups,
  and for a second API a permission held and one refused in every handler
  form, which is the only thing that
  checks the emitted handlers and client methods against rustc rather than
  against a `contains`; the router is then served on a local port, the
  generated client calls every form through it (the plain RPC ones through a
  client of the second API), a streamed body larger than `RAW_BODY_LIMIT` is
  sent, a body of an undeclared type is refused `415`, and the tagged code is
  checked to reach the envelope by its tag
- **http_wire** (1) — the `@http` server and client built from one SDL and
  served on a local port: hand-written requests pin the format (methods, path
  parameters, a query string, a form body, a bare JSON answer, the `400`,
  `401`, `403`, `413` and `500` a failure carries) and the generated client's
  pin that the two sides agree
- **fixture_builds** (5) — the Rust of the fixtures no wire test builds
  (`snapshots`, `snapshots_http`, `mcp_version`): types, server and client in
  one scratch crate each, held to `rustfmt --check` and `cargo clippy -D
  warnings`, so what `golden` pins as text is also what rustc accepts; and an
  API whose types are spelt like the runtime's items (`Request`, `State`,
  `Ctx`, `Refusal`, `RawBody`, `RawRequest`, `EventStream`, `RpcClient`,
  `NoInput`, `ClientError`, `ApiErrors`, `Router`, `Response`, `Serialize`,
  `WireEnum`, `Validate`, …, `tests/sdl/runtime_names.graphql`), built in both
  layouts, once in a 2021 crate and once in a 2024 one, formatted for that
  edition and with a field, an input field and an operation named `gen`.
  Nothing runs; the five share `raw_wire`'s target directory and
  dependency set, so each compiles only its own crate
- **error_groups** (2) — the `error_groups` fixture's types and server
  compiled with `treat`'s `tracked` feature (so on nightly) beside a service
  written over its groups, and run: a code raised with `.wrap_api_code` into a
  group's failure with its `@message` and its cause, an included group's
  failure converted keeping its message, status, `meta`, cause, raise site and
  hops, a group's failure into the flat `Failure` and narrowed back, and the
  service trait's edge taking a group's failure with `?`; and an edge whose
  operation does not cover the group, which must fail to build. The two
  share a target directory of their own, since `tracked` changes what `treat`
  compiles to
- **harness** (5) — the scratch-project setup the wire tests share, on
  synthetic manifests and lockfiles: every dependency asks for the version the
  lockfile carries for it, whatever its source; a crate the lockfile lacks, or
  carries at two versions, is an error; the root manifest's `[patch.*]` tables
  are copied, a relative `path` made absolute against the crate root and a git
  source kept as written; and a root without `[patch]` writes none
- **ts_check** (2, ignored) — the generated TypeScript and zod for every
  fixture, and for `tests/sdl/runtime_names.graphql` with its types spelt like
  the globals the client runtime uses (`Response`, `Headers`, `Promise`,
  `Record`, …), through `tsc --strict`; and the generated client run under node
  against a recording `fetch` (`tests/ts/client_runtime.test.ts`): each field
  kind in a query string, a form body and a path, the `Content-Type` each body
  goes with whatever the default headers hold, and a refusal told from a raw
  answer

`oneof_wire`, `raw_wire`, `http_wire`, `fixture_builds` and `error_groups` build generated code
against the versions in this repo's `Cargo.lock`, offline: the lockfile is copied into each
scratch project, and this crate's `[patch.*]` tables into its manifest. Before
its tests run, each project writes the generated files through the generator's
formatting step, as the CLI does, and is held to `rustfmt --check` on them and
`cargo clippy --lib -- -D warnings`, so the output is formatted and
warning-free as written; each also checks its operation table with
`treat::rpc::route::check_table`. Those
crates (`treat`, `axum`, `reqwest`, …) are dev-dependencies here
for exactly that reason: the generator itself needs none of them, but the
lockfile has to carry them for the tests to resolve a version without reaching
the network.

That is also what keeps the dependency table above honest. A crate or a feature
the emitted code reaches for is easy to leave undocumented, because nothing
notices until a consumer writes their first `@raw` operation — `reqwest`'s
`stream` feature was exactly that, before the runtime moved into `treat`. Each
scratch project lists the crates and `treat` features the table names for what
it compiles, so dropping one fails the test with the error the consumer would
have seen.

## Out of scope

No GraphQL runtime or resolvers. No `utoipa` or Rust-to-OpenAPI inference —
OpenAPI comes from the IR. No hand-written OpenAPI. No Node in the compile path.
No executable decorators or plugin system. No websockets, no schema registry, no
compatibility checker, no binary protocols, no SDL imports. `@http` binds one
operation to a route fixed elsewhere; it is not a REST resource model — no
content negotiation, no response headers or cookies, no status per success.

## License

MIT — see [LICENSE](LICENSE).
