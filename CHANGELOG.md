# Changelog

Each release says what changed and why. The README's "Upgrading from …" notes
hold the exact migration steps; this file holds the motivation. Releases before
0.10.1 are in the git history.

## Unreleased

### Added
- **`@http`: an operation on the method and path its clients already call.**
  `@http(method:, path:, body:, envelope:)` mounts an operation on `GET`,
  `POST`, `PUT`, `PATCH`, `DELETE` or `ANY`, fills `{name}` path segments from
  input fields, reads the other fields from the query string, a JSON body or a
  form body over the query string, and answers a success with the bare value
  when `envelope: false`. Failures keep the envelope. Every target follows: the
  Rust server and client, the TypeScript client (`RpcClient.http`,
  `RpcHttpSpec`), and OpenAPI (path and query parameters, form bodies,
  `x-rpc-any-method`). `tests/http_wire.rs` serves the generated router and
  calls it with the generated client and by hand.
- **Every Rust enum lists and spells itself.** Each SDL enum, the `ErrorCode`
  registry included, gets `pub const ALL: &[Self]` (every value, in SDL
  order), `from_name` and `as_str` over its wire names, and derives `Hash`.

### Changed
- Decoding an input from a URL moved out of the streaming runtime into its own
  section, emitted for any API that reads one (a subscription, a raw request or
  an `@http` operation), so an `@http` API compiles no SSE support and needs no
  `futures-*` crates. The Rust client's URL helpers moved the same way.
- `operations::ALL` is sorted by path and then method, and `by_route` finds the
  row of the method asked for among several on one path; an `ANY` row answers
  every method.
- **Breaking:** a consumer's own inherent `ALL`, `from_name` or `as_str` on a
  generated enum, or its own `impl Hash` for one, now collides with the
  generated item and must be deleted.
- The `@roles`, `@permissions` and `@scopes` enums get `from_name` and
  `as_str` from the general rule; only `Role::authorizes` is still emitted
  for one kind of enum alone.
- An `ErrorCode` value named `ALL`, `from_name` or `as_str` is refused.

### Why
`@http` exists for replacing a service behind clients that cannot move with it:
the first consumer is a single sign-on service whose hosts switch one at a time
by configuration, so the new service must answer the old URLs, methods, form
bodies and bare JSON byte for byte while its API is still generated from the
schema.

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
