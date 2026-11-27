# Changelog

Each release says what changed and why. The README's "Upgrading from …" notes
hold the exact migration steps; this file holds the motivation. Releases before
0.10.1 are in the git history.

## Unreleased

### Added
- **Error groups: `@errorGroup(includes:)`, `@throws(groups:)`,
  `@message(text:)`.** For a service moving to layered errors: each area keeps
  a small table of its own codes and a failure type, the code of that area
  names its code where the failure happens, and the edge converts once — with
  the names written once, in the SDL, and a mismatch failing the build rather
  than answering `internal_error`.
  - A group's codes are its own values, every included group's (transitively)
    and `internal_error`, its `#[undeclared]` value. Its own codes join
    `ErrorCode`, which stays the one list of everything the API answers; a
    code lives in `ErrorCode` or in exactly one group, so codes move into
    groups one at a time.
  - Rust, in the types target: per group an `<Area>Code` enum with the
    registry's derives and an `<Area>Failure(pub ApiError<<Area>Code>)`, an
    `erris::tracked::TrackedError`, built from `.wrap_api_code(<Area>Code::x)?`
    or a code; `From` from every included group's code and failure, keeping
    the message, `meta`, status, source, raise site and hops; `From` into
    `ErrorCode` and the flat `ApiError<ErrorCode>`; and `From<<Area>Failure>`
    for the `ApiError` of every operation whose codes hold the group's, so
    the trait method's `?` compiles exactly where `@throws` covers the group.
  - `@throws(groups:)` adds a group's codes to the operation's, beside or
    instead of `codes:`; TypeScript, zod and OpenAPI carry each group as they
    carry an enum, and its codes in `ErrorCode` and in each operation's
    failure enum.
  - `@message(text:)` on a code of `ErrorCode` or of a group sets the message
    it carries; a GraphQL directive argument is named, hence `text:`.
  - Refused, each at its declaration: a code declared twice, a runtime code
    in a group, an include that is not a group or closes a cycle, a group not
    named `<Area>Code` or marked as an access set, `@errorGroup` without or on
    `ErrorCode`, a type spelt like `<Area>Failure`, `@throws(groups:)` naming a
    non-group, and `@message` off an error code or without a string.
- An API declaring a group needs `treat`'s `tracked` feature where its types
  compile, and so a nightly toolchain. A `?` from one group's
  `erris::Result` into another's waits for erris to convert between tracked
  errors through `From`; the conversions are generated for it.

## 0.16.0 — 2026-11-25

Written for a project upgrading from 0.15.0. When you regenerate with this
release, raise `rpcgen_version` in `rpcgen.toml` to it, so a teammate or a CI
job still on 0.15 refuses the settings instead of regenerating 0.15's output
over yours (see Migration).

### Added
- **`@http`: an operation on the method and path its clients already call.**
  For a service replaced behind clients that cannot change with it.
  - `@http(method:, path:, body:, envelope:)` mounts an operation on `GET`,
    `POST`, `PUT`, `PATCH`, `DELETE`, or `ANY` for an endpoint that never
    checked its method. A `{name}` path segment is filled from the required
    scalar or enum input field of that name, which must have a text form — a
    `JSON` field, read as JSON text, has no segment spelling and is refused.
    The other fields travel in the query string (`body: NONE`, the default for
    `GET`, `DELETE` and `ANY`), as a JSON body (`JSON`, the default for
    `POST`, `PUT` and `PATCH`) or as an `application/x-www-form-urlencoded`
    body (any case) read over the query string (`FORM`). `envelope: false`
    answers a success with the bare value.
  - Failures keep the envelope, under a status such a caller reads: `400` for
    an input the runtime cannot read or one breaking a constraint (a body it
    cannot read at all carries that refusal's status, `413` past the body
    limit), `401` for a refused session, `403` for a denied role or
    permission, and, with `envelope: false`, `500` for a service failure left
    at any `2xx` status — a bare success is told from a failure by its status
    alone.
  - Routes: two operations may share a path under different methods, `ANY`
    shares it with none, and two paths naming a parameter in one position
    differently are refused, since the router would panic on them. A path
    with a `.`, `..` or empty segment (but a trailing slash) is refused, since
    no client reaches it as mounted. A service's `@version` does not prefix an
    `@http` path — it is the URL clients already call — and `@version` on the
    operation is refused. `@http` does not combine with `@rpc`, `@raw`,
    `@subscription` or `@mcp`, and a quoted `method:` or an unquoted `path:`
    is named as such.
  - The operation table holds the binding: `operations::ALL` is sorted by path
    and then method, and `by_route` takes the request's path as well as the
    route's, matching a `{name}` segment against any one segment and
    preferring a fixed segment as the router does, so a gate reading
    `request.uri().path()` finds an operation with path parameters; an `ANY`
    row answers every method.
  - Every target follows. The Rust server and client share the operation's
    `wire::<OP>_HTTP` spec (`HttpSpec`); the client refuses a path parameter
    that is missing, `null`, empty, `.` or `..` before sending
    (`ClientError::Encode`), since URL parsing would carry the call to another
    route. The TypeScript client has `RpcClient.http` and `RpcHttpSpec`, and
    throws a `TypeError` for such a parameter. The zod `inputSchemas` key an
    `@http` operation by `METHOD path`, since two may share a path. OpenAPI
    describes path and query parameters, form bodies, an enveloped
    operation's runtime failures under `default`, a value read from a URL or
    a form as JSON text as a string holding JSON (`contentMediaType`), and
    an `ANY` operation under the method clients send it with, marked
    `x-rpc-any-method`.
  - `tests/http_wire.rs` serves the generated router and calls it with the
    generated client and by hand.
- **`@explicitNulls` on an output type** writes an absent optional value as
  `null` instead of leaving the key out, for readers of an existing format
  that expect every key.
- **`ApiClient::rpc()` and `into_inner()`**: the `treat::rpc::client::RpcClient`
  under the generated client, for a call no typed method makes — the generic
  `call`, `subscribe`, `raw`, `raw_request` and `raw_request_json` 0.15's
  client carried itself.
- **`[rust] edition`**, the edition the Rust targets are formatted for. By
  default it is the edition of the package each file is written into (see
  the `rustfmt` bullet under Changed).

### Changed
- **Breaking: the Rust runtime is `treat::rpc`.** The generated server and
  client no longer carry a pasted copy of the schema-independent code —
  reading an input from a body, a query string or a path, refusing a request,
  answering with the envelope, bytes or Server-Sent Events, sending a call and
  reading its answer, the SSE reader, `Narrow`. They call `treat::rpc`
  (treat 0.27) instead: a handler is a few `Rt::…` calls
  (`type Rt = ::treat::rpc::Runtime<ErrorCode>;`), a client method one
  `RpcClient` call. The schema stays generated: every enum derives
  `treat::rpc::WireEnum`, the registry implements `treat::rpc::RuntimeCodes`,
  each code set derives `ApiErrorCode` and `WireEnum` (`#[skip(Display)]`, a
  `#[code]` per variant, `#[undeclared]` on `InternalError`), and the types
  target gains `wire`: the `static`s both ends read an operation off —
  `<OP>_FIELDS` (a subscription's or a raw request's query table, built with
  `QueryField::str("id")`, `QueryField::int("n").list()`, …), `<OP>_HTTP` (an
  `@http` operation's `HttpSpec`), `<OP>_TYPES` (a raw request's media
  types).
- **Breaking: the Rust targets go through `rustfmt` when they are generated.**
  The emitters write plain code — one statement per line, a signature or a
  derive list on one line — and `generate` and `check` pass each Rust target
  through `rustfmt` (`$RUSTFMT`, else `rustfmt` on `PATH`), run from the
  directory the file is written to, so the project's `rustfmt.toml` applies as
  it does to `cargo fmt`. `--edition` is what `cargo fmt` passes for that
  file: `[rust] edition` when set, else the `package.edition` of the nearest
  `Cargo.toml` above it (or `workspace.package.edition` when the package
  inherits it), else 2021. `rustfmt` derives its style edition from it, so a
  2024 crate's files come out in the 2024 style its own `cargo fmt --check`
  keeps. The generator used to lay the code out itself by restating
  `rustfmt`'s width rules, which held for its default settings and few name
  lengths: a sweep found 2614 of 2880 combinations of name lengths failing
  `rustfmt --check`, and any project `rustfmt.toml` failed it too. `rustfmt`
  is therefore needed at generation time; a missing one fails the run naming
  the target and `[rust] format = false`, which writes the plain output, and
  a rustup toolchain without the `rustfmt` component is named as such rather
  than as a configuration error. Output is byte-identical for the same SDL,
  generator version and `rustfmt` (version and configuration). For a library
  caller `generate_all` stays pure and returns the plain output, and
  `format_outputs(&mut files, root, &config)` is the formatting step;
  `GeneratedFile` gains `target`, and `format::rustfmt` takes the edition.
- **Breaking: an RPC body must be declared JSON.** `Content-Type:
  application/json` (or a `+json` type) is required, else `415` with
  `invalid_body` before the body is read: a browser posts a form or plain text
  across origins without a CORS preflight, so a server reading JSON under any
  type ran such requests with the visitor's cookies. The generated clients
  send the header. Likewise a `@raw(request:)` body is read only under a type
  its list declares (`415`).
- **What else the runtime answers differently.**
  - A JSON body that does not parse answers with the registry's own
    `invalid_body` at `422`, with `source.pointer` at the offending field; one
    over the body limit answers with the envelope at `413` rather than axum's
    plain text.
  - A buffered `@raw(request:)` body over the limit answers `413` (was `200`
    with the envelope).
  - A value of the wrong type in a query string, a form or a path names the
    parameter in `source.parameter`.
  - Query strings and form bodies are decoded by the runtime itself, and a
    parameter that is not UTF-8 once decoded — escaped (`%E9`) or raw — is
    refused as `invalid_body` rather than altered: replacing the byte with
    U+FFFD handed the service a value nobody sent, a password among them.
    This applies to subscriptions and raw requests.
  - A `RawBody`'s headers are appended, so a repeated name is sent each time;
    `content-type`, `content-length` and `x-rpc-status` among them are not
    sent (the body's own type and length are).
  - With `treat`'s `rpc-status-header`, a raw, streamed or bare success
    carries `x-rpc-status: ok`, as a failure carries `error`.
- **The Rust client reads answers as the server writes them.** `raw()` on an
  operation answering `application/json` returns the response instead of
  failing on a success; `raw_request` tells a refusal sent at `200` from the
  bytes (by `x-rpc-status`, then by an `application/json` envelope with
  `errors[]`, read up to 64 KiB); `subscribe` opens a stream only on a `2xx`
  of type `text/event-stream` and reads any other answer as a refusal; a
  subscription's or a raw request's query field is written from the operation's
  `wire` table, a JSON-typed one as JSON text, and a `null` item in a list
  that is not JSON-typed is `ClientError::Encode` before sending.
- **TypeScript: the client writes what `treat::rpc` reads.**
  - A field in a query string or a form body is spelled from the operation's
    field table, which the spec now carries as `fields`
    (`RpcRawSpec.fields`, `RpcHttpSpec.fields`, and a fourth argument of
    `subscribe`) — `{ id: 'str', tags: 'str[]', filter: 'json' }`, the
    server's `wire` table in new exported types `RpcFieldTable` and
    `RpcQueryKind`, left out where every field is written as plain text
    anyway. A JSON-typed field is JSON text, a string quoted (was the bare
    string, which the server cannot parse); only a list repeats its key, and
    an array in any other field is one JSON value (was one key per item); a
    `null` item in a list that is not JSON-typed throws a `TypeError` (was
    sent as the text `null`).
  - A body's `Content-Type` is the client's, whatever `RpcClientOptions.headers`
    holds: exactly `application/json` for an RPC call and a
    `@raw(response:)` call, and the declared (or
    `RpcCallOptions.contentType`) type for a raw body. A default
    `Content-Type` used to replace the JSON one, and a lower-case
    `content-type` to be sent beside it — both now refused by the server at
    `415`. Headers are merged through `Headers`, case-insensitively.
  - `raw()` tells a refusal from a raw answer as the Rust client does: a
    non-`2xx` status, then `x-rpc-status` when present (`error` / `ok`), then
    only an answer whose media type is exactly `application/json` that is an
    envelope with a non-empty `errors[]`, read through `response.clone()` up to
    `RpcClient.RAW_PEEK_LIMIT` (64 KiB; a longer body, or a longer declared
    `Content-Length`, is not read). Any other JSON answer — the operation's
    own — is returned with its body intact; it used to be thrown as an
    `RpcError`, and so was any type merely containing `application/json`
    (`application/jsonl`).
  - The `RpcError` documentation lists the statuses the runtime now answers.
  - The client runtime names every global it uses as `globalThis.X`
    (`globalThis.Response`, `globalThis.Promise<…>`, `globalThis.Record<…>`),
    and so do the generated signatures: a type the API declares in `api.ts` —
    an SDL `type Response`, `Headers`, `Promise` or `Record` — took the
    global's place in every bare reference there, and `api.ts` failed
    `tsc`.
  - `createApi` is built on the `create<Service>Operations` objects instead of
    spelling each request again; the exported names, entry shapes, types and
    keys are unchanged. `roleAuthorizes` type-checks under `tsc --strict` for
    a `@roles` enum of one role, where comparing `held` first narrowed it to
    `never` before it indexed the inheritance table.
- **Enums.** `ALL`, `from_name` and `as_str`, which 0.15 emitted as inherent
  items, are the items of the `treat::rpc::WireEnum` trait the enums derive,
  re-exported by the types module. A consumer's own inherent `ALL`,
  `from_name` or `as_str` therefore no longer collides with them, as 0.15
  said it would: it compiles, and shadows the trait's item wherever both are
  in reach. The `@roles`, `@permissions` and `@scopes` enums' `from_name` and
  `as_str` are the trait's too.
- **SDL types spelt like runtime items compile.** The generated Rust imports no
  name beside the API's own types: it writes `::treat::rpc::server::Refusal`,
  `::axum::extract::Request`, `::serde::Serialize`, `::validator::Validate`
  and the rest by their absolute paths. An `input Request` used to be shadowed
  by the server's `use axum::extract::Request` and fail with `E0277`; types
  called `Ctx`, `Refusal`, `RawBody`, `RawRequest`, `EventStream`, `State`,
  `Response`, `Router`, `RpcClient`, `NoInput`, `ClientError`, `ApiErrors`,
  `Serialize`, `WireEnum`, `Validate`, … now build in both layouts. A
  re-export a type is spelt like (`pub use ::treat::rpc::server::RawBody` beside
  a `RawBody` of the API's) is left out, and the server no longer re-exports
  `Ctx`, which only its handlers name.
- **Breaking:** a type spelt like an item the generated Rust declares itself
  (`Rt`, `Failure`, `FromRequestContext`, `ApiClient`, the `operations` and
  `wire` modules, a service's `<S>Service` and `<S>Operations`, an
  operation's `<S><Op>Codes`) or like a prelude name it writes unqualified
  (`Option`, `Some`, `None`, `Result`, `Ok`, `Err`, `Vec`, `Box`, `String`,
  `Clone`, `Send`, `Sync`, `Sized`, `From`, `Into` — `From` also because
  `treat`'s `ApiErrorCode` derive and `error_set!` write it unqualified) is
  refused with what it collides with. It was a duplicate item, or silently
  the generator's item, in generated code.
- **Breaking:** an API with any operation must declare the `ErrorCode`
  registry, `@throws` or not, listing at least `invalid_body` and
  `internal_error`. It was demanded only beside `@throws`, and without it the
  generated server and client — typed over the registry — did not compile;
  the compiler now says so at the first operation. An SDL of types alone still
  needs none.
- **Breaking:** two operations, or two services, whose generated names are one
  name are refused where the second is declared, naming the first and what
  they share. `Users.getAll` and `UsersGet.all` were two routes but one handler
  `users_get_all`, one `wire::USERS_GET_ALL_*` and one `UsersGetAllCodes`, and
  rustc failed in generated code; `Users` and `USERS` were one Rust module and
  one `createApi` key, of which TypeScript kept the second without a word.
  A duplicate path and a duplicate `operationId` are likewise reported at the
  second operation.
- **Breaking (library API):** `compile`, `compile_str`, `generate_all`, every
  target's `generate`, `Config::from_toml_file`, `Template` and the version
  check return `erris::Result` rather than `Result<_, CompileError>`. A failure
  reading or parsing a file now keeps the I/O, TOML or SDL error as its cause
  instead of flattening it into a string. A report's `Display` is its outer
  message alone (`failed to parse api/a.graphql`); `error::describe` joins the
  whole chain into one line (`failed to parse api/a.graphql: Parse error at
  2:1 …`), which is what the CLI prints. `CompileError` remains the located
  diagnostic the semantic pass collects.
- `gen`, reserved by the 2024 edition, is escaped as `r#gen` like the other
  keywords, and an operation or a service named like a keyword gets a handler
  and a router function that are one identifier (`users_type`, not
  `users_r#type`).
- The checks the compiler runs over the IR report where the SDL declares what
  they refuse: an `@http` binding, a `@raw` body, a missing `@throws` code
  (`must list role_required`), an `@auth` value, a duplicate path, an input or
  output of the wrong kind at the operation; a binary field, an input/output
  mix-up, a registry code the runtime needs at the type.
- `check` reads a file that differs from the output only in its line endings
  as up to date, and `generate` leaves it alone: a checkout with
  `core.autocrlf` (Git for Windows' default) turned every file stale.
- The generated Rust carries no blanket
  `#![allow(clippy::all, dead_code, unused_imports)]`: re-exports and
  `@pattern` checks are emitted only where used, and a `@pattern` check binds
  its outcome before the `if` that reads it. The wire tests hold the generated
  files, formatted as the CLI writes them, to `rustfmt --check` and
  `cargo clippy -- -D warnings`, in a 2021 and a 2024 crate.
- The generated server indents the doc comment of the service trait's
  `type Ctx`; the Rust and TypeScript clients' comments no longer say every
  call is `POST` answered with `200`.
- Templates: a section's name is checked like a variable, and a variable is a
  flag for `{{#x}}` / `{{^x}}`, so one template serves an operation with and
  without an input. The implicit iterator `{{{.}}}` passes the check: it is
  the section's value, not a variable. A config naming
  `handler_no_input_template` or `method_no_input_template` is refused with
  what replaced it and a pointer to the README's migration note, instead of
  serde's unknown-field message, which read like a typo.
- The generator's own code denies panics and unchecked arithmetic
  (`indexing_slicing`, `arithmetic_side_effects`, `string_slice`,
  `panic_in_result_fn` and the rest of the set its sibling crates use).

### Migration
- **The version gate.** Set `rpcgen_version` to this release in the same
  change that regenerates with it. The gate refuses only a generator older
  than the requirement, so left at `0.15.0` it lets a teammate or a CI job
  still on 0.15 run `generate` and write 0.15's output — the pasted runtime,
  the old formatting — over yours, or run `check` and call it stale.
- **Dependencies.** `treat` 0.27 with its RPC features:
  `rpc-server` for `rust_server`, `rpc-client` for `rust_client`, `rpc-stream` with any
  `@subscription` or `@raw`, `validator` for a `@pattern`. Drop
  `futures-core`, `futures-util`, `bytes`, `regex` and the `json` and
  `stream` features you added to `reqwest` for the generated code, unless
  your own code uses them; `treat`'s `axum` and `validator-extract` features
  are no longer needed by generated code. `treat::rpc::futures_util` is
  there for reading a `RawRequest`.
- **TLS and `reqwest`'s defaults.** `treat` builds `reqwest` without its
  default features, so the generated client has no TLS, HTTP/2 or system
  proxy support of its own: an `https` base URL fails at the first request
  with `ClientError::Transport`. Keep your own `reqwest` dependency with a TLS
  feature — and `http2` or `system-proxy` if you relied on `reqwest`'s
  defaults — which Cargo unifies with `treat`'s, or enable `treat`'s
  `rpc-client-rustls`, or pass a configured client to `ApiClient::with_http`.
- **Context.** `impl FromRequestContext for MyContext { fn from_parts(..) ->
  Result<Self, Response> }` becomes `impl treat::rpc::server::ResolveContext for
  MyContext { async fn from_parts(..) -> Result<Self, Refusal> }` plus `impl
  FromRequestContext for MyContext`, which keeps only `require_role` /
  `require_permission`, returning `Result<(), Refusal>`. A refusal is
  `Err(error(ErrorCode::…).into())` or a ready `Response` `.into()`. `Refusal`
  and `ResolveContext` are re-exported from the server module.
- **`RawBody`** is no longer `IntoResponse`: a service returns it and the
  handler answers through `Rt::respond_raw`. `RawBody`, `RawRequest`,
  `EventStream`, `event_stream` and `RAW_BODY_LIMIT` are `treat`'s,
  re-exported where they were. A header set on a `RawBody` twice is now sent
  twice, and its `content-type`, `content-length` or `x-rpc-status` is not
  sent at all.
- **The Rust client.** `ApiClient` is a newtype over
  `treat::rpc::client::RpcClient` and no longer has the generic `call`,
  `subscribe`, `raw`, `raw_request` and `raw_request_json`: reach them as
  `api.rpc().call::<_, Output, Codes>("/path", &input)`. `call` and `raw`
  take the same arguments; `subscribe`, `raw_request` and
  `raw_request_json` take the operation's field table after the path
  (`&wire::<OP>_FIELDS`, or `&[]`), and an `@http` operation is
  `call_http(&wire::<OP>_HTTP, &input)`. `ClientError` is `treat`'s,
  `#[non_exhaustive]` (give a `match` a catch-all), and gains `Encode` for a
  failure before sending — a missing, `null` or dot-segment path parameter or
  a `null` list item among them, which was `Decode`. A `2xx` envelope without
  `errors[]` where bytes or a stream were expected is `Decode` (was
  `Status(200)`). `errors.code()` is `errors.code::<ErrorCode>()`. The client
  module re-exports `ClientError`, `ApiErrors`, `ClientResult`,
  `ApiResponse`, `ErrorMessage`, `ErrorSource`, `reqwest` and, with
  `rpc-stream`, `EventStream` and `StreamError`. Code that matched an error
  from `raw()` on a successful JSON answer, or treated a `2xx` that is not
  `text/event-stream` as an open subscription, now gets the response, or a
  refusal.
- **Enums.** `ALL`, `from_name` and `as_str` come from the `WireEnum` trait,
  which the types module re-exports: bring it into scope where they are
  called (a glob import of the types does). Delete an inherent `ALL`,
  `from_name` or `as_str` of your own unless it is meant to differ — it now
  shadows the trait's item instead of failing to compile. The per-set
  `TryFrom<ErrorCode>` and `Undeclared` impls and the generated `Narrow`,
  `Undeclared`, `Failure` and `UNDECLARED_CODE_META` are `treat::rpc`'s
  (`Failure` is `treat::rpc::Failure<ErrorCode>`). Error sets are `Copy`; a
  set a project composes from them in its own `error_set!` copies the
  variants' `#[code]` attributes and so must carry `#[skip(Display)]` and
  `#[derive(Clone, Copy, ApiErrorCode, WireEnum)]` too.
- **Callers of an RPC route** that are not a generated client must send
  `Content-Type: application/json` (`415` otherwise), and a raw request one of
  its listed types; an unparseable body is refused with `invalid_body` at
  `422`. A test or a probe posting without the header is the usual casualty.
- **Behaviour your code may lean on.** A buffered raw body over
  `RAW_BODY_LIMIT` is `413`, no longer `200`; a parameter that is not UTF-8
  is refused rather than replaced with U+FFFD; with `rpc-status-header` every
  success carries `x-rpc-status: ok`. See "What else the runtime answers
  differently" above.
- **TypeScript client.** Regenerate `api.ts`; generated calls need no change.
  A default `Content-Type` in `RpcClientOptions.headers` no longer reaches a
  request with a body — drop it. A `@raw(response:)` operation answering JSON
  now resolves with that `Response` instead of throwing; a refusal is still
  thrown. Code building `RpcRawSpec` by hand, or calling `subscribe`
  directly, passes `fields` for a list or a JSON-typed field — without a
  table every field is written as plain text, and an array as one JSON value
  rather than a repeated key. A list holding `null` now throws a `TypeError`
  before sending.
- **Formatting.** `generate` and `check` now need `rustfmt` (`rustup component
  add rustfmt`, or `$RUSTFMT`). Without one, set `[rust] format = false`; a
  project that checks formatting then excludes the files, e.g. with
  `#[rustfmt::skip] mod api;`. The project's `rustfmt.toml` and its crate's
  edition now apply to the generated files, so the first `generate` may
  rewrite them; keep a `rustfmt.toml` in the project, empty if need be, so a
  developer's own `~/.config/rustfmt/rustfmt.toml` is not what formats them.
  A file written outside the crate that compiles it names its edition with
  `[rust] edition`.
- **Names.** An SDL type spelt like one of the generated items or prelude
  names listed above must be renamed. Code that named `Ctx` through the server
  module names `treat::rpc::server::Ctx`; a re-export left out for a type of
  the same name is reached by its `treat` path.
- **Templates.** A project overriding a built-in template rewrites it against
  the new variables — the full list is in "Upgrading from 0.15" under
  Retargeting in the README:
  - `handler_no_input_template` and `method_no_input_template` are refused:
    the one template renders with `input` empty.
  - `handler`: of 0.15's `handler`, `trait_name`, `input`, `output`, `method`,
    `path`, `query_fields`, `body_field`, `context_extractor`, `context_arg`
    and `access_guard`, the first six, `body_field` and `access_guard` remain.
    `query_fields` (a `[QueryField; N]` literal) became `fields` (the name of
    a `wire` static); `context_extractor` and `context_arg` became `context`
    (a flag), `params` (every extractor, comma-separated) and `call_args`
    (what the service call takes). New: `spec`, `types`, `read_input`,
    `read_body` and `respond` — a handler is `Rt::…` calls, which these
    spell.
  - `preamble`: 0.15's `error_code_enum`, `invalid_body_code` and
    `types_import`. `invalid_body_code` is gone — the registry implements
    `treat::rpc::RuntimeCodes`, which is where the runtime finds the code —
    and `reexports`, `operations` and `context_trait` are new. It replaces
    the server preamble only, in both layouts; the types preamble is
    `types_preamble_template` (`error_code_enum`, `reexports`, `registry`),
    and the single-file layout renders both.
  - `router`: unchanged (`service`, `module`, `trait_name`, `routes`).
  - Client `preamble`: 0.15's `error_code_enum`, `types_import` and
    `client_name`, plus `reexports`. Client `method`: 0.15's `method`,
    `input`, `output`, `path`, `operation_id`, `error` and `body_field`, plus
    `fields`, `spec`, `params`, `success`, `client_call` and `call_args`; it
    serves every form but a raw request, whose template stays built in.
  - The runtime the templates' output sat beside — 0.15's built-in
    `rpc_runtime`, `context_runtime`, `stream_runtime` and `client_runtime` —
    is `treat::rpc`, and a template names its items by their absolute paths.

### Why
The runtime was the same few hundred lines in every generated project, fixed
by regenerating every project. In `treat` it is one crate, tested once —
client against server over a socket — and versioned on its own, and what is
left in the generated files is the schema. Formatting is `rustfmt`'s at
generation time for the same reason: restating its layout rules in the emitter
was a second copy of `rustfmt`, which drifted from the first with every name
length, every project setting and every edition.

`@http` exists for replacing a service behind clients that cannot move with it:
the first consumer is a single sign-on service whose hosts switch one at a time
by configuration, so the new service must answer the old URLs, methods, form
bodies and bare JSON byte for byte while its API is still generated from the
schema.

## 0.15.0 — 2026-11-24

### Added
- **Every Rust enum lists and spells itself.** Each SDL enum, the `ErrorCode`
  registry included, gets `pub const ALL: &[Self]` (every value, in SDL
  order), `from_name` and `as_str` over its wire names, and derives `Hash`.

### Changed
- **Breaking:** a consumer's own inherent `ALL`, `from_name` or `as_str` on a
  generated enum, or its own `impl Hash` for one, now collides with the
  generated item and must be deleted.
- The `@roles`, `@permissions` and `@scopes` enums get `from_name` and
  `as_str` from the general rule; only `Role::authorizes` is still emitted
  for one kind of enum alone.
- An `ErrorCode` value named `ALL`, `from_name` or `as_str` is refused.

### Why
A project that stores an enum as text, filters a query by it or lists its
choices on a screen needs the set and the spellings. Only the access enums
had `as_str`, and no enum had its set, so consumers wrote both by hand beside
the generated type. In the first consumer that meant arrays of variants for
filters and validation, string constants repeating an enum's wire names, and
hand-written `as_str` and `parse` impls for the enums it stores. Each of
these drifts from the SDL silently: a new value is missing from the array,
and an edited `@variant(tag:)` leaves the old string in the constant.

- **One rule for every enum.** `from_name` and `as_str` already existed for
  the access enums, emitted per kind with the same body. Every other enum
  needed the same two methods for the same reason, so the per-kind impls
  became one rule over all of them.
- **`as_str` is the serde spelling.** It is the one string the wire, a
  stored row and a log line can agree on, the tag included. A second
  vocabulary is the consumer's to name.
- **The registry is an enum like the others.** Its codes are what a client
  branches on and a log records, so the same reasons hold. `as_str` equals
  its `Display`.
- **A colliding registry code is refused.** Registry variants keep their SDL
  spelling, so a code named `ALL` would shadow the constant; rustc only
  warns about that, and the code would silently mean the variant.
- **`Hash`** because a set of values is the natural way to check membership
  against `ALL`; every enum is a fieldless `Copy` type, so it costs nothing.
- **No `Display` or `FromStr`.** The generator has no pattern for either on
  an ordinary enum, and a consumer may already implement them.

## 0.14.0 — 2026-11-21

### Added
- **The operation table.** `pub mod operations` in the Rust types target lists
  every operation exactly as the router mounts it: operationId, HTTP method,
  path with its version prefix, kind, `@auth` requirement, and the role,
  permission and scope columns the API declares sets for. It is sorted by path
  and has `by_route` and `by_id` lookups.
- **Token scopes declared in the SDL.** `@scopes(query:)` marks the enum of
  scopes a delegated credential (an API token, a sync key, an agent's
  connection) carries. `scope:` on `@query`, `@mutation` or `@subscription`
  names an operation's scope. Queries and subscriptions default to `query:`.
- **The scope in every target**: `x-scope` in OpenAPI, `scope` on each
  `createApi` entry and `scopes` in TypeScript, and the table's `scope` column.
- **`operationKeys.<service>.<op>`** in TypeScript: each operation's TanStack
  query-key prefix, the same pair `queryKey` and `mutationKey` use.

### Changed
- **Breaking:** `Permission::OPERATIONS` and `Role::OPERATIONS` are removed. The
  table's `permission` and `role` columns hold the same facts.
- The router and OpenAPI read the HTTP method from `Operation::http_method`.
- Scope errors are reported at the operation's declaration (`file:line:col`).

### Why
A role or a permission says what the *user* may do. A credential acting for a
user is usually narrowed further: a token that may read must not delete,
whatever its owner may. Which scope an operation needs is as much a fact of
the API as its path, but it had no place in the SDL. Consumers therefore
restated it in hand-written tables keyed by `(method, path)`, plus tests that
re-parsed `openapi.json` to keep those tables honest. In the first consumer,
297 of 307 rows of such a table were derivable from the SDL. The only fact
missing was which mutations remove data.

- **The table carries every column of one operation in one place.**
  `Permission::OPERATIONS` held a single column of the same fact under a second
  shape, which left each consumer to choose between two sources.
- **Forgetting a scope fails the build.** A mutation a credential can reach has
  no default scope, so a new mutation cannot fall silently into a weaker one.
  A hand-written table that refuses unlisted routes has the same fail-closed
  property; this keeps it.
- **A raw-body query also states its scope.** A `@query` with
  `@raw(request:)` takes bytes in, so it gets no read default either.
- **The vocabulary is the API's own enum**, as `@permissions` already is.
  Scopes are part of the contract and reach every target as a type, so they
  are not a setting in `rpcgen.toml`.
- **The scope goes on the kind directive, not on `@auth`.** Restating `@auth`
  replaces the service's whole rule, so a scope there would have dropped
  inherited checks.
- **Next to a permission, a scope is optional.** An API whose permission check
  already judges the credential needs none. One whose check sees only the user
  can still narrow tokens there. The README states the implementation's duty
  for the first case.
- **`HEAD` has no row**, so a gate built on `by_route` refuses it.
- **The method is spelled in one place.** It was spelled in three, so the
  table, the router and OpenAPI could drift apart.
- **`operationKeys` replaces untyped key literals** (`['files', 'list']`) that
  clients wrote to invalidate a query. A renamed operation now breaks the
  build instead of silently leaving a cache stale.

### Maintenance
- `deny.toml` ignores RUSTSEC-2026-0543 (`chrono` is unmaintained, with no
  known flaw). Only the wire tests link chrono, through the `DateTime` scalar
  of the code they compile. The generator itself does not.

## 0.13.0 — 2026-11-10

### Added
- **Permissions declared in the SDL**: a `@permissions(codes:)` enum, and
  `@auth(permission:)` checked per operation.
- **`explicit: true` services.** Each operation of such a service states its
  own `@auth`; an operation that states none fails the build.
- **`unguarded: true`**, the only way to drop a service's check for one
  operation.
- **Each operation's role or permission listed on the set**, by operationId.

### Changed
- An API declares a role set or a permission set, not both.
- The access check runs before the body is read, in every handler form.
- A handler without the check its API demands is refused at generation.
- An argument a directive does not read is refused rather than ignored.

### Why
- **Roles alone could not express admin panels with per-area rights** (users,
  storages, security…). Those rights are data the server keeps, not a
  hierarchy, so the SDL names the permission and the server decides who holds
  it.
- **Checking before the body is read.** A JSON handler had parsed the body
  first, so a caller without access who sent a malformed body got
  `invalid_body`, while the raw and subscription forms answered with the
  refusal. Now every form refuses first and parses nothing for a refused
  caller.
- **The other changes stop a check being lost by accident.** Without
  `explicit`, a new operation could inherit a read permission for a write.
  Without `unguarded`, a check could vanish when `@auth` was restated.
  Without refusing unread arguments, a typo in a directive was ignored.

## 0.12.2 — 2026-11-10

### Fixed
- The Rust server sends tagged error codes by their wire name.
- A namespaced code's default message is named by its last segment.

### Why
A code declared as `ns__name @variant(tag: "ns::name")` reached the envelope as
`ns__name`, while the TypeScript client expected `ns::name`. The default
message of `portal_db__clan_not_found` read "portal db  clan not found", with
a double space.

## 0.12.1 — 2026-11-09

### Added
- `-V` / `--version` prints the generator version.

### Why
A project pins the version its generated code was produced with
(`rpcgen_version`). The binary had to be able to say which one it is.

## 0.12.0 — 2026-11-09

### Added
- **Roles and their hierarchy declared in the SDL**: a `@roles` enum, with a
  bare `role:` on `@auth`.
- **Each operation's role on its `createApi` entry.**

### Changed
- `operationRoles` is removed: read the role from the `createApi` entry and the
  list from `roles`.

### Why
Access control follows ANSI INCITS 359-2012 RBAC (Core plus General
Hierarchical). The role set and its `@inherits` hierarchy are declared once and
closed over by the compiler. A quoted, unknown or set-less role, or a cycle, is
a build error instead of a string checked at run time.

## 0.11.0 — 2026-11-06

### Added
- `operationRoles` in the TypeScript client: each operation's required role,
  inherited roles included.

### Why
A UI that says who may do what reads the declaration instead of restating it.

## 0.10.1 — 2026-10-08

### Added
- `@raw(stream: true)` hands the raw request body to the service unread.

### Why
A service that consumes the body as a stream (an upload) gets it unread
instead of buffered by the handler first.
