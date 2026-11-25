//! OpenAPI 3.1 emitter, driven entirely by the IR.
//!
//! No Rust schema reflection is involved: the document is built from the same
//! semantic model the Rust and TypeScript emitters read.

use serde_json::{json, Map, Value};

use crate::config::Config;
use crate::ir::*;
use erris::prelude::*;

const OPENAPI_VERSION: &str = "3.1.0";
const DEFAULT_DESCRIPTION: &str =
    "Generated from GraphQL SDL by graphql-rpcgen. All operations are POST + JSON.";
/// The default for an API with an `@http` operation, which is neither.
const DEFAULT_DESCRIPTION_HTTP: &str = "Generated from GraphQL SDL by graphql-rpcgen. RPC \
     operations are POST + JSON; an operation bound to HTTP keeps its own method, path \
     and body, as each one describes.";

pub fn generate(api: &Api, config: &Config) -> erris::Result<String> {
    let mut doc = Map::new();
    doc.insert("openapi".into(), json!(OPENAPI_VERSION));
    doc.insert(
        "info".into(),
        json!({
            "title": config.openapi.title,
            "version": config.openapi.version,
            "description": config
                .openapi
                .description
                .as_deref()
                .unwrap_or(
                    match api.services.iter().flat_map(|s| &s.operations).any(|op| op.http.is_some()) {
                        true => DEFAULT_DESCRIPTION_HTTP,
                        false => DEFAULT_DESCRIPTION,
                    }
                ),
        }),
    );

    // A relative server keeps Swagger UI's "Try it out" on the same origin
    // that served the document, which is why it is the default.
    doc.insert(
        "servers".into(),
        Value::Array(
            config
                .openapi
                .servers
                .iter()
                .map(|url| json!({ "url": url }))
                .collect(),
        ),
    );
    doc.insert("tags".into(), Value::Array(tags(api)));
    doc.insert("paths".into(), Value::Object(paths(api)));
    doc.insert(
        "components".into(),
        json!({
            "schemas": Value::Object(schemas(api)),
            // Every operation answers with the same header, so it is defined
            // once and referenced rather than copied into each response.
            "headers": { RPC_STATUS_HEADER: {
                "description": "`ok` when `errors[]` is empty, `error` otherwise.",
                "schema": { "type": "string", "enum": ["ok", "error"] },
            } },
        }),
    );

    // Trailing newline keeps the file POSIX-friendly and diff-stable.
    let rendered = serde_json::to_string_pretty(&Value::Object(doc))
        .wrap_report("the OpenAPI document does not serialize")?;
    Ok(format!("{rendered}\n"))
}

fn tags(api: &Api) -> Vec<Value> {
    api.services
        .iter()
        .map(|s| {
            let mut tag = Map::new();
            tag.insert("name".into(), json!(s.name));
            if let Some(d) = &s.description {
                tag.insert("description".into(), json!(d));
            }
            Value::Object(tag)
        })
        .collect()
}

fn paths(api: &Api) -> Map<String, Value> {
    let mut paths = Map::new();
    for service in &api.services {
        for op in &service.operations {
            let mut operation = Map::new();
            operation.insert("operationId".into(), json!(op.operation_id));
            operation.insert("tags".into(), json!([service.name]));
            operation.insert(
                "summary".into(),
                json!(format!("{}.{}", service.name, op.name)),
            );
            if let Some(d) = &op.description {
                operation.insert("description".into(), json!(d));
            }
            // @query / @mutation is RPC semantics, not an HTTP method; surface
            // it so generated clients and docs agree.
            operation.insert(
                "x-rpc-kind".into(),
                json!(match op.kind {
                    OperationKind::Query => "query",
                    OperationKind::Mutation => "mutation",
                    OperationKind::Subscription => "subscription",
                }),
            );
            // `@mcp` marks the operation for MCP servers built over this
            // document; absence means not exposed, so the flag is only ever
            // `true` and never a decorative `false`.
            if op.mcp {
                operation.insert("x-mcp".into(), json!(true));
            }
            // What `@auth` checks beyond a session, by its wire name, so a
            // consumer reading the document — an MCP server deciding whether a
            // tool may run, a test holding every operation of a surface to a
            // check — sees the requirement rather than restating it.
            match &op.access {
                None => {}
                Some(access @ Access::Role(_)) => {
                    operation.insert("x-role".into(), json!(api.access_wire_name(access)));
                }
                Some(access @ Access::Permission(_)) => {
                    operation.insert("x-permission".into(), json!(api.access_wire_name(access)));
                }
            }
            // The scope a credential must carry, by its wire name, resolved:
            // a query that states none carries its set's default here, so a
            // gate reading the document sees every operation's answer.
            if let Some(scope) = &op.scope {
                operation.insert("x-scope".into(), json!(api.scope_wire_name(scope)));
            }

            if let Some(http) = &op.http {
                http_request(api, op, http, &mut operation);
                if http.method == HttpMethod::Any {
                    // OpenAPI has no "any method": the operation is described
                    // under the method a client sends, and this says the route
                    // takes every other one too.
                    operation.insert("x-rpc-any-method".into(), json!(true));
                }
                operation.insert("responses".into(), Value::Object(responses(api, op)));
                let method = op.client_method().to_ascii_lowercase();
                insert_operation(&mut paths, &op.path, &method, operation);
                continue;
            }

            // An input that cannot ride in the body rides in the URL: a
            // subscription is opened by `EventSource`, which only GETs, and a
            // raw request has already spent its body on the payload.
            if op.input_in_query() {
                let params: Vec<Value> = api
                    .query_fields(op)
                    .into_iter()
                    .map(|f| {
                        let mut param = json!({
                            "name": f.name,
                            "in": "query",
                            "required": !f.ty.is_nullable(),
                            "schema": url_schema(api, &f.ty),
                        });
                        if let (Some(d), Some(fields)) = (&f.description, param.as_object_mut()) {
                            fields.insert("description".into(), json!(d));
                        }
                        param
                    })
                    .collect();
                if !params.is_empty() {
                    operation.insert("parameters".into(), json!(params));
                }
            }

            match (op.kind, &op.raw_request) {
                // A subscription sends nothing: its parameters are above and it
                // is opened with GET.
                (OperationKind::Subscription, _) => {}
                (_, Some(media_types)) => {
                    operation.insert(
                        "requestBody".into(),
                        json!({
                            "required": true,
                            "content": media_content(media_types),
                        }),
                    );
                }
                (_, None) => {
                    // An operation with no `input` still posts a body, so the
                    // entry stays; it just describes the empty object that is
                    // actually sent.
                    let request_schema = match &op.input {
                        Some(input) => type_ref_schema(api, input),
                        None => json!({
                            "type": "object",
                            "additionalProperties": false,
                            "description":
                                "This operation takes no arguments; send an empty object.",
                        }),
                    };
                    operation.insert(
                        "requestBody".into(),
                        json!({
                            "required": true,
                            "content": { "application/json": { "schema": request_schema } },
                        }),
                    );
                }
            }
            operation.insert("responses".into(), Value::Object(responses(api, op)));

            // OpenAPI keys an operation by the method's lower-case name.
            let method = op.http_method().to_ascii_lowercase();
            insert_operation(&mut paths, &op.path, &method, operation);
        }
    }
    paths
}

/// Add one operation under `path`, beside any other method already there.
fn insert_operation(
    paths: &mut Map<String, Value>,
    path: &str,
    method: &str,
    operation: Map<String, Value>,
) {
    let entry = paths.entry(path.to_string()).or_insert_with(|| json!({}));
    if let Value::Object(methods) = entry {
        methods.insert(method.to_string(), Value::Object(operation));
    }
}

/// The parameters and the body of an `@http` operation: its path parameters,
/// then the other fields in the query string or in a JSON or form body.
fn http_request(api: &Api, op: &Operation, http: &HttpBinding, operation: &mut Map<String, Value>) {
    let fields = api.query_fields(op);
    let parameter = |f: &Field, location: &str| {
        let mut param = json!({
            "name": f.name,
            "in": location,
            "required": location == "path" || !f.ty.is_nullable(),
            "schema": url_schema(api, &f.ty),
        });
        if let (Some(d), Some(fields)) = (&f.description, param.as_object_mut()) {
            fields.insert("description".into(), json!(d));
        }
        param
    };
    let (path_fields, other_fields): (Vec<&Field>, Vec<&Field>) = fields
        .into_iter()
        .partition(|f| http.path_params.contains(&f.name));

    let mut params: Vec<Value> = path_fields.iter().map(|f| parameter(f, "path")).collect();
    if http.body == HttpBody::None {
        params.extend(other_fields.iter().map(|f| parameter(f, "query")));
    }
    if !params.is_empty() {
        operation.insert("parameters".into(), json!(params));
    }

    let body_fields: Vec<Field> = other_fields.iter().map(|f| (*f).clone()).collect();
    match http.body {
        HttpBody::None => {}
        HttpBody::Json => {
            // The input type itself while the path takes none of it; with path
            // parameters, the fields left over, since the server reads those
            // from the path and a body naming them is overridden.
            // No input is no body: the handler reads none.
            let schema = match (&op.input, path_fields.is_empty()) {
                (Some(input), true) => Some(type_ref_schema(api, input)),
                (Some(_), false) => Some(object_schema(api, &None, &body_fields, false)),
                (None, _) => None,
            };
            if let Some(schema) = schema {
                operation.insert(
                    "requestBody".into(),
                    json!({ "required": true, "content": { "application/json": { "schema": schema } } }),
                );
            }
        }
        HttpBody::Form => {
            // The same fields are read from the query string too, the body
            // winning; the form is what a client sends, so it is what is shown.
            // A field read as JSON text is shown as such, rather than as an
            // object a client would explode into keys of its own.
            let mut properties = Map::new();
            let mut required = Vec::new();
            for f in &body_fields {
                let schema = match api.is_json_text(f.ty.base_name()) {
                    true => url_schema(api, &f.ty),
                    false => field_schema(api, f),
                };
                properties.insert(f.name.clone(), schema);
                if !f.ty.is_nullable() {
                    required.push(f.name.clone());
                }
            }
            let mut schema = json!({ "type": "object", "properties": properties });
            if let (false, Some(object)) = (required.is_empty(), schema.as_object_mut()) {
                object.insert("required".into(), json!(required));
            }
            operation.insert(
                "requestBody".into(),
                json!({
                    "required": false,
                    "content": { "application/x-www-form-urlencoded": { "schema": schema } },
                }),
            );
        }
    }
}

/// A field's schema as a URL or a form body carries it: its own, but for a
/// value read as JSON text, which is a string holding that JSON — one per item
/// of a list, since a list repeats its key.
fn url_schema(api: &Api, ty: &TypeRef) -> Value {
    match ty {
        TypeRef::List { inner, .. } => json!({ "type": "array", "items": url_schema(api, inner) }),
        TypeRef::Named { name, .. } if api.is_json_text(name) => json!({
            "type": "string",
            "contentMediaType": "application/json",
            "contentSchema": type_ref_schema(api, ty),
        }),
        TypeRef::Named { .. } => type_ref_schema(api, ty),
    }
}

/// The out-of-band outcome signal `treat` sets, named once because both the
/// component and the reference to it spell it.
const RPC_STATUS_HEADER: &str = "x-rpc-status";

/// A `content` map over declared media types, each an opaque binary body.
///
/// One entry per type rather than a single wildcard: an operation that takes
/// CSV or JSON accepts exactly those two, and `*/*` would say it takes anything.
fn media_content(media_types: &[String]) -> Value {
    let mut content = Map::new();
    for media_type in media_types {
        content.insert(
            media_type.clone(),
            json!({ "schema": { "type": "string", "format": "binary" } }),
        );
    }
    Value::Object(content)
}

/// Every operation answers `200`, success or failure alike.
///
/// The status line carries no outcome — that is the portal's convention, which
/// `treat` keeps — so one response describes both halves of the envelope and
/// the `x-rpc-status` header gives the out-of-band signal. An `@http` operation
/// is the exception: its callers read the status line, so its failures are
/// described under `default` as well.
fn responses(api: &Api, op: &Operation) -> Map<String, Value> {
    let mut responses = Map::new();

    // A stream and a raw body are both described by their media type rather
    // than by a schema, so they answer here and never build the envelope below.
    if op.kind == OperationKind::Subscription {
        responses.insert(
            "200".into(),
            json!({
                "description":
                    "An open event stream. Each `data:` frame is one JSON event of the \
                     declared type.",
                "content": {
                    "text/event-stream": { "schema": type_ref_schema(api, &op.output) },
                },
            }),
        );
        return responses;
    }

    if let Some(media_types) = &op.raw_response {
        responses.insert(
            "200".into(),
            json!({
                "description": "The body itself, not the JSON envelope.",
                "content": media_content(media_types),
            }),
        );
        responses.insert(
            "default".into(),
            json!({
                "description": "A failure, which is still the JSON envelope.",
                "content": { "application/json": {
                    "schema": { "$ref": "#/components/schemas/RpcErrorItem" },
                } },
            }),
        );
        return responses;
    }

    if !op.enveloped() {
        responses.insert(
            "200".into(),
            json!({
                "description": "The value itself, not the `data` envelope.",
                "content": { "application/json": { "schema": type_ref_schema(api, &op.output) } },
            }),
        );
        responses.insert(
            "default".into(),
            json!({
                "description":
                    "A failure: the envelope with `errors[]`, under the status the error \
                     carries, `500` when it carries none; `400` (`413` past the body \
                     limit) for an input the runtime cannot read, `401` for a refused \
                     session, `403` for denied access.",
                "content": { "application/json": { "schema": failure_schema(api, op) } },
            }),
        );
        return responses;
    }

    // Success: the payload under `data`. Failure: `errors[]`, one entry per
    // problem, each pinned to one of the codes this operation declares.
    let mut alternatives = vec![json!({
        "type": "object",
        "title": "Success",
        "properties": {
            "data": type_ref_schema(api, &op.output),
            // `type` is stated explicitly: a schema carrying only a
            // description has no type at all, and Swagger UI renders such a
            // field as a string.
            //
            // `additionalProperties` is deliberately omitted. An object already
            // permits any key, so stating it adds nothing — and it makes
            // Swagger UI invent `additionalProp1/2/3` in the example body.
            "meta": {
                "type": "object",
                "description": "Free-form response metadata.",
            },
        },
        "required": ["data"],
    })];

    alternatives.push(failure_schema(api, op));

    // An `@http` route is read by callers that look at the status line, so the
    // failures the runtime builds carry one; the service's own still answer
    // `200` unless it gives them another.
    let description = match op.http {
        Some(_) => {
            "Success carries `data`; failure carries `errors[]`. A failure the \
             service returns answers 200 unless it carries another status; the \
             others are described under `default`."
        }
        None => {
            "Success carries `data`; failure carries `errors[]`. The status line is \
             always 200 — read `errors[]`, or the `x-rpc-status` header."
        }
    };
    if op.http.is_some() {
        responses.insert(
            "default".into(),
            json!({
                "description":
                    "A failure the runtime answers with a status: `400` for an input it \
                     cannot read (`413` for a body past the limit), `401` for a refused \
                     session, `403` for denied access.",
                "content": { "application/json": { "schema": failure_schema(api, op) } },
            }),
        );
    }

    responses.insert(
        "200".into(),
        json!({
            "description": description,
            "headers": {
                RPC_STATUS_HEADER: { "$ref": format!("#/components/headers/{RPC_STATUS_HEADER}") },
            },
            "content": { "application/json": { "schema": { "oneOf": alternatives } } },
        }),
    );

    responses
}

/// The failure envelope with `code` narrowed to what this operation declares,
/// plus the codes the runtime can always emit.
fn failure_schema(api: &Api, op: &Operation) -> Value {
    let codes: Vec<&str> = op
        .error_codes()
        .into_iter()
        .map(|code| api.error_code_wire_name(code))
        .collect();
    json!({
        "title": "Failure",
        "type": "object",
        "properties": {
            "errors": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "allOf": [{ "$ref": "#/components/schemas/RpcErrorItem" }],
                    "properties": { "code": { "enum": codes } },
                },
            },
        },
        "required": ["errors"],
    })
}

fn schemas(api: &Api) -> Map<String, Value> {
    let mut schemas = Map::new();

    for ty in &api.types {
        match ty {
            // Scalars are inlined at use sites; emitting a named schema would
            // add a level of indirection for no benefit.
            ApiType::Scalar(_) => {}
            ApiType::Enum(e) => {
                let mut schema = Map::new();
                if let Some(d) = &e.description {
                    schema.insert("description".into(), json!(d));
                }
                schema.insert("type".into(), json!("string"));
                schema.insert(
                    "enum".into(),
                    json!(e
                        .values
                        .iter()
                        .map(EnumValue::wire_name)
                        .collect::<Vec<_>>()),
                );
                schemas.insert(e.name.clone(), Value::Object(schema));
            }
            ApiType::Object(o) => {
                let schema = match api.sibling_tagged(o) {
                    Some(sibling) => sibling_holder_schema(api, o, sibling),
                    None => object_schema(api, &o.description, &o.fields, o.explicit_nulls),
                };
                schemas.insert(o.name.clone(), schema);
            }
            ApiType::InputObject(i) if i.one_of => {
                // Within its own variant the chosen member is present and
                // non-null, even though the SDL field is nullable.
                let variants: Vec<Value> = i
                    .fields
                    .iter()
                    .map(|f| {
                        let present = Field {
                            ty: non_null(&f.ty),
                            ..f.clone()
                        };
                        json!({
                            "type": "object",
                            "properties": { f.name.clone(): field_schema(api, &present) },
                            "required": [f.name.clone()],
                            "additionalProperties": false,
                        })
                    })
                    .collect();
                let mut schema = Map::new();
                if let Some(d) = &i.description {
                    schema.insert("description".into(), json!(d));
                }
                schema.insert("oneOf".into(), Value::Array(variants));
                schemas.insert(i.name.clone(), Value::Object(schema));
            }
            ApiType::InputObject(i) => {
                schemas.insert(
                    i.name.clone(),
                    object_schema(api, &i.description, &i.fields, false),
                );
            }
            ApiType::Union(u) => {
                let Tagging::Internal { tag } = &u.tagging else {
                    // The holder carries the tag and pairs it with the member,
                    // so the union alone is only the shapes it may take.
                    let mut schema = Map::new();
                    if let Some(d) = &u.description {
                        schema.insert("description".into(), json!(d));
                    }
                    let members: Vec<Value> = u
                        .members
                        .iter()
                        .map(|m| json!({ "$ref": format!("#/components/schemas/{}", m.name) }))
                        .collect();
                    schema.insert("oneOf".into(), Value::Array(members));
                    schemas.insert(u.name.clone(), Value::Object(schema));
                    continue;
                };
                let variants: Vec<Value> = u
                    .members
                    .iter()
                    .map(|m| json!({ "$ref": format!("#/components/schemas/{}_Tagged", m.name) }))
                    .collect();
                // The mapping is keyed by the tag value the wire carries, which
                // is what a client reads to pick the variant.
                let mut mapping = Map::new();
                for m in &u.members {
                    mapping.insert(
                        m.tag.clone(),
                        json!(format!("#/components/schemas/{}_Tagged", m.name)),
                    );
                }
                let mut schema = Map::new();
                if let Some(d) = &u.description {
                    schema.insert("description".into(), json!(d));
                }
                schema.insert("oneOf".into(), Value::Array(variants));
                schema.insert(
                    "discriminator".into(),
                    json!({ "propertyName": tag, "mapping": Value::Object(mapping) }),
                );
                schemas.insert(u.name.clone(), Value::Object(schema));

                // The wire object carries the tag inline, so each member needs
                // a tagged companion schema.
                for m in &u.members {
                    schemas.insert(
                        format!("{}_Tagged", m.name),
                        json!({
                            "allOf": [
                                { "$ref": format!("#/components/schemas/{}", m.name) },
                                {
                                    "type": "object",
                                    "properties": {
                                        tag.clone(): { "type": "string", "const": m.tag }
                                    },
                                    "required": [tag.clone()],
                                }
                            ]
                        }),
                    );
                }
            }
        }
    }

    emit_error_schemas(&mut schemas);
    schemas
}

/// Envelope schemas: a generic one plus a per-error-type variant that pins
/// `code` and its payload.
fn emit_error_schemas(schemas: &mut Map<String, Value>) {
    schemas.insert(
        "RpcErrorItem".into(),
        json!({
            "type": "object",
            "description": "One entry of `errors[]`, as produced by `treat::ErrorMessage`.",
            "properties": {
                "code": { "type": "string" },
                "message": { "type": "string" },
                "meta": {
                    "type": "object",
                    "description": "Free-form context; its keys depend on the code.",
                },
                "type": { "type": "string", "description": "RFC 9457 problem type URI." },
                "instance": { "type": "string", "description": "RFC 9457 occurrence id." },
                "source": {
                    "type": "object",
                    "description": "Locator for the offending input (JSON:API `source`).",
                    "properties": {
                        "pointer": { "type": "string", "description": "RFC 6901 JSON Pointer." },
                        "parameter": { "type": "string" },
                        "header": { "type": "string" },
                    },
                },
            },
            "required": ["code"],
        }),
    );

    schemas.insert(
        "ErrorResponse".into(),
        json!({
            "type": "object",
            "properties": {
                "errors": { "type": "array", "items": { "$ref": "#/components/schemas/RpcErrorItem" } }
            },
            "required": ["errors"],
        }),
    );
}

fn non_null(ty: &TypeRef) -> TypeRef {
    match ty {
        TypeRef::Named { name, .. } => TypeRef::Named {
            name: name.clone(),
            nullable: false,
        },
        TypeRef::List { inner, .. } => TypeRef::List {
            inner: inner.clone(),
            nullable: false,
        },
    }
}

/// An object's schema. With `explicit_nulls` every key is present — an absent
/// value is written as `null` — so every field is required, nullable or not.
fn object_schema(
    api: &Api,
    description: &Option<String>,
    fields: &[Field],
    explicit_nulls: bool,
) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();

    for f in fields {
        properties.insert(f.name.clone(), field_schema(api, f));
        if explicit_nulls || !f.ty.is_nullable() {
            required.push(f.name.clone());
        }
    }

    let mut schema = Map::new();
    if let Some(d) = description {
        schema.insert("description".into(), json!(d));
    }
    schema.insert("type".into(), json!("object"));
    schema.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        schema.insert("required".into(), json!(required));
    }
    Value::Object(schema)
}

/// An object holding a sibling-tagged union: its other fields, plus one branch
/// per member pinning the tag to that member's value and the held field to its
/// shape.
///
/// A `discriminator` cannot express this: OpenAPI reads the tag from inside the
/// variant, and here the variant is two keys of the holder.
fn sibling_holder_schema(
    api: &Api,
    o: &ObjectType,
    (held, union, tag): (&Field, &UnionType, &str),
) -> Value {
    let rest: Vec<Field> = o
        .fields
        .iter()
        .filter(|f| f.name != tag && f.name != held.name)
        .cloned()
        .collect();
    let tag_description = o
        .fields
        .iter()
        .find(|f| f.name == tag)
        .and_then(|f| f.description.clone());

    let branches: Vec<Value> = union
        .members
        .iter()
        .map(|m| {
            let mut tag_schema = Map::new();
            if let Some(d) = &tag_description {
                tag_schema.insert("description".into(), json!(d));
            }
            tag_schema.insert("type".into(), json!("string"));
            tag_schema.insert("const".into(), json!(m.tag));
            let member = Field {
                ty: TypeRef::Named {
                    name: m.name.clone(),
                    nullable: false,
                },
                ..held.clone()
            };
            json!({
                "type": "object",
                "properties": {
                    tag: Value::Object(tag_schema),
                    held.name.clone(): field_schema(api, &member),
                },
                "required": [tag, held.name.clone()],
            })
        })
        .collect();

    let mut schema = Map::new();
    if let Some(d) = &o.description {
        schema.insert("description".into(), json!(d));
    }
    // A holder with nothing but the pair is the branches alone; an empty object
    // schema beside them would constrain nothing.
    match rest.is_empty() {
        true => schema.insert("oneOf".into(), Value::Array(branches)),
        false => schema.insert(
            "allOf".into(),
            json!([object_schema(api, &None, &rest, o.explicit_nulls), { "oneOf": branches }]),
        ),
    };
    Value::Object(schema)
}

/// Schema for one field: its type plus description and constraints.
fn field_schema(api: &Api, f: &Field) -> Value {
    let mut schema = type_ref_schema(api, &f.ty);
    let extras = {
        let mut m = Map::new();
        if let Some(d) = &f.description {
            m.insert("description".into(), json!(d));
        }
        add_constraints(&mut m, &f.constraints, is_list(&f.ty));
        m
    };

    if extras.is_empty() {
        return schema;
    }

    // A bare $ref may not carry sibling keywords in JSON Schema; wrap it in
    // allOf so description and constraints survive.
    if schema.get("$ref").is_some() {
        let mut wrapper = extras;
        wrapper.insert("allOf".into(), json!([schema]));
        return Value::Object(wrapper);
    }

    if let Some(obj) = schema.as_object_mut() {
        for (k, v) in extras {
            obj.insert(k, v);
        }
    }
    schema
}

/// Whether a use site is a list, which decides how `@length` is spelled.
fn is_list(ty: &TypeRef) -> bool {
    matches!(ty, TypeRef::List { .. })
}

/// `@length` counts what the value has: characters for a string, elements for a
/// list. JSON Schema spells those as different keywords, and applies each only
/// to its own type — `minLength` on an array is ignored by every validator, so
/// emitting it there would silently drop the constraint.
fn add_constraints(target: &mut Map<String, Value>, c: &Constraints, is_list: bool) {
    let (min_key, max_key) = match is_list {
        true => ("minItems", "maxItems"),
        false => ("minLength", "maxLength"),
    };
    if let Some(v) = c.min_length {
        target.insert(min_key.into(), json!(v));
    }
    if let Some(v) = c.max_length {
        target.insert(max_key.into(), json!(v));
    }
    if let Some(v) = c.minimum {
        target.insert("minimum".into(), json!(v));
    }
    if let Some(v) = c.maximum {
        target.insert("maximum".into(), json!(v));
    }
    if let Some(v) = &c.pattern {
        target.insert("pattern".into(), json!(v));
    }
}

/// Schema for a type reference.
///
/// OpenAPI 3.1 is JSON Schema, so nullability is `type: [T, "null"]` rather
/// than the 3.0 `nullable` keyword.
fn type_ref_schema(api: &Api, ty: &TypeRef) -> Value {
    let base = match ty {
        TypeRef::Named { name, .. } => named_schema(api, name),
        TypeRef::List { inner, .. } => {
            json!({ "type": "array", "items": type_ref_schema(api, inner) })
        }
    };

    if !ty.is_nullable() {
        return base;
    }
    make_nullable(base)
}

fn make_nullable(schema: Value) -> Value {
    let Some(obj) = schema.as_object() else {
        return schema;
    };

    // $ref cannot take a sibling type, so express "T or null" as a oneOf.
    if obj.contains_key("$ref") {
        return json!({ "oneOf": [schema, { "type": "null" }] });
    }

    let mut out = obj.clone();
    match obj.get("type") {
        Some(Value::String(t)) => {
            out.insert("type".into(), json!([t, "null"]));
        }
        // An untyped schema (JSON scalar) already admits null.
        None => {}
        Some(_) => {}
    }
    Value::Object(out)
}

fn named_schema(api: &Api, name: &str) -> Value {
    let Some(s) = api.find_scalar(name) else {
        return json!({ "$ref": format!("#/components/schemas/{name}") });
    };

    // An empty openapi_type marks an unconstrained JSON value. The permitted
    // types are listed explicitly rather than left absent: in OpenAPI 3.1 a
    // schema with no `type` does mean "anything", but Swagger UI renders such
    // a field as a string, which misreports every use of the scalar.
    if s.openapi_type.is_empty() {
        let mut schema = Map::new();
        schema.insert(
            "type".into(),
            json!(["object", "array", "string", "number", "boolean", "null"]),
        );
        schema.insert(
            "description".into(),
            json!(s.description.as_deref().unwrap_or("Arbitrary JSON value.")),
        );
        return Value::Object(schema);
    }

    let mut schema = Map::new();
    schema.insert("type".into(), json!(s.openapi_type));
    if let Some(format) = &s.openapi_format {
        schema.insert("format".into(), json!(format));
    }
    if let Some(description) = &s.description {
        schema.insert("description".into(), json!(description));
    }
    // Constraints inherent to the scalar apply wherever it is used; a field may
    // still narrow them further.
    add_constraints(&mut schema, &s.constraints, false);
    Value::Object(schema)
}
