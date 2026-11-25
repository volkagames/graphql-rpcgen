//! Rust emitter: wire types, service traits and web bindings. Reads only the
//! IR and the [`Config`].
//!
//! # Three targets
//!
//! The types, the server bindings and the client are emitted separately
//! because their dependencies differ: types need `serde`, the server adds a
//! web framework, the client adds an HTTP stack. A project that only calls the
//! API takes the types and the client and never compiles `axum`.
//!
//! Emitting `rust_server` without `types_path` reproduces the single-file
//! layout, so a project that wants one file is not forced to split: the types
//! preamble, then the server's, then the types and the services.
//!
//! The preambles, the router and the handler forms come from mustache
//! templates. The built-in ones target `treat` + `axum` and are compiled in; a
//! project on another stack points [`crate::config::RustConfig`] at its own
//! files.
//!
//! # The runtime is `treat::rpc`
//!
//! Nothing schema-independent is emitted: reading an input from a body, a
//! query string, a form or a path, validating it, refusing a request, and
//! answering with the envelope, a bare value, bytes or Server-Sent Events are
//! `treat::rpc`'s, which the generated server names once as
//! `type Rt = ::treat::rpc::Runtime<ErrorCode>;`. A handler is therefore a few
//! lines: the access check, one call reading the input, the service call, and
//! one call answering with its result. Every step that can refuse the request
//! is a `?` on a `Result<_, Refusal>`.
//!
//! What is emitted is the schema: the types and their `validator` rules, the
//! registry and the per-operation code sets (`WireEnum`, so the runtime reads
//! a code by its wire name), the `operations` table, the `wire` statics —
//! query tables, `@http` specs, raw media types — the server reads a request
//! with and the client writes one with, the service traits and their routers.
//!
//! # Names
//!
//! The generated code imports no name beside the API's types: a crate's item is
//! named by its absolute path (`::treat::rpc::server::Refusal`,
//! `::axum::extract::Request`, `::serde::Serialize`), so an SDL type spelt like
//! one of them — an input called `Request` — is the only item of that name in
//! the module and means the SDL's type wherever the file writes it. The two
//! imports it does make cannot meet an SDL type: `wire`'s, inside a module
//! holding nothing else, and `use ::validator::Validate as _`, which binds no
//! name, for the `validator` derive's method calls. What a module re-exports for
//! the code implementing or calling the API (`Refusal`, `EventStream`,
//! `ApiError`, `WireEnum`, …) is left out where the API declares a type of the
//! same name, which would otherwise be a duplicate item or be shadowed by it.
//! The few names the generator declares itself and the prelude names it writes
//! unqualified are refused as type names by the compiler.
//!
//! # Layout
//!
//! The output is plain: one statement per line, a signature, a derive list or
//! a `use` list on one line however long, nothing laid out by width. Its
//! layout is `rustfmt`'s, applied by [`crate::format_outputs`] — which also
//! applies the project's own `rustfmt.toml`.

use std::fmt::Write;

use crate::config::Config;
use crate::emit_common::{
    emit_doc_rust, fields_static_name, reexport, rust_type, rust_type_non_null, use_list, HEADER,
};
use crate::ir::*;
use crate::naming::{codes_type_name, pascal_case, snake_case, snake_case_raw};
use crate::template::{Template, Vars};

const DEFAULT_TYPES_PREAMBLE: &str = include_str!("../templates/rust/types_preamble.mustache");
const DEFAULT_SERVER_PREAMBLE: &str = include_str!("../templates/rust/server_preamble.mustache");
/// Every handler form but a buffered raw request: the access check, the one
/// runtime call that reads the input, the service call, the answer.
const DEFAULT_HANDLER: &str = include_str!("../templates/rust/handler.mustache");
/// A buffered `@raw(request:)`: the query string, then the body into the
/// input's bytes field, then the input's rules — three steps the runtime cannot
/// take in one call, since only the generated code knows the field.
const DEFAULT_HANDLER_RAW_REQUEST: &str =
    include_str!("../templates/rust/handler_raw_request.mustache");
const DEFAULT_ROUTER: &str = include_str!("../templates/rust/router.mustache");

/// Variables the types preamble may reference, besides `template_vars`.
///
/// `reexports` is the `pub use` of the runtime's code traits, computed so it
/// leaves out a name the API declares a type of; `registry` is set when the
/// API has an `ErrorCode` enum, which is what `Failure` aliases.
const TYPES_PREAMBLE_VARS: [&str; 3] = ["error_code_enum", "reexports", "registry"];
/// Variables the server preamble may reference, besides `template_vars`.
///
/// `types_import` is empty for the single-file layout, so the same template
/// serves both arrangements. `reexports` is the `pub use` lines of what an
/// implementation of the API's traits names — the envelope, the refusal, the
/// stream and body types — as the API uses them and less any name the API
/// declares a type of; `operations` is set when there is a handler to name
/// `Rt`; `context_trait` is the `FromRequestContext` declaration, empty for an
/// API guarding nothing on a session.
const SERVER_PREAMBLE_VARS: [&str; 5] = [
    "error_code_enum",
    "types_import",
    "reexports",
    "operations",
    "context_trait",
];
/// Variables the handler templates may reference, besides `template_vars`.
///
/// What differs between the forms is computed here, as plain text: the
/// parameters, the statements checking access and reading the input, the
/// expression answering. The template places them around the function's
/// fixed shape; the rest are the pieces they are built from, for a project's
/// own template.
const HANDLER_VARS: [&str; 17] = [
    "handler",
    "trait_name",
    "input",
    "output",
    "method",
    "path",
    // The `wire` statics the form reads with, each empty where it has none:
    // the query table, the `@http` operation's `HttpSpec`, the media types a
    // raw request accepts.
    "fields",
    "spec",
    "types",
    // The struct field a buffered raw request writes its body to.
    "body_field",
    // Set for a session-guarded operation, and the arguments the service call
    // takes: `&ctx, input`, `input`, `&ctx` or nothing.
    "context",
    "call_args",
    // The extractors, comma-separated in the order axum takes them; the
    // statements before the service call — the access check, the input read,
    // a buffered raw request's body read — each line indented and ending in
    // a newline; and the expression answering with `result`.
    "params",
    "access_guard",
    "read_input",
    "read_body",
    "respond",
];
/// Variables the router template may reference, besides `template_vars`.
///
/// `routes` is the `.route(..)` calls, one per line.
const ROUTER_VARS: [&str; 4] = ["service", "module", "trait_name", "routes"];

/// The templates the emitter renders, compiled once per run.
struct Templates {
    types_preamble: Template,
    server_preamble: Template,
    handler: Template,
    handler_raw_request: Template,
    router: Template,
    /// Project-supplied values merged into every render.
    extra: Vars,
}

impl Templates {
    /// Compile the templates, each the project's own where it names one.
    fn load(config: &Config) -> erris::Result<Self> {
        let extra_names: Vec<&str> = config
            .rust
            .template_vars
            .keys()
            .map(String::as_str)
            .collect();

        let compile = |source: &str,
                       origin: &str,
                       path: &Option<std::path::PathBuf>,
                       base: &[&str]|
         -> erris::Result<Template> {
            let mut allowed: Vec<&str> = base.to_vec();
            allowed.extend_from_slice(&extra_names);
            match path {
                Some(path) => Template::from_file(path, &allowed),
                None => Template::compile(source, origin, &allowed),
            }
        };

        Ok(Self {
            types_preamble: compile(
                DEFAULT_TYPES_PREAMBLE,
                "<builtin types preamble>",
                &config.rust.types_preamble_template,
                &TYPES_PREAMBLE_VARS,
            )?,
            server_preamble: compile(
                DEFAULT_SERVER_PREAMBLE,
                "<builtin server preamble>",
                &config.rust.preamble_template,
                &SERVER_PREAMBLE_VARS,
            )?,
            handler: compile(
                DEFAULT_HANDLER,
                "<builtin handler>",
                &config.rust.handler_template,
                &HANDLER_VARS,
            )?,
            // The buffered raw form has no config hook: it is the built-in or
            // nothing, until a project asks to replace it.
            handler_raw_request: compile(
                DEFAULT_HANDLER_RAW_REQUEST,
                "<builtin raw-request handler>",
                &None,
                &HANDLER_VARS,
            )?,
            router: compile(
                DEFAULT_ROUTER,
                "<builtin router>",
                &config.rust.router_template,
                &ROUTER_VARS,
            )?,
            extra: config.rust.template_vars.clone(),
        })
    }

    /// A render context seeded with the project's own variables.
    fn vars(&self, pairs: &[(&str, &str)]) -> Vars {
        let mut vars = self.extra.clone();
        for (key, value) in pairs {
            vars.insert((*key).to_string(), (*value).to_string());
        }
        vars
    }
}

/// Every operation of the API, service by service.
fn all_operations(api: &Api) -> impl Iterator<Item = &Operation> {
    api.services.iter().flat_map(|service| &service.operations)
}

/// Server bindings, and the wire types too unless they live elsewhere.
///
/// With no `types_path` configured this is the whole API in one file, which is
/// the layout a project gets before it splits anything: the types preamble
/// and the server preamble one after the other, exactly what the two files of
/// the split layout start with.
pub fn generate(api: &Api, config: &Config) -> erris::Result<String> {
    let split = config.rust.types_path.is_some();
    let templates = Templates::load(config)?;

    let mut out = String::new();
    out.push_str(HEADER);
    if !split {
        out.push_str(&render_types_preamble(api, &templates)?);
    }
    out.push_str(&render_server_preamble(api, config, &templates)?);

    // Without a types module of their own, the types belong here: the server
    // file is then self-contained, exactly as before the split existed.
    if !split {
        emit_types(&mut out, api);
    }

    for service in &api.services {
        emit_service(&mut out, api, service, &templates)?;
    }

    Ok(out)
}

/// Wire types alone: no service traits, no transport, no web framework.
pub fn generate_types(api: &Api, config: &Config) -> erris::Result<String> {
    let templates = Templates::load(config)?;

    let mut out = String::new();
    out.push_str(HEADER);
    out.push_str(&render_types_preamble(api, &templates)?);
    emit_types(&mut out, api);

    Ok(out)
}

/// The types preamble: the runtime's code traits re-exported, and the
/// `Failure` alias.
fn render_types_preamble(api: &Api, templates: &Templates) -> erris::Result<String> {
    let registry = api.find_type(crate::semantic::ERROR_CODE_ENUM).is_some();
    let reexports = reexport(
        api,
        "::treat::rpc",
        &["Narrow", "Undeclared", "WireEnum", "UNDECLARED_CODE_META"],
    );

    templates.types_preamble.render(&templates.vars(&[
        ("error_code_enum", crate::semantic::ERROR_CODE_ENUM),
        ("reexports", &reexports),
        ("registry", flag(registry)),
    ]))
}

/// A template flag: set, or empty and so absent.
fn flag(on: bool) -> &'static str {
    match on {
        true => "true",
        false => "",
    }
}

/// What a set of operations needs re-exported from `treat::rpc::server`, so
/// the server re-exports what an implementation of it names.
#[derive(Default)]
struct ServerNeeds {
    /// Any operation at all: `Refusal`, `Rt`.
    operations: bool,
    /// A buffered raw request, which bounds its body by `RAW_BODY_LIMIT`.
    raw_body: bool,
    /// A streamed raw request.
    raw_request: bool,
    /// A subscription.
    events: bool,
    /// A raw response.
    raw_response: bool,
    /// A session-guarded operation.
    context: bool,
}

impl ServerNeeds {
    fn of(api: &Api) -> Self {
        let mut needs = Self::default();
        for op in all_operations(api) {
            needs.operations = true;
            needs.context |= op.auth.has_context();
            match form(op) {
                Form::Json | Form::Http => {}
                Form::Subscription => needs.events = true,
                Form::RawRequest => needs.raw_body = true,
                Form::RawRequestStream => needs.raw_request = true,
            }
            needs.raw_response |= op.raw_response.is_some() && op.enveloped();
        }
        needs
    }
}

/// The server preamble: the re-exports, the runtime alias and the context
/// trait.
fn render_server_preamble(
    api: &Api,
    config: &Config,
    templates: &Templates,
) -> erris::Result<String> {
    let needs = ServerNeeds::of(api);

    // What an implementation of the traits names, re-exported so implementing
    // a service takes the one import it always did. The handlers name each by
    // its path and need none of it.
    let mut server: Vec<&str> = Vec::new();
    if needs.operations {
        server.push("Refusal");
    }
    if needs.context {
        server.push("ResolveContext");
    }
    if needs.events {
        server.extend(["event_stream", "EventStream"]);
    }
    if needs.raw_response {
        server.push("RawBody");
    }
    if needs.raw_request {
        server.push("RawRequest");
    }
    if needs.raw_body {
        server.push("RAW_BODY_LIMIT");
    }
    let mut reexports = reexport(api, "::treat::rpc::server", &server);
    reexports.push_str(&reexport(
        api,
        "::treat",
        &[
            "error",
            "error_and_message",
            "failure",
            "success",
            "ApiError",
            "ApiResponse",
            "ErrorMessage",
        ],
    ));

    let types_import = match config.rust.types_path.as_deref() {
        Some(path) => format!("pub use {path}::*;\n"),
        None => String::new(),
    };
    let context_trait = match needs.context {
        true => context_trait(api),
        false => String::new(),
    };

    templates.server_preamble.render(&templates.vars(&[
        ("error_code_enum", crate::semantic::ERROR_CODE_ENUM),
        ("types_import", &types_import),
        ("reexports", &reexports),
        ("operations", flag(needs.operations)),
        ("context_trait", &context_trait),
    ]))
}

/// `FromRequestContext`: `treat`'s `ResolveContext` plus the access checks the
/// API's `@auth` declares, each only where the API has the enum it takes.
///
/// Without one there is no signature to give it, and nothing would call it, so
/// an API guarding only on a session gets the bare extension.
fn context_trait(api: &Api) -> String {
    let body = format!(
        "{}{}",
        require_role_method(api),
        require_permission_method(api)
    );
    format!("pub trait FromRequestContext: ::treat::rpc::server::ResolveContext {{{body}}}")
}

/// Every declared type, in the IR's sorted order, then the error vocabulary:
/// the registry enum, and one error set per operation; then the tables code
/// outside the handlers reads.
///
/// Both belong here rather than next to the service traits because both sides
/// name them — the server answers with those codes and the client branches on
/// them — and only the types module is shared by both.
fn emit_types(out: &mut String, api: &Api) {
    let nests = api.types.iter().any(|ty| match ty {
        ApiType::InputObject(i) => !i.one_of && i.fields.iter().any(|f| is_nested_arm(api, f)),
        _ => false,
    });
    if nests {
        out.push_str(
            "\n// `validator`'s derive calls `.validate()` on a nested input, which needs the\n\
             // trait in scope: imported anonymously, so no SDL type can be spelt like it.\n\
             use ::validator::Validate as _;\n",
        );
    }
    emit_pattern_statics(out, api);

    for ty in &api.types {
        match ty {
            ApiType::Scalar(s) if s.rust_newtype => emit_scalar_newtype(out, s),
            ApiType::Scalar(_) => {}
            // The `@permissions` and `@scopes` enums need nothing beyond what
            // every enum gets: neither declares a hierarchy to compile into an
            // `authorizes`, so which implies which stays the implementation's.
            ApiType::Enum(e) => {
                emit_enum(out, e);
                if api.roles.as_ref().is_some_and(|r| r.enum_name == e.name) {
                    emit_role_impl(out, api, e);
                }
            }
            ApiType::Object(o) => emit_object(out, api, o),
            ApiType::InputObject(i) => emit_input_object(out, api, i),
            ApiType::Union(u) => emit_union(out, u),
        }
    }

    emit_error_sets(out, api);
    emit_operations_module(out, api);
    emit_wire_module(out, api);
}

/// The shape a handler takes, which is also what its client method sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// An RPC operation: `POST` + JSON body, a `@raw(response:)` included.
    Json,
    /// An `@http` operation.
    Http,
    /// A `@subscription`: the input in the query string, events back.
    Subscription,
    /// A buffered `@raw(request:)`: the fields in the query string, the bytes
    /// in the body.
    RawRequest,
    /// A `@raw(request:, stream: true)`: the body handed over unread.
    RawRequestStream,
}

fn form(op: &Operation) -> Form {
    if op.http.is_some() {
        return Form::Http;
    }
    if op.kind == OperationKind::Subscription {
        return Form::Subscription;
    }
    if op.raw_request_stream {
        return Form::RawRequestStream;
    }
    if op.raw_request.is_some() {
        return Form::RawRequest;
    }
    Form::Json
}

/// Whether the operation has a `wire::*_FIELDS` table of its own: a
/// subscription's input, a raw request's other fields — what travels in the
/// query string. An `@http` operation's table is part of its `HttpSpec`.
fn has_fields_table(op: &Operation) -> bool {
    match form(op) {
        Form::Json | Form::Http => false,
        Form::Subscription => op.input.is_some(),
        Form::RawRequest | Form::RawRequestStream => true,
    }
}

/// The `wire` statics of one operation, by suffix: `_FIELDS` (its query
/// table), `_HTTP` (an `@http` operation's `HttpSpec`), `_TYPES` (what a raw
/// request accepts). One function for the server and the client, which name
/// the same `static`.
pub(crate) fn wire_static(service: &Service, op: &Operation, suffix: &str) -> String {
    format!(
        "wire::{}_{suffix}",
        fields_static_name(&service.name, &op.name)
    )
}

/// The axum routing function an operation is mounted with: the method's
/// lower-case name, `any` for `ANY`.
fn routing_function(op: &Operation) -> &'static str {
    match op.http_method() {
        "GET" => "get",
        "PUT" => "put",
        "PATCH" => "patch",
        "DELETE" => "delete",
        "ANY" => "any",
        _ => "post",
    }
}

/// A transparent wrapper around a scalar's Rust representation.
///
/// `#[serde(transparent)]` keeps the wire format identical to the inner type,
/// so the newtype is a compile-time distinction only.
///
/// The conversions come from `derive_more` rather than hand-written impls.
/// `Deref` is deliberately not among them: it would auto-deref the whole inner
/// API onto the wrapper — `clan_id.as_u128()` — which is exactly the leak the
/// newtype exists to prevent. `AsRef` offers the same access explicitly.
fn emit_scalar_newtype(out: &mut String, s: &ScalarType) {
    let name = &s.name;
    let inner = &s.rust;

    out.push('\n');
    emit_doc_rust(out, "", &s.description);
    let mut derives = vec!["Debug", "Clone"];
    // `Copy` only holds when the wrapped type is itself `Copy`: a UUID-backed
    // id is, a String-backed one is not.
    if s.rust_copy {
        derives.push("Copy");
    }
    // `Eq`, `Ord` and `Hash` follow the inner type, and a float has none of the
    // three. Deriving them unconditionally made `rustRange: true` — the setting
    // that exists for numeric newtypes — a guaranteed rustc failure over `f64`.
    match is_rust_float(inner) {
        true => derives.extend(["PartialEq", "PartialOrd"]),
        false => derives.extend(["PartialEq", "Eq", "PartialOrd", "Ord", "Hash"]),
    }
    derives.extend([
        "::serde::Serialize",
        "::serde::Deserialize",
        "::derive_more::AsRef",
        "::derive_more::Display",
        "::derive_more::From",
        "::derive_more::Into",
    ]);
    let _ = writeln!(out, "#[derive({})]", derives.join(", "));
    out.push_str("#[serde(transparent)]\n");
    let _ = writeln!(out, "pub struct {name}(pub {inner});");

    // A constraint on a field of this type has to reach through the wrapper, and
    // `validator` only knows the types it ships impls for. The wire form of a
    // newtype is its inner value, so that is what the rules are measured
    // against: directly for a string, through `Display` otherwise — the
    // rendering a `uuid::Uuid` or a number has on the wire.
    let subject = match inner.as_str() {
        "String" => "&self.0".to_string(),
        _ => "&self.to_string()".to_string(),
    };
    let _ = writeln!(
        out,
        "\nimpl ::validator::ValidateLength<u64> for {name} {{\n    \
         fn length(&self) -> Option<u64> {{\n        \
         ::validator::ValidateLength::length({subject})\n    }}\n}}"
    );
    let _ = writeln!(
        out,
        "\nimpl ::validator::ValidateRegex for {name} {{\n    \
         fn validate_regex(&self, regex: impl ::validator::AsRegex) -> bool {{\n        \
         ::validator::ValidateRegex::validate_regex({subject}, regex)\n    }}\n}}"
    );

    // `@range` compares numbers, and the validator ships those impls only for
    // its own primitives — a numeric newtype opts in with `rustRange` and gets
    // the same reach-through adapter.
    if s.rust_range {
        let _ = writeln!(
            out,
            "\nimpl ::validator::ValidateRange<{inner}> for {name} {{\n    \
             fn greater_than(&self, other: {inner}) -> Option<bool> {{\n        \
             Some(self.0 > other)\n    }}\n\n    \
             fn less_than(&self, other: {inner}) -> Option<bool> {{\n        \
             Some(self.0 < other)\n    }}\n}}"
        );
    }
}

/// An SDL enum, the registry included.
///
/// Every one derives `treat::rpc::WireEnum` — `ALL`, `from_name` and `as_str`
/// over the wire names — so a consumer that stores a value as text, filters a
/// query by it, or lists the choices on a screen reads the SDL's set rather
/// than restating it in an array and a `match` of its own that drift from it.
/// `as_str` is exactly what serde writes, the `@variant(tag:)` when a value has
/// one, since names such as `admin.users.read` are seldom identifiers.
fn emit_enum(out: &mut String, e: &EnumType) {
    if e.name == crate::semantic::ERROR_CODE_ENUM {
        emit_error_code_enum(out, e);
        return;
    }

    out.push('\n');
    emit_doc_rust(out, "", &e.description);
    out.push_str(
        "#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ::serde::Serialize, \
         ::serde::Deserialize, ::treat::rpc::WireEnum)]\n",
    );
    let _ = writeln!(out, "pub enum {} {{", e.name);
    for v in &e.values {
        emit_doc_rust(out, "    ", &v.description);
        // The Rust variant is PascalCase while the wire carries the SDL
        // spelling, or whatever `@variant(tag:)` overrode it with, so the wire
        // name is pinned explicitly rather than left to a rename rule. It is
        // also what `WireEnum` reads.
        let _ = writeln!(out, "    #[serde(rename = {:?})]", v.wire_name());
        let _ = writeln!(out, "    {},", pascal_case(&v.name));
    }
    out.push_str("}\n");
}

/// What the server needs of the `@roles` enum beyond what every enum gets: the
/// role hierarchy.
///
/// The hierarchy is matched out per role rather than walked at runtime, since
/// the compiler has already closed over it: a role's arm lists itself and every
/// role it inherits, and nothing else is authorized by it.
fn emit_role_impl(out: &mut String, api: &Api, e: &EnumType) {
    let Some(roles) = &api.roles else { return };
    let variant = |name: &str| format!("Self::{}", pascal_case(name));

    let _ = writeln!(out, "\nimpl {} {{", e.name);
    out.push_str(
        "    /// Whether a user assigned `self` is authorized for `required`: the same\n    \
         /// role, or one `self` inherits through `@inherits`, directly or not.\n    \
         pub fn authorizes(self, required: Self) -> bool {\n        match self {\n",
    );
    for (role, inherited) in &roles.inherits {
        let granted = std::iter::once(role)
            .chain(inherited)
            .map(|name| variant(name))
            .collect::<Vec<_>>()
            .join(" | ");
        let _ = writeln!(
            out,
            "            {} => matches!(required, {granted}),",
            variant(role)
        );
    }
    out.push_str("        }\n    }\n");
    out.push_str("}\n");
}

/// `operations`: every operation of the API as the router mounts it, one const
/// table sorted by path, with what the SDL says of each.
///
/// For code that decides something per operation outside the generated
/// handler: a gate in front of the router that reads `(method, path)` off the
/// request — a token's scope, a rate limit — and a caller reaching a service
/// other than through its router, an MCP tool or a job runner, which passes no
/// generated guard and looks the check up by `operationId`. Either reads the
/// SDL's answer here rather than restating it in a table of its own.
///
/// A module of its own so that its names — `Operation`, `Kind`, `Auth` — cannot
/// collide with the API's types, which it names through `super::`. A column
/// exists only where the API declares its set: `role` with `@roles`,
/// `permission` with `@permissions`, `scope` with `@scopes`.
///
/// The lookups are `treat::rpc::route`'s, over the table as a `RouteRow`
/// slice.
fn emit_operations_module(out: &mut String, api: &Api) {
    let role = api.role_enum().map(|e| e.name.as_str());
    let permission = api.permission_enum().map(|e| e.name.as_str());
    let scope = api.scope_enum().map(|e| e.name.as_str());

    out.push_str(
        "\n/// Every operation of the API as its router mounts it, with what the SDL says\n\
         /// of it: for code that decides something per operation outside the generated\n\
         /// handler, by `(method, path)` or by `operationId`.\n\
         pub mod operations {\n",
    );
    emit_operation_types(out, role, permission, scope);
    out.push('\n');
    emit_operation_table(out, api, role, permission, scope);
    out.push('\n');
    out.push_str(OPERATION_LOOKUPS);
    out.push_str("}\n");
}

/// The `operations` module's types: the kind, the auth requirement, and the
/// row, which has a column for each access set the API declares.
fn emit_operation_types(
    out: &mut String,
    role: Option<&str>,
    permission: Option<&str>,
    scope: Option<&str>,
) {
    out.push_str(
        "    /// Which of `@query`, `@mutation` and `@subscription` the operation is.\n    \
         #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]\n    \
         pub enum Kind {\n        Query,\n        Mutation,\n        Subscription,\n    }\n\n    \
         /// What `@auth(require:)` asks of the caller.\n    \
         #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]\n    \
         pub enum Auth {\n        Session,\n        Token,\n        Public,\n    }\n\n    \
         /// One operation, as the router mounts it.\n    \
         #[derive(Debug, Clone, Copy, PartialEq, Eq)]\n    \
         pub struct Operation {\n        \
         /// The OpenAPI `operationId`.\n        \
         pub id: &'static str,\n        \
         /// `POST` for an RPC operation, `GET` for a subscription, or an `@http`\n        \
         /// operation's `GET`, `PUT`, `PATCH`, `DELETE` or `ANY`, upper-case as\n        \
         /// HTTP spells it. `ANY` answers every method.\n        \
         pub method: &'static str,\n        \
         /// The path the router mounts, `@version` prefix included; an `@http`\n        \
         /// path as the SDL writes it, `{name}` segments and all.\n        \
         pub path: &'static str,\n        \
         /// `@query`, `@mutation` or `@subscription`.\n        \
         pub kind: Kind,\n        \
         /// What `@auth(require:)` asks, the service's when the operation states none.\n        \
         pub auth: Auth,\n",
    );
    for (field, ty, doc) in [
        (
            "role",
            role,
            "The role `@auth(role:)` assigns the operation to; `None` for none.",
        ),
        (
            "permission",
            permission,
            "The permission `@auth(permission:)` says it needs; `None` for none. With no\n        \
             /// `scope`, the permission check is all that judges a delegated credential, so\n        \
             /// `require_permission` must refuse a credential not allowed it, not only a user.",
        ),
        (
            "scope",
            scope,
            "The scope a delegated credential must carry, a query's default included.\n        \
             /// `None` for a public operation, and for one with a permission that states\n        \
             /// none, whose permission check must then judge the credential itself.",
        ),
    ] {
        if let Some(ty) = ty {
            let _ = writeln!(
                out,
                "        /// {doc}\n        pub {field}: Option<super::{ty}>,"
            );
        }
    }
    out.push_str("    }\n");
}

/// `operations::ALL`: one row per operation, sorted as the lookups expect.
fn emit_operation_table(
    out: &mut String,
    api: &Api,
    role: Option<&str>,
    permission: Option<&str>,
    scope: Option<&str>,
) {
    out.push_str(
        "    /// Every operation, sorted by `path` and then `method`: two operations share\n    \
         /// a path only under different methods.\n",
    );

    let mut ops: Vec<&Operation> = all_operations(api).collect();
    ops.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then(a.http_method().cmp(b.http_method()))
    });
    let some = |ty: Option<&str>, name: Option<&String>| match (ty, name) {
        (Some(ty), Some(name)) => format!("Some(super::{ty}::{})", pascal_case(name)),
        _ => "None".to_string(),
    };
    let mut rows: Vec<Vec<(&str, String)>> = Vec::new();
    for op in ops {
        let kind = match op.kind {
            OperationKind::Query => "Query",
            OperationKind::Mutation => "Mutation",
            OperationKind::Subscription => "Subscription",
        };
        let auth = match op.auth {
            AuthRequirement::Session => "Session",
            AuthRequirement::Token => "Token",
            AuthRequirement::Public => "Public",
        };
        let mut row = vec![
            ("id", format!("{:?}", op.operation_id)),
            ("method", format!("{:?}", op.http_method())),
            ("path", format!("{:?}", op.path)),
            ("kind", format!("Kind::{kind}")),
            ("auth", format!("Auth::{auth}")),
        ];
        let (op_role, op_permission) = match &op.access {
            Some(Access::Role(r)) => (Some(r), None),
            Some(Access::Permission(p)) => (None, Some(p)),
            None => (None, None),
        };
        for (field, ty, name) in [
            ("role", role, op_role),
            ("permission", permission, op_permission),
            ("scope", scope, op.scope.as_ref()),
        ] {
            if ty.is_some() {
                row.push((field, some(ty, name)));
            }
        }
        rows.push(row);
    }
    out.push_str(&struct_table(
        4,
        "pub const ALL: &[Operation] =",
        "Operation",
        &rows,
    ));
}

/// `head &[Type { .. }, ..];` at `indent`: each row on lines of its own, one
/// field per line — a fixed shape, whatever the values.
fn struct_table(indent: usize, head: &str, ty: &str, rows: &[Vec<(&str, String)>]) -> String {
    let pad = " ".repeat(indent);
    let inner = " ".repeat(indent.saturating_add(4));
    let field_pad = " ".repeat(indent.saturating_add(8));
    let mut out = format!("{pad}{head} &[\n");
    for row in rows {
        let _ = writeln!(out, "{inner}{ty} {{");
        for (name, value) in row {
            let _ = writeln!(out, "{field_pad}{name}: {value},");
        }
        let _ = writeln!(out, "{inner}}},");
    }
    let _ = writeln!(out, "{pad}];");
    out
}

/// The `operations` module's lookups over `ALL`, the same in every API.
const OPERATION_LOOKUPS: &str = "    \
         impl ::treat::rpc::route::RouteRow for Operation {\n        \
         fn method(&self) -> &str {\n            \
         self.method\n        \
         }\n\n        \
         fn path(&self) -> &str {\n            \
         self.path\n        \
         }\n    \
         }\n\n    \
         /// The operation the router mounts at `method path`; `None` for a route it\n    \
         /// does not, so a caller that refuses on `None` fails closed. `method` is\n    \
         /// matched as spelled: `HEAD`, which a router may answer for a `GET` route,\n    \
         /// is no row, and is refused rather than read as the `GET`. An `ANY` row\n    \
         /// answers every method, as its route does.\n    \
         ///\n    \
         /// `path` is the request's (`/api/user/u1`) or the route's (`/api/user/{user_id}`,\n    \
         /// a router's matched path): a `{name}` segment of a row takes any one segment.\n    \
         /// The route is chosen as the router chooses it, a fixed segment before a\n    \
         /// parameter, and only then the method: a method the chosen route lacks is\n    \
         /// `None`, as the router answers it `405`, even where another route would fit.\n    \
         /// See `treat::rpc::route::by_route`.\n    \
         pub fn by_route(method: &str, path: &str) -> Option<&'static Operation> {\n        \
         ::treat::rpc::route::by_route(ALL, method, path)\n    }\n\n    \
         /// The operation whose `operationId` is `id`; `None` for an id the API lacks,\n    \
         /// so a misspelt one never reads as an operation that needs nothing.\n    \
         pub fn by_id(id: &str) -> Option<&'static Operation> {\n        \
         ALL.iter().find(|op| op.id == id)\n    }\n";

/// `wire`: how each operation that does not simply send JSON sits on the
/// wire, as the `static`s both the server and the client read.
///
/// - `<OP>_FIELDS`: how a subscription or a raw request spells its input in
///   the query string — the table the server reads it with and the client
///   writes it with, so a JSON-typed field is quoted on both sides and a list
///   repeats its key on both;
/// - `<OP>_HTTP`: an `@http` operation's `HttpSpec` — its method, path and
///   field table, which path segments are input fields, where the rest travel,
///   and whether a success is the envelope;
/// - `<OP>_TYPES`: the media types a `@raw(request: [...])` declares, which
///   the server checks the `Content-Type` against.
///
/// In the types module because both sides need them and neither depends on
/// the other: a client-only project has no server module to reach into, and a
/// table restated in each could disagree.
fn emit_wire_module(out: &mut String, api: &Api) {
    let mut items = String::new();
    let (mut fields, mut http, mut body) = (false, false, false);
    for service in &api.services {
        for op in &service.operations {
            let name = fields_static_name(&service.name, &op.name);
            let operation = format!("{}.{}", service.name, op.name);
            let table = query_field_table(api, op);
            if has_fields_table(op) {
                fields = true;
                let _ = writeln!(items, "\n    /// `{operation}`: its query string.");
                let _ = writeln!(
                    items,
                    "    pub static {name}_FIELDS: &[QueryField] = {table};"
                );
            }
            if let Some(binding) = &op.http {
                http = true;
                fields |= !api.query_fields(op).is_empty();
                body |= binding.body != HttpBody::default_for(binding.method);
                let _ = writeln!(items, "\n    /// `{operation}`: its route.");
                let _ = writeln!(
                    items,
                    "    pub static {name}_HTTP: HttpSpec = {};",
                    http_spec(api, op, binding)
                );
            }
            if let Some(types) = &op.raw_request {
                let quoted: Vec<String> = types.iter().map(|t| format!("{t:?}")).collect();
                let _ = writeln!(
                    items,
                    "\n    /// `{operation}`: the request body's media types."
                );
                let _ = writeln!(
                    items,
                    "    pub static {name}_TYPES: &[&str] = &[{}];",
                    quoted.join(", ")
                );
            }
        }
    }
    if items.is_empty() {
        return;
    }

    out.push_str(
        "\n/// How each operation that does not simply send JSON sits on the wire: the\n\
         /// tables and routes the server reads a request with and the client writes\n\
         /// one with, so the two agree by construction.\n\
         pub mod wire {\n",
    );
    // Imported by name, since inside a module of its own, which declares
    // nothing but these statics, no SDL type can meet the names.
    let mut names: Vec<&str> = Vec::new();
    if body {
        names.push("HttpBodyKind");
    }
    if http {
        names.push("HttpSpec");
    }
    if fields {
        names.push("QueryField");
    }
    out.push_str(&format!("    {}", use_list("use ::treat::rpc::", &names)));
    out.push_str(&items);
    out.push_str("}\n");
}

/// An operation's query table, as an expression: every input field but a raw
/// request's body, in declaration order, each through `QueryField`'s
/// constructor for its kind — `&[QueryField::str("id"), QueryField::int("n").list()]`.
fn query_field_table(api: &Api, op: &Operation) -> String {
    let fields: Vec<String> = api
        .query_fields(op)
        .into_iter()
        .map(|f| {
            let list = match matches!(f.ty, TypeRef::List { .. }) {
                true => ".list()",
                false => "",
            };
            format!("QueryField::{}({:?}){list}", query_kind(api, f), f.name)
        })
        .collect();
    format!("&[{}]", fields.join(", "))
}

/// An `@http` operation's `HttpSpec`, as the expression a `static` holds: the
/// constructor for its method, then a builder for each thing its directive
/// says beyond the defaults — the field table, the path fields, a body other
/// than the method's, a bare success.
///
/// The one place an `@http` binding is spelled for Rust: the server reads the
/// request with it and answers through it, the client sends with it.
fn http_spec(api: &Api, op: &Operation, binding: &HttpBinding) -> String {
    let method = match binding.method {
        HttpMethod::Get => "get",
        HttpMethod::Post => "post",
        HttpMethod::Put => "put",
        HttpMethod::Patch => "patch",
        HttpMethod::Delete => "delete",
        HttpMethod::Any => "any",
    };
    let mut spec = format!("HttpSpec::{method}({:?})", op.path);
    let fields = api.query_fields(op);
    if !fields.is_empty() {
        let _ = write!(spec, ".fields({})", query_field_table(api, op));
    }
    if !binding.path_params.is_empty() {
        let quoted: Vec<String> = binding
            .path_params
            .iter()
            .map(|p| format!("{p:?}"))
            .collect();
        let _ = write!(spec, ".path_fields(&[{}])", quoted.join(", "));
    }
    if binding.body != HttpBody::default_for(binding.method) {
        let body = match binding.body {
            HttpBody::Json => "Json",
            HttpBody::Form => "Form",
            HttpBody::None => "None",
        };
        let _ = write!(spec, ".body(HttpBodyKind::{body})");
    }
    if !op.enveloped() {
        spec.push_str(".bare()");
    }
    spec
}

/// The error registry, as a `treat` code enum.
///
/// Variants keep their SDL spelling so the Rust name and the wire code are the
/// same string, which is how the portal's own `PortalErrorCode` is written.
/// `#[derive(ApiErrorCode)]` supplies `Display`, `From<ErrorCode> for
/// ApiError<ErrorCode>` and the declared `#[message(...)]`; `WireEnum` the
/// names by which the runtime narrows a code into an operation's set.
///
/// A value with `@variant(tag:)` cannot keep that symmetry: the SDL spelling
/// is an identifier and the tag (`portal_db::clan_not_found`) need not be.
/// The tag is pinned twice, as `#[code]` for `Display` and `WireEnum`, which is
/// what `treat` writes into the envelope, and as `#[serde(rename)]`, which is
/// what a client reads a code back through; `WireEnum` refuses the two if they
/// ever disagree.
///
/// `RuntimeCodes` tells the runtime which values are the two it reports on its
/// own: a request it could not read, and a failure of its own.
fn emit_error_code_enum(out: &mut String, e: &EnumType) {
    let registry = &e.name;
    out.push('\n');
    emit_doc_rust(out, "", &e.description);
    out.push_str("#[allow(non_camel_case_types, reason = \"variants keep the SDL's spelling\")]\n");
    out.push_str(
        "#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ::serde::Serialize, \
         ::serde::Deserialize, ::treat::ApiErrorCode, ::treat::rpc::WireEnum)]\n",
    );
    let _ = writeln!(out, "pub enum {registry} {{");
    for v in &e.values {
        emit_doc_rust(out, "    ", &v.description);
        // The default message travels with the code, so a handler that has
        // nothing to add does not restate it at the call site.
        let _ = writeln!(out, "    #[message({:?})]", default_message(&v.name));
        if let Some(tag) = &v.variant_tag {
            let _ = writeln!(out, "    #[code({tag:?})]");
            let _ = writeln!(out, "    #[serde(rename = {tag:?})]");
        }
        let _ = writeln!(out, "    {},", v.name);
    }
    out.push_str("}\n");

    let _ = writeln!(
        out,
        "\nimpl ::treat::rpc::RuntimeCodes for {registry} {{\n    \
         const INVALID_BODY: Self = Self::{};\n    \
         const INTERNAL_ERROR: Self = Self::{};\n}}",
        crate::semantic::CODE_INVALID_BODY,
        crate::semantic::CODE_INTERNAL_ERROR
    );
}

/// Human-readable default for a code: `clan_not_found` -> `clan not found`.
///
/// A namespaced code (`portal_db__clan_not_found`) reads as its last segment,
/// `clan not found`: the namespace says where the code comes from, not what
/// went wrong, and the `__` would otherwise leave a double space.
fn default_message(code: &str) -> String {
    let name = code
        .rsplit("__")
        .find(|segment| !segment.is_empty())
        .unwrap_or(code);
    name.replace('_', " ")
}

/// One error set per operation, in a single `error_set!` declaration.
///
/// The flat registry says what the API can return; it does not say what *this*
/// operation can, so without a per-operation set a handler could answer with any
/// code in it and `@throws` would be documentation nobody checks.
///
/// A variant is named after the registry code and carries that code's wire
/// name as its `#[code]` — the SDL spelling, or its `@variant(tag:)` — so a set
/// writes into the envelope exactly what the registry value would. The set
/// derives `ApiErrorCode` (its `Display`, which `error_set`'s own would
/// otherwise claim — hence `#[skip(Display)]`) and `WireEnum`, which is what
/// `Narrow` reads a flat `Failure`'s code into it by; its `#[undeclared]`
/// variant, `internal_error`, is what it answers for a code it does not
/// declare.
///
/// A set composed from these in a project's own `error_set!` copies the
/// variants' attributes, so it must carry the same `#[skip(Display)]` and
/// derives.
fn emit_error_sets(out: &mut String, api: &Api) {
    let has_operations = all_operations(api).next().is_some();
    if !has_operations {
        return;
    }
    out.push_str(
        "\n// Each set is one operation's `@throws`, plus the codes the runtime emits\n\
         // before the operation runs. Anything else is a compile error at the handler.\n",
    );
    out.push_str("::error_set::error_set! {\n");
    for service in &api.services {
        for op in &service.operations {
            emit_operation_error_set(out, api, service, op);
        }
    }
    out.push_str("}\n");
}

/// One operation's set, as an `error_set!` member.
fn emit_operation_error_set(out: &mut String, api: &Api, service: &Service, op: &Operation) {
    let _ = writeln!(
        out,
        "    /// Error codes `{}.{}` may answer with.",
        service.name, op.name
    );
    out.push_str("    #[skip(Display)]\n");
    out.push_str("    #[derive(Clone, Copy, ::treat::ApiErrorCode, ::treat::rpc::WireEnum)]\n");
    let _ = writeln!(
        out,
        "    {} := {{",
        codes_type_name(&service.name, &op.name)
    );
    for code in op.error_codes() {
        let undeclared = match code == crate::semantic::CODE_INTERNAL_ERROR {
            true => "#[undeclared] ",
            false => "",
        };
        let _ = writeln!(
            out,
            "        {undeclared}#[code({:?})] {},",
            api.error_code_wire_name(code),
            code_type_name(code)
        );
    }
    out.push_str("    }\n");
}

/// The Rust type standing for a wire code: `clan_not_found` -> `ClanNotFound`.
fn code_type_name(code: &str) -> String {
    pascal_case(code)
}

fn emit_object(out: &mut String, api: &Api, o: &ObjectType) {
    out.push('\n');
    emit_doc_rust(out, "", &o.description);
    out.push_str("#[derive(Debug, Clone, PartialEq, ::serde::Serialize, ::serde::Deserialize)]\n");
    let _ = writeln!(out, "pub struct {} {{", o.name);
    let sibling = api.sibling_tagged(o);
    for f in &o.fields {
        match sibling {
            // The enum writes and reads the tag itself, so a field of its own
            // would claim the same key twice.
            Some((_, _, tag)) if f.name == tag => {}
            Some((held, _, _)) if held.name == f.name => {
                emit_doc_rust(out, "    ", &f.description);
                out.push_str("    #[serde(flatten)]\n");
                emit_field_body(out, api, &o.name, f);
            }
            _ => emit_struct_field(out, api, &o.name, f),
        }
    }
    out.push_str("}\n");
}

fn emit_input_object(out: &mut String, api: &Api, i: &InputObjectType) {
    out.push('\n');
    emit_doc_rust(out, "", &i.description);

    if i.one_of {
        // Exactly-one-of maps to serde's default representation: renamed
        // newtype variants. On the wire that is exactly the one-key object
        // {"card": {...}}, and deserialization of the tagged form rejects a
        // body that names two arms or any arm that was not chosen —
        // exclusivity is the wire format, not a check. `#[serde(untagged)]`
        // would change what matches and must not be added.
        out.push_str(
            "#[derive(Debug, Clone, PartialEq, ::serde::Serialize, ::serde::Deserialize)]\n",
        );
        let _ = writeln!(out, "pub enum {} {{", i.name);
        for f in &i.fields {
            emit_doc_rust(out, "    ", &f.description);
            let _ = writeln!(out, "    #[serde(rename = {:?})]", f.name);
            let inner = one_of_arm_type(api, &i.name, &f.ty);
            let _ = writeln!(out, "    {}({inner}),", pascal_case(&f.name));
        }
        out.push_str("}\n");
        emit_one_of_validate(out, api, i);
        return;
    }

    out.push_str("#[derive(Debug, Clone, PartialEq, ::serde::Serialize, ::serde::Deserialize, ::validator::Validate)]\n");
    let _ = writeln!(out, "pub struct {} {{", i.name);
    for f in &i.fields {
        emit_struct_field_validated(out, api, &i.name, f);
    }
    out.push_str("}\n");
}

/// Whether a field — an `@oneOf` arm or an ordinary one — is an input object,
/// which validates itself.
fn is_nested_arm(api: &Api, f: &Field) -> bool {
    matches!(
        api.find_type(f.ty.base_name()),
        Some(ApiType::InputObject(_))
    )
}

/// `Validate` for an `@oneOf` input, which is an enum and so out of reach of
/// the derive. The chosen arm is checked exactly like an ordinary field of the
/// same name: a nested input object validates itself, and a scalar is measured
/// through the same validator traits a `#[validate(...)]` attribute calls —
/// with the same rules every other target resolves through
/// `resolved_constraints`, so browser and server keep checking one check.
fn emit_one_of_validate(out: &mut String, api: &Api, i: &InputObjectType) {
    let _ = writeln!(out, "\nimpl ::validator::Validate for {} {{", i.name);
    out.push_str("    fn validate(&self) -> Result<(), ::validator::ValidationErrors> {\n");
    out.push_str("        match self {\n");
    for f in &i.fields {
        let variant = pascal_case(&f.name);
        if is_nested_arm(api, f) {
            // Through the trait by its path, which no import has to bring
            // into scope; a boxed arm hands over the input inside the box.
            let value = match refers_back(api, &i.name, &f.ty) {
                true => "&**value",
                false => "value",
            };
            let _ = writeln!(
                out,
                "            Self::{variant}(value) => ::validator::Validate::validate({value}),"
            );
            continue;
        }
        let rules = arm_validation_rules(api, &i.name, f);
        if rules.is_empty() {
            let _ = writeln!(out, "            Self::{variant}(_) => Ok(()),");
            continue;
        }
        let _ = writeln!(out, "            Self::{variant}(value) => {{");
        out.push_str("                let mut errors = ::validator::ValidationErrors::new();\n");
        for rule in rules {
            out.push_str(&rule);
        }
        out.push_str(
            "                if errors.is_empty() {\n                    Ok(())\n                } else {\n                    Err(errors)\n                }\n",
        );
        out.push_str("            }\n");
    }
    out.push_str("        }\n    }\n}\n");
}

/// One check an `@oneOf` arm runs against its value, in the shape the
/// `Validate` derive would emit for an ordinary field of the same name: the
/// same trait call, the same message, and the arm's key as the field name in
/// the resulting errors, so a violation points at the arm.
///
/// Each check binds its outcome before the `if` that adds the error: the
/// condition reads a value, it does not run the check.
fn arm_validation_rules(api: &Api, owner: &str, f: &Field) -> Vec<String> {
    let c = api.resolved_constraints(f);
    let mut rules: Vec<String> = Vec::new();
    let failed = |binding: &str, check: String, code: &str, message: String| -> String {
        format!(
            "                let {binding} = {check};\n                \
             if !{binding} {{\n                    \
             let mut error = ::validator::ValidationError::new({code:?});\n                    \
             error.message = Some({message}.into());\n                    \
             errors.add({:?}, error);\n                }}\n",
            f.name
        )
    };

    if c.min_length.is_some() || c.max_length.is_some() {
        let mut args = vec!["value".to_string()];
        args.extend(option_args(c.min_length, c.max_length, |v| v.to_string()));
        args.push("None".to_string());
        let message = format!(
            "length must be {}",
            describe(c.min_length, c.max_length, |v| v.to_string())
        );
        let check = format!(
            "::validator::ValidateLength::validate_length({})",
            args.join(", ")
        );
        rules.push(failed("length_ok", check, "length", format!("{message:?}")));
    }

    if c.minimum.is_some() || c.maximum.is_some() {
        let render = |v: f64| number_literal(api, f, v);
        let mut args = vec!["value".to_string()];
        args.extend(option_args(c.minimum, c.maximum, render));
        args.extend(["None".to_string(), "None".to_string()]);
        let message = format!("must be {}", describe(c.minimum, c.maximum, render));
        let check = format!(
            "::validator::ValidateRange::validate_range({})",
            args.join(", ")
        );
        rules.push(failed("range_ok", check, "range", format!("{message:?}")));
    }

    let static_name = pattern_static_name(api, owner, f);
    if let Some(name) = static_name {
        let check = format!("::treat::rpc::validate::matches_pattern(&{name}, value)");
        let message = format!("format!(\"must match {{{}}}\")", pattern_source_name(&name));
        rules.push(failed("matched", check, "regex", message));
    }

    rules
}

/// `Some(a)`, `Some(b)` as call arguments, `None` for a bound not declared.
/// Positional on purpose: Rust does not allow a positional argument after a
/// named one, and the validator calls end in more of them.
fn option_args<T: Copy>(
    min: Option<T>,
    max: Option<T>,
    render: impl Fn(T) -> String,
) -> [String; 2] {
    let arg = |bound: Option<T>| match bound {
        Some(v) => format!("Some({})", render(v)),
        None => "None".to_string(),
    };
    [arg(min), arg(max)]
}

/// An input field, which carries its constraints as `#[validate(..)]`.
fn emit_struct_field_validated(out: &mut String, api: &Api, owner: &str, f: &Field) {
    emit_doc_rust(out, "    ", &f.description);
    emit_validate_attr(out, api, owner, f);
    emit_field_body(out, api, owner, f);
}

fn emit_struct_field(out: &mut String, api: &Api, owner: &str, f: &Field) {
    emit_doc_rust(out, "    ", &f.description);
    emit_field_body(out, api, owner, f);
}

fn emit_field_body(out: &mut String, api: &Api, owner: &str, f: &Field) {
    let rust_name = snake_case(&f.name);
    if rust_name != f.name {
        let _ = writeln!(out, "    #[serde(rename = {:?})]", f.name);
    }
    let explicit_nulls =
        matches!(api.find_type(owner), Some(ApiType::Object(o)) if o.explicit_nulls);
    if f.ty.is_nullable() && explicit_nulls {
        // `@explicitNulls`: None is written as `null`, and read from `null` or
        // from an absent key alike.
        out.push_str("    #[serde(default)]\n");
    } else if f.ty.is_nullable() {
        // Absent and null are treated alike, and None is omitted on the wire
        // so optional fields do not appear as explicit nulls.
        out.push_str("    #[serde(default, skip_serializing_if = \"Option::is_none\")]\n");
    } else if api.is_binary(f.ty.base_name()) {
        // A raw request's payload is the body, never a member of the object the
        // other fields travel in. `default` is what lets the server build the
        // input from the query string before assigning the body it just read;
        // `skip_serializing` is what keeps the client from putting megabytes of
        // byte array in the query string it builds from the same struct.
        out.push_str("    #[serde(default, skip_serializing)]\n");
    }
    let _ = writeln!(
        out,
        "    pub {rust_name}: {},",
        field_rust_type(api, owner, &f.ty)
    );
}

/// Name of the `LazyLock<Regex>` static holding a field's `@pattern`.
///
/// A pattern declared on a scalar is compiled once and shared by every field of
/// that type; one written on a field gets its own static.
fn pattern_static_name(api: &Api, owner: &str, f: &Field) -> Option<String> {
    if f.constraints.pattern.is_some() {
        // Prefixed to keep the two sources of a static apart: a scalar named
        // `UserName` and a field `User.name` both used to reach for
        // `USER_NAME_PATTERN`, and the file declared it twice.
        return Some(format!(
            "FIELD_{}_{}_PATTERN",
            snake_case_raw(owner).to_uppercase(),
            snake_case_raw(&f.name).to_uppercase()
        ));
    }
    let scalar = api.find_scalar(f.ty.base_name())?;
    if matches!(f.ty, TypeRef::List { .. }) || scalar.constraints.pattern.is_none() {
        return None;
    }
    Some(scalar_pattern_name(&scalar.name))
}

/// Name of the static holding a scalar's own `@pattern`, shared by every field
/// of that type. Spelled here rather than at both ends: the definition and the
/// reference are emitted by different functions, and a name built twice is a
/// name that can be built two ways.
fn scalar_pattern_name(scalar: &str) -> String {
    format!("SCALAR_{}_PATTERN", snake_case_raw(scalar).to_uppercase())
}

/// Name of the `const` holding a pattern static's source text.
fn pattern_source_name(static_name: &str) -> String {
    match static_name.strip_suffix("_PATTERN") {
        Some(stem) => format!("{stem}_SOURCE"),
        None => format!("{static_name}_SOURCE"),
    }
}

/// The pattern statics something reads, each with whether a
/// `#[validate(custom(..))]` attribute names its check function (an `@oneOf`
/// arm calls `matches_pattern` itself).
///
/// A static or a check no field reads would be dead code, which the generated
/// file is not allowed to carry: a scalar's pattern is emitted only where an
/// input field of that type is checked against it.
fn used_pattern_statics(api: &Api) -> std::collections::BTreeMap<String, bool> {
    let mut used = std::collections::BTreeMap::new();
    for ty in &api.types {
        let ApiType::InputObject(i) = ty else {
            continue;
        };
        for f in &i.fields {
            if i.one_of && is_nested_arm(api, f) {
                continue;
            }
            let Some(name) = pattern_static_name(api, &i.name, f) else {
                continue;
            };
            let by_attribute = used.entry(name).or_insert(false);
            *by_attribute |= !i.one_of;
        }
    }
    used
}

/// Every `@pattern` an input is checked against, as its source text, a lazily
/// compiled regex, and — for a field that names it in `#[validate(..)]` — the
/// check that reads it.
///
/// Compiling at first use rather than per request is what makes a pattern
/// constraint free to declare. A pattern that does not compile is kept as
/// `None` and rejects every value: `graphql-rpcgen` compiles each one before
/// emitting it, so that branch is a generator bug, and rejecting is the safe
/// side of one — the alternative admits input nothing checked.
///
/// The source is a `const` of its own, which the regex is compiled from and the
/// message a failure carries quotes through `format!`.
fn emit_pattern_statics(out: &mut String, api: &Api) {
    let used = used_pattern_statics(api);
    let mut statics: Vec<(String, String)> = Vec::new();

    for ty in &api.types {
        if let ApiType::Scalar(s) = ty {
            if let Some(pattern) = &s.constraints.pattern {
                statics.push((scalar_pattern_name(&s.name), pattern.clone()));
            }
        }
        let ApiType::InputObject(i) = ty else {
            continue;
        };
        for f in &i.fields {
            // Both halves come from the same declaration: `pattern_static_name`
            // answers for exactly the fields that carry a `@pattern`.
            let static_name = pattern_static_name(api, &i.name, f);
            if let (Some(pattern), Some(name)) = (&f.constraints.pattern, static_name) {
                statics.push((name, pattern.clone()));
            }
        }
    }

    for (name, pattern) in statics {
        let Some(by_attribute) = used.get(&name) else {
            continue;
        };
        let source = pattern_source_name(&name);
        let _ = writeln!(out, "\nconst {source}: &str = {pattern:?};");
        let _ = writeln!(
            out,
            "static {name}: ::std::sync::LazyLock<Option<::treat::rpc::validate::regex::Regex>> = \
             ::std::sync::LazyLock::new(|| ::treat::rpc::validate::regex::Regex::new({source}).ok());"
        );
        if !by_attribute {
            continue;
        }
        let _ = writeln!(
            out,
            "\n/// `#[validate(custom(..))]` for every field carrying `{name}`.\n\
             fn {}<T>(value: &T) -> Result<(), ::validator::ValidationError>\n\
             where\n    T: ::validator::ValidateRegex + ?Sized,\n{{\n    \
             let matched = ::treat::rpc::validate::matches_pattern(&{name}, value);\n    \
             if matched {{\n        return Ok(());\n    }}\n    \
             let mut error = ::validator::ValidationError::new(\"regex\");\n    \
             error.message = Some(format!(\"must match {{{source}}}\").into());\n    \
             Err(error)\n}}",
            pattern_check_name(&name)
        );
    }
}

/// Name of the `custom` function that checks one `@pattern` static.
fn pattern_check_name(static_name: &str) -> String {
    format!("check_{}", static_name.to_lowercase())
}

/// `#[validate(..)]` for one input field, one attribute per rule, or nothing
/// when it has no rules.
///
/// One rule per attribute, which `validator` reads like one list of them.
///
/// The message travels to the client verbatim: `treat` turns each violation into
/// one `errors[]` entry with a JSON Pointer to the field, so a caller learns
/// which field was wrong and what was expected without reading the SDL.
fn emit_validate_attr(out: &mut String, api: &Api, owner: &str, f: &Field) {
    let c = api.resolved_constraints(f);

    // Every message is written with `{:?}`, so a pattern's own backslashes reach
    // the generated file as a Rust string literal rather than as escapes Rust
    // tries to interpret itself.
    if c.min_length.is_some() || c.max_length.is_some() {
        let mut args = bound_args(c.min_length, c.max_length, |v| v.to_string());
        let noun = match matches!(f.ty, TypeRef::List { .. }) {
            true => "element count",
            false => "length",
        };
        let message = format!(
            "{noun} must be {}",
            describe(c.min_length, c.max_length, |v| v.to_string())
        );
        args.push(format!("message = {message:?}"));
        let _ = writeln!(out, "    #[validate(length({}))]", args.join(", "));
    }

    if c.minimum.is_some() || c.maximum.is_some() {
        let render = |v: f64| number_literal(api, f, v);
        let mut args = bound_args(c.minimum, c.maximum, render);
        let message = format!("must be {}", describe(c.minimum, c.maximum, render));
        args.push(format!("message = {message:?}"));
        let _ = writeln!(out, "    #[validate(range({}))]", args.join(", "));
    }

    // `custom` rather than `regex(path = ..)`: the static is an `Option`, so
    // that a pattern which failed to compile rejects instead of panicking, and
    // `validator`'s own regex rule cannot read one.
    let static_name = pattern_static_name(api, owner, f);
    if let Some(name) = static_name {
        let function = format!("function = {}", pattern_check_name(&name));
        let _ = writeln!(out, "    #[validate(custom({function}))]");
    }

    // An input nested inside another carries its own rules, and `nested` is what
    // walks into it — including through a list, so `/list/3/name` is reachable.
    if matches!(
        api.find_type(f.ty.base_name()),
        Some(ApiType::InputObject(_))
    ) {
        out.push_str("    #[validate(nested)]\n");
    }
}

/// `min = a`, `max = b`, keeping only the bounds that were declared.
fn bound_args<T: Copy>(
    min: Option<T>,
    max: Option<T>,
    render: impl Fn(T) -> String,
) -> Vec<String> {
    [min.map(|v| ("min", v)), max.map(|v| ("max", v))]
        .into_iter()
        .flatten()
        .map(|(key, v)| format!("{key} = {}", render(v)))
        .collect()
}

/// The bounds in prose, for the message a caller reads.
fn describe<T: Copy>(min: Option<T>, max: Option<T>, render: impl Fn(T) -> String) -> String {
    match (min, max) {
        (Some(a), Some(b)) => format!("between {} and {}", render(a), render(b)),
        (Some(a), None) => format!("at least {}", render(a)),
        (None, Some(b)) => format!("at most {}", render(b)),
        (None, None) => String::new(),
    }
}

/// A `@range` bound spelled so it has the field's own Rust type.
///
/// `validator` compares the bound against the value directly, so an integer
/// field needs `0` and a floating-point one `0.0`: the literal is the type.
fn number_literal(api: &Api, f: &Field, v: f64) -> String {
    let float = api
        .find_scalar(f.ty.base_name())
        .is_some_and(|s| is_rust_float(&s.rust));
    match float {
        true => format!("{v:?}"),
        false => format!("{}", v as i64),
    }
}

/// Whether a scalar's Rust representation is one of the two floating-point
/// primitives, which decides both how a `@range` bound is spelled and which
/// traits a newtype over it can derive.
///
/// Named exactly rather than matched by prefix: a project mapping a scalar to
/// `foo::Bar` is not asking for a float, and the prefix test said it was.
fn is_rust_float(rust: &str) -> bool {
    matches!(rust, "f32" | "f64")
}

/// The type one `@oneOf` arm carries.
///
/// Non-null, because the chosen arm is present whatever the SDL field's
/// nullability says about the choice — that nullability describes the choice.
/// Boxed when the arm refers back to the enum, which is the same relief
/// [`field_rust_type`] gives an ordinary struct field: without it an input that
/// describes itself has no finite size.
fn one_of_arm_type(api: &Api, owner: &str, ty: &TypeRef) -> String {
    let inner = rust_type_non_null(api, ty);
    match refers_back(api, owner, ty) {
        true => format!("Box<{inner}>"),
        false => inner,
    }
}

/// Whether a field of `owner` carrying `ty` has to be boxed: when the type is
/// `owner` itself, or reaches it, and so would make `owner` infinitely sized.
fn refers_back(api: &Api, owner: &str, ty: &TypeRef) -> bool {
    let TypeRef::Named { name, .. } = ty else {
        return false;
    };
    let reaches = api.reaches(name, owner);
    name == owner || reaches
}

fn emit_union(out: &mut String, u: &UnionType) {
    out.push('\n');
    emit_doc_rust(out, "", &u.description);
    out.push_str("#[derive(Debug, Clone, PartialEq, ::serde::Serialize, ::serde::Deserialize)]\n");
    match &u.tagging {
        Tagging::Internal { tag } => {
            let _ = writeln!(out, "#[serde(tag = {tag:?})]");
        }
        // Adjacently tagged, and flattened into the holder by `emit_object`:
        // the tag and the member become two keys of the holding object.
        Tagging::Sibling { tag, content } => {
            let _ = writeln!(out, "#[serde(tag = {tag:?}, content = {content:?})]");
        }
    }
    let _ = writeln!(out, "pub enum {} {{", u.name);
    for member in &u.members {
        // Internally tagged newtype variants flatten the member's fields
        // alongside the tag, giving {"kind":"CardPayment","last4":"4242"};
        // adjacently tagged ones nest them under the content key.
        if member.tag != member.name {
            let _ = writeln!(out, "    #[serde(rename = {:?})]", member.tag);
        }
        let _ = writeln!(out, "    {}({}),", member.name, member.name);
    }
    out.push_str("}\n");
}

fn emit_service(
    out: &mut String,
    api: &Api,
    service: &Service,
    templates: &Templates,
) -> erris::Result<()> {
    let trait_name = format!("{}Service", service.name);
    let module = snake_case_raw(&service.name);

    out.push('\n');
    emit_doc_rust(out, "", &service.description);
    out.push_str("#[::async_trait::async_trait]\n");
    let _ = writeln!(
        out,
        "pub trait {trait_name}: Clone + Send + Sync + 'static {{"
    );
    // Only a service with something to guard names a context, so one that
    // guards nothing is the trait it always was.
    let names_context = service.operations.iter().any(|op| op.auth.has_context());
    if names_context {
        out.push_str(
            "    /// The caller, resolved from the request before a guarded operation runs.\n\
             \x20   ///\n\
             \x20   /// An associated type rather than a name from this crate: resolving a session\n\
             \x20   /// is the implementation's, and it lives above this one.\n\
             \x20   type Ctx: FromRequestContext;\n",
        );
    }
    for (index, op) in service.operations.iter().enumerate() {
        if index > 0 || names_context {
            out.push('\n');
        }
        emit_doc_rust(out, "    ", &op.description);
        if op.raw_request_stream {
            emit_stream_doc(out, api, op);
        }
        let error = codes_type_name(&service.name, &op.name);
        // The context comes first because it is the caller, not the request: it
        // reads as the subject of the call, and it is the parameter every
        // session-guarded operation shares. An operation with no `input`
        // argument takes no parameter for it: there is no placeholder value
        // for an implementation to receive and ignore. The streamed body comes
        // last, because it is what is left of the request once the input has
        // been read out of its URL.
        let mut params = vec!["&self".to_string()];
        if op.auth.has_context() {
            params.push("ctx: &Self::Ctx".to_string());
        }
        if let Some(input) = &op.input {
            params.push(format!("input: {}", rust_type(api, input)));
        }
        if op.raw_request_stream {
            params.push("body: ::treat::rpc::server::RawRequest".to_string());
        }
        // The failure half is `ApiError<Codes>`, which names the codes the
        // operation may use and is what `?` and treat's `wrap_api_error` family
        // produce. The success half is whatever this operation actually
        // answers with — see [`success_type`].
        let _ = writeln!(
            out,
            "    async fn {}({}) -> Result<{}, ::treat::ApiError<{error}>>;",
            snake_case(&op.name),
            params.join(", "),
            success_type(api, op)
        );
    }
    out.push_str("}\n");

    // The route list is assembled here rather than in the template: a flat
    // string context has nothing for a mustache section to iterate.
    let mut routes = String::new();
    for op in &service.operations {
        let _ = writeln!(
            routes,
            "        .route({:?}, ::axum::routing::{}({module}_{}::<S>))",
            op.path,
            routing_function(op),
            snake_case_raw(&op.name)
        );
    }

    out.push_str(&templates.router.render(&templates.vars(&[
        ("service", &service.name),
        ("module", &module),
        ("trait_name", &trait_name),
        ("routes", &routes),
    ]))?);

    for op in &service.operations {
        emit_handler(out, api, service, op, &trait_name, &module, templates)?;
    }

    Ok(())
}

/// One operation's handler, rendered from its form's template.
fn emit_handler(
    out: &mut String,
    api: &Api,
    service: &Service,
    op: &Operation,
    trait_name: &str,
    module: &str,
    templates: &Templates,
) -> erris::Result<()> {
    let handler = format!("{module}_{}", snake_case_raw(&op.name));
    let method = snake_case(&op.name);
    let form = form(op);
    let input = match &op.input {
        Some(input) => rust_type(api, input),
        None => String::new(),
    };
    let context = op.auth.has_context();
    // Empty for a form that reads none of them: rendering a variable a
    // template never mentions costs nothing, and it keeps one set of vars per
    // form.
    let wire = |suffix: &str, present: bool| match present {
        true => wire_static(service, op, suffix),
        false => String::new(),
    };
    let fields = wire("FIELDS", has_fields_table(op));
    let spec = wire("HTTP", op.http.is_some());
    let types = wire("TYPES", op.raw_request.is_some());
    let body_field = api.body_field(op);
    let body_into = match body_field.and_then(|f| api.find_scalar(f.ty.base_name())) {
        // The runtime reads bytes; a newtype over them takes them through its
        // `From`.
        Some(scalar) if scalar.rust_newtype => ".into()",
        _ => "",
    };
    let body_field = body_field.map(|f| snake_case(&f.name)).unwrap_or_default();

    // The extractors, in the order axum requires: the state and the parts
    // first, whatever consumes the body last.
    let mut params = vec!["::axum::extract::State(service): ::axum::extract::State<S>".to_string()];
    if context {
        params.push(match form {
            // An `@http` handler takes the extraction's `Result` rather than
            // letting axum answer the refusal, so it can give the refusal the
            // status a caller of a route fixed elsewhere reads.
            Form::Http => {
                "ctx: Result<::treat::rpc::server::Ctx<S::Ctx>, ::treat::rpc::server::Refusal>"
                    .to_string()
            }
            _ => "::treat::rpc::server::Ctx(ctx): ::treat::rpc::server::Ctx<S::Ctx>".to_string(),
        });
    }
    match form {
        Form::Http | Form::Subscription if op.input.is_none() => {}
        Form::Json | Form::Http | Form::RawRequest => {
            params.push("request: ::axum::extract::Request".to_string());
        }
        Form::Subscription => {
            params.push("::axum::extract::RawQuery(query): ::axum::extract::RawQuery".to_string());
        }
        Form::RawRequestStream => params.push("raw: ::treat::rpc::server::RawRequest".to_string()),
    }

    let read_input = read_input(op, form, &input, &fields, &spec, &types);
    // A buffered raw request's bytes, read once the query string has been:
    // the type checked (`415`), the size bounded (`413`).
    let read_body = match form {
        Form::RawRequest => format!(
            "    input.{body_field} = Rt::raw_body(request, {types}, \
             ::treat::rpc::server::RAW_BODY_LIMIT).await?{body_into};\n"
        ),
        _ => String::new(),
    };

    let mut call_args: Vec<String> = Vec::new();
    if context {
        call_args.push("&ctx".to_string());
    }
    if op.input.is_some() {
        call_args.push("input".to_string());
    }
    if form == Form::RawRequestStream {
        call_args.push("raw".to_string());
    }

    // An `@http` operation answers as its spec says, enveloped or bare.
    let respond = match (form, op.raw_response.is_some()) {
        (Form::Subscription, _) => "Rt::respond_events(result)".to_string(),
        (Form::Http, _) => format!("Rt::respond_http(&{spec}, result)"),
        (_, true) => "Rt::respond_raw(result)".to_string(),
        (_, false) => "Rt::respond(result)".to_string(),
    };

    let template = match form {
        Form::RawRequest => &templates.handler_raw_request,
        _ => &templates.handler,
    };
    out.push_str(&template.render(&templates.vars(&[
        ("handler", &handler),
        ("trait_name", trait_name),
        ("input", &input),
        ("output", &rust_type(api, &op.output)),
        ("method", &method),
        ("path", &op.path),
        ("fields", &fields),
        ("spec", &spec),
        ("types", &types),
        ("body_field", &body_field),
        ("context", flag(context)),
        ("call_args", &call_args.join(", ")),
        ("params", &params.join(", ")),
        ("access_guard", &access_guard(api, op)?),
        ("read_input", &read_input),
        ("read_body", &read_body),
        ("respond", &respond),
    ]))?);
    Ok(())
}

/// The statements reading an operation's input, by form; empty for an
/// operation without one that reads nothing.
///
/// An RPC operation without an input still reads its body, which must be JSON:
/// a caller sending anything else hears that its request is malformed. An
/// `@http` or a subscription without one reads nothing, since a `GET` reaching
/// it has no body to check. A streamed raw request checks its body's type
/// before anything else: the service reads the body, and the runtime never
/// does.
fn read_input(
    op: &Operation,
    form: Form,
    input: &str,
    fields: &str,
    spec: &str,
    types: &str,
) -> String {
    match form {
        Form::Json if op.input.is_some() => {
            "    let input = Rt::json_input(request).await?;\n".to_string()
        }
        Form::Json => "    Rt::no_input(request).await?;\n".to_string(),
        Form::Http if op.input.is_some() => {
            format!("    let input = Rt::http_input(request, &{spec}).await?;\n")
        }
        Form::Subscription if op.input.is_some() => {
            format!("    let input = Rt::query_input(query.as_deref(), {fields})?;\n")
        }
        Form::Http | Form::Subscription => String::new(),
        // The type is named because the body field is assigned before the
        // input reaches a call that would say it.
        Form::RawRequest => format!(
            "    let mut input: {input} = Rt::input_from_query(request.uri().query(), {fields})?;\n"
        ),
        Form::RawRequestStream => format!(
            "    Rt::require_content_type(raw.headers(), {types})?;\n    \
             let input = Rt::query_input(raw.query(), {fields})?;\n"
        ),
    }
}

/// What a streamed operation's implementation has to know that its signature
/// does not say: where the payload went, and that nothing bounds it.
fn emit_stream_doc(out: &mut String, api: &Api, op: &Operation) {
    if op.description.is_some() {
        out.push_str("    ///\n");
    }
    let field = api
        .body_field(op)
        .map(|f| snake_case(&f.name))
        .unwrap_or_default();
    let _ = writeln!(
        out,
        "    /// The request body arrives unread as `body`; `input.{field}` is always empty.\n    \
         /// No size limit is applied to it: this method must bound it — see\n    \
         /// [`treat::rpc::server::RawRequest`]."
    );
}

/// What a service method hands back when it succeeds.
///
/// Three shapes, because three things can be answered with: a stream of events,
/// a body of bytes, or the JSON envelope every other operation returns.
fn success_type(api: &Api, op: &Operation) -> String {
    if op.kind == OperationKind::Subscription {
        // The output type is one event, not the stream, so the stream is named
        // here rather than in the SDL.
        return format!(
            "::treat::rpc::server::EventStream<{}>",
            rust_type(api, &op.output)
        );
    }
    if !op.enveloped() {
        // `@http(envelope: false)`: the value is the body, so the service
        // returns the value itself and the handler writes it as JSON.
        return rust_type(api, &op.output);
    }
    if op.raw_response.is_some() {
        // The declared type is the binary scalar, which says the body is bytes
        // and nothing more; `RawBody` is what carries them plus the headers
        // that describe them.
        return "::treat::rpc::server::RawBody".to_string();
    }
    format!("::treat::ApiResponse<{}>", rust_type(api, &op.output))
}

/// The access gate, emitted between the extractors and the input read: the
/// role or the permission `@auth` demands, handed to the context's check.
///
/// It runs before the request's body is read, in every handler form: a caller
/// without access hears that, never what its body got wrong, and no body of
/// its is parsed.
///
/// The permission check is awaited and the role check is not: a role is read
/// off the session, while which roles hold a permission is the
/// implementation's data, which it may have to fetch.
///
/// An `Api` the compiler would refuse — a check without a session, or without
/// the set naming it — is an error rather than a handler without the check:
/// `generate` is public, and an `Api` built or changed by hand reaches it
/// without the compiler's checks.
///
/// An `@http` handler's gate starts by unwrapping the session the extractor
/// left as a `Result`, and both refusals get the status an `@http` caller
/// reads unless they carry one: `401` for no session (`Rt::http_session`),
/// `403` for no access (`Rt::http_access`). An RPC caller reads `errors[]`, so
/// an RPC handler applies the check with a plain `?`.
fn access_guard(api: &Api, op: &Operation) -> erris::Result<String> {
    let http = op.http.is_some();
    let session = match http && op.auth.has_context() {
        true => "    let ctx = Rt::http_session(ctx)?;\n",
        false => "",
    };
    let Some(access) = &op.access else {
        return Ok(session.to_string());
    };
    let unchecked = |why: &str| {
        erris::report!(
            "`{}`: @auth demands a check, but {why}; the handler would run without it",
            op.operation_id
        )
    };
    if !op.auth.has_context() {
        return Err(unchecked("the operation has no session to check it on"));
    }
    let (method, arg, tail): (&str, String, &str) = match access {
        Access::Role(role) => {
            let Some(roles) = &api.roles else {
                return Err(unchecked("the API has no @roles enum"));
            };
            (
                "require_role",
                format!("{}::{}", roles.enum_name, pascal_case(role)),
                "",
            )
        }
        Access::Permission(permission) => {
            let Some(permissions) = &api.permissions else {
                return Err(unchecked("the API has no @permissions enum"));
            };
            (
                "require_permission",
                format!("{}::{}", permissions.enum_name, pascal_case(permission)),
                ".await",
            )
        }
    };
    let check = match http {
        true => {
            format!("    let access = ctx.{method}({arg}){tail};\n    Rt::http_access(access)?;\n")
        }
        false => format!("    ctx.{method}({arg}){tail}?;\n"),
    };
    Ok(format!("{session}{check}"))
}

/// `FromRequestContext::require_role`, for an API with a `@roles` enum.
///
/// Its contract is the standard's CheckAccess narrowed to what `@auth` can see:
/// the permission is the operation, already assigned to `role` in the SDL, so
/// what remains is whether the session's roles reach it.
fn require_role_method(api: &Api) -> String {
    let Some(roles) = &api.roles else {
        return String::new();
    };
    format!(
        "\n    /// Whether this caller is authorized for `role`, as `@auth(role:)` demands:\n    \
         /// whether a role active in the session is `role` or inherits it — see\n    \
         /// [`{name}::authorizes`].\n    \
         ///\n    \
         /// Returns the refusal to answer with when it is not, so the policy — which\n    \
         /// code, which message — stays with the project rather than being fixed\n    \
         /// here: an `ApiError` or a `Response`, `.into()`. Only called for an\n    \
         /// operation that declares a role.\n    \
         fn require_role(&self, role: {name}) -> Result<(), ::treat::rpc::server::Refusal>;\n",
        name = roles.enum_name,
    )
}

/// `FromRequestContext::require_permission`, for an API with a `@permissions`
/// enum.
///
/// The standard's CheckAccess with the permission named by the SDL and the
/// rest — which roles hold it, which of them the session has active — left to
/// the implementation. Async, since that assignment is data the implementation
/// keeps, and reading it may mean a round trip.
fn require_permission_method(api: &Api) -> String {
    let Some(permissions) = &api.permissions else {
        return String::new();
    };
    let codes = permissions
        .codes
        .iter()
        .map(|code| format!("`{code}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let separator = match api.roles.is_some() {
        true => "\n",
        false => "",
    };
    format!(
        "{separator}\n    /// Whether this caller holds `permission`, as `@auth(permission:)` demands.\n    \
         /// Which roles hold a permission is the implementation's to decide.\n    \
         ///\n    \
         /// Returns the refusal to answer with when it is not, with one of the\n    \
         /// codes `@permissions(codes:)` declares ({codes}), which every guarded\n    \
         /// operation lists. Only called for an operation that declares a permission.\n    \
         fn require_permission(&self, permission: {name}) -> impl ::std::future::Future<Output = \
         Result<(), ::treat::rpc::server::Refusal>> + Send;\n",
        name = permissions.enum_name
    )
}

/// The `treat::rpc::QueryField` constructor for the kind a field's text is
/// read as: the Rust spelling of [`Api::query_kind`].
fn query_kind(api: &Api, f: &Field) -> &'static str {
    match api.query_kind(f) {
        QueryKind::Str => "str",
        QueryKind::Int => "int",
        QueryKind::Float => "float",
        QueryKind::Bool => "bool",
        QueryKind::Json => "json",
    }
}

/// Field type inside `owner`, inserting `Box` where the reference would make
/// the struct infinitely sized.
fn field_rust_type(api: &Api, owner: &str, ty: &TypeRef) -> String {
    let needs_box = refers_back(api, owner, ty);

    if !needs_box {
        return rust_type(api, ty);
    }
    let inner = rust_type_non_null(api, ty);
    if ty.is_nullable() {
        format!("Option<Box<{inner}>>")
    } else {
        format!("Box<{inner}>")
    }
}
