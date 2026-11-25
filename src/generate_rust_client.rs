//! Rust client emitter: a typed method per operation over `treat`'s
//! `RpcClient`.
//!
//! The client is a separate target from the server because its dependencies
//! are: it needs an HTTP client, not a web framework. A project consuming an
//! API it does not serve generates `rust_types` + `rust_client` and compiles
//! neither `axum` nor `treat`'s server half.
//!
//! Operations are grouped by service, matching the TypeScript client, so
//! `client.clans().create(..)` reads the same in both languages.
//!
//! Everything schema-independent — sending a request, reading the envelope,
//! telling a refusal from a success, the SSE reader — is
//! `treat::rpc::client::RpcClient`'s, so a generated method is one call into
//! it, naming the operation's path (or an `@http` operation's `HttpSpec`), its
//! `wire` query table where it writes its input into a URL, and its code set
//! through the return type. The types a caller names (`ClientError`,
//! `ApiErrors`, ...) are re-exported from the generated module, so their paths
//! are the ones they always were; the generated code itself names every one by
//! its absolute path, and imports nothing an SDL type could be spelt like.

use std::fmt::Write;

use crate::config::Config;
use crate::emit_common::{emit_doc_rust, reexport, rust_type, HEADER};
use crate::generate_rust::wire_static;
use crate::ir::*;
use crate::naming::{codes_type_name, pascal_case, snake_case};
use crate::template::{Template, Vars};

const DEFAULT_PREAMBLE: &str = include_str!("../templates/rust/client_preamble.mustache");
/// Every method form but a raw request: the signature and the one call.
const DEFAULT_METHOD: &str = include_str!("../templates/rust/client_method.mustache");
/// A raw request sends a field of its input as the body, so it takes it out
/// of the input first.
const DEFAULT_METHOD_RAW_REQUEST: &str =
    include_str!("../templates/rust/client_method_raw_request.mustache");
/// Default name of the generated client struct.
const DEFAULT_CLIENT_NAME: &str = "ApiClient";

/// Variables the client preamble may reference, besides `template_vars`.
///
/// `reexports` is computed so the file re-exports what the API's methods
/// answer with and nothing the API declares a type of.
const PREAMBLE_VARS: [&str; 4] = [
    "error_code_enum",
    "types_import",
    "client_name",
    "reexports",
];
/// Variables the method templates may reference, besides `template_vars`.
///
/// `error` names the operation's own code enum, which is what the call fails
/// with — the flat registry would let a caller match on codes this operation
/// never returns. What differs between the forms is computed as plain text:
/// the method's parameters, what it answers with, the `RpcClient` method it
/// calls and that call's arguments.
const METHOD_VARS: [&str; 13] = [
    "method",
    "input",
    "output",
    "path",
    "operation_id",
    "error",
    // The input field a raw request sends as its body; empty for every other
    // form, which never mentions it.
    "body_field",
    // The `wire` statics the call names: the query table a subscription or a
    // raw request writes its input into the URL with, and an `@http`
    // operation's `HttpSpec`. Empty where unused.
    "fields",
    "spec",
    // The parameters, `&self` first, comma-separated; the success type of
    // the `ClientResult`; the `RpcClient` method and its arguments.
    "params",
    "success",
    "client_call",
    "call_args",
];

/// The method templates, compiled once per run.
struct Methods {
    method: Template,
    raw_request: Template,
}

pub fn generate(api: &Api, config: &Config) -> erris::Result<String> {
    let client = &config.rust_client;
    let extra_names: Vec<&str> = client.template_vars.keys().map(String::as_str).collect();

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

    let preamble = compile(
        DEFAULT_PREAMBLE,
        "<builtin client preamble>",
        &client.preamble_template,
        &PREAMBLE_VARS,
    )?;
    let methods = Methods {
        method: compile(
            DEFAULT_METHOD,
            "<builtin client method>",
            &client.method_template,
            &METHOD_VARS,
        )?,
        // The raw-request form has no config hook: it is the built-in or
        // nothing, until a project asks to replace it.
        raw_request: compile(
            DEFAULT_METHOD_RAW_REQUEST,
            "<builtin raw-request client method>",
            &None,
            &METHOD_VARS,
        )?,
    };

    let vars = |pairs: &[(&str, &str)]| -> Vars {
        let mut vars = client.template_vars.clone();
        for (key, value) in pairs {
            vars.insert((*key).to_string(), (*value).to_string());
        }
        vars
    };

    let operations = || api.services.iter().flat_map(|service| &service.operations);
    let has_operations = operations().next().is_some();
    let types_import = match client.resolved_types_path(&config.rust) {
        Some(path) if has_operations => format!("use {path}::*;\n"),
        _ => String::new(),
    };

    // What a caller of the methods names, re-exported so calling the API
    // takes the one import it always did.
    let mut reexports = vec![
        "reqwest",
        "ApiErrors",
        "ApiResponse",
        "ClientError",
        "ClientResult",
        "ErrorMessage",
        "ErrorSource",
    ];
    // Only with `rpc-stream`, which a client without a subscription or a raw
    // body does not turn on.
    let streams = operations().any(needs_stream);
    if streams {
        reexports.extend(["EventStream", "StreamError"]);
    }
    let reexports = reexport(api, "::treat::rpc::client", &reexports);

    let mut out = String::new();
    out.push_str(HEADER);
    out.push_str(&preamble.render(&vars(&[
        ("error_code_enum", crate::semantic::ERROR_CODE_ENUM),
        ("types_import", &types_import),
        ("client_name", DEFAULT_CLIENT_NAME),
        ("reexports", &reexports),
    ]))?);

    for service in &api.services {
        emit_service(&mut out, api, service, &methods, &vars)?;
    }

    Ok(out)
}

/// Whether an operation needs `treat`'s `rpc-stream`: an event stream back, or
/// bytes either way.
fn needs_stream(op: &Operation) -> bool {
    op.kind == OperationKind::Subscription || op.raw_request.is_some() || op.raw_response.is_some()
}

/// One service: an accessor on the client plus a borrowed group holding the
/// operations.
///
/// The group borrows the client rather than cloning it, so
/// `client.clans().create(..)` costs nothing beyond the call itself.
fn emit_service(
    out: &mut String,
    api: &Api,
    service: &Service,
    methods: &Methods,
    vars: &impl Fn(&[(&str, &str)]) -> Vars,
) -> erris::Result<()> {
    let group = format!("{}Operations", pascal_case(&service.name));
    let accessor = snake_case(&service.name);

    out.push('\n');
    emit_doc_rust(out, "", &service.description);
    let _ = writeln!(out, "pub struct {group}<'a> {{");
    out.push_str("    client: &'a ::treat::rpc::client::RpcClient,\n");
    out.push_str("}\n\n");

    let _ = writeln!(out, "impl {DEFAULT_CLIENT_NAME} {{");
    // The description belongs to the group type, which is where the operations
    // are; repeating it on the accessor would say the same thing twice on the
    // same screen.
    let _ = writeln!(out, "    /// The `{}` operations.", service.name);
    let _ = writeln!(out, "    pub fn {accessor}(&self) -> {group}<'_> {{");
    let _ = writeln!(out, "        {group} {{ client: &self.0 }}");
    out.push_str("    }\n}\n\n");

    let _ = writeln!(out, "impl {group}<'_> {{");
    for (index, op) in service.operations.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        emit_doc_rust(out, "    ", &op.description);
        emit_media_types_doc(out, op);
        emit_errors_doc(out, op);
        emit_method(out, api, service, op, methods, vars)?;
    }
    out.push_str("}\n");

    Ok(())
}

/// One operation's method.
fn emit_method(
    out: &mut String,
    api: &Api,
    service: &Service,
    op: &Operation,
    methods: &Methods,
    vars: &impl Fn(&[(&str, &str)]) -> Vars,
) -> erris::Result<()> {
    let method = snake_case(&op.name);
    let error = codes_type_name(&service.name, &op.name);
    let input = match &op.input {
        Some(input) => rust_type(api, input),
        None => String::new(),
    };
    let output = rust_type(api, &op.output);
    let body_field = api
        .body_field(op)
        .map(|f| snake_case(&f.name))
        .unwrap_or_default();
    let raw_request = op.raw_request.is_some();
    // A table only for an operation whose input travels in a query string;
    // one without an input writes nothing there.
    let url_input = op.kind == OperationKind::Subscription || raw_request;
    let fields = match (url_input, &op.input) {
        (true, Some(_)) => wire_static(service, op, "FIELDS"),
        (true, None) => "&[]".to_string(),
        (false, _) => String::new(),
    };
    let spec = match op.http {
        Some(_) => wire_static(service, op, "HTTP"),
        None => String::new(),
    };
    let input_arg = match op.input {
        Some(_) => "input",
        None => "&::treat::rpc::client::NoInput",
    };
    let path = format!("{:?}", op.path);

    let mut params = vec!["&self".to_string()];
    if op.input.is_some() {
        params.push(format!("input: &{input}"));
    }
    // The SDL lists what a raw request accepts; only the caller knows which of
    // them it is sending.
    if raw_request {
        params.push("content_type: &str".to_string());
    }

    let response = "::treat::rpc::client::reqwest::Response".to_string();
    let (success, client_call, args) = if op.http.is_some() {
        (
            output.clone(),
            "call_http",
            vec![format!("&{spec}"), input_arg.to_string()],
        )
    } else if op.kind == OperationKind::Subscription {
        (
            format!("::treat::rpc::client::EventStream<{output}>"),
            "subscribe",
            vec![path.clone(), fields.clone(), input_arg.to_string()],
        )
    } else if raw_request {
        // Both directions raw: the answer is the bytes. A raw request answered
        // with the envelope reads it like an RPC call.
        let (success, client_call) = match op.raw_response.is_some() {
            true => (response, "raw_request"),
            false => (output.clone(), "raw_request_json"),
        };
        (
            success,
            client_call,
            vec![
                path.clone(),
                fields.clone(),
                "input".to_string(),
                "body".to_string(),
                "content_type".to_string(),
            ],
        )
    } else if op.raw_response.is_some() {
        (response, "raw", vec![path.clone(), input_arg.to_string()])
    } else {
        (
            output.clone(),
            "call",
            vec![path.clone(), input_arg.to_string()],
        )
    };

    let template = match raw_request {
        true => &methods.raw_request,
        false => &methods.method,
    };
    out.push_str(&template.render(&vars(&[
        ("method", &method),
        ("input", &input),
        ("output", &output),
        ("path", &op.path),
        ("operation_id", &op.operation_id),
        ("error", &error),
        ("body_field", &body_field),
        ("fields", &fields),
        ("spec", &spec),
        ("params", &params.join(", ")),
        ("success", &success),
        ("client_call", client_call),
        ("call_args", &args.join(", ")),
    ]))?);
    Ok(())
}

/// What a `@raw` operation accepts or answers with.
///
/// A raw request takes the content type as an argument, so the list the SDL
/// declares is what the caller picks from; a raw response states what may
/// arrive, since the caller reads the bytes itself.
fn emit_media_types_doc(out: &mut String, op: &Operation) {
    let (label, types) = match (&op.raw_request, &op.raw_response) {
        (Some(types), _) => ("Accepts", types),
        (None, Some(types)) => ("Answers with", types),
        (None, None) => return,
    };
    if op.description.is_some() {
        out.push_str("    ///\n");
    }
    let _ = writeln!(out, "    /// {label}: `{}`.", types.join("`, `"));
}

/// The codes an operation declares with `@throws`, so a caller can see what to
/// branch on without opening the SDL.
fn emit_errors_doc(out: &mut String, op: &Operation) {
    if op.errors.is_empty() {
        return;
    }
    if op.description.is_some() || op.raw_request.is_some() || op.raw_response.is_some() {
        out.push_str("    ///\n");
    }
    // The heading is the rustdoc convention; what it introduces is this
    // operation's codes, so nothing else is said before the list.
    out.push_str("    /// # Errors\n    ///\n");
    for code in &op.errors {
        let _ = writeln!(out, "    /// - `{code}`");
    }
}
