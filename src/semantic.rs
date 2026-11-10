//! Semantic analysis: GraphQL AST -> validated IR.
//!
//! Everything downstream reads the IR, so all rejection of unsupported or
//! inconsistent SDL happens here.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use graphql_parser::schema::{
    Definition, Directive, EnumValue as GqlEnumValue, Field as GqlField, InputValue,
    ScalarType as ScalarTypeDef, Type as GqlType, TypeDefinition, Value,
};
use graphql_parser::Pos;

use crate::config::builtin_scalars;
use crate::error::{CompileError, Errors};
use crate::ir::*;
use crate::parser::SdlFile;

/// Directives consumed by the compiler, each with the arguments it reads.
/// Anything else is rejected — an unknown directive, or an argument the
/// directive does not read — since a silently ignored declaration would look
/// like it had an effect. A misspelt `@auth` argument would leave an operation
/// open to every caller.
const DIRECTIVES: &[(&str, &[&str])] = &[
    ("service", &[]),
    ("query", &[]),
    ("mutation", &[]),
    ("subscription", &[]),
    ("raw", &["request", "response", "stream"]),
    ("rpc", &["path"]),
    ("version", &["n"]),
    ("auth", &["require", "role", "permission"]),
    ("mcp", &["expose"]),
    ("throws", &["codes"]),
    ("length", &["min", "max"]),
    ("range", &["min", "max"]),
    ("pattern", &["value"]),
    ("oneOf", &[]),
    ("discriminator", &["field", "sibling"]),
    ("variant", &["tag"]),
    (
        "scalar",
        &[
            "rust",
            "typescript",
            "rustNewtype",
            "rustCopy",
            "rustRange",
            "openapiType",
            "openapiFormat",
        ],
    ),
    ("roles", &[]),
    ("inherits", &["roles"]),
    ("permissions", &["codes"]),
];

const DEFAULT_DISCRIMINATOR: &str = "kind";

/// What a field carries on the wire, which is what decides whether a constraint
/// on it means anything. Coarser than the type: every id, timestamp string and
/// decimal is `Text` as far as `@length` and `@pattern` are concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WireKind {
    Text,
    Number,
    Other,
}

impl std::fmt::Display for WireKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            WireKind::Text => "a string",
            WireKind::Number => "a number",
            WireKind::Other => "neither",
        };
        f.write_str(name)
    }
}

/// Enum that lists every error code the API may emit.
pub const ERROR_CODE_ENUM: &str = "ErrorCode";

/// Codes the runtime produces on its own, outside any `@throws`. The registry
/// must list them so clients see one complete set.
pub const CODE_INVALID_BODY: &str = "invalid_body";
pub const CODE_INTERNAL_ERROR: &str = "internal_error";
/// The code a generated role check answers with, which an operation declaring
/// `@auth(role:)` must therefore list in `@throws`.
pub const ROLE_REQUIRED_CODE: &str = "role_required";

pub const RUNTIME_ERROR_CODES: [&str; 2] = [CODE_INVALID_BODY, CODE_INTERNAL_ERROR];

pub fn build(files: &[SdlFile]) -> Result<Api, CompileError> {
    Analyzer::new(files).run()
}

struct Analyzer<'a> {
    files: &'a [SdlFile],
    errors: Errors,
    /// Declaration site of each type name, for duplicate detection.
    declared: HashMap<String, (String, Pos)>,
    /// `@variant(tag:)` per object type, resolved into union members once every
    /// file has been read: the tag sits on the member, which may be declared
    /// after — or in another file than — the union that names it.
    variant_tags: HashMap<String, String>,
    /// Every enum marked `@roles`, with the `@inherits` each value declares.
    /// Resolved into [`Roles`] once every file is read, since there must be
    /// exactly one and only then is it known whether there is.
    role_enums: Vec<RoleEnum>,
    /// Every enum marked `@permissions`, with the codes it declares. Resolved
    /// into [`Permissions`] once every file is read, for the same reason.
    permission_enums: Vec<PermissionEnum>,
}

/// An enum marked `@roles`, as declared: where, and what each value inherits
/// directly.
struct RoleEnum {
    file: PathBuf,
    pos: Pos,
    name: String,
    inherits: Vec<(String, Vec<String>)>,
}

/// An enum marked `@permissions`, as declared: where, and the codes its check
/// answers with.
struct PermissionEnum {
    file: PathBuf,
    pos: Pos,
    name: String,
    codes: Vec<String>,
}

impl<'a> Analyzer<'a> {
    fn new(files: &'a [SdlFile]) -> Self {
        Self {
            files,
            errors: Errors::default(),
            declared: HashMap::new(),
            variant_tags: HashMap::new(),
            role_enums: Vec::new(),
            permission_enums: Vec::new(),
        }
    }

    fn run(mut self) -> Result<Api, CompileError> {
        let mut types: Vec<ApiType> = builtin_scalars();
        let mut services: Vec<Service> = Vec::new();

        for scalar in &types {
            self.declared.insert(
                scalar.name().to_string(),
                ("<builtin>".into(), Pos { line: 0, column: 0 }),
            );
        }

        for file in self.files {
            for def in &file.document.definitions {
                match def {
                    // Directive definitions are the vocabulary itself; they
                    // declare no API types.
                    Definition::DirectiveDefinition(_) => {}
                    Definition::TypeDefinition(td) => {
                        self.collect_type(&file.path, td, &mut types, &mut services);
                    }
                    Definition::SchemaDefinition(s) => {
                        self.errors.push(CompileError::at(
                            "`schema { ... }` is not supported: services are declared with @service",
                            &file.path,
                            s.position,
                        ));
                    }
                    Definition::TypeExtension(_) => {
                        self.errors.push(CompileError::new(format!(
                            "{}: type extensions are not supported",
                            file.path.display()
                        )));
                    }
                }
            }
        }

        self.resolve_variant_tags(&mut types);
        self.resolve_sibling_contents(&mut types);
        let roles = self.resolve_roles();
        let permissions = self.resolve_permissions();

        // Sorting here is what makes generation deterministic; every generator
        // walks these vectors in order.
        types.sort_by(|a, b| a.name().cmp(b.name()));
        services.sort_by(|a, b| a.name.cmp(&b.name));

        let api = Api {
            types,
            services,
            roles,
            permissions,
        };
        self.check_references(&api);
        self.check_paths(&api);
        self.check_operation_ids(&api);
        self.check_errors(&api);
        self.check_error_code_registry(&api);
        self.check_input_output_separation(&api);
        self.check_constraints(&api);
        self.check_raw_bodies(&api);
        self.check_union_discriminators(&api);
        self.check_operation_access(&api);

        self.errors.into_result()?;
        Ok(api)
    }

    /// Where `name` was declared, rendered for a message, and whether that was
    /// one of the built-in scalars rather than a line of this project's SDL.
    fn declared_at(&self, name: &str) -> Option<(String, bool)> {
        let (file, pos) = self.declared.get(name)?;
        match pos.line {
            0 => Some(("built-in".to_string(), true)),
            line => Some((format!("{file}:{line}:{}", pos.column), false)),
        }
    }

    /// The two ways a set of field names cannot be carried to the targets.
    ///
    /// Takes the SDL spelling beside the wire name because [`Field::name`] is
    /// already normalized, and by then the pair that collided is one string:
    /// `userId` and `user_id` are both `user_id`, which the JSON object can
    /// only hold once and the Rust struct declares twice.
    fn check_field_names<'n>(
        &mut self,
        file: &Path,
        owner: &str,
        pos: Pos,
        names: impl Iterator<Item = (&'n str, &'n str)>,
    ) {
        let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
        for (sdl, wire) in names {
            if let Some(other) = seen.insert(wire, sdl) {
                self.errors.push(CompileError::at(
                    format!("type `{owner}`: fields `{other}` and `{sdl}` both become `{wire}`"),
                    file,
                    pos,
                ));
            }
            if crate::naming::is_unescapable_keyword(wire) {
                self.errors.push(CompileError::at(
                    format!(
                        "type `{owner}`: field `{sdl}` has no Rust spelling — `{wire}` is one of \
                         the keywords `r#` cannot escape; rename it"
                    ),
                    file,
                    pos,
                ));
            }
        }
    }

    fn collect_type(
        &mut self,
        file: &Path,
        td: &TypeDefinition<'static, String>,
        types: &mut Vec<ApiType>,
        services: &mut Vec<Service>,
    ) {
        let (name, pos) = match td {
            TypeDefinition::Scalar(t) => (&t.name, t.position),
            TypeDefinition::Object(t) => (&t.name, t.position),
            TypeDefinition::Interface(t) => (&t.name, t.position),
            TypeDefinition::Union(t) => (&t.name, t.position),
            TypeDefinition::Enum(t) => (&t.name, t.position),
            TypeDefinition::InputObject(t) => (&t.name, t.position),
        };

        if let TypeDefinition::Scalar(t) = td {
            self.collect_scalar(file, t, types);
            return;
        }

        if let Some((previous, _)) = self.declared_at(name) {
            self.errors.push(CompileError::at(
                format!("duplicate type `{name}`, already declared at {previous}"),
                file,
                pos,
            ));
            return;
        }
        self.declared
            .insert(name.clone(), (file.display().to_string(), pos));

        match td {
            // Handled by the early return above, which is where a scalar has to
            // be taken: it may override a built-in, which the duplicate check
            // between here and there would refuse.
            TypeDefinition::Scalar(_) => {}
            TypeDefinition::Object(t) => {
                self.check_directives(
                    file,
                    &t.directives,
                    &["service", "version", "variant", "auth", "mcp"],
                );
                let is_service = find_directive(&t.directives, "service").is_some();

                if let Some(d) = find_directive(&t.directives, "variant") {
                    match string_arg(d, "tag") {
                        Some(tag) => {
                            self.variant_tags.insert(t.name.clone(), tag);
                        }
                        None => self.errors.push(CompileError::at(
                            format!(
                                "type `{}`: malformed @variant, expected a string `tag` argument",
                                t.name
                            ),
                            file,
                            d.position,
                        )),
                    }
                }

                // A version is a property of the routes, so on a plain output
                // type it would have nothing to prefix.
                if !is_service && find_directive(&t.directives, "version").is_some() {
                    self.errors.push(CompileError::at(
                        format!(
                            "type `{}`: @version applies to a @service or one of its operations",
                            t.name
                        ),
                        file,
                        t.position,
                    ));
                }

                // Authentication guards operations. On a plain output type
                // there is nothing to guard, so a rule written there would
                // read as protection that is not happening.
                if !is_service && find_directive(&t.directives, "auth").is_some() {
                    self.errors.push(CompileError::at(
                        format!(
                            "type `{}`: @auth applies to a @service or one of its operations",
                            t.name
                        ),
                        file,
                        t.position,
                    ));
                }

                // Tool exposure is a property of operations. On a plain output
                // type the directive would expose nothing and read as if it did.
                if !is_service && find_directive(&t.directives, "mcp").is_some() {
                    self.errors.push(CompileError::at(
                        format!(
                            "type `{}`: @mcp applies to a @service or one of its operations",
                            t.name
                        ),
                        file,
                        t.position,
                    ));
                }

                if !t.implements_interfaces.is_empty() {
                    self.errors.push(CompileError::at(
                        format!("type `{}`: interfaces are not supported", t.name),
                        file,
                        t.position,
                    ));
                }

                if is_service {
                    if let Some(service) = self.build_service(file, t) {
                        services.push(service);
                    }
                } else {
                    let fields: Vec<Field> = t
                        .fields
                        .iter()
                        .map(|f| self.output_field(file, &t.name, f))
                        .collect();
                    self.check_field_names(
                        file,
                        &t.name,
                        t.position,
                        t.fields
                            .iter()
                            .map(|f| f.name.as_str())
                            .zip(fields.iter().map(|f| f.name.as_str())),
                    );
                    types.push(ApiType::Object(ObjectType {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        fields,
                    }));
                }
            }
            TypeDefinition::InputObject(t) => {
                self.check_directives(file, &t.directives, &["oneOf"]);
                let one_of = find_directive(&t.directives, "oneOf").is_some();
                let fields: Vec<Field> = t
                    .fields
                    .iter()
                    .map(|f| self.input_field(file, &t.name, f))
                    .collect();
                self.check_field_names(
                    file,
                    &t.name,
                    t.position,
                    t.fields
                        .iter()
                        .map(|f| f.name.as_str())
                        .zip(fields.iter().map(|f| f.name.as_str())),
                );

                if one_of {
                    self.validate_one_of(file, t.position, &t.name, &fields);
                }

                types.push(ApiType::InputObject(InputObjectType {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    fields,
                    one_of,
                }));
            }
            TypeDefinition::Enum(t) => {
                self.check_directives(file, &t.directives, &["roles", "permissions"]);
                let is_roles = find_directive(&t.directives, "roles").is_some();
                let permissions = find_directive(&t.directives, "permissions");
                // The registry names what can go wrong, not who may act, and
                // its Rust form is not the one an access enum gets.
                for (marked, directive) in
                    [(is_roles, "roles"), (permissions.is_some(), "permissions")]
                {
                    if marked && t.name == ERROR_CODE_ENUM {
                        self.errors.push(CompileError::at(
                            format!(
                                "`{ERROR_CODE_ENUM}` is the error code registry; it cannot be \
                                 @{directive}"
                            ),
                            file,
                            t.position,
                        ));
                    }
                }
                // A role is who acts and a permission is what is done: one value
                // cannot be both, and `@auth` names either by its set.
                if is_roles && permissions.is_some() {
                    self.errors.push(CompileError::at(
                        format!(
                            "enum `{}`: @roles and @permissions are two sets; mark one",
                            t.name
                        ),
                        file,
                        t.position,
                    ));
                }
                if let Some(d) = permissions {
                    let codes = self.permission_codes(file, &t.name, d);
                    self.permission_enums.push(PermissionEnum {
                        file: file.to_path_buf(),
                        pos: t.position,
                        name: t.name.clone(),
                        codes,
                    });
                }
                let mut inherits = Vec::new();
                let values: Vec<EnumValue> = t
                    .values
                    .iter()
                    .map(|v| {
                        self.check_directives(file, &v.directives, &["variant", "inherits"]);
                        match (is_roles, find_directive(&v.directives, "inherits")) {
                            (true, _) => inherits
                                .push((v.name.clone(), self.inherited_roles(file, &t.name, v))),
                            // The hierarchy is between roles; on any other
                            // enum it would read as a grant that is not made.
                            (false, Some(d)) => self.errors.push(CompileError::at(
                                format!(
                                    "enum `{}`: @inherits applies to a value of the @roles enum",
                                    t.name
                                ),
                                file,
                                d.position,
                            )),
                            (false, None) => {}
                        }
                        EnumValue {
                            name: v.name.clone(),
                            description: v.description.clone(),
                            variant_tag: self.enum_variant_tag(file, &t.name, v),
                        }
                    })
                    .collect();
                if is_roles {
                    self.role_enums.push(RoleEnum {
                        file: file.to_path_buf(),
                        pos: t.position,
                        name: t.name.clone(),
                        inherits,
                    });
                }
                self.check_enum_wire_names(file, &t.name, &values, t.position);
                types.push(ApiType::Enum(EnumType {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    values,
                }));
            }
            TypeDefinition::Union(t) => {
                self.check_directives(file, &t.directives, &["discriminator"]);
                if t.types.is_empty() {
                    self.errors.push(CompileError::at(
                        format!("union `{}` has no members", t.name),
                        file,
                        t.position,
                    ));
                }
                let tagging = self.union_tagging(file, t);
                types.push(ApiType::Union(UnionType {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    // The tag defaults to the type name here and is replaced by
                    // any `@variant` once every file has been read.
                    members: t
                        .types
                        .iter()
                        .map(|m| UnionMember {
                            name: m.clone(),
                            tag: m.clone(),
                        })
                        .collect(),
                    tagging,
                }));
            }
            TypeDefinition::Interface(t) => {
                self.errors.push(CompileError::at(
                    format!("interface `{}` is not supported", t.name),
                    file,
                    t.position,
                ));
            }
        }
    }

    /// A `scalar` declaration, with its target mapping from `@scalar`.
    ///
    /// Redeclaring a built-in without `@scalar` is what makes the SDL
    /// self-contained: `scalar UUID` names a type the compiler already knows.
    /// With `@scalar` the SDL overrides that mapping, so a project can retarget
    /// `DateTime` without the generator knowing about it.
    fn collect_scalar(
        &mut self,
        file: &Path,
        t: &ScalarTypeDef<'static, String>,
        types: &mut Vec<ApiType>,
    ) {
        self.check_directives(
            file,
            &t.directives,
            &["scalar", "length", "range", "pattern"],
        );

        // Replacing a built-in is the one redeclaration a scalar is allowed:
        // it is how an SDL retargets `DateTime`. Any other name is already
        // taken, and the overwrite below would otherwise swap an object type
        // for a scalar — or one scalar for another — without saying so.
        match self.declared_at(&t.name) {
            None | Some((_, true)) => {}
            Some((previous, false)) => {
                self.errors.push(CompileError::at(
                    format!(
                        "duplicate type `{}`, already declared at {previous}",
                        t.name
                    ),
                    file,
                    t.position,
                ));
                return;
            }
        }
        self.declared
            .insert(t.name.clone(), (file.display().to_string(), t.position));

        let Some(d) = find_directive(&t.directives, "scalar") else {
            // No mapping given: only valid for a scalar the compiler knows.
            if !types.iter().any(|k| k.name() == t.name) {
                self.errors.push(CompileError::at(
                    format!(
                        "scalar `{}` has no target mapping: add `@scalar(rust: \"...\", \
                         typescript: \"...\")`, since only {} are built in",
                        t.name,
                        builtin_names().join(", ")
                    ),
                    file,
                    t.position,
                ));
            }
            return;
        };

        let (Some(rust), Some(typescript)) = (string_arg(d, "rust"), string_arg(d, "typescript"))
        else {
            self.errors.push(CompileError::at(
                format!(
                    "scalar `{}`: @scalar requires `rust` and `typescript`",
                    t.name
                ),
                file,
                d.position,
            ));
            return;
        };

        let rust_newtype = bool_arg(d, "rustNewtype").unwrap_or(false);
        let rust_copy = bool_arg(d, "rustCopy").unwrap_or(false);
        let rust_range = bool_arg(d, "rustRange").unwrap_or(false);
        if rust_range && !rust_newtype {
            self.errors.push(CompileError::at(
                format!(
                    "scalar `{}`: `rustRange` only applies to a newtype; add \
                     `rustNewtype: true` or drop it",
                    t.name
                ),
                file,
                d.position,
            ));
        }
        if rust_copy && !rust_newtype {
            self.errors.push(CompileError::at(
                format!(
                    "scalar `{}`: `rustCopy` only applies to a newtype; add \
                     `rustNewtype: true` or drop it",
                    t.name
                ),
                file,
                d.position,
            ));
        }

        let scalar = ScalarType {
            name: t.name.clone(),
            description: t.description.clone(),
            rust,
            typescript,
            // An absent openapiType marks an unconstrained JSON value.
            openapi_type: string_arg(d, "openapiType").unwrap_or_default(),
            openapi_format: string_arg(d, "openapiFormat"),
            constraints: self.constraints(file, &t.directives),
            rust_newtype,
            rust_copy,
            rust_range,
        };

        // Overriding a built-in replaces it rather than colliding with it.
        match types.iter_mut().find(|k| k.name() == t.name) {
            Some(existing) => *existing = ApiType::Scalar(scalar),
            None => types.push(ApiType::Scalar(scalar)),
        }
    }

    fn build_service(
        &mut self,
        file: &Path,
        t: &graphql_parser::schema::ObjectType<'static, String>,
    ) -> Option<Service> {
        let mut operations = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        // Rust method name -> the SDL operation that claimed it.
        let mut methods: BTreeMap<String, String> = BTreeMap::new();
        let service_version = self.version_prefix(file, &t.name, &t.directives, t.position);
        // The service states the rule its operations follow; each may restate
        // it. An unannotated service leaves every operation `public`, so a
        // schema that never mentions authentication generates what it did
        // before the directive existed.
        let service_auth = self
            .auth_rule(file, &t.name, &t.directives, t.position)
            .unwrap_or_default();
        // `@mcp` on the service exposes every operation; each may restate it.
        // An unannotated service exposes nothing, so a schema that never
        // mentions MCP generates what it did before the directive existed.
        let service_mcp = self
            .mcp_rule(file, &t.name, &t.directives, t.position)
            .unwrap_or(false);

        for field in &t.fields {
            if !seen.insert(field.name.clone()) {
                self.errors.push(CompileError::at(
                    format!("service `{}`: duplicate operation `{}`", t.name, field.name),
                    file,
                    field.position,
                ));
                continue;
            }
            // The trait method, the handler function and the client method are
            // all the snake_case of this name, so two operations that normalize
            // alike declare one another twice. The path check cannot stand in
            // for this: `getById` and `get_by_id` are two routes and one method.
            let method = crate::naming::snake_case_raw(&field.name);
            if let Some(other) = methods.insert(method.clone(), field.name.clone()) {
                self.errors.push(CompileError::at(
                    format!(
                        "service `{}`: operations `{other}` and `{}` both become `{method}`",
                        t.name, field.name
                    ),
                    file,
                    field.position,
                ));
                continue;
            }
            if crate::naming::is_unescapable_keyword(&method) {
                self.errors.push(CompileError::at(
                    format!(
                        "service `{}`: operation `{}` has no Rust spelling — `{method}` is one of \
                         the keywords `r#` cannot escape; rename it",
                        t.name, field.name
                    ),
                    file,
                    field.position,
                ));
                continue;
            }
            if let Some(op) = self.build_operation(
                file,
                &t.name,
                field,
                service_version.as_deref(),
                &service_auth,
                service_mcp,
            ) {
                operations.push(op);
            }
        }

        operations.sort_by(|a, b| a.name.cmp(&b.name));
        Some(Service {
            name: t.name.clone(),
            description: t.description.clone(),
            operations,
        })
    }

    fn build_operation(
        &mut self,
        file: &Path,
        service: &str,
        field: &GqlField<'static, String>,
        service_version: Option<&str>,
        service_auth: &(AuthRequirement, Option<Access>),
        service_mcp: bool,
    ) -> Option<Operation> {
        self.check_directives(
            file,
            &field.directives,
            &[
                "query",
                "mutation",
                "subscription",
                "raw",
                "rpc",
                "version",
                "throws",
                "auth",
                "mcp",
            ],
        );

        let declared: Vec<(&str, OperationKind)> = [
            ("query", OperationKind::Query),
            ("mutation", OperationKind::Mutation),
            ("subscription", OperationKind::Subscription),
        ]
        .into_iter()
        .filter(|(name, _)| find_directive(&field.directives, name).is_some())
        .collect();

        let kind = match declared.as_slice() {
            [(_, kind)] => *kind,
            [] => {
                self.errors.push(CompileError::at(
                    format!(
                        "`{service}.{}` must be annotated with @query, @mutation or @subscription",
                        field.name
                    ),
                    file,
                    field.position,
                ));
                return None;
            }
            many => {
                let names = many
                    .iter()
                    .map(|(name, _)| format!("@{name}"))
                    .collect::<Vec<_>>()
                    .join(" and ");
                self.errors.push(CompileError::at(
                    format!("`{service}.{}` is both {names}", field.name),
                    file,
                    field.position,
                ));
                return None;
            }
        };

        let (raw_request, raw_response, raw_request_stream) = self
            .raw_bodies(file, service, field, kind)
            .unwrap_or_default();

        // The operation's own rule wins whole rather than field by field: a
        // restatement says what this operation requires, and inheriting a role
        // or a permission into a rule that dropped it would contradict the
        // narrower spelling.
        let (auth, access) = self
            .auth_rule(
                file,
                &format!("{service}.{}", field.name),
                &field.directives,
                field.position,
            )
            .unwrap_or_else(|| service_auth.clone());

        // The operation's own `@mcp` wins over its service's, so a service can
        // expose everything and carve out exceptions with `expose: false`.
        let mcp = self
            .mcp_rule(
                file,
                &format!("{service}.{}", field.name),
                &field.directives,
                field.position,
            )
            .unwrap_or(service_mcp);

        // A tool call is one JSON request and one JSON response. A subscription
        // is an open stream and a raw body is not the envelope, so exposing
        // either would promise a call shape the operation cannot honour.
        if mcp && kind == OperationKind::Subscription {
            self.errors.push(CompileError::at(
                format!(
                    "`{service}.{}`: a @subscription cannot be an MCP tool; \
                     exclude it with @mcp(expose: false)",
                    field.name
                ),
                file,
                field.position,
            ));
            return None;
        }
        if mcp && (raw_request.is_some() || raw_response.is_some()) {
            self.errors.push(CompileError::at(
                format!(
                    "`{service}.{}`: a @raw operation cannot be an MCP tool; \
                     exclude it with @mcp(expose: false)",
                    field.name
                ),
                file,
                field.position,
            ));
            return None;
        }

        // At most one argument, named `input`: this keeps the wire format a
        // single JSON body rather than a positional argument list. An
        // operation that reads everything from the session declares no
        // argument at all and sends `{}`.
        if field.arguments.len() > 1 || field.arguments.iter().any(|a| a.name != "input") {
            self.errors.push(CompileError::at(
                format!(
                    "`{service}.{}` must take a single argument named `input`, or none at all \
                     (found {})",
                    field.name,
                    describe_args(&field.arguments)
                ),
                file,
                field.position,
            ));
            return None;
        }

        let input = field
            .arguments
            .first()
            .map(|arg| self.convert_type(&arg.value_type));
        let output = self.convert_type(&field.field_type);

        let path = match find_directive(&field.directives, "rpc") {
            Some(d) => match string_arg(d, "path") {
                Some(p) if is_route(&p) => p,
                Some(p) => {
                    self.errors.push(CompileError::at(
                        format!(
                            "`{service}.{}`: @rpc(path:) must start with `/` and use only \
                             unreserved URL characters, got `{p}`",
                            field.name
                        ),
                        file,
                        field.position,
                    ));
                    return None;
                }
                None => {
                    self.errors.push(CompileError::at(
                        format!(
                            "`{service}.{}`: malformed @rpc, expected a string `path` argument",
                            field.name
                        ),
                        file,
                        field.position,
                    ));
                    return None;
                }
            },
            None => format!(
                "/rpc/{}/{}",
                service.to_lowercase(),
                field.name.to_lowercase()
            ),
        };

        // The operation's own @version wins over its service's, so one endpoint
        // can move ahead of the rest. The prefix leads whichever path was
        // resolved above, derived or pinned.
        let path = match self
            .version_prefix(
                file,
                &format!("{service}.{}", field.name),
                &field.directives,
                field.position,
            )
            .as_deref()
            .or(service_version)
        {
            Some(prefix) => format!("{prefix}{path}"),
            None => path,
        };

        let errors = match find_directive(&field.directives, "throws") {
            Some(d) => match string_list_arg(d, "codes") {
                Some(list) => list,
                None => {
                    self.errors.push(CompileError::at(
                        format!(
                            "`{service}.{}`: @throws(codes:) must be a list of strings",
                            field.name
                        ),
                        file,
                        field.position,
                    ));
                    Vec::new()
                }
            },
            None => Vec::new(),
        };

        Some(Operation {
            name: field.name.clone(),
            description: field.description.clone(),
            kind,
            path,
            operation_id: format!("{}_{}", service.to_lowercase(), field.name.to_lowercase()),
            input,
            output,
            errors,
            raw_request,
            raw_request_stream,
            raw_response,
            auth,
            access,
            mcp,
        })
    }

    /// What `@mcp` on this object declares, if anything.
    ///
    /// Returns `None` both when the directive is absent and when it is
    /// malformed, having recorded why in the latter case — the caller then
    /// falls back to its default so the rest of the type is still checked.
    fn mcp_rule(
        &mut self,
        file: &Path,
        owner: &str,
        directives: &[Directive<'static, String>],
        pos: Pos,
    ) -> Option<bool> {
        let d = find_directive(directives, "mcp")?;

        // `expose:` defaults to `true`, so the common case is `@mcp` bare and
        // the exceptions are the ones that spell themselves out.
        match bool_arg(d, "expose") {
            Some(expose) => Some(expose),
            None if d.arguments.iter().all(|(n, _)| n != "expose") => Some(true),
            None => {
                self.errors.push(CompileError::at(
                    format!("`{owner}`: @mcp(expose:) must be a boolean"),
                    file,
                    pos,
                ));
                None
            }
        }
    }

    /// What `@auth` on this object declares, if anything.
    ///
    /// Returns `None` both when the directive is absent and when it is
    /// malformed, having recorded why in the latter case — the caller then
    /// falls back to its default so the rest of the type is still checked.
    fn auth_rule(
        &mut self,
        file: &Path,
        owner: &str,
        directives: &[Directive<'static, String>],
        pos: Pos,
    ) -> Option<(AuthRequirement, Option<Access>)> {
        let d = find_directive(directives, "auth")?;

        // `require:` defaults to `session`, so the common case is `@auth` bare
        // and the exceptions are the ones that spell themselves out.
        let require = match enum_arg(d, "require") {
            None if d.arguments.iter().all(|(n, _)| n != "require") => AuthRequirement::Session,
            Some(v) if v == "session" => AuthRequirement::Session,
            Some(v) if v == "token" => AuthRequirement::Token,
            Some(v) if v == "public" => AuthRequirement::Public,
            other => {
                let got = other.unwrap_or_else(|| "a non-enum value".to_string());
                self.errors.push(CompileError::at(
                    format!(
                        "`{owner}`: @auth(require:) must be `session`, `token` or `public`, \
                         got `{got}`"
                    ),
                    file,
                    pos,
                ));
                return None;
            }
        };

        // A role and a permission are values of their enums, so each is written
        // bare, as an enum-typed argument is in GraphQL. Whether the value
        // exists is checked once every file is read: the enum may be declared
        // after the service.
        let mut bare = |argument: &str, set: &str, example: &str| -> Result<Option<String>, ()> {
            match d.arguments.iter().find(|(n, _)| n == argument) {
                None => Ok(None),
                Some((_, Value::Enum(value))) => Ok(Some(value.clone())),
                Some(_) => {
                    self.errors.push(CompileError::at(
                        format!(
                            "`{owner}`: @auth({argument}:) names a value of the @{set} enum, \
                             written bare — `{argument}: {example}`, not a string"
                        ),
                        file,
                        pos,
                    ));
                    Err(())
                }
            }
        };
        let role = bare("role", "roles", "content_edit");
        let permission = bare("permission", "permissions", "admin_users_read");
        let access = match (role.ok()?, permission.ok()?) {
            (None, None) => None,
            (Some(role), None) => Some(Access::Role(role)),
            (None, Some(permission)) => Some(Access::Permission(permission)),
            // Two answers to one question. Under a static assignment the
            // permission adds nothing the role does not say; under a dynamic
            // one the role cannot be named in the SDL. Both together would
            // also leave open whether either or both must hold.
            (Some(_), Some(_)) => {
                self.errors.push(CompileError::at(
                    format!(
                        "`{owner}`: @auth takes `role:` or `permission:`, not both — a role \
                         assigned in the SDL, or a permission whose holders the implementation \
                         decides"
                    ),
                    file,
                    pos,
                ));
                return None;
            }
        };

        // A role is assigned to a user, and the other two requirements name no
        // user: a token is not one, and `public` is nobody. ANSI RBAC's users
        // are people, so demanding a role there could never be satisfied. A
        // permission is checked against the same session, and only a session
        // operation has a context to check it on.
        if let (Some(access), false) = (&access, require == AuthRequirement::Session) {
            let message = match access {
                Access::Role(_) => {
                    "@auth(role:) needs `require: session`; a token or a public operation \
                     identifies no user whose roles could be checked"
                }
                Access::Permission(_) => {
                    "@auth(permission:) needs `require: session`; a token or a public operation \
                     has no context to check the permission against"
                }
            };
            self.errors
                .push(CompileError::at(format!("`{owner}`: {message}"), file, pos));
            return None;
        }

        Some((require, access))
    }

    /// The media types `@raw` declares for each direction, and whether the
    /// request body is streamed rather than buffered.
    ///
    /// Returns `None` on a malformed declaration, having recorded why; the
    /// caller carries on with no raw bodies so the rest of the operation is
    /// still checked and the author sees every problem in one run.
    #[allow(clippy::type_complexity)]
    fn raw_bodies(
        &mut self,
        file: &Path,
        service: &str,
        field: &GqlField<'static, String>,
        kind: OperationKind,
    ) -> Option<(Option<Vec<String>>, Option<Vec<String>>, bool)> {
        let d = find_directive(&field.directives, "raw")?;
        let op = format!("`{service}.{}`", field.name);

        // A subscription's response framing is already SSE, and its request is
        // already a query string, so there is no direction left for @raw to
        // describe — it would be describing something that is not there.
        if kind == OperationKind::Subscription {
            self.errors.push(CompileError::at(
                format!("{op}: @raw cannot be combined with @subscription"),
                file,
                field.position,
            ));
            return None;
        }

        let request = string_list_arg(d, "request");
        let response = string_list_arg(d, "response");

        if request.is_none() && response.is_none() {
            self.errors.push(CompileError::at(
                format!(
                    "{op}: @raw must name at least one of `request:` or `response:`, each a \
                     non-empty list of media types"
                ),
                file,
                field.position,
            ));
            return None;
        }

        for (arg, types) in [("request", &request), ("response", &response)] {
            let Some(types) = types else { continue };
            if types.is_empty() || types.iter().any(|t| t.trim().is_empty()) {
                self.errors.push(CompileError::at(
                    format!("{op}: @raw({arg}:) must be a non-empty list of media types"),
                    file,
                    field.position,
                ));
                return None;
            }
        }

        let stream = match bool_arg(d, "stream") {
            Some(stream) => stream,
            None if d.arguments.iter().all(|(n, _)| n != "stream") => false,
            None => {
                self.errors.push(CompileError::at(
                    format!("{op}: @raw(stream:) must be a boolean"),
                    file,
                    field.position,
                ));
                return None;
            }
        };
        // Only the request body is ever streamed, and without `request:` the
        // request has no body of its own to stream.
        if stream && request.is_none() {
            self.errors.push(CompileError::at(
                format!("{op}: @raw(stream: true) needs `request:` — it streams the request body"),
                file,
                field.position,
            ));
            return None;
        }

        Some((request, response, stream))
    }

    /// Keep a binary scalar and `@raw` in step with each other.
    ///
    /// The two say the same thing from opposite ends — `@raw` that a body is not
    /// JSON, a binary scalar what that body holds — and either one alone is a
    /// contradiction rather than a partial description. A binary field inside a
    /// JSON envelope has no encoding, and a `@raw` direction with no binary
    /// value has no payload; both would generate code that cannot work, so both
    /// are rejected here instead.
    fn check_raw_bodies(&mut self, api: &Api) {
        // Binary is a body, and a body is a whole message. Nested in an object
        // it would have to be encoded into JSON — base64 or otherwise — which is
        // exactly what `@raw` exists to avoid claiming.
        for ty in &api.types {
            let (owner, fields, kind) = match ty {
                ApiType::Object(o) => (&o.name, &o.fields, "output"),
                ApiType::InputObject(i) => (&i.name, &i.fields, "input"),
                _ => continue,
            };
            for f in fields {
                if !api.is_binary(f.ty.base_name()) {
                    continue;
                }
                if matches!(f.ty, TypeRef::List { .. }) {
                    self.errors.push(CompileError::new(format!(
                        "`{owner}.{}`: a binary body cannot be a list — one message has one body",
                        f.name
                    )));
                }
                if kind == "output" {
                    self.errors.push(CompileError::new(format!(
                        "`{owner}.{}`: a binary value cannot be a field of an output type; \
                         return it from an operation declaring @raw(response:) instead",
                        f.name
                    )));
                }
            }
        }

        for service in &api.services {
            for op in &service.operations {
                let name = format!("`{}.{}`", service.name, op.name);

                let returns_binary = api.is_binary(op.output.base_name());
                match (&op.raw_response, returns_binary) {
                    (Some(_), false) => self.errors.push(CompileError::new(format!(
                        "{name}: @raw(response:) requires the operation to return a binary \
                         scalar, but it returns `{}`",
                        op.output.base_name()
                    ))),
                    (None, true) => self.errors.push(CompileError::new(format!(
                        "{name}: returns the binary scalar `{}`, so it must declare \
                         @raw(response:) — bytes have no place in the JSON envelope",
                        op.output.base_name()
                    ))),
                    _ => {}
                }

                // Counted over the declared fields rather than found with
                // `body_field`, because "which one is the body" is only a
                // question worth answering once there is exactly one.
                let bodies: Vec<&str> = api
                    .input_fields(op)
                    .iter()
                    .filter(|f| api.is_binary(f.ty.base_name()))
                    .map(|f| f.name.as_str())
                    .collect();

                match (&op.raw_request, bodies.as_slice()) {
                    (Some(_), []) => self.errors.push(CompileError::new(format!(
                        "{name}: @raw(request:) requires exactly one input field of a binary \
                         scalar to carry the body, but the input declares none"
                    ))),
                    (Some(_), [_]) => {}
                    (Some(_), many) => self.errors.push(CompileError::new(format!(
                        "{name}: @raw(request:) requires exactly one binary input field, but \
                         the input declares {} ({})",
                        many.len(),
                        many.join(", ")
                    ))),
                    (None, []) => {}
                    (None, many) => self.errors.push(CompileError::new(format!(
                        "{name}: input field `{}` is a binary scalar, so the operation must \
                         declare @raw(request:)",
                        many[0]
                    ))),
                }

                // A streamed body is never read into the field, so a bound on it
                // would measure the empty value the server leaves there and pass
                // or fail regardless of what was sent. Resolved, because a
                // `@length` on the binary scalar reaches the field the same way.
                if op.raw_request_stream {
                    if let Some(f) = api.body_field(op) {
                        let c = api.resolved_constraints(f);
                        if c.min_length.is_some() || c.max_length.is_some() {
                            self.errors.push(CompileError::new(format!(
                                "{name}: @length on `{}` cannot apply to a streamed body, \
                                 which is never read into the field; bound it in the service, \
                                 from `RawRequest::content_length` and what it reads",
                                f.name
                            )));
                        }
                    }
                }

                if op.kind == OperationKind::Subscription && !bodies.is_empty() {
                    self.errors.push(CompileError::new(format!(
                        "{name}: a subscription sends no body, so its input cannot carry the \
                         binary field `{}`",
                        bodies[0]
                    )));
                }
            }
        }
    }

    /// Reject a constraint the value it is attached to cannot satisfy.
    ///
    /// `@length` measures characters or elements, `@range` compares numbers and
    /// `@pattern` matches text, so each one only means something on part of the
    /// type space. Attached elsewhere it is not a stricter rule but no rule at
    /// all — every target would either drop it or refuse to compile, and a
    /// dropped one reads like enforcement that is not happening.
    fn check_constraints(&mut self, api: &Api) {
        for ty in &api.types {
            let (owner, fields) = match ty {
                ApiType::Object(o) => (&o.name, &o.fields),
                ApiType::InputObject(i) => (&i.name, &i.fields),
                _ => continue,
            };
            for f in fields {
                let c = &f.constraints;
                let kind = self.wire_kind(api, f);
                let list = matches!(f.ty, TypeRef::List { .. });

                let mut reject = |directive: &str, wanted: &str| {
                    self.errors.push(CompileError::new(format!(
                        "`{owner}.{}`: @{directive} applies to {wanted}, but the field is {kind}",
                        f.name
                    )))
                };

                if (c.min_length.is_some() || c.max_length.is_some())
                    && !list
                    && kind != WireKind::Text
                {
                    reject("length", "a string or a list");
                }
                if (c.minimum.is_some() || c.maximum.is_some())
                    && (list || kind != WireKind::Number)
                {
                    reject("range", "a number");
                }
                if c.pattern.is_some() && (list || kind != WireKind::Text) {
                    reject("pattern", "a string");
                }

                // A numeric newtype is the one constraint the runtime cannot
                // reach: `validator` implements `ValidateRange` for the
                // primitives but not for a wrapper, and `rustRange` is what
                // emits the missing `impl`. Without it the generated code
                // fails in rustc instead of the SDL failing here.
                let resolved = api.resolved_constraints(f);

                // A field may tighten one half of a scalar's rule and leave the
                // other, so the pair actually checked at runtime only exists
                // after resolution — neither declaration is inverted on its own.
                // A field declaring both itself was already answered by
                // `check_bounds`, which could name the directive's position.
                let inverted = |min: Option<f64>, max: Option<f64>| match (min, max) {
                    (Some(min), Some(max)) => min > max,
                    _ => false,
                };
                if !inverted(
                    c.min_length.map(|v| v as f64),
                    c.max_length.map(|v| v as f64),
                ) && inverted(
                    resolved.min_length.map(|v| v as f64),
                    resolved.max_length.map(|v| v as f64),
                ) {
                    self.errors.push(CompileError::new(format!(
                        "`{owner}.{}`: @length resolves to an empty range against scalar `{}`",
                        f.name,
                        f.ty.base_name()
                    )));
                }
                if !inverted(c.minimum, c.maximum) && inverted(resolved.minimum, resolved.maximum) {
                    self.errors.push(CompileError::new(format!(
                        "`{owner}.{}`: @range resolves to an empty range against scalar `{}`",
                        f.name,
                        f.ty.base_name()
                    )));
                }

                if resolved.minimum.is_some() || resolved.maximum.is_some() {
                    if let Some(s) = api.find_scalar(f.ty.base_name()) {
                        if s.rust_newtype && !s.rust_range && !list && kind == WireKind::Number {
                            self.errors.push(CompileError::new(format!(
                                "`{owner}.{}`: @range cannot reach inside the `{}` newtype; \
                                 declare the scalar with rustRange: true",
                                f.name, s.name
                            )));
                        }
                    }
                }
            }
        }
    }

    /// What one field carries on the wire, as far as a constraint can tell.
    fn wire_kind(&self, api: &Api, f: &Field) -> WireKind {
        match api.find_scalar(f.ty.base_name()) {
            Some(s) => match s.openapi_type.as_str() {
                "string" => WireKind::Text,
                "integer" | "number" => WireKind::Number,
                // An empty mapping is the unconstrained-JSON scalar: no rule can
                // hold for every value it admits.
                _ => WireKind::Other,
            },
            None => WireKind::Other,
        }
    }

    /// The wire spelling `@variant(tag:)` gives one enum value.
    ///
    /// Separate from the union-member tags in [`Self::variant_tags`]: those are
    /// resolved against a union once every file is read, while an enum value's
    /// tag belongs to the value itself and needs nothing from elsewhere.
    fn enum_variant_tag(
        &mut self,
        file: &Path,
        owner: &str,
        value: &GqlEnumValue<'static, String>,
    ) -> Option<String> {
        let d = find_directive(&value.directives, "variant")?;
        match string_arg(d, "tag") {
            Some(tag) if tag.is_empty() => {
                self.errors.push(CompileError::at(
                    format!(
                        "enum `{owner}`: value `{}` has an empty @variant tag; a wire value \
                         cannot be the empty string",
                        value.name
                    ),
                    file,
                    d.position,
                ));
                None
            }
            Some(tag) => Some(tag),
            None => {
                self.errors.push(CompileError::at(
                    format!(
                        "enum `{owner}`: value `{}` has a malformed @variant, expected a string \
                         `tag` argument",
                        value.name
                    ),
                    file,
                    d.position,
                ));
                None
            }
        }
    }

    /// The roles `@inherits(roles:)` on one value of the `@roles` enum names, as
    /// written. Whether each exists and whether together they form a hierarchy
    /// is [`Self::resolve_roles`]'s to check, once the enum is complete.
    fn inherited_roles(
        &mut self,
        file: &Path,
        owner: &str,
        value: &GqlEnumValue<'static, String>,
    ) -> Vec<String> {
        let Some(d) = find_directive(&value.directives, "inherits") else {
            return Vec::new();
        };
        match enum_list_arg(d, "roles") {
            Some(roles) if !roles.is_empty() => roles,
            Some(_) => {
                self.errors.push(CompileError::at(
                    format!(
                        "enum `{owner}`: value `{}` inherits nothing; drop the @inherits",
                        value.name
                    ),
                    file,
                    d.position,
                ));
                Vec::new()
            }
            None => {
                self.errors.push(CompileError::at(
                    format!(
                        "enum `{owner}`: value `{}` has a malformed @inherits, expected `roles:` \
                         as a list of role names written bare",
                        value.name
                    ),
                    file,
                    d.position,
                ));
                Vec::new()
            }
        }
    }

    /// The `@roles` enum resolved into [`Roles`]: each `@inherits` checked
    /// against the enum's values, and the hierarchy closed over.
    ///
    /// The standard requires the role hierarchy to be a partial order, so a
    /// cycle is refused rather than collapsed: roles that inherit each other
    /// grant the same permissions, which makes them one role under two names.
    fn resolve_roles(&mut self) -> Option<Roles> {
        let mut enums = std::mem::take(&mut self.role_enums).into_iter();
        let RoleEnum {
            file,
            pos,
            name,
            inherits,
        } = enums.next()?;
        // `@auth(role:)` names a bare value, which only means something against
        // one set of roles.
        for other in enums {
            self.errors.push(CompileError::at(
                format!(
                    "enum `{}`: @roles is already on `{name}`; an API has one role set",
                    other.name
                ),
                &other.file,
                other.pos,
            ));
        }

        let roles: Vec<&str> = inherits.iter().map(|(role, _)| role.as_str()).collect();
        let mut direct: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (role, juniors) in &inherits {
            let mut seen: BTreeSet<&str> = BTreeSet::new();
            for junior in juniors {
                let problem = if !roles.contains(&junior.as_str()) {
                    Some("which is not one of its values")
                } else if junior == role {
                    Some("which is itself")
                } else if !seen.insert(junior) {
                    Some("twice")
                } else {
                    None
                };
                match problem {
                    Some(problem) => self.errors.push(CompileError::at(
                        format!("enum `{name}`: `{role}` inherits `{junior}`, {problem}"),
                        &file,
                        pos,
                    )),
                    None => direct.entry(role).or_default().push(junior),
                }
            }
        }

        let mut resolved = Vec::new();
        let mut cyclic = Vec::new();
        for &role in &roles {
            let mut reached: BTreeSet<&str> = BTreeSet::new();
            let mut pending: Vec<&str> = direct.get(role).cloned().unwrap_or_default();
            while let Some(next) = pending.pop() {
                if reached.insert(next) {
                    pending.extend(direct.get(next).into_iter().flatten().copied());
                }
            }
            if reached.contains(role) {
                cyclic.push(format!("`{role}`"));
            }
            let inherited = roles
                .iter()
                .filter(|other| **other != role && reached.contains(*other))
                .map(|other| other.to_string())
                .collect();
            resolved.push((role.to_string(), inherited));
        }
        if !cyclic.is_empty() {
            self.errors.push(CompileError::at(
                format!(
                    "enum `{name}`: @inherits forms a cycle through {}; a role hierarchy is a \
                     partial order, and roles that inherit each other are one role",
                    cyclic.join(", ")
                ),
                &file,
                pos,
            ));
        }

        Some(Roles {
            enum_name: name,
            inherits: resolved,
        })
    }

    /// Every `@auth(role:)` names a value of the `@roles` enum, and every
    /// `@auth(permission:)` a value of the `@permissions` enum.
    ///
    /// Here rather than where the directive is read, since the enum may be
    /// declared after — or in another file than — the service naming it. A value
    /// outside the set would otherwise reach the server as a name no session
    /// can hold, refusing everyone.
    fn check_operation_access(&mut self, api: &Api) {
        let values = |e: Option<&EnumType>| -> Option<BTreeSet<String>> {
            e.map(|e| e.values.iter().map(|v| v.name.clone()).collect())
        };
        let roles = values(api.role_enum());
        let permissions = values(api.permission_enum());
        for s in &api.services {
            for op in &s.operations {
                let (argument, value, set, set_name) = match &op.access {
                    None => continue,
                    Some(Access::Role(role)) => (
                        "role",
                        role,
                        &roles,
                        api.roles.as_ref().map(|r| r.enum_name.as_str()),
                    ),
                    Some(Access::Permission(permission)) => (
                        "permission",
                        permission,
                        &permissions,
                        api.permissions.as_ref().map(|p| p.enum_name.as_str()),
                    ),
                };
                let marker = if argument == "role" {
                    "roles"
                } else {
                    "permissions"
                };
                match set {
                    None => self.errors.push(CompileError::new(format!(
                        "`{}.{}`: @auth({argument}: {value}) needs an enum marked @{marker} \
                         listing it",
                        s.name, op.name
                    ))),
                    Some(values) if !values.contains(value) => {
                        self.errors.push(CompileError::new(format!(
                            "`{}.{}`: @auth({argument}: {value}) is not a value of the @{marker} \
                             enum `{}`",
                            s.name,
                            op.name,
                            set_name.unwrap_or_default()
                        )));
                    }
                    Some(_) => {}
                }
            }
        }
    }

    /// The codes `@permissions(codes:)` names: the refusals the permission check
    /// may answer with, which every operation with a permission must declare.
    ///
    /// Required rather than defaulted, because the check's answer is the
    /// implementation's — whether a credential that does not reach the
    /// permission and a user who lacks it are one refusal or two — and a default
    /// would describe a refusal the server may never send.
    fn permission_codes(
        &mut self,
        file: &Path,
        owner: &str,
        d: &Directive<'static, String>,
    ) -> Vec<String> {
        let problem = match string_list_arg(d, "codes") {
            Some(codes) if codes.is_empty() => "lists no code",
            Some(codes) => {
                let mut seen: BTreeSet<&str> = BTreeSet::new();
                match codes.iter().find(|code| !seen.insert(code.as_str())) {
                    None => return codes,
                    Some(_) => "lists a code twice",
                }
            }
            None if d.arguments.iter().all(|(n, _)| n != "codes") => "is missing",
            None => "must be a list of strings",
        };
        self.errors.push(CompileError::at(
            format!(
                "enum `{owner}`: @permissions(codes:) {problem}; it names the codes the \
                 permission check answers with, which each guarded operation declares"
            ),
            file,
            d.position,
        ));
        Vec::new()
    }

    /// The `@permissions` enum resolved into [`Permissions`]: one set per API,
    /// since `permission:` names a bare value and that means something against
    /// one enum only.
    fn resolve_permissions(&mut self) -> Option<Permissions> {
        let mut enums = std::mem::take(&mut self.permission_enums).into_iter();
        let PermissionEnum { name, codes, .. } = enums.next()?;
        for other in enums {
            self.errors.push(CompileError::at(
                format!(
                    "enum `{}`: @permissions is already on `{name}`; an API has one permission set",
                    other.name
                ),
                &other.file,
                other.pos,
            ));
        }
        Some(Permissions {
            enum_name: name,
            codes,
        })
    }

    /// Reject two values of one enum that serialise to the same string.
    ///
    /// A tag that collides with another value's tag — or with a plain value's
    /// own name — would make the wire ambiguous in one direction and lossy in
    /// the other, which no target can encode its way out of.
    fn check_enum_wire_names(&mut self, file: &Path, owner: &str, values: &[EnumValue], pos: Pos) {
        let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
        for v in values {
            if let Some(other) = seen.insert(v.wire_name(), &v.name) {
                self.errors.push(CompileError::at(
                    format!(
                        "enum `{owner}`: values `{other}` and `{}` both serialise to `{}`",
                        v.name,
                        v.wire_name()
                    ),
                    file,
                    pos,
                ));
            }
        }
    }

    /// Where a union's tag sits, from `@discriminator(field:)` or
    /// `@discriminator(sibling:)`.
    ///
    /// A sibling tag's `content` is left empty: it is the name of the field
    /// holding the union, known only once every object is read, and
    /// [`Self::resolve_sibling_contents`] fills it in.
    fn union_tagging(
        &mut self,
        file: &Path,
        t: &graphql_parser::schema::UnionType<'static, String>,
    ) -> Tagging {
        let default = || Tagging::Internal {
            tag: DEFAULT_DISCRIMINATOR.to_string(),
        };
        let Some(d) = find_directive(&t.directives, "discriminator") else {
            return default();
        };
        let field = d.arguments.iter().find(|(name, _)| name == "field");
        let sibling = d.arguments.iter().find(|(name, _)| name == "sibling");
        // An argument other than these two is refused with the directive's
        // others, in `check_directives`.
        if d.arguments
            .iter()
            .any(|(name, _)| name != "field" && name != "sibling")
        {
            return default();
        }

        let (argument, value) = match (field, sibling) {
            (None, None) => return default(),
            (Some(_), Some(_)) => {
                self.errors.push(CompileError::at(
                    format!(
                        "union `{}`: @discriminator takes `field` or `sibling`, not both — the tag \
                         sits either inside the member or beside it",
                        t.name
                    ),
                    file,
                    t.position,
                ));
                return default();
            }
            (Some((_, v)), None) => ("field", v),
            (None, Some((_, v))) => ("sibling", v),
        };
        let Value::String(tag) = value else {
            self.errors.push(CompileError::at(
                format!(
                    "union `{}`: @discriminator({argument}:) must be a string",
                    t.name
                ),
                file,
                t.position,
            ));
            return default();
        };
        // Every other wire name reaches the targets through a GraphQL name,
        // which is an identifier already. This one is a directive argument,
        // and TypeScript spells it as a bare key: `{ kind-of: 'card' }` does
        // not parse.
        if !crate::naming::is_identifier(tag) {
            self.errors.push(CompileError::at(
                format!(
                    "union `{}`: @discriminator({argument}: \"{tag}\") must be an identifier — it \
                     is emitted as an object key, not as a string",
                    t.name
                ),
                file,
                t.position,
            ));
        }
        match argument {
            "field" => Tagging::Internal { tag: tag.clone() },
            // The tag names a holder field, and fields are snake_cased on the
            // way into the IR; unconverted, `settingKind` would match nothing.
            _ => Tagging::Sibling {
                tag: crate::naming::snake_case_raw(tag),
                content: String::new(),
            },
        }
    }

    /// Name each sibling-tagged union's `content` key after the field holding it.
    ///
    /// One name across every holder, because the Rust enum states the key once:
    /// two holders spelling it differently would need two enums for one union.
    /// A union no object holds has no key to sit under at all.
    fn resolve_sibling_contents(&mut self, types: &mut [ApiType]) {
        let mut holders: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for ty in types.iter() {
            let ApiType::Object(o) = ty else { continue };
            for f in &o.fields {
                holders
                    .entry(f.ty.base_name().to_string())
                    .or_default()
                    .push((o.name.clone(), f.name.clone()));
            }
        }

        for ty in types.iter_mut() {
            let ApiType::Union(u) = ty else { continue };
            let Tagging::Sibling { content, .. } = &mut u.tagging else {
                continue;
            };
            let held_by = holders.get(&u.name).map(Vec::as_slice).unwrap_or_default();
            let Some((_, first)) = held_by.first() else {
                self.errors.push(CompileError::new(format!(
                    "union `{}`: @discriminator(sibling:) puts the tag beside the field holding the \
                     union, but no object field holds it",
                    u.name
                )));
                continue;
            };
            if held_by.iter().any(|(_, name)| name != first) {
                let sites = held_by
                    .iter()
                    .map(|(owner, name)| format!("`{owner}.{name}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                self.errors.push(CompileError::new(format!(
                    "union `{}`: every field holding a sibling-tagged union must share one name, \
                     but it is held by {sites}",
                    u.name
                )));
            }
            *content = first.clone();
        }
    }

    /// Put each `@variant(tag:)` on the union member it belongs to, and reject
    /// the two ways it can be meaningless: a tag on a type no union names, and
    /// two members of one union answering to the same tag.
    fn resolve_variant_tags(&mut self, types: &mut [ApiType]) {
        let mut used: BTreeSet<&str> = BTreeSet::new();

        for ty in types.iter_mut() {
            let ApiType::Union(u) = ty else { continue };
            let mut tags: BTreeMap<String, String> = BTreeMap::new();
            for member in &mut u.members {
                if let Some(tag) = self.variant_tags.get(&member.name) {
                    member.tag = tag.clone();
                }
                if let Some(other) = tags.insert(member.tag.clone(), member.name.clone()) {
                    self.errors.push(CompileError::new(format!(
                        "union `{}`: members `{other}` and `{}` share the tag `{}`",
                        u.name, member.name, member.tag
                    )));
                }
            }
            used.extend(u.members.iter().map(|m| m.name.as_str()));
        }

        for name in self.variant_tags.keys() {
            if !used.contains(name.as_str()) {
                self.errors.push(CompileError::new(format!(
                    "type `{name}`: @variant sets a union tag, but no union has it as a member"
                )));
            }
        }
    }

    /// `@version(n:)` as the path prefix it stands for, such as `/v2`.
    ///
    /// `None` means the SDL declared no version here, which leaves the path
    /// exactly as written — an API that never mentions a version is unversioned
    /// rather than implicitly `v1`, so adding the directive later is what moves
    /// a route, and nothing moves by default.
    fn version_prefix(
        &mut self,
        file: &Path,
        owner: &str,
        directives: &[Directive<'static, String>],
        pos: Pos,
    ) -> Option<String> {
        let d = find_directive(directives, "version")?;
        match int_arg(d, "n") {
            Some(n) if n >= 1 => Some(format!("/v{n}")),
            Some(n) => {
                self.errors.push(CompileError::at(
                    format!("`{owner}`: @version(n:) must be 1 or greater, got `{n}`"),
                    file,
                    pos,
                ));
                None
            }
            None => {
                self.errors.push(CompileError::at(
                    format!("`{owner}`: malformed @version, expected an integer `n` argument"),
                    file,
                    pos,
                ));
                None
            }
        }
    }

    fn validate_one_of(&mut self, file: &Path, pos: Pos, name: &str, fields: &[Field]) {
        if fields.len() < 2 {
            self.errors.push(CompileError::at(
                format!("@oneOf input `{name}` must declare at least two variants"),
                file,
                pos,
            ));
        }
        // A non-null member would force that variant to always be present,
        // contradicting "exactly one of".
        for f in fields {
            if !f.ty.is_nullable() {
                self.errors.push(CompileError::at(
                    format!("@oneOf input `{name}`: field `{}` must be nullable", f.name),
                    file,
                    pos,
                ));
            }
            if matches!(f.ty, TypeRef::List { .. }) {
                self.errors.push(CompileError::at(
                    format!(
                        "@oneOf input `{name}`: field `{}` must not be a list",
                        f.name
                    ),
                    file,
                    pos,
                ));
            }
        }
    }

    fn output_field(&mut self, file: &Path, owner: &str, f: &GqlField<'static, String>) -> Field {
        self.check_directives(file, &f.directives, &["length", "range", "pattern"]);
        if !f.arguments.is_empty() {
            self.errors.push(CompileError::at(
                format!(
                    "`{owner}.{}`: field arguments are only allowed on @service types",
                    f.name
                ),
                file,
                f.position,
            ));
        }
        Field {
            name: crate::naming::snake_case_raw(&f.name),
            description: f.description.clone(),
            ty: self.convert_type(&f.field_type),
            constraints: self.constraints(file, &f.directives),
        }
    }

    fn input_field(&mut self, file: &Path, owner: &str, f: &InputValue<'static, String>) -> Field {
        self.check_directives(file, &f.directives, &["length", "range", "pattern"]);
        if f.default_value.is_some() {
            self.errors.push(CompileError::at(
                format!(
                    "`{owner}.{}`: input default values are not supported",
                    f.name
                ),
                file,
                f.position,
            ));
        }
        Field {
            name: crate::naming::snake_case_raw(&f.name),
            description: f.description.clone(),
            ty: self.convert_type(&f.value_type),
            constraints: self.constraints(file, &f.directives),
        }
    }

    /// Reject a bound pair no value can sit between.
    ///
    /// Both halves are optional and either alone is a fine rule; together and
    /// inverted they are not a stricter rule but an unsatisfiable one, which
    /// every target would faithfully emit as a check that always fails.
    fn check_bounds<T: PartialOrd + std::fmt::Display>(
        &mut self,
        file: &Path,
        pos: Pos,
        directive: &str,
        min: Option<T>,
        max: Option<T>,
    ) {
        let (Some(min), Some(max)) = (min, max) else {
            return;
        };
        if min > max {
            self.errors.push(CompileError::at(
                format!("{directive}(min: {min}) is above its max of {max}: nothing satisfies it"),
                file,
                pos,
            ));
        }
    }

    /// Whether a `@pattern` is usable, having recorded why when it is not.
    ///
    /// Three ways it can fail. An empty pattern constrains nothing and emits
    /// `.regex(//)`, which JavaScript reads as the start of a comment. An
    /// invalid one would become a check that rejects every value. And one the
    /// Rust `regex` crate accepts is not automatically one a `RegExp` literal
    /// does — see [`js_incompatible`].
    fn check_pattern(&mut self, file: &Path, pos: Pos, pattern: &str) -> bool {
        if pattern.is_empty() {
            self.errors.push(CompileError::at(
                "@pattern is empty: a pattern that constrains nothing should be left out",
                file,
                pos,
            ));
            return false;
        }
        if let Err(e) = regex::Regex::new(pattern) {
            self.errors.push(CompileError::at(
                format!("@pattern `{pattern}` is not a valid regex: {e}"),
                file,
                pos,
            ));
            return false;
        }
        if let Some((construct, fix)) = js_incompatible(pattern) {
            self.errors.push(CompileError::at(
                format!(
                    "@pattern `{pattern}` uses `{construct}`, which the browser cannot read: the \
                     zod target emits this pattern as a `/.../` literal. {fix}"
                ),
                file,
                pos,
            ));
            return false;
        }
        true
    }

    fn constraints(
        &mut self,
        file: &Path,
        directives: &[Directive<'static, String>],
    ) -> Constraints {
        let mut c = Constraints::default();
        for d in directives {
            match d.name.as_str() {
                "length" => {
                    c.min_length = int_arg(d, "min");
                    c.max_length = int_arg(d, "max");
                    if c.min_length.is_none() && c.max_length.is_none() {
                        self.errors.push(CompileError::at(
                            "@length requires at least one of `min` / `max`",
                            file,
                            d.position,
                        ));
                    }
                    // A length counts characters or elements, so a negative
                    // bound describes nothing — and `validator` measures against
                    // a `u64`, so the emitted `length(min = -5)` would not even
                    // compile.
                    for (arg, bound) in [("min", c.min_length), ("max", c.max_length)] {
                        if bound.is_some_and(|v| v < 0) {
                            self.errors.push(CompileError::at(
                                format!("@length({arg}:) must not be negative"),
                                file,
                                d.position,
                            ));
                        }
                    }
                    self.check_bounds(file, d.position, "@length", c.min_length, c.max_length);
                }
                "range" => {
                    c.minimum = float_arg(d, "min");
                    c.maximum = float_arg(d, "max");
                    if c.minimum.is_none() && c.maximum.is_none() {
                        self.errors.push(CompileError::at(
                            "@range requires at least one of `min` / `max`",
                            file,
                            d.position,
                        ));
                    }
                    self.check_bounds(file, d.position, "@range", c.minimum, c.maximum);
                }
                // Compiled here so a pattern the Rust target cannot build is
                // a compile error naming the directive, rather than a
                // generated check that silently rejects everything.
                "pattern" => match string_arg(d, "value") {
                    Some(v) if self.check_pattern(file, d.position, &v) => c.pattern = Some(v),
                    Some(_) => {}
                    None => self.errors.push(CompileError::at(
                        "@pattern requires a string `value`",
                        file,
                        d.position,
                    )),
                },
                _ => {}
            }
        }
        c
    }

    /// Reject unknown directives and directives used in the wrong position.
    fn check_directives(
        &mut self,
        file: &Path,
        directives: &[Directive<'static, String>],
        allowed: &[&str],
    ) {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for d in directives {
            let Some((_, arguments)) = DIRECTIVES.iter().find(|(name, _)| *name == d.name) else {
                self.errors.push(CompileError::at(
                    format!("unknown directive `@{}`", d.name),
                    file,
                    d.position,
                ));
                continue;
            };
            if !allowed.contains(&d.name.as_str()) {
                self.errors.push(CompileError::at(
                    format!("directive `@{}` is not valid in this position", d.name),
                    file,
                    d.position,
                ));
                continue;
            }
            if !seen.insert(d.name.as_str()) {
                self.errors.push(CompileError::at(
                    format!("directive `@{}` is repeated", d.name),
                    file,
                    d.position,
                ));
            }
            // Two arguments of one name is not a declaration the reader meant:
            // first-wins would make the ignored one look like it had an effect.
            let mut args: BTreeSet<&str> = BTreeSet::new();
            for (name, _) in &d.arguments {
                if !arguments.contains(&name.as_str()) {
                    let takes = match arguments {
                        [] => "it takes none".to_string(),
                        _ => format!(
                            "it takes {}",
                            arguments
                                .iter()
                                .map(|a| format!("`{a}`"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    };
                    self.errors.push(CompileError::at(
                        format!("directive `@{}` has no argument `{name}`; {takes}", d.name),
                        file,
                        d.position,
                    ));
                }
                if !args.insert(name) {
                    self.errors.push(CompileError::at(
                        format!("directive `@{}`: argument `{name}` is repeated", d.name),
                        file,
                        d.position,
                    ));
                }
            }
        }
    }

    fn convert_type(&self, ty: &GqlType<'static, String>) -> TypeRef {
        // GraphQL marks non-null explicitly; the IR stores nullability on the
        // node itself, so unwrap NonNull and flip the flag.
        match ty {
            GqlType::NamedType(name) => TypeRef::Named {
                name: name.clone(),
                nullable: true,
            },
            GqlType::ListType(inner) => TypeRef::List {
                inner: Box::new(self.convert_type(inner)),
                nullable: true,
            },
            GqlType::NonNullType(inner) => match self.convert_type(inner) {
                TypeRef::Named { name, .. } => TypeRef::Named {
                    name,
                    nullable: false,
                },
                TypeRef::List { inner, .. } => TypeRef::List {
                    inner,
                    nullable: false,
                },
            },
        }
    }

    fn check_references(&mut self, api: &Api) {
        let known: BTreeSet<&str> = api.types.iter().map(|t| t.name()).collect();

        let check = |ty: &TypeRef, context: &str, errors: &mut Errors| {
            let base = ty.base_name();
            if !known.contains(base) {
                errors.push(CompileError::new(format!(
                    "{context} references unknown type `{base}`"
                )));
            }
        };

        for t in &api.types {
            match t {
                ApiType::Object(o) => {
                    for f in &o.fields {
                        check(&f.ty, &format!("`{}.{}`", o.name, f.name), &mut self.errors);
                    }
                }
                ApiType::InputObject(i) => {
                    for f in &i.fields {
                        check(&f.ty, &format!("`{}.{}`", i.name, f.name), &mut self.errors);
                    }
                }
                ApiType::Union(u) => {
                    for member in &u.members {
                        let name = &member.name;
                        match api.find_type(name) {
                            Some(ApiType::Object(_)) => {}
                            Some(_) => self.errors.push(CompileError::new(format!(
                                "union `{}`: member `{name}` must be an object type",
                                u.name
                            ))),
                            None => self.errors.push(CompileError::new(format!(
                                "union `{}` references unknown type `{name}`",
                                u.name
                            ))),
                        }
                    }
                }
                ApiType::Scalar(_) | ApiType::Enum(_) => {}
            }
        }

        for s in &api.services {
            for op in &s.operations {
                if let Some(input) = &op.input {
                    check(
                        input,
                        &format!("`{}.{}` input", s.name, op.name),
                        &mut self.errors,
                    );
                }
                check(
                    &op.output,
                    &format!("`{}.{}` output", s.name, op.name),
                    &mut self.errors,
                );
            }
        }
    }

    fn check_paths(&mut self, api: &Api) {
        let mut seen: BTreeMap<&str, String> = BTreeMap::new();
        for s in &api.services {
            for op in &s.operations {
                let owner = format!("{}.{}", s.name, op.name);
                if let Some(prev) = seen.insert(&op.path, owner.clone()) {
                    self.errors.push(CompileError::new(format!(
                        "duplicate RPC path `{}`: used by `{prev}` and `{owner}`",
                        op.path
                    )));
                }
            }
        }
    }

    /// Two operations answering to one `operationId`.
    ///
    /// The id is the service and the operation lowercased, so `getById` and
    /// `getbyid` share one while their paths differ and [`Self::check_paths`]
    /// sees nothing. Generated OpenAPI clients name their methods after it, and
    /// a duplicate drops one of the two rather than failing.
    fn check_operation_ids(&mut self, api: &Api) {
        let mut seen: BTreeMap<&str, String> = BTreeMap::new();
        for s in &api.services {
            for op in &s.operations {
                let owner = format!("{}.{}", s.name, op.name);
                if let Some(prev) = seen.insert(&op.operation_id, owner.clone()) {
                    self.errors.push(CompileError::new(format!(
                        "duplicate operationId `{}`: used by `{prev}` and `{owner}`",
                        op.operation_id
                    )));
                }
            }
        }
    }

    /// Every `@error` code must name a value of the registry enum, which is
    /// what keeps codes from being invented per type.
    fn check_error_code_registry(&mut self, api: &Api) {
        let registry: Option<BTreeSet<&str>> = match api.find_type(ERROR_CODE_ENUM) {
            Some(ApiType::Enum(e)) => Some(e.values.iter().map(|v| v.name.as_str()).collect()),
            Some(_) => {
                self.errors.push(CompileError::new(format!(
                    "`{ERROR_CODE_ENUM}` must be an enum: it is the error code registry"
                )));
                return;
            }
            None => {
                // Only demanded once an operation actually declares errors, so
                // an API without any does not have to provide the registry.
                if api
                    .services
                    .iter()
                    .all(|s| s.operations.iter().all(|o| o.errors.is_empty()))
                {
                    return;
                }
                self.errors.push(CompileError::new(format!(
                    "@throws requires an `{ERROR_CODE_ENUM}` enum listing every code"
                )));
                return;
            }
        };
        let Some(registry) = registry else { return };

        for value in &RUNTIME_ERROR_CODES {
            if !registry.contains(value) {
                self.errors.push(CompileError::new(format!(
                    "`{ERROR_CODE_ENUM}` must list `{value}`: the runtime emits it"
                )));
            }
        }
    }

    /// `@throws` names codes, each of which must exist in the registry.
    fn check_errors(&mut self, api: &Api) {
        let registry: BTreeSet<&str> = match api.find_type(ERROR_CODE_ENUM) {
            Some(ApiType::Enum(e)) => e.values.iter().map(|v| v.name.as_str()).collect(),
            // A missing or malformed registry is reported separately; skip
            // here so one mistake does not produce an error per operation.
            _ => return,
        };

        for s in &api.services {
            for op in &s.operations {
                let mut seen: BTreeSet<&str> = BTreeSet::new();
                for code in &op.errors {
                    if !seen.insert(code) {
                        self.errors.push(CompileError::new(format!(
                            "`{}.{}`: @throws lists `{code}` twice",
                            s.name, op.name
                        )));
                    }
                    if !registry.contains(code.as_str()) {
                        self.errors.push(CompileError::new(format!(
                            "`{}.{}`: @throws names `{code}`, which is not listed in \
                             `{ERROR_CODE_ENUM}`",
                            s.name, op.name
                        )));
                    }
                }

                // A generated check refuses before the method runs, and an
                // operation's declared codes are what its clients are typed
                // against: a refusal missing from them is one no client
                // expects. A role check answers `role_required`; a permission
                // check, the codes its set declares.
                let (argument, required): (&str, Vec<&str>) = match &op.access {
                    None => continue,
                    Some(Access::Role(_)) => ("role", vec![ROLE_REQUIRED_CODE]),
                    Some(Access::Permission(_)) => (
                        "permission",
                        api.permissions
                            .iter()
                            .flat_map(|p| p.codes.iter().map(String::as_str))
                            .collect(),
                    ),
                };
                for code in required {
                    if !op.errors.iter().any(|c| c == code) {
                        self.errors.push(CompileError::new(format!(
                            "`{}.{}`: @auth({argument}:) is checked before the handler, so \
                             @throws(codes:) must list `{code}`",
                            s.name, op.name
                        )));
                    }
                }
            }
        }
    }

    /// A union's tag must have somewhere to sit that nothing else claims.
    ///
    /// Inside the member, a field of the same name would collide with the tag
    /// the serializer injects, and serde would fail at every request. Beside
    /// it, the holder must declare the tag as an enum naming exactly the
    /// union's tags, and hold the union in the one shape a single tag key can
    /// select: a non-null field that is not a list.
    fn check_union_discriminators(&mut self, api: &Api) {
        for ty in &api.types {
            let ApiType::Union(u) = ty else { continue };
            match &u.tagging {
                Tagging::Internal { tag } => self.check_internal_tag(api, u, tag),
                Tagging::Sibling { tag, .. } => self.check_sibling_tag(api, u, tag),
            }
        }
    }

    fn check_internal_tag(&mut self, api: &Api, u: &UnionType, tag: &str) {
        for member in &u.members {
            let Some(ApiType::Object(o)) = api.find_type(member.name.as_str()) else {
                continue;
            };
            if o.fields.iter().any(|f| f.name == tag) {
                // The IR keeps no field positions, but the union's own
                // declaration site is where the discriminator was chosen.
                let (site, pos) = self
                    .declared
                    .get(&u.name)
                    .cloned()
                    .unwrap_or_else(|| ("<builtin>".into(), Pos { line: 0, column: 0 }));
                self.errors.push(CompileError::at(
                    format!(
                        "union `{}`: member `{}` declares a field `{}`; that name is the \
                         union's discriminator and is reserved for the variant tag",
                        u.name, member.name, tag
                    ),
                    Path::new(&site),
                    pos,
                ));
            }
        }
    }

    fn check_sibling_tag(&mut self, api: &Api, u: &UnionType, tag: &str) {
        let member_tags: BTreeSet<&str> = u.members.iter().map(|m| m.tag.as_str()).collect();

        for holder in &api.types {
            let ApiType::Object(o) = holder else { continue };
            let held: Vec<&Field> = o
                .fields
                .iter()
                .filter(|f| f.ty.base_name() == u.name)
                .collect();
            let Some(field) = held.first() else { continue };

            if held.len() > 1
                || api
                    .sibling_tagged(o)
                    .is_some_and(|(_, other, _)| other.name != u.name)
            {
                self.errors.push(CompileError::new(format!(
                    "`{}` holds more than one sibling-tagged union; each needs a tag key of its own \
                     and the object has one set of keys",
                    o.name
                )));
                continue;
            }
            if !matches!(
                field.ty,
                TypeRef::Named {
                    nullable: false,
                    ..
                }
            ) {
                self.errors.push(CompileError::new(format!(
                    "`{}.{}` holds union `{}`, whose tag sits beside it; the field must be \
                     non-null and not a list, since one tag key selects one member",
                    o.name, field.name, u.name
                )));
            }

            let tag_enum = o
                .fields
                .iter()
                .find(|f| f.name == tag)
                .and_then(|f| match &f.ty {
                    TypeRef::Named {
                        name,
                        nullable: false,
                    } => match api.find_type(name) {
                        Some(ApiType::Enum(e)) => Some(e),
                        _ => None,
                    },
                    _ => None,
                });
            let Some(tag_enum) = tag_enum else {
                self.errors.push(CompileError::new(format!(
                    "`{}.{}` holds union `{}`, tagged by the sibling `{tag}`; `{}` must declare \
                     `{tag}` as a non-null enum",
                    o.name, field.name, u.name, o.name
                )));
                continue;
            };

            // Exactly, not a subset: a value with no member leaves the holder a
            // tag it cannot deserialize, and a member with no value is a shape
            // the holder could never carry.
            let values: BTreeSet<&str> = tag_enum.values.iter().map(EnumValue::wire_name).collect();
            if values != member_tags {
                let missing: Vec<&str> = member_tags.difference(&values).copied().collect();
                let extra: Vec<&str> = values.difference(&member_tags).copied().collect();
                self.errors.push(CompileError::new(format!(
                    "`{}.{tag}` is `{}`, whose values must be exactly the tags of union `{}`; \
                     members with no value: {missing:?}, values with no member: {extra:?}",
                    o.name, tag_enum.name, u.name
                )));
            }
        }

        for s in &api.services {
            for op in &s.operations {
                if op.output.base_name() == u.name {
                    self.errors.push(CompileError::new(format!(
                        "`{}.{}` outputs union `{}`, whose tag sits beside it in a holding \
                         object; an operation's output has no holder",
                        s.name, op.name, u.name
                    )));
                }
            }
        }
    }

    /// Inputs and outputs live in disjoint worlds: an input object may not
    /// reference an output-only type, and vice versa.
    fn check_input_output_separation(&mut self, api: &Api) {
        for t in &api.types {
            match t {
                ApiType::InputObject(i) => {
                    for f in &i.fields {
                        match api.find_type(f.ty.base_name()) {
                            Some(ApiType::Object(_)) => self.errors.push(CompileError::new(
                                format!(
                                    "`{}.{}` references output object `{}`; inputs may only use input objects, enums and scalars",
                                    i.name, f.name, f.ty.base_name()
                                ),
                            )),
                            Some(ApiType::Union(_)) => self.errors.push(CompileError::new(
                                format!(
                                    "`{}.{}` references union `{}`; use an @oneOf input instead",
                                    i.name, f.name, f.ty.base_name()
                                ),
                            )),
                            _ => {}
                        }
                    }
                }
                ApiType::Object(o) => {
                    for f in &o.fields {
                        if let Some(ApiType::InputObject(_)) = api.find_type(f.ty.base_name()) {
                            self.errors.push(CompileError::new(format!(
                                "`{}.{}` references input object `{}` in output position",
                                o.name,
                                f.name,
                                f.ty.base_name()
                            )));
                        }
                    }
                }
                _ => {}
            }
        }

        for s in &api.services {
            for op in &s.operations {
                if let Some(input) = &op.input {
                    if let Some(ApiType::Object(_)) = api.find_type(input.base_name()) {
                        self.errors.push(CompileError::new(format!(
                            "`{}.{}`: input `{}` must be an input object, not an output type",
                            s.name,
                            op.name,
                            input.base_name()
                        )));
                    }
                }
                if let Some(ApiType::InputObject(_)) = api.find_type(op.output.base_name()) {
                    self.errors.push(CompileError::new(format!(
                        "`{}.{}`: output `{}` must not be an input object",
                        s.name,
                        op.name,
                        op.output.base_name()
                    )));
                }
            }
        }
    }
}

/// The first construct in `pattern` that Rust's `regex` accepts and a
/// JavaScript `RegExp` literal does not, paired with what to write instead.
///
/// One `@pattern` is compiled twice: by the Rust server, and by the zod schema
/// in the browser as a `/.../` literal carrying no flags. A construct only the
/// first understands is not a stricter check — it is a syntax error in the
/// generated TypeScript, and nothing in this repository parses JavaScript, so
/// the portable subset is what an SDL may declare.
///
/// Scans rather than searches: `\(` is a literal paren, and a `(?` found inside
/// one — or inside a character class such as `[(?]` — opens no group.
fn js_incompatible(pattern: &str) -> Option<(String, &'static str)> {
    const FLAGS: &str = "JavaScript carries flags after the closing slash, not inside the pattern.";

    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    let mut in_class = false;

    while i < chars.len() {
        match chars[i] {
            // An escape and the character it escapes are one unit, so neither
            // opens a group nor a class.
            '\\' => {
                let (construct, fix) = match chars.get(i + 1) {
                    Some(c @ ('p' | 'P')) if chars.get(i + 2) == Some(&'{') => (
                        format!("\\{c}{{"),
                        "Spell the class out; `\\p` needs the `u` flag, which the literal does \
                         not carry.",
                    ),
                    Some('A') => ("\\A".to_string(), "Use `^`."),
                    Some('z') => ("\\z".to_string(), "Use `$`."),
                    _ => {
                        i += 2;
                        continue;
                    }
                };
                return Some((construct, fix));
            }
            '[' if !in_class => {
                if chars[i..].starts_with(&['[', '[', ':']) {
                    return Some((
                        "[[:".to_string(),
                        "Spell the POSIX class out, e.g. `[a-zA-Z]` for `[[:alpha:]]`.",
                    ));
                }
                in_class = true;
            }
            ']' if in_class => in_class = false,
            // `(?:` is the portable non-capturing group and `(?<name>` the
            // portable named one. Every other `(?` the Rust crate accepts sets
            // inline flags, which a JavaScript literal has no syntax for.
            '(' if !in_class && chars.get(i + 1) == Some(&'?') => {
                match chars.get(i + 2) {
                    Some(':' | '<') => {}
                    Some('P') => return Some(("(?P<".to_string(), "Write `(?<name>...)`.")),
                    _ => {
                        let flags: String = chars[i + 2..]
                            .iter()
                            .take_while(|c| **c != ')' && **c != ':')
                            .collect();
                        return Some((format!("(?{flags}"), FLAGS));
                    }
                }
                i += 2;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Whether a string is usable as the route of an operation.
///
/// The path reaches a Rust string literal, a TypeScript string literal and a
/// JSON object key unchanged — the built-in templates spell it as
/// `"{{{path}}}"`, so nothing escapes it on the way. A quote or a backslash
/// would end one of those literals early, and the rest of what this excludes
/// is simply not path syntax.
fn is_route(path: &str) -> bool {
    path.starts_with('/')
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '.' | '_' | '~'))
}

fn describe_args(args: &[InputValue<'static, String>]) -> String {
    if args.is_empty() {
        return "no arguments".to_string();
    }
    let names: Vec<&str> = args.iter().map(|a| a.name.as_str()).collect();
    format!("`{}`", names.join("`, `"))
}

/// Names of the scalars the compiler maps without an `@scalar` directive.
fn builtin_names() -> Vec<String> {
    builtin_scalars()
        .iter()
        .map(|t| t.name().to_string())
        .collect()
}

fn find_directive<'d>(
    directives: &'d [Directive<'static, String>],
    name: &str,
) -> Option<&'d Directive<'static, String>> {
    directives.iter().find(|d| d.name == name)
}

fn string_arg(d: &Directive<'static, String>, name: &str) -> Option<String> {
    d.arguments
        .iter()
        .find(|(n, _)| n == name)
        .and_then(|(_, v)| match v {
            Value::String(s) => Some(s.clone()),
            _ => None,
        })
}

/// A GraphQL enum value such as `session`, as written.
///
/// Its own helper rather than `string_arg` because an enum value is not a
/// string literal to the parser, and accepting both would let two spellings of
/// one argument into the SDL.
fn enum_arg(d: &Directive<'static, String>, name: &str) -> Option<String> {
    d.arguments
        .iter()
        .find(|(n, _)| n == name)
        .and_then(|(_, v)| match v {
            Value::Enum(s) => Some(s.clone()),
            _ => None,
        })
}

fn bool_arg(d: &Directive<'static, String>, name: &str) -> Option<bool> {
    d.arguments
        .iter()
        .find(|(n, _)| n == name)
        .and_then(|(_, v)| match v {
            Value::Boolean(b) => Some(*b),
            _ => None,
        })
}

fn int_arg(d: &Directive<'static, String>, name: &str) -> Option<i64> {
    d.arguments
        .iter()
        .find(|(n, _)| n == name)
        .and_then(|(_, v)| match v {
            Value::Int(i) => i.as_i64(),
            _ => None,
        })
}

fn float_arg(d: &Directive<'static, String>, name: &str) -> Option<f64> {
    d.arguments
        .iter()
        .find(|(n, _)| n == name)
        .and_then(|(_, v)| match v {
            Value::Float(f) => Some(*f),
            Value::Int(i) => i.as_i64().map(|v| v as f64),
            _ => None,
        })
}

/// A list of GraphQL enum values such as `[content_edit, content_commit]`.
///
/// Its own helper for the reason [`enum_arg`] is: a list of strings is a
/// different literal, and accepting both would be two spellings of one list.
fn enum_list_arg(d: &Directive<'static, String>, name: &str) -> Option<Vec<String>> {
    d.arguments
        .iter()
        .find(|(n, _)| n == name)
        .and_then(|(_, v)| match v {
            Value::List(items) => items
                .iter()
                .map(|i| match i {
                    Value::Enum(s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
            _ => None,
        })
}

fn string_list_arg(d: &Directive<'static, String>, name: &str) -> Option<Vec<String>> {
    d.arguments
        .iter()
        .find(|(n, _)| n == name)
        .and_then(|(_, v)| match v {
            Value::List(items) => items
                .iter()
                .map(|i| match i {
                    Value::String(s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
            _ => None,
        })
}
