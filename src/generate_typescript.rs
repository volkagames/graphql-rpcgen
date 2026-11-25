//! TypeScript emitter: interfaces, unions, a fetch-based RPC client and
//! TanStack Query helpers. Reads only the IR.

use std::fmt::Write;

use crate::emit_common::{emit_doc_jsdoc, quoted, HEADER};
use crate::ir::*;

pub fn generate(api: &Api) -> String {
    let mut out = String::new();
    out.push_str(HEADER);

    emit_types(&mut out, api);
    emit_roles(&mut out, api);
    emit_permissions(&mut out, api);
    emit_scopes(&mut out, api);
    emit_error_types(&mut out, api);
    emit_client(&mut out);
    emit_services(&mut out, api);
    emit_api_object(&mut out, api);
    emit_operation_keys(&mut out, api);

    out
}

fn emit_types(out: &mut String, api: &Api) {
    for ty in &api.types {
        match ty {
            ApiType::Scalar(_) => {}
            ApiType::Enum(e) => {
                out.push('\n');
                emit_doc_jsdoc(out, "", &e.description);
                // A union of literals stays structural, so plain strings from
                // the server assign without a runtime enum object.
                let variants = e
                    .values
                    .iter()
                    .map(|v| quoted(v.wire_name()))
                    .collect::<Vec<_>>()
                    .join(" | ");
                let _ = writeln!(out, "export type {} = {variants}", e.name);
            }
            ApiType::Object(o) if api.sibling_tagged(o).is_some() => {
                emit_sibling_holder(out, api, o);
            }
            ApiType::Object(o) => {
                out.push('\n');
                emit_doc_jsdoc(out, "", &o.description);
                let _ = writeln!(out, "export interface {} {{", o.name);
                for f in &o.fields {
                    emit_interface_field(out, api, f, o.explicit_nulls);
                }
                out.push_str("}\n");
            }
            ApiType::InputObject(i) if i.one_of => emit_one_of(out, api, i),
            ApiType::InputObject(i) => {
                out.push('\n');
                emit_doc_jsdoc(out, "", &i.description);
                let _ = writeln!(out, "export interface {} {{", i.name);
                for f in &i.fields {
                    emit_interface_field(out, api, f, false);
                }
                out.push_str("}\n");
            }
            ApiType::Union(u) => {
                out.push('\n');
                emit_doc_jsdoc(out, "", &u.description);
                let arms = u
                    .members
                    .iter()
                    .map(|m| match &u.tagging {
                        Tagging::Internal { tag } => {
                            format!("  | ({{ {tag}: {} }} & {})", quoted(&m.tag), m.name)
                        }
                        // The tag lives on the holder, which pairs it with the
                        // member there; the union alone is just the shapes.
                        Tagging::Sibling { .. } => format!("  | {}", m.name),
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let _ = writeln!(out, "export type {} =\n{arms}", u.name);
            }
        }
    }
}

/// An object holding a sibling-tagged union: its other fields, intersected with
/// one arm per member pairing the tag literal with that member's shape.
///
/// An interface cannot say that `type: 'bool'` goes with `settings:
/// BoolSetting`; the intersection can, and narrowing on the tag narrows the
/// member with it. The two fields' descriptions sit above the arms, which have
/// no member of their own to carry them.
fn emit_sibling_holder(out: &mut String, api: &Api, o: &ObjectType) {
    let Some((held, union, tag)) = api.sibling_tagged(o) else {
        return;
    };
    let rest: Vec<&Field> = o
        .fields
        .iter()
        .filter(|f| f.name != tag && f.name != held.name)
        .collect();

    out.push('\n');
    emit_doc_jsdoc(out, "", &o.description);
    // A holder with nothing but the pair is the arms alone; an empty `{}`
    // intersected with them would say nothing.
    if rest.is_empty() {
        let _ = writeln!(out, "export type {} =", o.name);
    } else {
        let _ = writeln!(out, "export type {} = {{", o.name);
        for f in &rest {
            emit_interface_field(out, api, f, o.explicit_nulls);
        }
        out.push_str("} & (\n");
    }
    let tag_field = o.fields.iter().find(|f| f.name == tag);
    if let Some(f) = tag_field {
        emit_doc_jsdoc(out, "  ", &f.description);
    }
    emit_doc_jsdoc(out, "  ", &held.description);
    for m in &union.members {
        let _ = writeln!(
            out,
            "  | {{ {tag}: {}; {}: {} }}",
            quoted(&m.tag),
            held.name,
            m.name
        );
    }
    if !rest.is_empty() {
        out.push_str(")\n");
    }
}

/// One field of an interface. With `explicit_nulls` an absent value arrives as
/// `null`, so the key is always there and only its value may be null.
fn emit_interface_field(out: &mut String, api: &Api, f: &Field, explicit_nulls: bool) {
    emit_doc_jsdoc(out, "  ", &f.description);
    let optional = if f.ty.is_nullable() && !explicit_nulls {
        "?"
    } else {
        ""
    };
    let _ = writeln!(out, "  {}{optional}: {}", f.name, ts_type(api, &f.ty));
}

/// `@oneOf` becomes an exclusive union: each arm marks the other keys `never`,
/// so supplying two variants is a type error rather than a runtime surprise.
fn emit_one_of(out: &mut String, api: &Api, i: &InputObjectType) {
    out.push('\n');
    emit_doc_jsdoc(out, "", &i.description);
    let _ = writeln!(out, "export type {} =", i.name);
    for (index, field) in i.fields.iter().enumerate() {
        let mut arm = String::from("  | {\n");
        for other in &i.fields {
            if other.name == field.name {
                let _ = writeln!(
                    arm,
                    "      {}: {}",
                    other.name,
                    ts_type_non_null(api, &other.ty)
                );
            } else {
                let _ = writeln!(arm, "      {}?: never", other.name);
            }
        }
        arm.push_str("    }");
        out.push_str(&arm);
        if index
            .checked_add(1)
            .is_some_and(|next| next < i.fields.len())
        {
            out.push('\n');
        }
    }
    out.push('\n');
}

/// The wire error shape, mirroring `treat::ErrorMessage`.
///
/// There is no payload type per code: an error is a `code`, a `message` and
/// optional `meta`, so `RpcErrorCode` is simply the registry enum.
fn emit_error_types(out: &mut String, api: &Api) {
    let registry = crate::semantic::ERROR_CODE_ENUM;
    if !matches!(api.find_type(registry), Some(ApiType::Enum(_))) {
        return;
    }

    let _ = write!(
        out,
        "\n/** Every code this API can return, declared or emitted by the runtime. */\n\
         export type RpcErrorCode = {registry}\n"
    );

    // Emitted by the runtime rather than declared in SDL: a malformed body or
    // an unexpected server failure can occur on any operation.
    let runtime_codes = crate::semantic::RUNTIME_ERROR_CODES
        .iter()
        .map(|c| format!("'{c}'"))
        .collect::<Vec<_>>()
        .join(" | ");
    let _ = write!(
        out,
        "\n/** Transport-level codes the runtime can return for any operation. */\n\
         export type RpcRuntimeErrorCode = {runtime_codes}\n"
    );

    out.push_str("\n/** Locator for the offending input (JSON:API `source`). */\n");
    out.push_str("export interface RpcErrorSource {\n  /** RFC 6901 JSON Pointer, e.g. `/device_info/sdk_version`. */\n  pointer?: string\n  parameter?: string\n  header?: string\n}\n");

    out.push_str("\n/** One entry of the wire `errors[]` array. */\n");
    out.push_str("export interface RpcErrorItem {\n");
    out.push_str("  code: RpcErrorCode\n");
    out.push_str("  message?: string\n");
    // An object rather than `unknown`: `meta` is a bag of named details, so
    // indexing it must not require a cast at every call site.
    out.push_str(
        "  /** Free-form context; its keys depend on the code. */\n  \
         meta?: globalThis.Record<string, unknown>\n",
    );
    out.push_str("  source?: RpcErrorSource\n");
    // RFC 9457 `type` and `instance`. The server can set either on any error, so
    // a client that does not declare them drops them silently.
    out.push_str(
        "  /** RFC 9457 `type`: a URI identifying the error kind. */\n  \
         type?: string\n",
    );
    out.push_str(
        "  /** RFC 9457 `instance`: id or URI of this specific occurrence. */\n  \
         instance?: string\n",
    );
    out.push_str("}\n");
}

/// The fetch-based client every generated `api.ts` carries: the envelope and
/// error types, `RpcClient` and its transports, and the helpers they share.
///
/// It is static TypeScript, kept as a `.ts` file so an editor reads it as such.
/// It is not compilable on its own — it names the generated `RpcErrorItem` and
/// `RpcErrorCode` — and is type-checked as part of the `api.ts` it lands in.
const CLIENT_RUNTIME: &str = include_str!("../templates/typescript/client_runtime.ts");

/// The code `syntheticError` gives a failure that arrived without an envelope,
/// as the runtime file spells it until the registry's value replaces it.
const INTERNAL_ERROR_PLACEHOLDER: &str = "INTERNAL_ERROR_PLACEHOLDER";

fn emit_client(target: &mut String) {
    // The file is plain TypeScript rather than a template, so the registry's
    // value is substituted into it here rather than interpolated.
    target.push('\n');
    target.push_str(&CLIENT_RUNTIME.replace(
        INTERNAL_ERROR_PLACEHOLDER,
        crate::semantic::CODE_INTERNAL_ERROR,
    ));
}

/// Per-service call methods, grouped into one object per service.
fn emit_services(out: &mut String, api: &Api) {
    for service in &api.services {
        out.push('\n');
        emit_doc_jsdoc(out, "", &service.description);
        let _ = writeln!(
            out,
            "export function create{}Operations(client: RpcClient) {{\n  return {{",
            service.name
        );
        for op in &service.operations {
            emit_doc_jsdoc(out, "    ", &op.description);
            let _ = writeln!(out, "    {}: {},", op.name, call_expr(api, op, "      "));
        }
        out.push_str("  }\n}\n");
        let _ = writeln!(
            out,
            "\nexport type {}Operations = globalThis.ReturnType<typeof create{}Operations>",
            service.name, service.name
        );
    }
}

/// The `@roles` enum's runtime half: the list of roles and the hierarchy
/// check, so a client that says who may do what runs the server's check rather
/// than a copy of it.
///
/// The type itself is the enum's literal union, emitted with the other types.
/// The closure stays unexported: `roleAuthorizes` is the question a caller has,
/// and the table is how this file answers it.
fn emit_roles(out: &mut String, api: &Api) {
    let (Some(roles), Some(e)) = (&api.roles, api.role_enum()) else {
        return;
    };
    let ty = &e.name;
    let wire = |name: &str| quoted(api.role_wire_name(name));

    let list = e
        .values
        .iter()
        .map(|v| quoted(v.wire_name()))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(
        out,
        "\n/** Every role of `{ty}`, in declaration order. */\nexport const roles: readonly {ty}[] = [{list}]"
    );

    let _ = writeln!(
        out,
        "\n/** Per role, every role it inherits through `@inherits`, directly or not. */\nconst roleInherits: globalThis.Record<{ty}, readonly {ty}[]> = {{"
    );
    for (role, inherited) in &roles.inherits {
        let inherited = inherited
            .iter()
            .map(|name| wire(name))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(out, "  {}: [{inherited}],", wire(role));
    }
    out.push_str("}\n");

    // The table is read before `held` is compared: with a single role,
    // `held !== required` narrows `held` to `never`, and indexing the table
    // with it then fails `tsc --strict`.
    let _ = writeln!(
        out,
        "\n/** Whether a user assigned `held` is authorized for `required`: the same role, or one `held` inherits. */\n\
         export function roleAuthorizes(held: {ty}, required: {ty}): boolean {{\n  \
         const inherited: readonly {ty}[] = roleInherits[held]\n  \
         return held === required || inherited.includes(required)\n}}"
    );
}

/// The `@permissions` enum's runtime half: the list of permissions, for a client
/// that edits which roles hold which. Whether a session holds one is the
/// server's to decide, so there is no counterpart to `roleAuthorizes`.
///
/// The type itself is the enum's literal union, emitted with the other types.
fn emit_permissions(out: &mut String, api: &Api) {
    let Some(e) = api.permission_enum() else {
        return;
    };
    let ty = &e.name;
    let list = e
        .values
        .iter()
        .map(|v| quoted(v.wire_name()))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(
        out,
        "\n/** Every permission of `{ty}`, in declaration order. */\nexport const permissions: readonly {ty}[] = [{list}]"
    );
}

/// The `@scopes` enum's runtime half: the list of scopes, for a client that
/// mints credentials and offers what one may carry.
///
/// The type itself is the enum's literal union, emitted with the other types.
fn emit_scopes(out: &mut String, api: &Api) {
    let Some(e) = api.scope_enum() else {
        return;
    };
    let ty = &e.name;
    let list = e
        .values
        .iter()
        .map(|v| quoted(v.wire_name()))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(
        out,
        "\n/** Every scope of `{ty}`, in declaration order. */\nexport const scopes: readonly {ty}[] = [{list}]"
    );
}

/// The `api` factory: per operation, its call, the TanStack helpers layered
/// over it, and the role or the permission `@auth` asks for.
///
/// It is built on the `create<Service>Operations` objects: each entry's call is
/// the one they hold, and its helpers call that rather than the client.
fn emit_api_object(out: &mut String, api: &Api) {
    out.push_str(
        r#"
/** Deterministic TanStack query key: [service, operation, input]. */
export function queryKey(service: string, operation: string, input: unknown) {
  return [service, operation, input] as const
}

export interface QueryOptionsResult<Output> {
  queryKey: readonly unknown[]
  queryFn: (context: { signal?: globalThis.AbortSignal }) => globalThis.Promise<Output>
}

export interface MutationOptionsResult<Input, Output> {
  mutationKey: readonly unknown[]
  mutationFn: (input: Input) => globalThis.Promise<Output>
}
"#,
    );

    out.push_str("\n/** Typed facade over the RPC client, including TanStack helpers. */\n");
    out.push_str("export function createApi(client: RpcClient) {\n");

    // An API without services has nothing to hold, and two empty locals would
    // only trip `noUnusedLocals`.
    if !api.services.is_empty() {
        emit_api_locals(api, out);
    }

    out.push_str("  return {\n");
    for service in &api.services {
        let service_key = service.name.to_lowercase();
        let _ = writeln!(out, "    {service_key}: {{");
        for op in &service.operations {
            let input = ts_input_type(api, op);
            let output = ts_type(api, &op.output);
            let param = ts_input_param(api, op);
            // What the TanStack helpers hand the call and the key builder: the
            // input when there is one, nothing otherwise.
            let (arg, leading_arg) = if op.input.is_some() {
                ("input", "input, ")
            } else {
                ("", "")
            };
            let call = format!("ops.{service_key}.{}", op.name);
            let key = format!("keys.{service_key}.{}", op.name);
            let _ = writeln!(out, "      {}: {{", op.name);
            match &op.access {
                None => {}
                Some(access @ Access::Role(_)) => {
                    let _ = writeln!(
                        out,
                        "        role: {} as const,",
                        quoted(api.access_wire_name(access))
                    );
                }
                Some(access @ Access::Permission(_)) => {
                    let _ = writeln!(
                        out,
                        "        permission: {} as const,",
                        quoted(api.access_wire_name(access))
                    );
                }
            }
            if let Some(scope) = &op.scope {
                let _ = writeln!(
                    out,
                    "        scope: {} as const,",
                    quoted(api.scope_wire_name(scope))
                );
            }
            let verb = if op.kind == OperationKind::Subscription {
                "subscribe"
            } else {
                "call"
            };
            let _ = writeln!(out, "        {verb}: {call},");
            let _ = writeln!(out, "        queryKey: {key},");

            // A raw response is a single-use stream: caching it would hand the
            // second reader a consumed body, so a download gets no TanStack
            // helper and stays an explicit call.
            if op.raw_response.is_some() {
                out.push_str("      },\n");
                continue;
            }

            match op.kind {
                OperationKind::Query => {
                    let _ = writeln!(
                        out,
                        "        queryOptions: ({param}): QueryOptionsResult<{output}> => ({{\n          queryKey: {key}({arg}),\n          queryFn: ({{ signal }}) => {call}({leading_arg}{{ signal }}),\n        }}),"
                    );
                }
                OperationKind::Mutation => {
                    let _ = writeln!(
                        out,
                        "        mutationOptions: (): MutationOptionsResult<{input}, {output}> => ({{\n          mutationKey: ['{service_key}', '{}'],\n          mutationFn: ({param}) => {call}({arg}),\n        }}),",
                        op.name
                    );
                }
                // A stream has no single value to cache, so there are no
                // TanStack options for it — `queryKey` is still emitted above
                // so an event can be pushed into the cache of a related query.
                OperationKind::Subscription => {}
            }
            out.push_str("      },\n");
        }
        out.push_str("    },\n");
    }

    out.push_str("  }\n}\n");
    out.push_str("\nexport type Api = globalThis.ReturnType<typeof createApi>\n");
}

/// The `ops` and `keys` locals `createApi` reads its entries from.
///
/// Every call is the one the service's own object holds, so the facade
/// describes no request itself and cannot tell a call differently.
fn emit_api_locals(api: &Api, out: &mut String) {
    out.push_str("  const ops = {\n");
    for service in &api.services {
        let _ = writeln!(
            out,
            "    {}: create{}Operations(client),",
            service.name.to_lowercase(),
            service.name
        );
    }
    out.push_str("  }\n");

    // Each key is built here once: the entry exposes the builder as `queryKey`,
    // and its `queryOptions` calls the same one rather than spelling it again.
    out.push_str("  const keys = {\n");
    for service in &api.services {
        let service_key = service.name.to_lowercase();
        let _ = writeln!(out, "    {service_key}: {{");
        for op in &service.operations {
            // With no input there is no parameter to declare, and the key
            // keeps its arity with `undefined` in the input's place.
            let param = ts_input_param(api, op);
            let key_arg = if op.input.is_some() {
                "input"
            } else {
                "undefined"
            };
            let _ = writeln!(
                out,
                "      {}: ({param}) => queryKey('{service_key}', '{}', {key_arg}),",
                op.name, op.name
            );
        }
        out.push_str("    },\n");
    }
    out.push_str("  }\n");
}

/// `operationKeys`: each operation's TanStack key prefix, `[service, operation]`,
/// which every `queryKey` and `mutationKey` the operation builds starts with.
///
/// A module-level constant rather than a field of `createApi`, because the
/// place that invalidates every query of one operation — whatever its input —
/// is often a module with no client in reach, and without it each such place
/// spells the pair as an untyped literal that a rename silently stops matching.
fn emit_operation_keys(out: &mut String, api: &Api) {
    out.push_str(
        "\n/** Each operation's TanStack key prefix, `[service, operation]`: what its `queryKey` and `mutationKey` start with, for invalidating every input's query at once. */\n\
         export const operationKeys = {\n",
    );
    for service in &api.services {
        let service_key = service.name.to_lowercase();
        let _ = writeln!(out, "  {service_key}: {{");
        for op in &service.operations {
            let _ = writeln!(
                out,
                "    {}: ['{service_key}', '{}'] as const,",
                op.name, op.name
            );
        }
        out.push_str("  },\n");
    }
    out.push_str("} as const\n");
}

/// The lambda that performs one operation, whatever its transport.
///
/// Written once, into the per-service object: the TanStack facade takes its
/// calls from there, so the two cannot describe the same call differently.
fn call_expr(api: &Api, op: &Operation, indent: &str) -> String {
    let output = ts_type(api, &op.output);
    let param = ts_input_param(api, op);
    let leading = if param.is_empty() {
        String::new()
    } else {
        format!("{param}, ")
    };
    let body = if op.input.is_some() { "input" } else { "{}" };

    if op.kind == OperationKind::Subscription {
        let fields = match field_table(api, op) {
            Some(table) => format!(", {table}"),
            None => String::new(),
        };
        return format!(
            "({leading}handlers: RpcEventHandlers<{output}>): RpcSubscription =>\n{indent}\
             client.subscribe<{output}>('{}', {body}, handlers{fields})",
            op.path
        );
    }

    let http = http_spec(api, op);
    if let Some(spec) = http {
        return format!(
            "({leading}options?: RpcCallOptions): globalThis.Promise<{output}> =>\n{indent}\
             client.http<{output}>('{}', {body}, {spec}, options)",
            op.path
        );
    }
    let Some(spec) = raw_spec(api, op) else {
        return format!(
            "({leading}options?: RpcCallOptions): globalThis.Promise<{output}> =>\n{indent}\
             client.call<{output}>('{}', {body}, options)",
            op.path
        );
    };

    // A raw response is bytes plus the headers that describe them, so the
    // caller gets the `Response` and reads whichever it needs.
    if op.raw_response.is_some() {
        format!(
            "({leading}options?: RpcCallOptions): globalThis.Promise<globalThis.Response> =>\n{indent}\
             client.raw('{}', {body}, {spec}, options)",
            op.path
        )
    } else {
        format!(
            "({leading}options?: RpcCallOptions): globalThis.Promise<{output}> =>\n{indent}\
             client.rawJson<{output}>('{}', {body}, {spec}, options)",
            op.path
        )
    }
}

/// The `RpcHttpSpec` literal of an `@http` operation.
fn http_spec(api: &Api, op: &Operation) -> Option<String> {
    let http = op.http.as_ref()?;
    let path_fields: Vec<String> = http.path_params.iter().map(|p| quoted(p)).collect();
    let body = match http.body {
        HttpBody::None => "none",
        HttpBody::Json => "json",
        HttpBody::Form => "form",
    };
    let fields = match field_table(api, op) {
        Some(table) => format!(", fields: {table}"),
        None => String::new(),
    };
    Some(format!(
        "{{ method: '{}', pathFields: [{}], body: '{body}', envelope: {}{fields} }}",
        op.client_method(),
        path_fields.join(", "),
        http.envelope
    ))
}

/// The `RpcRawSpec` literal for an operation, or `None` when it is plain JSON.
fn raw_spec(api: &Api, op: &Operation) -> Option<String> {
    if op.raw_request.is_none() && op.raw_response.is_none() {
        return None;
    }
    let mut parts = Vec::new();
    if let Some(field) = api.body_field(op) {
        parts.push(format!("bodyField: {}", quoted(&field.name)));
    }
    // The first declared media type is what a generated caller sends unless
    // `RpcCallOptions.contentType` names another: only the caller knows which
    // of several its body is.
    if let Some(types) = &op.raw_request {
        if let Some(first) = types.first() {
            parts.push(format!("requestType: {}", quoted(first)));
        }
    }
    if op.raw_response.is_some() {
        parts.push("rawResponse: true".to_string());
    }
    let table = field_table(api, op);
    if let Some(table) = table {
        parts.push(format!("fields: {table}"));
    }
    Some(format!("{{ {} }}", parts.join(", ")))
}

/// The `RpcFieldTable` literal of an operation whose input travels as text —
/// in a query string, a form body or a path — or `None` when it needs none.
///
/// The table the Rust server reads the request with (`wire::<OP>_FIELDS`, or
/// an `HttpSpec`'s `fields`), each kind spelled as `query_kind` says and a
/// list marked `[]`: `{ id: 'str', tags: 'str[]', filter: 'json' }`.
///
/// Left out when no entry would change what the runtime writes, so most
/// operations carry none: a field the table does not name is written as a
/// `str` that is not a list, which is also how an `int`, `float` or `bool`
/// one is, and only a `json` kind or a list differs. An `@http` operation
/// with a JSON body writes its fields as JSON whatever the table says, so for
/// one only a path parameter counts.
fn field_table(api: &Api, op: &Operation) -> Option<String> {
    let travels_as_text = match &op.http {
        Some(http) => !http.path_params.is_empty() || http.body != HttpBody::Json,
        None => op.kind == OperationKind::Subscription || op.raw_request.is_some(),
    };
    if !travels_as_text {
        return None;
    }
    let fields = api.query_fields(op);
    let counts = |f: &&&Field| match &op.http {
        Some(http) if http.body == HttpBody::Json => http.path_params.contains(&f.name),
        _ => true,
    };
    let differs = fields
        .iter()
        .filter(counts)
        .any(|f| api.query_kind(f) == QueryKind::Json || matches!(f.ty, TypeRef::List { .. }));
    if !differs {
        return None;
    }
    let entries: Vec<String> = fields
        .iter()
        .map(|f| {
            let list = match f.ty {
                TypeRef::List { .. } => "[]",
                TypeRef::Named { .. } => "",
            };
            format!("{}: '{}{list}'", f.name, query_kind(api, f))
        })
        .collect();
    Some(format!("{{ {} }}", entries.join(", ")))
}

/// How one field is spelled as text: the `RpcQueryKind` of its declared type.
///
/// The TypeScript spelling of [`Api::query_kind`], which the Rust server's
/// table spells its own way; `tests/snapshots.rs` holds the two to the same
/// entries.
fn query_kind(api: &Api, f: &Field) -> &'static str {
    match api.query_kind(f) {
        QueryKind::Str => "str",
        QueryKind::Int => "int",
        QueryKind::Float => "float",
        QueryKind::Bool => "bool",
        QueryKind::Json => "json",
    }
}

/// The type of an operation's `input`, or `void` when it declares none.
///
/// `void` is what the TanStack result types name as the input of an operation
/// that has none (`MutationOptionsResult<void, Output>`); the helpers
/// themselves then declare no parameter at all — see [`ts_input_param`].
fn ts_input_type(api: &Api, op: &Operation) -> String {
    match &op.input {
        Some(input) => ts_type(api, input),
        None => "void".to_string(),
    }
}

/// The parameter declaration for an operation's input, empty when it has none.
fn ts_input_param(api: &Api, op: &Operation) -> String {
    match &op.input {
        Some(input) => format!("input: {}", ts_type(api, input)),
        None => String::new(),
    }
}

fn ts_type(api: &Api, ty: &TypeRef) -> String {
    let inner = ts_type_non_null(api, ty);
    if ty.is_nullable() {
        format!("{inner} | null")
    } else {
        inner
    }
}

fn ts_type_non_null(api: &Api, ty: &TypeRef) -> String {
    match ty {
        TypeRef::Named { name, .. } => scalar_ts_name(api, name),
        TypeRef::List { inner, .. } => {
            let item = ts_type(api, inner);
            // Parenthesise unions so `(string | null)[]` is not read as
            // `string | (null[])`.
            if item.contains('|') {
                format!("({item})[]")
            } else {
                format!("{item}[]")
            }
        }
    }
}

fn scalar_ts_name(api: &Api, name: &str) -> String {
    match api.find_scalar(name) {
        Some(s) => s.typescript.clone(),
        None => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    /// The registry's code reaches `syntheticError` only through this one
    /// substitution, and `str::replace` says nothing when it finds no match: a
    /// placeholder renamed in the `.ts` file would ship the placeholder itself.
    #[test]
    fn the_runtime_names_the_internal_error_placeholder_once() {
        let found = super::CLIENT_RUNTIME
            .matches(super::INTERNAL_ERROR_PLACEHOLDER)
            .count();
        assert_eq!(
            found, 1,
            "client_runtime.ts must spell the placeholder exactly once"
        );
    }
}
