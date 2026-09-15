//! Semantic IR: the single model every generator reads.
//!
//! Deliberately free of GraphQL, Axum, OpenAPI and TypeScript concepts so that
//! adding a generator never requires touching the front end of the compiler.

/// Reference to a type at a use site, carrying nullability and list nesting.
///
/// GraphQL nullability is preserved exactly: `[String]` is a nullable list of
/// nullable items, which is a different type from `[String!]!`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeRef {
    Named { name: String, nullable: bool },
    List { inner: Box<TypeRef>, nullable: bool },
}

impl TypeRef {
    pub fn is_nullable(&self) -> bool {
        match self {
            TypeRef::Named { nullable, .. } | TypeRef::List { nullable, .. } => *nullable,
        }
    }

    /// Innermost named type, skipping list wrappers.
    pub fn base_name(&self) -> &str {
        match self {
            TypeRef::Named { name, .. } => name,
            TypeRef::List { inner, .. } => inner.base_name(),
        }
    }
}

/// Validation constraints from `@length`, `@range` and `@pattern`.
///
/// Enforced, not just described: they become the OpenAPI schema, a `validator`
/// rule on the Rust input type and a zod refinement in the browser. See
/// [`Api::resolved_constraints`] for how a field's rules combine with its
/// scalar's, which every target resolves through the same call.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Constraints {
    pub min_length: Option<i64>,
    pub max_length: Option<i64>,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub pattern: Option<String>,
}

impl Constraints {
    pub fn is_empty(&self) -> bool {
        self.min_length.is_none()
            && self.max_length.is_none()
            && self.minimum.is_none()
            && self.maximum.is_none()
            && self.pattern.is_none()
    }
}

#[derive(Debug, Clone)]
pub struct Field {
    /// Wire name, normalized to snake_case regardless of how the SDL spelled
    /// it, so every target agrees on the JSON key without renaming.
    pub name: String,
    pub description: Option<String>,
    pub ty: TypeRef,
    pub constraints: Constraints,
}

#[derive(Debug, Clone)]
pub struct ObjectType {
    pub name: String,
    pub description: Option<String>,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone)]
pub struct InputObjectType {
    pub name: String,
    pub description: Option<String>,
    pub fields: Vec<Field>,
    /// `@oneOf`: exactly one field must be present.
    pub one_of: bool,
}

#[derive(Debug, Clone)]
pub struct EnumType {
    pub name: String,
    pub description: Option<String>,
    pub values: Vec<EnumValue>,
}

#[derive(Debug, Clone)]
pub struct EnumValue {
    pub name: String,
    pub description: Option<String>,
    /// The string this value serialises to, when it is not the SDL spelling.
    ///
    /// Set by `@variant(tag:)`. GraphQL enum values must be identifiers and
    /// several wire vocabularies are not — `content-rule`, `%`, `id+checkbox` —
    /// so without this the choice would be between renaming them on the wire
    /// and dropping the enum for an untyped string.
    pub variant_tag: Option<String>,
}

impl EnumValue {
    /// What this value is called on the wire: its tag, or its SDL name.
    ///
    /// Every target serialises through here, which is what keeps the Rust
    /// `#[serde(rename)]`, the TypeScript literal, the zod member and the
    /// OpenAPI value the same string.
    pub fn wire_name(&self) -> &str {
        self.variant_tag.as_deref().unwrap_or(&self.name)
    }
}

#[derive(Debug, Clone)]
pub struct UnionType {
    pub name: String,
    pub description: Option<String>,
    pub members: Vec<UnionMember>,
    /// Wire field holding the variant tag; defaults to `kind`.
    pub discriminator: String,
}

/// One member of a union, and the tag value that selects it on the wire.
#[derive(Debug, Clone)]
pub struct UnionMember {
    /// Name of the member's object type.
    pub name: String,
    /// Value of the discriminator field for this member.
    ///
    /// Defaults to the type name, which is what a target's own tagging would
    /// produce; `@variant(tag:)` sets it to whatever the wire actually carries,
    /// since a provider tag like `googleplay` is rarely spelled as a type name.
    pub tag: String,
}

/// A custom scalar and its per-target representations.
#[derive(Debug, Clone)]
pub struct ScalarType {
    pub name: String,
    pub description: Option<String>,
    pub rust: String,
    pub typescript: String,
    pub openapi_type: String,
    pub openapi_format: Option<String>,
    /// Constraints every use of the scalar inherits, so a rule like "money is
    /// positive" is stated once instead of at each field.
    pub constraints: Constraints,
    /// Emit a Rust newtype wrapping [`Self::rust`] instead of using it
    /// directly, so distinct identifiers cannot be swapped for one another.
    ///
    /// The wrapper is `#[serde(transparent)]`: it exists inside Rust only and
    /// leaves the wire format and every other target untouched.
    pub rust_newtype: bool,
    /// Whether [`Self::rust`] is `Copy`, which decides if the newtype can
    /// derive it. `uuid::Uuid` is; `String` is not.
    pub rust_copy: bool,
    /// Whether the wrapped Rust type orders numerically, so a `@range` bound
    /// can be compared against it.
    ///
    /// The `validator` runtime implements `ValidateRange` for the primitives
    /// but not for a wrapper, so a numeric newtype opts in and the generator
    /// emits the `impl` next to the `ValidateLength` / `ValidateRegex`
    /// adapters it already has to emit.
    pub rust_range: bool,
}

impl ScalarType {
    /// Whether this scalar is an opaque byte body rather than a JSON value.
    ///
    /// Read off `openapiFormat: "binary"`, which is OpenAPI's own word for it,
    /// so a project declares one by describing it accurately instead of by
    /// naming it something the compiler recognises.
    pub fn is_binary(&self) -> bool {
        self.openapi_format.as_deref() == Some("binary")
    }
}

#[derive(Debug, Clone)]
pub enum ApiType {
    Scalar(ScalarType),
    Enum(EnumType),
    Object(ObjectType),
    InputObject(InputObjectType),
    Union(UnionType),
}

impl ApiType {
    pub fn name(&self) -> &str {
        match self {
            ApiType::Scalar(t) => &t.name,
            ApiType::Enum(t) => &t.name,
            ApiType::Object(t) => &t.name,
            ApiType::InputObject(t) => &t.name,
            ApiType::Union(t) => &t.name,
        }
    }
}

/// What a caller must prove before an operation runs.
///
/// Three values rather than a flag, because the requirement is not uniform: an
/// operation with a consumer outside our own client authenticates differently
/// from one behind a browser session, and a public one not at all. A boolean
/// would force those three into two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthRequirement {
    /// A valid session. The handler resolves it and hands the service a request
    /// context, so a method that needs to know who is calling can.
    Session,
    /// A shared token instead of a session, for a caller that has no browser.
    /// No context reaches the service: a token names no user.
    Token,
    /// Nothing. The operation answers unauthenticated.
    ///
    /// The default, so an SDL that says nothing about authentication generates
    /// exactly what it generated before `@auth` existed. Protection is opt-in
    /// because silence is not a claim about who may call — a schema that never
    /// mentions auth is one where auth lives elsewhere, and inventing a session
    /// requirement for it would break every such project.
    #[default]
    Public,
}

impl AuthRequirement {
    /// Whether the handler passes a request context to the service method.
    ///
    /// Only a session identifies a user, and the user is what the context
    /// carries, so this is the one requirement that adds a parameter.
    pub fn has_context(self) -> bool {
        self == Self::Session
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationKind {
    Query,
    Mutation,
    /// A stream of events rather than one answer, served over SSE.
    ///
    /// Its `output` is the type of a single event, not of the stream: a
    /// subscription that ends has sent some number of those, and there is no
    /// terminal value distinct from them.
    Subscription,
}

#[derive(Debug, Clone)]
pub struct Operation {
    pub name: String,
    pub description: Option<String>,
    pub kind: OperationKind,
    pub path: String,
    /// Stable identifier such as `users_get`, used for OpenAPI operationId.
    pub operation_id: String,
    /// The single `input` argument, absent when the operation takes none.
    ///
    /// An operation reading everything from the session has nothing to send,
    /// and `None` says so directly rather than through a placeholder type with
    /// a field nobody reads. The wire format is unaffected: the body is `{}`
    /// either way.
    pub input: Option<TypeRef>,
    pub output: TypeRef,
    /// Error codes this operation may return, in declaration order.
    /// Each names a value of the `ErrorCode` registry.
    pub errors: Vec<String>,
    /// Media types the request body carries, when it is not the JSON envelope.
    ///
    /// Set by `@raw(request:)`. The payload is then the input's single binary
    /// field, sent as the body verbatim, and the input's remaining fields move
    /// to the query string — there is only one body and the payload has it.
    pub raw_request: Option<Vec<String>>,
    /// Media types the response body carries, when it is not the JSON envelope.
    ///
    /// Set by `@raw(response:)`. A failure still answers with the envelope, so
    /// a caller reads the content type rather than assuming the happy path.
    pub raw_response: Option<Vec<String>>,
    /// What the caller must prove, from `@auth` on the operation or its service.
    pub auth: AuthRequirement,
    /// The role the session must carry, checked before the method is called.
    ///
    /// Set by `@auth(role:)`. `None` means any authenticated caller, which is
    /// what most operations want — a role is the exception, not the rule.
    pub role: Option<String>,
    /// Whether the operation is exposed as an MCP tool, from `@mcp` on the
    /// operation or its service.
    ///
    /// Surfaces in the OpenAPI document as `x-mcp: true`; an MCP server built
    /// over that document derives its tool list from the flag, so the SDL stays
    /// the one place deciding what an agent may call.
    pub mcp: bool,
}

impl Operation {
    /// Every code this operation is allowed to answer with: its declared
    /// `@throws` set, then the ones the runtime emits on its own.
    ///
    /// The runtime codes are appended rather than assumed to be absent: an SDL
    /// free to list `invalid_body` explicitly must not get it twice.
    pub fn error_codes(&self) -> Vec<&str> {
        let mut codes: Vec<&str> = self.errors.iter().map(String::as_str).collect();
        for code in crate::semantic::RUNTIME_ERROR_CODES {
            if !codes.contains(&code) {
                codes.push(code);
            }
        }
        codes
    }

    /// Whether the input travels in the query string instead of as a JSON body.
    ///
    /// True for a subscription, because an `EventSource` can only GET, and for a
    /// raw request, because the one body it has is already the payload. Both
    /// cases leave the input's scalar fields nowhere else to go.
    pub fn input_in_query(&self) -> bool {
        self.kind == OperationKind::Subscription || self.raw_request.is_some()
    }
}

#[derive(Debug, Clone)]
pub struct Service {
    pub name: String,
    pub description: Option<String>,
    pub operations: Vec<Operation>,
}

#[derive(Debug, Clone)]
pub struct Api {
    pub types: Vec<ApiType>,
    pub services: Vec<Service>,
}

impl Api {
    pub fn find_type(&self, name: &str) -> Option<&ApiType> {
        self.types.iter().find(|t| t.name() == name)
    }

    /// The scalar named `name`, if it is one.
    ///
    /// Scalars come from the project's config, so the generators resolve a
    /// target representation through here rather than from a fixed table.
    pub fn find_scalar(&self, name: &str) -> Option<&ScalarType> {
        match self.find_type(name) {
            Some(ApiType::Scalar(s)) => Some(s),
            _ => None,
        }
    }

    /// Whether the named type is a binary scalar. See [`ScalarType::is_binary`].
    pub fn is_binary(&self, name: &str) -> bool {
        self.find_scalar(name).is_some_and(ScalarType::is_binary)
    }

    /// The fields of an operation's input, or none when it takes no input.
    pub fn input_fields(&self, op: &Operation) -> &[Field] {
        let Some(input) = &op.input else { return &[] };
        match self.find_type(input.base_name()) {
            Some(ApiType::InputObject(i)) => &i.fields,
            _ => &[],
        }
    }

    /// The input field carrying a raw request body, if the operation has one.
    ///
    /// Exactly one such field is what the compiler enforces, so a caller here
    /// can take the first without wondering which body wins.
    pub fn body_field(&self, op: &Operation) -> Option<&Field> {
        self.input_fields(op)
            .iter()
            .find(|f| self.is_binary(f.ty.base_name()))
    }

    /// The input fields that travel in the query string.
    ///
    /// Everything but the body: a raw request puts its payload in the body and
    /// its parameters in the URL, and a subscription has no body at all.
    pub fn query_fields(&self, op: &Operation) -> Vec<&Field> {
        self.input_fields(op)
            .iter()
            .filter(|f| !self.is_binary(f.ty.base_name()))
            .collect()
    }

    /// The constraints one field is actually checked against.
    ///
    /// A field's own bound wins per rule and the scalar's fills in the rest, so a
    /// scalar states the general rule and a single field may tighten — or
    /// loosen — one part of it without discarding the others.
    ///
    /// A list inherits nothing: `@length` on a list counts elements, while the
    /// item scalar's `@length` counts what is inside one element, and applying
    /// the second to the first would measure the wrong thing.
    ///
    /// Every target resolves this the same way because they all call this, which
    /// is what keeps the browser's check and the server's check the same check.
    pub fn resolved_constraints(&self, f: &Field) -> Constraints {
        let mut c = f.constraints.clone();
        if matches!(f.ty, TypeRef::List { .. }) {
            return c;
        }
        if let Some(s) = self.find_scalar(f.ty.base_name()) {
            c.min_length = c.min_length.or(s.constraints.min_length);
            c.max_length = c.max_length.or(s.constraints.max_length);
            c.minimum = c.minimum.or(s.constraints.minimum);
            c.maximum = c.maximum.or(s.constraints.maximum);
            c.pattern = c.pattern.or_else(|| s.constraints.pattern.clone());
        }
        c
    }

    /// Whether `from` can reach `to` by following named field types.
    ///
    /// Rust needs `Box` to break such a cycle; `Vec` already provides
    /// indirection, so list-typed fields are not traversed.
    pub fn reaches(&self, from: &str, to: &str) -> bool {
        let mut seen = std::collections::BTreeSet::new();
        self.reaches_inner(from, to, &mut seen)
    }

    fn reaches_inner(
        &self,
        from: &str,
        to: &str,
        seen: &mut std::collections::BTreeSet<String>,
    ) -> bool {
        if !seen.insert(from.to_string()) {
            return false;
        }
        let fields = match self.find_type(from) {
            Some(ApiType::Object(o)) => &o.fields,
            Some(ApiType::InputObject(i)) => &i.fields,
            Some(ApiType::Union(u)) => {
                return u
                    .members
                    .iter()
                    .any(|m| m.name == to || self.reaches_inner(&m.name, to, seen));
            }
            _ => return false,
        };
        fields.iter().any(|f| {
            if matches!(f.ty, TypeRef::List { .. }) {
                return false;
            }
            let base = f.ty.base_name();
            base == to || self.reaches_inner(base, to, seen)
        })
    }
}
