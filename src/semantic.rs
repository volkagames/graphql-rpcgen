//! Semantic analysis: GraphQL AST -> validated IR.
//!
//! Everything downstream reads the IR, so all rejection of unsupported or
//! inconsistent SDL happens here.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use graphql_parser::schema::{
    Definition, Directive, EnumType as EnumTypeDef, EnumValue as GqlEnumValue, Field as GqlField,
    InputObjectType as InputObjectTypeDef, InputValue, ObjectType as ObjectTypeDef,
    ScalarType as ScalarTypeDef, Type as GqlType, TypeDefinition, UnionType as UnionTypeDef, Value,
};
use graphql_parser::Pos;

use crate::config::builtin_scalars;
use crate::emit_common::fields_static_name;
use crate::error::{CompileError, Errors};
use crate::ir::*;
use crate::naming::{codes_type_name, pascal_case, snake_case_raw};
use crate::parser::SdlFile;

/// Directives consumed by the compiler, each with the arguments it reads.
/// Anything else is rejected — an unknown directive, or an argument the
/// directive does not read — since a silently ignored declaration would look
/// like it had an effect. A misspelt `@auth` argument would leave an operation
/// open to every caller.
const DIRECTIVES: &[(&str, &[&str])] = &[
    ("service", &[]),
    ("query", &["scope"]),
    ("mutation", &["scope"]),
    ("subscription", &["scope"]),
    ("raw", &["request", "response", "stream"]),
    ("rpc", &["path"]),
    ("http", &["method", "path", "body", "envelope"]),
    ("version", &["n"]),
    (
        "auth",
        &["require", "role", "permission", "unguarded", "explicit"],
    ),
    ("mcp", &["expose"]),
    ("explicitNulls", &[]),
    ("throws", &["codes", "groups"]),
    ("errorGroup", &["includes"]),
    ("message", &["text"]),
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
    ("scopes", &["query"]),
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

/// What an `@errorGroup` enum's name ends with. The rest is the area the group
/// covers, which also names its Rust failure type: `FolderCode`, `FolderFailure`.
pub const ERROR_GROUP_SUFFIX: &str = "Code";

/// The items every Rust enum gets beside its variants. A registry variant keeps
/// its SDL spelling, so a code spelt as one of these would shadow the item: rustc
/// resolves `ErrorCode::ALL` to the variant and only warns.
pub const ENUM_ITEMS: [&str; 3] = ["ALL", "from_name", "as_str"];

pub fn build(files: &[SdlFile]) -> erris::Result<Api> {
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
    /// Every enum marked `@scopes`, with its `query:` default. Resolved into
    /// [`Scopes`] once every file is read, for the same reason.
    scope_enums: Vec<ScopeEnum>,
    /// Where each operation is declared, by `Service.operation`, for a check
    /// that runs once every file is read to report at the declaration.
    operation_sites: HashMap<String, (PathBuf, Pos)>,
    /// Every enum marked `@errorGroup`, with the groups it includes directly.
    /// Resolved once every file is read, since an included group may be
    /// declared after — or in another file than — the group naming it.
    group_enums: Vec<GroupEnum>,
    /// Every value of the registry and of each group, in the order the SDL
    /// declares them, so a code declared twice is reported at its second
    /// declaration.
    code_sites: Vec<CodeSite>,
}

/// An enum marked `@errorGroup`, as declared: where, and what it includes.
struct GroupEnum {
    file: PathBuf,
    pos: Pos,
    name: String,
    includes: Vec<String>,
}

/// One error code as declared: by which enum, and where.
struct CodeSite {
    code: String,
    owner: String,
    file: PathBuf,
    pos: Pos,
}

/// An enum marked `@roles`, as declared: where, and what each value inherits
/// directly.
struct RoleEnum {
    file: PathBuf,
    pos: Pos,
    name: String,
    inherits: Vec<(String, Vec<String>)>,
}

/// What `@auth` on a service or an operation declares.
struct AuthRule {
    require: AuthRequirement,
    access: Option<Access>,
    /// `unguarded: true`: an operation of a service that checks a role or a
    /// permission needs neither. Said in so many words, because a restatement
    /// that merely leaves the check out reads the same as one that forgot it.
    unguarded: bool,
    /// `explicit: true`, on a service: each operation states its own check,
    /// and inherits none. See [`ServiceAuth::explicit`].
    explicit: bool,
}

/// The rule a service hands its operations.
#[derive(Debug, Clone, Default)]
struct ServiceAuth {
    require: AuthRequirement,
    access: Option<Access>,
    /// Each operation must state its own `@auth`, naming a role, a permission
    /// or `unguarded: true`. For a service whose operations need different
    /// checks: a default would hand a new operation whatever check it states —
    /// a write inheriting the read — and no default would leave a forgotten
    /// operation open to every session.
    explicit: bool,
}

impl ServiceAuth {
    /// Whether an operation of this service is held to a check, so that one
    /// without its own role or permission must say `unguarded: true`.
    fn guarded(&self) -> bool {
        self.access.is_some() || self.explicit
    }
}

/// An enum marked `@permissions`, as declared: where, and the codes its check
/// answers with.
struct PermissionEnum {
    file: PathBuf,
    pos: Pos,
    name: String,
    codes: Vec<String>,
}

/// A service or an operation holding the names the targets derive from it, as
/// [`Analyzer::check_generated_names`] reports it: `` service `Users` `` or
/// `` `Users.get` ``, and where it is declared, when that is known.
#[derive(Clone)]
struct Claimant {
    label: String,
    site: Option<String>,
}

/// An enum marked `@scopes`, as declared: where, and the scope a query needs
/// when it states none. Whether that default is one of the enum's values is
/// checked once the enum is resolved.
struct ScopeEnum {
    file: PathBuf,
    pos: Pos,
    name: String,
    query: Option<String>,
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
            scope_enums: Vec::new(),
            operation_sites: HashMap::new(),
            group_enums: Vec::new(),
            code_sites: Vec::new(),
        }
    }

    fn run(mut self) -> erris::Result<Api> {
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
        let scopes = self.resolve_scopes(&types);
        self.resolve_operation_scopes(&types, scopes.as_ref(), &mut services);
        self.resolve_error_groups(&mut types);
        self.resolve_operation_groups(&types, &mut services);
        // Under `@permissions` the roles are the implementation's data, so a
        // `@roles` enum beside it would describe a second, static set of roles
        // living next to the dynamic one. One enum marked both is reported
        // where it is declared.
        match (&roles, &permissions) {
            (Some(roles), Some(permissions)) if roles.enum_name != permissions.enum_name => {
                self.errors.push(CompileError::new(format!(
                    "enum `{}` is @roles and enum `{}` is @permissions: an API assigns \
                     permissions to roles in the SDL or leaves the assignment to the \
                     implementation; declare one set",
                    roles.enum_name, permissions.enum_name
                )));
            }
            _ => {}
        }

        // Sorting here is what makes generation deterministic; every generator
        // walks these vectors in order.
        types.sort_by(|a, b| a.name().cmp(b.name()));
        services.sort_by(|a, b| a.name.cmp(&b.name));

        let api = Api {
            types,
            services,
            roles,
            permissions,
            scopes,
        };
        self.check_references(&api);
        self.check_paths(&api);
        self.check_path_parameter_names(&api);
        self.check_operation_ids(&api);
        self.check_generated_names(&api);
        self.check_rust_item_names(&api);
        self.check_errors(&api);
        self.check_error_code_registry(&api);
        self.check_input_output_separation(&api);
        self.check_constraints(&api);
        self.check_raw_bodies(&api);
        self.check_http_bindings(&api);
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
            let previous = seen.insert(wire, sdl);
            if let Some(other) = previous {
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

        let previous = self.declared_at(name);
        if let Some((previous, _)) = previous {
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
            TypeDefinition::Object(t) => self.collect_object(file, t, types, services),
            TypeDefinition::InputObject(t) => self.collect_input_object(file, t, types),
            TypeDefinition::Enum(t) => self.collect_enum(file, t, types),
            TypeDefinition::Union(t) => self.collect_union(file, t, types),
            TypeDefinition::Interface(t) => {
                self.errors.push(CompileError::at(
                    format!("interface `{}` is not supported", t.name),
                    file,
                    t.position,
                ));
            }
        }
    }

    /// An object type: a `@service`, built into `services`, or an output type.
    fn collect_object(
        &mut self,
        file: &Path,
        t: &ObjectTypeDef<'static, String>,
        types: &mut Vec<ApiType>,
        services: &mut Vec<Service>,
    ) {
        self.check_directives(
            file,
            &t.directives,
            &[
                "service",
                "version",
                "variant",
                "auth",
                "mcp",
                "explicitNulls",
            ],
        );
        let is_service = find_directive(&t.directives, "service").is_some();
        let explicit_nulls = find_directive(&t.directives, "explicitNulls").is_some();
        if is_service && explicit_nulls {
            self.errors.push(CompileError::at(
                format!(
                    "type `{}`: @explicitNulls describes how an output type is written; \
                     a @service has no fields to write",
                    t.name
                ),
                file,
                t.position,
            ));
        }

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

        if !is_service {
            self.reject_operation_directives(file, t);
        }

        if !t.implements_interfaces.is_empty() {
            self.errors.push(CompileError::at(
                format!("type `{}`: interfaces are not supported", t.name),
                file,
                t.position,
            ));
        }

        if is_service {
            let service = self.build_service(file, t);
            if let Some(service) = service {
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
                explicit_nulls,
            }));
        }
    }

    /// The directives of a `@service` or its operations, written on a plain
    /// output type, where each would read as an effect that is not happening.
    fn reject_operation_directives(&mut self, file: &Path, t: &ObjectTypeDef<'static, String>) {
        // A version is a property of the routes, so on a plain output
        // type it would have nothing to prefix.
        if find_directive(&t.directives, "version").is_some() {
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
        if find_directive(&t.directives, "auth").is_some() {
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
        if find_directive(&t.directives, "mcp").is_some() {
            self.errors.push(CompileError::at(
                format!(
                    "type `{}`: @mcp applies to a @service or one of its operations",
                    t.name
                ),
                file,
                t.position,
            ));
        }
    }

    /// An input object, `@oneOf` or not.
    fn collect_input_object(
        &mut self,
        file: &Path,
        t: &InputObjectTypeDef<'static, String>,
        types: &mut Vec<ApiType>,
    ) {
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

    /// An enum, and the set it may be marked as: `@roles` with its `@inherits`
    /// hierarchy, `@permissions`, `@scopes`, or `@errorGroup`.
    fn collect_enum(
        &mut self,
        file: &Path,
        t: &EnumTypeDef<'static, String>,
        types: &mut Vec<ApiType>,
    ) {
        self.check_directives(
            file,
            &t.directives,
            &["roles", "permissions", "scopes", "errorGroup"],
        );
        let is_roles = find_directive(&t.directives, "roles").is_some();
        let permissions = find_directive(&t.directives, "permissions");
        let scopes = find_directive(&t.directives, "scopes");
        let group = find_directive(&t.directives, "errorGroup");
        self.check_enum_sets(
            file,
            t,
            [
                (is_roles, "roles"),
                (permissions.is_some(), "permissions"),
                (scopes.is_some(), "scopes"),
                (group.is_some(), "errorGroup"),
            ],
        );
        let error_group = group.map(|d| self.error_group(file, t, d));
        // The values of the registry and of a group are error codes: each is
        // declared once across them, and only they carry a `@message`.
        let codes = t.name == ERROR_CODE_ENUM || error_group.is_some();
        if codes {
            for v in &t.values {
                self.code_sites.push(CodeSite {
                    code: v.name.clone(),
                    owner: t.name.clone(),
                    file: file.to_path_buf(),
                    pos: v.position,
                });
            }
        }
        if let Some(d) = scopes {
            let query = self.scope_default(file, &t.name, d);
            self.scope_enums.push(ScopeEnum {
                file: file.to_path_buf(),
                pos: t.position,
                name: t.name.clone(),
                query,
            });
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
                self.check_directives(file, &v.directives, &["variant", "inherits", "message"]);
                match (is_roles, find_directive(&v.directives, "inherits")) {
                    (true, _) => {
                        inherits.push((v.name.clone(), self.inherited_roles(file, &t.name, v)))
                    }
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
                    message: self.code_message(file, &t.name, codes, v),
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
        // A code keeps its SDL spelling as a variant of the registry and of
        // every group holding it.
        if codes {
            for v in t
                .values
                .iter()
                .filter(|v| ENUM_ITEMS.contains(&v.name.as_str()))
            {
                self.errors.push(CompileError::at(
                    format!(
                        "`{}` cannot list `{}`: the generated `{}::{}` would be shadowed by it",
                        t.name, v.name, t.name, v.name
                    ),
                    file,
                    v.position,
                ));
            }
        }
        types.push(ApiType::Enum(EnumType {
            name: t.name.clone(),
            description: t.description.clone(),
            values,
            error_group,
        }));
    }

    /// `@errorGroup` on the enum `t`: the name checked for the suffix its
    /// failure type is derived from, and the groups `includes:` names as
    /// written. Whether each is a group, and whether the includes close into a
    /// cycle, is [`Self::resolve_error_groups`]'s to check once every file is
    /// read.
    fn error_group(
        &mut self,
        file: &Path,
        t: &EnumTypeDef<'static, String>,
        d: &Directive<'static, String>,
    ) -> ErrorGroup {
        let fail = |this: &mut Self, message: String| {
            this.errors
                .push(CompileError::at(message, file, t.position));
        };
        if t.name == ERROR_CODE_ENUM {
            fail(
                self,
                format!(
                    "`{ERROR_CODE_ENUM}` is the error code registry, which every group's codes \
                     join; it cannot be an @errorGroup itself"
                ),
            );
        }
        // The runtime answers these for any operation, so they stay the
        // registry's; `internal_error` is every group's already.
        for v in &t.values {
            if RUNTIME_ERROR_CODES.contains(&v.name.as_str()) {
                self.errors.push(CompileError::at(
                    format!(
                        "@errorGroup `{}` cannot declare `{}`: the runtime answers it for any \
                         operation, so it stays in `{ERROR_CODE_ENUM}`, and every group holds \
                         `{CODE_INTERNAL_ERROR}` already",
                        t.name, v.name
                    ),
                    file,
                    v.position,
                ));
            }
        }
        let area = t.name.strip_suffix(ERROR_GROUP_SUFFIX);
        if area.is_none_or(str::is_empty) {
            fail(
                self,
                format!(
                    "@errorGroup enum `{}` must be named `<Area>{ERROR_GROUP_SUFFIX}`: the \
                     area names its Rust failure type, `<Area>Failure`",
                    t.name
                ),
            );
        }
        let includes = match has_arg(d, "includes") {
            false => Vec::new(),
            true => match enum_list_arg(d, "includes") {
                Some(includes) => includes,
                None => {
                    fail(
                        self,
                        format!(
                            "enum `{}`: @errorGroup(includes:) must be a list of group names \
                             written bare",
                            t.name
                        ),
                    );
                    Vec::new()
                }
            },
        };
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for name in &includes {
            let first = seen.insert(name);
            if !first {
                fail(
                    self,
                    format!(
                        "enum `{}`: @errorGroup(includes:) names `{name}` twice",
                        t.name
                    ),
                );
            }
        }
        self.group_enums.push(GroupEnum {
            file: file.to_path_buf(),
            pos: t.position,
            name: t.name.clone(),
            includes: includes.clone(),
        });
        ErrorGroup {
            own: t.values.iter().map(|v| v.name.clone()).collect(),
            // The direct includes until the groups are resolved, which closes
            // over them.
            includes,
        }
    }

    /// `@message(text:)` on a value of the enum `owner`, which must be an
    /// error code (`codes`): of the registry or of an error group.
    fn code_message(
        &mut self,
        file: &Path,
        owner: &str,
        codes: bool,
        value: &GqlEnumValue<'static, String>,
    ) -> Option<String> {
        let d = find_directive(&value.directives, "message")?;
        if !codes {
            self.errors.push(CompileError::at(
                format!(
                    "enum `{owner}`: @message applies to an error code — a value of \
                     `{ERROR_CODE_ENUM}` or of an @errorGroup enum — not to `{}`",
                    value.name
                ),
                file,
                d.position,
            ));
            return None;
        }
        let text = string_arg(d, "text");
        if text.is_none() {
            self.errors.push(CompileError::at(
                format!(
                    "enum `{owner}`: value `{}` has a malformed @message, expected a string \
                     `text` argument",
                    value.name
                ),
                file,
                d.position,
            ));
        }
        text
    }

    /// The sets an enum is marked as, each a question of its own: who acts
    /// (`@roles`), what is done (`@permissions`), what a credential may do
    /// (`@scopes`), what can go wrong in one area (`@errorGroup`). One enum
    /// answers one of them, and the error code registry none of the first
    /// three. Each mark comes with its directive's name.
    fn check_enum_sets(
        &mut self,
        file: &Path,
        t: &EnumTypeDef<'static, String>,
        marks: [(bool, &str); 4],
    ) {
        let [(is_roles, _), (is_permissions, _), (is_scopes, _), (is_group, _)] = marks;
        // The registry names what can go wrong, not who may act, and
        // its Rust form is not the one an access enum gets. Whether it may
        // be a group is the group's check to say.
        for (marked, directive) in marks.into_iter().take(3) {
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
        if is_roles && is_permissions {
            self.errors.push(CompileError::at(
                format!(
                    "enum `{}`: @roles and @permissions are two sets; mark one",
                    t.name
                ),
                file,
                t.position,
            ));
        }
        // A scope is what a credential may do for whoever holds it,
        // not who acts or what a user may be granted: one value cannot
        // answer two of those questions.
        if is_scopes && (is_roles || is_permissions) {
            self.errors.push(CompileError::at(
                format!(
                    "enum `{}`: @scopes is the set a credential carries, apart from the \
                     roles and the permissions; mark one",
                    t.name
                ),
                file,
                t.position,
            ));
        }
        // What can go wrong is not who acts, what is done or what a
        // credential may do.
        let access = marks
            .iter()
            .take(3)
            .filter(|(marked, _)| *marked)
            .map(|(_, directive)| format!("@{directive}"))
            .collect::<Vec<_>>();
        if is_group && !access.is_empty() {
            self.errors.push(CompileError::at(
                format!(
                    "enum `{}`: an @errorGroup is a set of error codes, apart from {}; mark one",
                    t.name,
                    access.join(" and ")
                ),
                file,
                t.position,
            ));
        }
    }

    /// A union, its members tagged by name until `@variant` tags are resolved.
    fn collect_union(
        &mut self,
        file: &Path,
        t: &UnionTypeDef<'static, String>,
        types: &mut Vec<ApiType>,
    ) {
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
            let known = types.iter().any(|k| k.name() == t.name);
            if !known {
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
        let service_rule = self.auth_rule(file, &t.name, &t.directives, t.position);
        let service_auth = match service_rule {
            None => ServiceAuth::default(),
            Some(rule) => {
                // There is nothing above a service for it to drop.
                if rule.unguarded {
                    self.errors.push(CompileError::at(
                        format!(
                            "service `{}`: @auth(unguarded:) applies to an operation whose \
                             service checks a role or a permission, not to a service",
                            t.name
                        ),
                        file,
                        t.position,
                    ));
                }
                ServiceAuth {
                    require: rule.require,
                    access: rule.access,
                    explicit: rule.explicit,
                }
            }
        };
        // `@mcp` on the service exposes every operation; each may restate it.
        // An unannotated service exposes nothing, so a schema that never
        // mentions MCP generates what it did before the directive existed.
        let service_mcp = self
            .mcp_rule(file, &t.name, &t.directives, t.position)
            .unwrap_or(false);

        for field in &t.fields {
            let first = seen.insert(field.name.clone());
            if !first {
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
            let previous = methods.insert(method.clone(), field.name.clone());
            if let Some(other) = previous {
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
            let op = self.build_operation(
                file,
                &t.name,
                field,
                service_version.as_deref(),
                &service_auth,
                service_mcp,
            );
            if let Some(op) = op {
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
        service_auth: &ServiceAuth,
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
                "http",
                "version",
                "throws",
                "auth",
                "mcp",
            ],
        );

        let (kind_directive, kind) = self.operation_kind(file, service, field)?;

        let (raw_request, raw_response, raw_request_stream) = self
            .raw_bodies(file, service, field, kind)
            .unwrap_or_default();

        let http = match find_directive(&field.directives, "http") {
            Some(d) => {
                let binding = self.http_binding(file, service, field, d, kind);
                // A malformed binding is reported; the operation is dropped
                // rather than mounted on a route nobody declared.
                Some(binding?)
            }
            None => None,
        };
        if http.is_some() && (raw_request.is_some() || raw_response.is_some()) {
            self.errors.push(CompileError::at(
                format!(
                    "`{service}.{}`: @http cannot be combined with @raw; a raw body already \
                     decides how the operation sits on HTTP",
                    field.name
                ),
                file,
                field.position,
            ));
            return None;
        }

        // The scope as written; defaults and the rules about where one applies
        // are resolved once every file is read, since the `@scopes` enum may be
        // declared after the service.
        self.operation_sites.insert(
            format!("{service}.{}", field.name),
            (file.to_path_buf(), field.position),
        );
        let scope = self.declared_scope(
            file,
            &format!("{service}.{}", field.name),
            &field.directives,
            kind_directive,
            field.position,
        );

        // The operation's own rule wins whole rather than field by field: a
        // restatement says what this operation requires, and inheriting a role
        // or a permission into a rule that dropped it would contradict the
        // narrower spelling.
        let owner = format!("{service}.{}", field.name);
        let own_rule = self.auth_rule(file, &owner, &field.directives, field.position);
        self.check_operation_rule(
            file,
            &owner,
            field.position,
            own_rule.as_ref(),
            service_auth,
        );
        let (auth, access) = match own_rule {
            None => (service_auth.require, service_auth.access.clone()),
            Some(rule) => (rule.require, rule.access),
        };

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

        if mcp {
            let has_raw = raw_request.is_some() || raw_response.is_some();
            self.check_mcp_shape(file, service, field, kind, http.is_some(), has_raw)?;
        }

        // At most one argument, named `input`: this keeps the wire format a
        // single JSON body rather than a positional argument list. An
        // operation that reads everything from the session declares no
        // argument at all and sends `{}`.
        let other_argument = field.arguments.iter().any(|a| a.name != "input");
        if field.arguments.len() > 1 || other_argument {
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

        let path = self.declared_path(file, service, field, http.is_some())?;
        let path =
            self.versioned_path(file, service, field, http.is_some(), service_version, path)?;

        let (errors, error_groups) = self.declared_errors(file, service, field);

        Some(Operation {
            name: field.name.clone(),
            description: field.description.clone(),
            kind,
            path,
            operation_id: format!("{}_{}", service.to_lowercase(), field.name.to_lowercase()),
            input,
            output,
            errors,
            error_groups,
            raw_request,
            raw_request_stream,
            raw_response,
            auth,
            access,
            mcp,
            scope,
            http,
        })
    }

    /// Which of `@query`, `@mutation` and `@subscription` the operation is,
    /// with the directive that said so; `None` once reported for none or
    /// several.
    fn operation_kind(
        &mut self,
        file: &Path,
        service: &str,
        field: &GqlField<'static, String>,
    ) -> Option<(&'static str, OperationKind)> {
        let declared: Vec<(&'static str, OperationKind)> = [
            ("query", OperationKind::Query),
            ("mutation", OperationKind::Mutation),
            ("subscription", OperationKind::Subscription),
        ]
        .into_iter()
        .filter(|(name, _)| find_directive(&field.directives, name).is_some())
        .collect();

        match declared.as_slice() {
            [declared] => Some(*declared),
            [] => {
                self.errors.push(CompileError::at(
                    format!(
                        "`{service}.{}` must be annotated with @query, @mutation or @subscription",
                        field.name
                    ),
                    file,
                    field.position,
                ));
                None
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
                None
            }
        }
    }

    /// Refuse an operation `@mcp` exposes but a tool call cannot carry; `None`
    /// once reported.
    fn check_mcp_shape(
        &mut self,
        file: &Path,
        service: &str,
        field: &GqlField<'static, String>,
        kind: OperationKind,
        has_http: bool,
        has_raw: bool,
    ) -> Option<()> {
        // A tool call is one JSON request and one JSON response. A subscription
        // is an open stream and a raw body is not the envelope, so exposing
        // either would promise a call shape the operation cannot honour.
        if kind == OperationKind::Subscription {
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
        if has_http {
            self.errors.push(CompileError::at(
                format!(
                    "`{service}.{}`: an @http operation cannot be an MCP tool, whose call is \
                     the JSON envelope; exclude it with @mcp(expose: false)",
                    field.name
                ),
                file,
                field.position,
            ));
            return None;
        }
        if has_raw {
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
        Some(())
    }

    /// The route as declared, before any `@version` prefix: `@http(path:)`,
    /// `@rpc(path:)`, or `/rpc/{service}/{operation}` derived from the names;
    /// `None` once reported.
    fn declared_path(
        &mut self,
        file: &Path,
        service: &str,
        field: &GqlField<'static, String>,
        has_http: bool,
    ) -> Option<String> {
        let rpc = find_directive(&field.directives, "rpc");
        let http_path = match (has_http, rpc) {
            (true, Some(_)) => {
                self.errors.push(CompileError::at(
                    format!(
                        "`{service}.{}`: @http(path:) and @rpc(path:) both name the route; \
                         keep @http",
                        field.name
                    ),
                    file,
                    field.position,
                ));
                return None;
            }
            (true, None) => {
                find_directive(&field.directives, "http").and_then(|d| string_arg(d, "path"))
            }
            (false, _) => None,
        };
        match (http_path, rpc) {
            (Some(path), _) => Some(path),
            (None, Some(d)) => match string_arg(d, "path") {
                Some(p) => {
                    let routable = is_route(&p);
                    if routable {
                        return Some(p);
                    }
                    self.errors.push(CompileError::at(
                        format!(
                            "`{service}.{}`: @rpc(path:) must start with `/` and use only \
                             unreserved URL characters, got `{p}`",
                            field.name
                        ),
                        file,
                        field.position,
                    ));
                    None
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
                    None
                }
            },
            (None, None) => Some(format!(
                "/rpc/{}/{}",
                service.to_lowercase(),
                field.name.to_lowercase()
            )),
        }
    }

    /// `path` under the version prefix that applies to it; `None` once
    /// reported.
    fn versioned_path(
        &mut self,
        file: &Path,
        service: &str,
        field: &GqlField<'static, String>,
        has_http: bool,
        service_version: Option<&str>,
        path: String,
    ) -> Option<String> {
        // The operation's own @version wins over its service's, so one endpoint
        // can move ahead of the rest. The prefix leads whichever path was
        // resolved above, derived or pinned — but not an `@http` one: that is
        // the URL clients already call, and a prefix would move it. Its
        // service's version passes it by; its own is refused, since the
        // version belongs in the path it spells out.
        let own_version = self.version_prefix(
            file,
            &format!("{service}.{}", field.name),
            &field.directives,
            field.position,
        );
        if has_http && own_version.is_some() {
            self.errors.push(CompileError::at(
                format!(
                    "`{service}.{}`: @version would move the @http path clients already call; \
                     write the version into @http(path:)",
                    field.name
                ),
                file,
                field.position,
            ));
            return None;
        }
        let prefix = match has_http {
            true => None,
            false => own_version.as_deref().or(service_version),
        };
        Some(match prefix {
            Some(prefix) => format!("{prefix}{path}"),
            None => path,
        })
    }

    /// The codes and the groups `@throws` lists; none, once reported, for a
    /// malformed list. A `@throws` names at least one of the two arguments.
    fn declared_errors(
        &mut self,
        file: &Path,
        service: &str,
        field: &GqlField<'static, String>,
    ) -> (Vec<String>, Vec<String>) {
        let Some(d) = find_directive(&field.directives, "throws") else {
            return (Vec::new(), Vec::new());
        };
        let fail = |this: &mut Self, message: &str| {
            this.errors.push(CompileError::at(
                format!("`{service}.{}`: {message}", field.name),
                file,
                field.position,
            ));
        };
        let groups_given = has_arg(d, "groups");
        let codes = match (string_list_arg(d, "codes"), groups_given) {
            (Some(list), _) => list,
            (None, true) if !has_arg(d, "codes") => Vec::new(),
            (None, _) => {
                fail(self, "@throws(codes:) must be a list of strings");
                Vec::new()
            }
        };
        let groups = match (enum_list_arg(d, "groups"), groups_given) {
            (Some(list), _) => list,
            (None, false) => Vec::new(),
            (None, true) => {
                fail(
                    self,
                    "@throws(groups:) must be a list of @errorGroup names written bare",
                );
                Vec::new()
            }
        };
        (codes, groups)
    }

    /// Read `@http`: the method, the path and its `{parameters}`, where the
    /// other fields come from, and whether a success keeps the envelope.
    ///
    /// Only what the directive says on its own is checked here; whether the
    /// parameters name input fields that can travel that way needs the input
    /// type, which may be declared in a later file — see
    /// [`Self::check_http_bindings`].
    fn http_binding(
        &mut self,
        file: &Path,
        service: &str,
        field: &GqlField<'static, String>,
        d: &Directive<'static, String>,
        kind: OperationKind,
    ) -> Option<HttpBinding> {
        let op = format!("`{service}.{}`", field.name);
        let fail = |this: &mut Self, message: String| {
            this.errors.push(CompileError::at(
                format!("{op}: {message}"),
                file,
                field.position,
            ));
            None
        };

        if kind == OperationKind::Subscription {
            return fail(
                self,
                "@http cannot be combined with @subscription, which is always \
                               a GET answered with Server-Sent Events"
                    .to_string(),
            );
        }

        let method_given = has_arg(d, "method");
        let method = match enum_arg(d, "method") {
            Some(name) => match HttpMethod::from_name(&name) {
                Some(method) => method,
                None => {
                    return fail(
                        self,
                        format!("@http(method:) must be one of GET, POST, PUT, PATCH, DELETE or ANY, got `{name}`"),
                    );
                }
            },
            // Present but not an enum value is a quoted method, which is a
            // different mistake from leaving it out and is named as such.
            None if method_given => {
                return fail(
                    self,
                    "@http(method:) is an enum value, written unquoted: `method: GET`".to_string(),
                )
            }
            None => {
                return fail(
                    self,
                    "@http needs `method:`, one of GET, POST, PUT, PATCH, DELETE or ANY"
                        .to_string(),
                )
            }
        };

        let Some(path) = string_arg(d, "path") else {
            let why = match has_arg(d, "path") {
                true => "@http(path:) must be a string, quoted: `path: \"/items/{id}\"`",
                false => "@http needs `path:`, a string starting with `/`",
            };
            return fail(self, why.to_string());
        };
        let Some(path_params) = path_parameters(&path) else {
            return fail(
                self,
                format!(
                    "@http(path:) must start with `/` and use only unreserved URL characters, \
                     with `{{name}}` filling a whole segment and naming the field as the wire \
                     spells it, in lower snake_case (`{{user_id}}`, not `{{userId}}`), \
                     got `{path}`"
                ),
            );
        };
        let mut seen = BTreeSet::new();
        let twice = path_params.iter().find(|p| !seen.insert(p.as_str()));
        if let Some(twice) = twice {
            return fail(self, format!("@http(path:) names `{{{twice}}}` twice"));
        }

        let declared_body = d.arguments.iter().any(|(n, _)| n == "body");
        let body = match enum_arg(d, "body").as_deref() {
            Some("NONE") => HttpBody::None,
            Some("JSON") => HttpBody::Json,
            Some("FORM") => HttpBody::Form,
            Some(other) => {
                return fail(
                    self,
                    format!("@http(body:) must be NONE, JSON or FORM, got `{other}`"),
                )
            }
            None if declared_body => {
                return fail(self, "@http(body:) must be NONE, JSON or FORM".to_string())
            }
            None => HttpBody::default_for(method),
        };
        match (method, body) {
            (HttpMethod::Get | HttpMethod::Delete, HttpBody::Json | HttpBody::Form) => {
                return fail(
                    self,
                    format!(
                        "a {} carries no body, so @http(body:) can only be NONE",
                        method.as_str()
                    ),
                );
            }
            (HttpMethod::Post | HttpMethod::Put | HttpMethod::Patch, HttpBody::None) => {
                return fail(
                    self,
                    format!(
                        "a {} carries its input in the body: @http(body:) is JSON or FORM",
                        method.as_str()
                    ),
                );
            }
            // A GET reaching an `ANY` operation has no body, so its input must
            // fit the query string; a JSON body could not be read back from one.
            (HttpMethod::Any, HttpBody::Json) => {
                return fail(
                    self,
                    "@http(method: ANY) also answers GET, which has no body: its input comes from \
                     the query string and, with body: FORM, a form body; JSON cannot be one of them"
                        .to_string(),
                );
            }
            _ => {}
        }

        let envelope_given = has_arg(d, "envelope");
        let envelope = match bool_arg(d, "envelope") {
            Some(envelope) => envelope,
            None if !envelope_given => true,
            None => return fail(self, "@http(envelope:) must be a boolean".to_string()),
        };

        Some(HttpBinding {
            method,
            path_params,
            body,
            envelope,
        })
    }

    /// What `scope:` on the operation's kind directive — `@query`,
    /// `@mutation` or `@subscription` — names, as written.
    ///
    /// On the kind directive rather than a directive of its own, because which
    /// of the three an operation is decides whether it needs one: a query reads,
    /// and a mutation's scope is the judgement of how far it reaches. Neither
    /// restates `@auth`, whose rule replaces whole when restated.
    fn declared_scope(
        &mut self,
        file: &Path,
        owner: &str,
        directives: &[Directive<'static, String>],
        kind: &str,
        pos: Pos,
    ) -> Option<String> {
        let d = find_directive(directives, kind)?;
        match d.arguments.iter().find(|(n, _)| n == "scope") {
            None => None,
            Some((_, Value::Enum(value))) => Some(value.clone()),
            Some(_) => {
                self.errors.push(CompileError::at(
                    format!(
                        "`{owner}`: @{kind}(scope:) names a value of the @scopes enum, written \
                         bare: `scope: write`"
                    ),
                    file,
                    pos,
                ));
                None
            }
        }
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
        let expose_given = has_arg(d, "expose");
        match bool_arg(d, "expose") {
            Some(expose) => Some(expose),
            None if !expose_given => Some(true),
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
    ) -> Option<AuthRule> {
        let d = find_directive(directives, "auth")?;

        // `require:` defaults to `session`, so the common case is `@auth` bare
        // and the exceptions are the ones that spell themselves out.
        let require_given = has_arg(d, "require");
        let require = match enum_arg(d, "require") {
            None if !require_given => AuthRequirement::Session,
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
                             written bare: `{argument}: {example}`"
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
        let role = role.ok()?;
        let permission = permission.ok()?;
        let access = match (role, permission) {
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
        // user: a token is not one, and `public` is nobody. ANSI RBAC defines a
        // user as a person — the notion may be stretched to programs, but a
        // token here authenticates one without naming whose roles it carries —
        // so demanding a role there could never be satisfied. A permission is
        // checked against the same session, and only a session operation has a
        // context to check it on.
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

        let unguarded = self.true_flag(file, owner, pos, d, "unguarded").ok()?;
        let explicit = self.true_flag(file, owner, pos, d, "explicit").ok()?;
        if unguarded && access.is_some() {
            self.errors.push(CompileError::at(
                format!(
                    "`{owner}`: @auth(unguarded: true) says the operation needs no role or \
                     permission, so it names neither"
                ),
                file,
                pos,
            ));
            return None;
        }
        // The service names no check of its own, since each operation states
        // one, and the checks need a session to run on.
        if explicit && (access.is_some() || require != AuthRequirement::Session) {
            self.errors.push(CompileError::at(
                format!(
                    "`{owner}`: @auth(explicit: true) has each operation name its own check, so \
                     it takes `require: session` and no `role:` or `permission:`"
                ),
                file,
                pos,
            ));
            return None;
        }

        Some(AuthRule {
            require,
            access,
            unguarded,
            explicit,
        })
    }

    /// A flag of `@auth` written only to switch something on: `false` is the
    /// default and refused, so there is one way to write each rule.
    fn true_flag(
        &mut self,
        file: &Path,
        owner: &str,
        pos: Pos,
        d: &Directive<'static, String>,
        name: &str,
    ) -> Result<bool, ()> {
        let given = has_arg(d, name);
        let problem = match bool_arg(d, name) {
            None if !given => return Ok(false),
            Some(true) => return Ok(true),
            Some(false) => "`false` is the default; drop the argument",
            None => "must be `true`",
        };
        self.errors.push(CompileError::at(
            format!("`{owner}`: @auth({name}:) {problem}"),
            file,
            pos,
        ));
        Err(())
    }

    /// An operation drops its service's check only by saying so.
    ///
    /// In a service that checks a role or a permission, or that is
    /// `explicit`, an operation's own `@auth` without a role or a permission
    /// must say `unguarded: true` — whatever its `require:`, since `public`
    /// opens the operation wider still. Otherwise a copied
    /// `@auth(require: session)` would open it without a word. An `explicit`
    /// service's operations must each state their `@auth`.
    fn check_operation_rule(
        &mut self,
        file: &Path,
        owner: &str,
        pos: Pos,
        rule: Option<&AuthRule>,
        service: &ServiceAuth,
    ) {
        let problem = match rule {
            None if service.explicit => Some(
                "its service is @auth(explicit: true), so it states its own @auth: a role, a \
                 permission, or `unguarded: true`"
                    .to_string(),
            ),
            None => None,
            Some(rule) if rule.explicit => Some(
                "@auth(explicit:) applies to a service, whose operations then each state their \
                 own check"
                    .to_string(),
            ),
            Some(rule) if rule.access.is_none() && !rule.unguarded && service.guarded() => {
                let dropped = match &service.access {
                    Some(Access::Role(role)) => format!("its service's `role: {role}`"),
                    Some(Access::Permission(permission)) => {
                        format!("its service's `permission: {permission}`")
                    }
                    None => "a check its `explicit` service asks for".to_string(),
                };
                Some(format!(
                    "@auth restates the rule without {dropped}; name a role or a permission, or \
                     write `unguarded: true` if it needs neither"
                ))
            }
            Some(rule) if rule.unguarded && !service.guarded() => Some(
                "@auth(unguarded: true) has nothing to drop; its service checks no role or \
                 permission"
                    .to_string(),
            ),
            Some(_) => None,
        };
        if let Some(problem) = problem {
            self.errors
                .push(CompileError::at(format!("`{owner}`: {problem}"), file, pos));
        }
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
            let blank = types.iter().any(|t| t.trim().is_empty());
            if types.is_empty() || blank {
                self.errors.push(CompileError::at(
                    format!("{op}: @raw({arg}:) must be a non-empty list of media types"),
                    file,
                    field.position,
                ));
                return None;
            }
        }

        let stream_given = has_arg(d, "stream");
        let stream = match bool_arg(d, "stream") {
            Some(stream) => stream,
            None if !stream_given => false,
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
                    self.type_error(
                        owner,
                        format!(
                        "`{owner}.{}`: a binary body cannot be a list — one message has one body",
                        f.name
                    ),
                    );
                }
                if kind == "output" {
                    self.type_error(
                        owner,
                        format!(
                            "`{owner}.{}`: a binary value cannot be a field of an output type; \
                         return it from an operation declaring @raw(response:) instead",
                            f.name
                        ),
                    );
                }
            }
        }

        for service in &api.services {
            for op in &service.operations {
                let owner = format!("{}.{}", service.name, op.name);

                let returns_binary = api.is_binary(op.output.base_name());
                match (&op.raw_response, returns_binary) {
                    (Some(_), false) => self.operation_error(
                        &owner,
                        format!(
                            "@raw(response:) requires the operation to return a binary \
                         scalar, but it returns `{}`",
                            op.output.base_name()
                        ),
                    ),
                    // `@raw` does not combine with `@http`, so its advice would
                    // lead to a second refusal there.
                    (None, true) if op.http.is_some() => {
                        self.operation_error(
                            &owner,
                            format!(
                                "returns the binary scalar `{}`, which only a @raw \
                             operation answers with, and @http does not combine with @raw",
                                op.output.base_name()
                            ),
                        );
                    }
                    (None, true) => self.operation_error(
                        &owner,
                        format!(
                            "returns the binary scalar `{}`, so it must declare \
                         @raw(response:) — bytes have no place in the JSON envelope",
                            op.output.base_name()
                        ),
                    ),
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
                    (Some(_), []) => self.operation_error(
                        &owner,
                        "@raw(request:) requires exactly one input field of a binary scalar to \
                         carry the body, but the input declares none"
                            .to_string(),
                    ),
                    (Some(_), [_]) => {}
                    (Some(_), many) => self.operation_error(
                        &owner,
                        format!(
                            "@raw(request:) requires exactly one binary input field, but \
                         the input declares {} ({})",
                            many.len(),
                            many.join(", ")
                        ),
                    ),
                    // An `@http` operation's binary field is refused by
                    // `check_http_bindings`, without advice it could not follow.
                    (None, []) => {}
                    (None, _) if op.http.is_some() => {}
                    (None, [first, ..]) => self.operation_error(
                        &owner,
                        format!(
                            "input field `{first}` is a binary scalar, so the operation \
                         must declare @raw(request:)"
                        ),
                    ),
                }

                // A streamed body is never read into the field, so a bound on it
                // would measure the empty value the server leaves there and pass
                // or fail regardless of what was sent. Resolved, because a
                // `@length` on the binary scalar reaches the field the same way.
                if op.raw_request_stream {
                    if let Some(f) = api.body_field(op) {
                        let c = api.resolved_constraints(f);
                        if c.min_length.is_some() || c.max_length.is_some() {
                            self.operation_error(
                                &owner,
                                format!(
                                    "@length on `{}` cannot apply to a streamed body, \
                                 which is never read into the field; bound it in the service, \
                                 from `RawRequest::content_length` and what it reads",
                                    f.name
                                ),
                            );
                        }
                    }
                }

                if let (OperationKind::Subscription, [first, ..]) = (op.kind, bodies.as_slice()) {
                    self.operation_error(
                        &owner,
                        format!(
                            "a subscription sends no body, so its input cannot carry the \
                         binary field `{first}`"
                        ),
                    );
                }
            }
        }
    }

    /// Keep an `@http` binding's parameters in step with the input type.
    ///
    /// A path parameter fills one segment, so it names a required scalar or
    /// enum field of the input. Fields read from a query string or a form body
    /// have no nesting, so an object travels as JSON text there, the way a
    /// subscription's input already does; a binary value has no place at all,
    /// since `@raw` is what describes a body of bytes.
    fn check_http_bindings(&mut self, api: &Api) {
        for service in &api.services {
            for op in &service.operations {
                let Some(http) = &op.http else { continue };
                let owner = format!("{}.{}", service.name, op.name);
                let fields = api.input_fields(op);

                if op.input.is_none() && !http.path_params.is_empty() {
                    self.operation_error(
                        &owner,
                        format!(
                            "@http(path:) names `{{{}}}`, but the operation takes no input",
                            http.path_params.join("}`, `{")
                        ),
                    );
                    continue;
                }

                for param in &http.path_params {
                    let Some(field) = fields.iter().find(|f| &f.name == param) else {
                        self.operation_error(
                            &owner,
                            format!(
                            "@http(path:) names `{{{param}}}`, which is not a field of the input"
                        ),
                        );
                        continue;
                    };
                    // A value read as JSON text — the `JSON` scalar — has no
                    // segment spelling a client writes, so it is no parameter.
                    let scalar_or_enum = matches!(
                        api.find_type(field.ty.base_name()),
                        Some(ApiType::Scalar(_) | ApiType::Enum(_))
                    ) && !api.is_json_text(field.ty.base_name());
                    let one_value = matches!(
                        field.ty,
                        TypeRef::Named {
                            nullable: false,
                            ..
                        }
                    );
                    if !scalar_or_enum || !one_value {
                        self.operation_error(
                            &owner,
                            format!(
                                "path parameter `{param}` must be a required scalar or enum \
                             field with a text form — a string, number or boolean, not `JSON` — \
                             since it fills one path segment"
                            ),
                        );
                    }
                }

                for field in fields {
                    if api.is_binary(field.ty.base_name()) {
                        self.operation_error(
                            &owner,
                            format!(
                                "input field `{}` is a binary scalar, which only a @raw \
                             operation carries",
                                field.name
                            ),
                        );
                    }
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
                // Recorded whatever the checks below say: an unknown name or
                // the role itself is reported on every mention by the
                // not-a-value and itself checks, which come before `twice`,
                // so recording it changes nothing.
                let first = seen.insert(junior);
                let problem = if !roles.contains(&junior.as_str()) {
                    Some("which is not one of its values")
                } else if junior == role {
                    Some("which is itself")
                } else if !first {
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
                let first = reached.insert(next);
                if first {
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
                let (argument, marker, value, set, set_name) = match &op.access {
                    None => continue,
                    Some(Access::Role(role)) => (
                        "role",
                        "roles",
                        role,
                        &roles,
                        api.roles.as_ref().map(|r| r.enum_name.as_str()),
                    ),
                    Some(Access::Permission(permission)) => (
                        "permission",
                        "permissions",
                        permission,
                        &permissions,
                        api.permissions.as_ref().map(|p| p.enum_name.as_str()),
                    ),
                };
                let owner = format!("{}.{}", s.name, op.name);
                match set {
                    None => self.operation_error(
                        &owner,
                        format!(
                            "@auth({argument}: {value}) needs an enum marked @{marker} listing it"
                        ),
                    ),
                    Some(values) if !values.contains(value) => {
                        self.operation_error(
                            &owner,
                            format!(
                                "@auth({argument}: {value}) is not a value of the @{marker} enum `{}`",
                                set_name.unwrap_or_default()
                            ),
                        );
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
        let codes_given = has_arg(d, "codes");
        let problem = match string_list_arg(d, "codes") {
            Some(codes) if codes.is_empty() => "lists no code",
            Some(codes) => {
                let distinct: BTreeSet<&str> = codes.iter().map(String::as_str).collect();
                if distinct.len() == codes.len() {
                    return codes;
                }
                "lists a code twice"
            }
            None if !codes_given => "is missing",
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

    /// What `@scopes(query:)` names: the scope a query or a subscription needs
    /// when it states none, written bare like every other enum value.
    fn scope_default(
        &mut self,
        file: &Path,
        owner: &str,
        d: &Directive<'static, String>,
    ) -> Option<String> {
        match d.arguments.iter().find(|(n, _)| n == "query") {
            None => None,
            Some((_, Value::Enum(value))) => Some(value.clone()),
            Some(_) => {
                self.errors.push(CompileError::at(
                    format!(
                        "enum `{owner}`: @scopes(query:) names one of its values, written bare: \
                         `query: read`"
                    ),
                    file,
                    d.position,
                ));
                None
            }
        }
    }

    /// The `@scopes` enum resolved into [`Scopes`]: one set per API, since
    /// `scope:` names a bare value, and its `query:` default one of its values.
    fn resolve_scopes(&mut self, types: &[ApiType]) -> Option<Scopes> {
        let mut enums = std::mem::take(&mut self.scope_enums).into_iter();
        let ScopeEnum {
            file,
            pos,
            name,
            query,
        } = enums.next()?;
        for other in enums {
            self.errors.push(CompileError::at(
                format!(
                    "enum `{}`: @scopes is already on `{name}`; an API has one scope set",
                    other.name
                ),
                &other.file,
                other.pos,
            ));
        }
        let values = enum_values(types, &name);
        if let Some(default) = &query {
            if !values.contains(default) {
                self.errors.push(CompileError::at(
                    format!("enum `{name}`: @scopes(query: {default}) is not one of its values"),
                    &file,
                    pos,
                ));
            }
        }
        Some(Scopes {
            enum_name: name,
            query,
        })
    }

    /// Each operation's scope, resolved: the declared one checked against the
    /// set and against where a scope means something, a query's default filled
    /// in, and a missing one refused where it is a judgement.
    ///
    /// A mutation has no default. Whether it writes or removes — whether a
    /// token that may edit may also destroy — is decided per operation, and a
    /// default would decide it for every mutation added later without a word:
    /// the one that forgot would be reachable by every token that writes. A
    /// query taking a raw request body has none either: it reads bytes in,
    /// which is a write in all but name as often as not.
    ///
    /// A public operation takes no scope: it is called with no credential, so
    /// there is nothing to narrow. One with a permission may state one but
    /// need not, and gets no default: its permission check is where
    /// `@permissions` puts the question of what a credential reaches, and an
    /// implementation whose check judges only the user states the scope too.
    fn resolve_operation_scopes(
        &mut self,
        types: &[ApiType],
        scopes: Option<&Scopes>,
        services: &mut [Service],
    ) {
        let values = scopes.map(|set| enum_values(types, &set.enum_name));
        for s in services.iter_mut() {
            for op in &mut s.operations {
                let owner = format!("{}.{}", s.name, op.name);
                let kind = match op.kind {
                    OperationKind::Query => "query",
                    OperationKind::Mutation => "mutation",
                    OperationKind::Subscription => "subscription",
                };
                let declared = op.scope.take();
                let (Some(set), Some(values)) = (scopes, &values) else {
                    if let Some(scope) = &declared {
                        self.operation_error(
                            &owner,
                            format!(
                                "@{kind}(scope: {scope}) needs an enum marked @scopes listing it"
                            ),
                        );
                    }
                    continue;
                };
                let public = op.auth == AuthRequirement::Public;
                let permission = matches!(op.access, Some(Access::Permission(_)));
                let raw_request = op.raw_request.is_some();
                let defaulted = op.kind != OperationKind::Mutation && !permission && !raw_request;
                op.scope = match declared {
                    Some(scope) if public => {
                        self.operation_error(
                            &owner,
                            format!(
                                "@{kind}(scope: {scope}) applies to no credential: a public \
                                 operation is called with none, so there is nothing for a scope \
                                 to narrow; drop it"
                            ),
                        );
                        None
                    }
                    Some(scope) if !values.contains(&scope) => {
                        self.operation_error(
                            &owner,
                            format!(
                                "@{kind}(scope: {scope}) is not a value of the @scopes enum `{}`",
                                set.enum_name
                            ),
                        );
                        None
                    }
                    Some(scope) if defaulted && set.query.as_ref() == Some(&scope) => {
                        self.operation_error(
                            &owner,
                            format!(
                                "@{kind}(scope: {scope}) is the default @scopes(query:) gives \
                                 every {kind}; drop the argument"
                            ),
                        );
                        Some(scope)
                    }
                    Some(scope) => Some(scope),
                    None if public || permission => None,
                    None => match (op.kind, &set.query) {
                        (OperationKind::Mutation, _) => {
                            self.operation_error(
                                &owner,
                                format!(
                                    "a @mutation states the scope a credential needs to call it, \
                                     one of {}: `@mutation(scope: …)`",
                                    values.join(", ")
                                ),
                            );
                            None
                        }
                        (_, Some(_)) if raw_request => {
                            self.operation_error(
                                &owner,
                                format!(
                                    "a @{kind} with @raw(request:) takes a body in, so it states \
                                     its scope rather than taking the read default: one of {}",
                                    values.join(", ")
                                ),
                            );
                            None
                        }
                        (_, Some(default)) => Some(default.clone()),
                        (_, None) => {
                            self.operation_error(
                                &owner,
                                format!(
                                    "@{kind} needs `scope:` — one of {} — since the @scopes enum \
                                     `{}` gives queries no `query:` default",
                                    values.join(", "),
                                    set.enum_name
                                ),
                            );
                            None
                        }
                    },
                };
            }
        }
    }

    /// The error groups resolved: every code declared once, every include a
    /// group, no group including itself; then each group's values flattened —
    /// its own codes, those of every group it includes, and the registry's
    /// `internal_error`, which a group answers with for a code it does not
    /// hold — and the registry extended with every group's own codes, so it
    /// stays the one list of everything the API can answer.
    fn resolve_error_groups(&mut self, types: &mut [ApiType]) {
        self.check_codes_declared_once();
        let groups = std::mem::take(&mut self.group_enums);
        let Some(first) = groups.first() else { return };
        let registry = types.iter().find_map(|t| match t {
            ApiType::Enum(e) if e.name == ERROR_CODE_ENUM => Some(e.values.clone()),
            _ => None,
        });
        let Some(registry) = registry else {
            self.errors.push(CompileError::at(
                format!(
                    "@errorGroup `{}`: a group's codes join the error code registry, so the \
                     API must declare `enum {ERROR_CODE_ENUM}`",
                    first.name
                ),
                &first.file,
                first.pos,
            ));
            return;
        };

        let names: BTreeSet<&str> = groups.iter().map(|g| g.name.as_str()).collect();
        for g in &groups {
            for included in &g.includes {
                if names.contains(included.as_str()) {
                    continue;
                }
                let what = match self.declared_at(included) {
                    Some(_) => "which is not an @errorGroup enum",
                    None => "which is not declared",
                };
                self.errors.push(CompileError::at(
                    format!(
                        "enum `{}`: @errorGroup(includes:) names `{included}`, {what}",
                        g.name
                    ),
                    &g.file,
                    g.pos,
                ));
            }
        }

        let direct: BTreeMap<&str, &[String]> = groups
            .iter()
            .map(|g| (g.name.as_str(), g.includes.as_slice()))
            .collect();
        for g in &groups {
            // A cycle is reported once, at the group whose name sorts first
            // in it, naming the whole way round.
            let Some(cycle) = include_cycle(&direct, &g.name) else {
                continue;
            };
            let first_in_cycle = cycle.iter().min().is_some_and(|min| *min == g.name);
            if first_in_cycle {
                self.errors.push(CompileError::at(
                    format!(
                        "@errorGroup `{}` includes itself: {}; a group's codes would be its \
                         own included codes",
                        g.name,
                        cycle.join(" -> ")
                    ),
                    &g.file,
                    g.pos,
                ));
            }
        }

        // Each group's own values, as declared, before any is flattened.
        let own: BTreeMap<String, Vec<EnumValue>> = types
            .iter()
            .filter_map(|t| match t {
                ApiType::Enum(e) if names.contains(e.name.as_str()) => {
                    Some((e.name.clone(), e.values.clone()))
                }
                _ => None,
            })
            .collect();
        let internal_error = registry.iter().find(|v| v.name == CODE_INTERNAL_ERROR);
        let mut joined: Vec<EnumValue> = Vec::new();
        for ty in types.iter_mut() {
            let ApiType::Enum(e) = ty else { continue };
            let Some(group) = &mut e.error_group else {
                continue;
            };
            group.includes = included_groups(&direct, &e.name);
            let mut values: Vec<EnumValue> = Vec::new();
            let held = std::iter::once(&e.name).chain(&group.includes);
            for value in held.filter_map(|name| own.get(name)).flatten() {
                let present = values.iter().any(|v| v.name == value.name);
                if !present && value.name != CODE_INTERNAL_ERROR {
                    values.push(value.clone());
                }
            }
            values.extend(internal_error.cloned());
            e.values = values;
        }
        for value in own.values().flatten() {
            let present = registry.iter().chain(&joined).any(|v| v.name == value.name);
            if !present {
                joined.push(value.clone());
            }
        }

        // A group's code may carry a `@variant(tag:)` another enum's code is
        // spelt like, which no single enum's check sees.
        let mut wire: BTreeMap<&str, &str> = registry
            .iter()
            .map(|v| (v.wire_name(), v.name.as_str()))
            .collect();
        let mut clashes: Vec<String> = Vec::new();
        for value in &joined {
            if let Some(other) = wire.insert(value.wire_name(), &value.name) {
                clashes.push(format!(
                    "codes `{other}` and `{}` both serialise to `{}`",
                    value.name,
                    value.wire_name()
                ));
            }
        }
        for clash in clashes {
            self.type_error(ERROR_CODE_ENUM, format!("`{ERROR_CODE_ENUM}`: {clash}"));
        }
        for ty in types.iter_mut() {
            if let ApiType::Enum(e) = ty {
                if e.name == ERROR_CODE_ENUM {
                    e.values.extend(joined.iter().cloned());
                }
            }
        }
    }

    /// A code declared by two enums — the registry and a group, or two groups —
    /// reported at the second declaration, naming the first. One enum listing
    /// a value twice is its own wire-name check's to report.
    fn check_codes_declared_once(&mut self) {
        let sites = std::mem::take(&mut self.code_sites);
        let mut first: BTreeMap<&str, &CodeSite> = BTreeMap::new();
        for site in &sites {
            let earlier = match first.entry(site.code.as_str()) {
                Entry::Vacant(slot) => {
                    slot.insert(site);
                    continue;
                }
                Entry::Occupied(slot) => *slot.get(),
            };
            // A group declaring a runtime code is reported where the group
            // is read, saying why that code is the registry's.
            let runtime = RUNTIME_ERROR_CODES.contains(&site.code.as_str());
            if earlier.owner == site.owner || runtime {
                continue;
            }
            self.errors.push(CompileError::at(
                format!(
                    "error code `{}` is declared by `{}` and by `{}` (at {}:{}:{}); a code \
                     lives in `{ERROR_CODE_ENUM}` or in exactly one @errorGroup",
                    site.code,
                    site.owner,
                    earlier.owner,
                    earlier.file.display(),
                    earlier.pos.line,
                    earlier.pos.column
                ),
                &site.file,
                site.pos,
            ));
        }
    }

    /// `@throws(groups:)` resolved: each name an error group, named once, and
    /// the operation's codes extended with every group code it does not list
    /// already. `internal_error` is left to [`Operation::error_codes`], which
    /// every operation answers with.
    fn resolve_operation_groups(&mut self, types: &[ApiType], services: &mut [Service]) {
        for s in services.iter_mut() {
            for op in &mut s.operations {
                let owner = format!("{}.{}", s.name, op.name);
                let mut seen: BTreeSet<&str> = BTreeSet::new();
                for name in &op.error_groups {
                    let first = seen.insert(name);
                    if !first {
                        self.operation_error(
                            &owner,
                            format!("@throws(groups:) names `{name}` twice"),
                        );
                    }
                    let group = types.iter().find_map(|t| match t {
                        ApiType::Enum(e) if e.name == *name && e.error_group.is_some() => Some(e),
                        _ => None,
                    });
                    let Some(group) = group else {
                        self.operation_error(
                            &owner,
                            format!(
                                "@throws(groups:) names `{name}`, which is not an @errorGroup enum"
                            ),
                        );
                        continue;
                    };
                    for value in &group.values {
                        let listed = op.errors.contains(&value.name);
                        if !listed && value.name != CODE_INTERNAL_ERROR {
                            op.errors.push(value.name.clone());
                        }
                    }
                }
            }
        }
    }

    /// An error about the operation `owner` (`Service.operation`), reported
    /// where it is declared.
    fn operation_error(&mut self, owner: &str, message: String) {
        let message = format!("`{owner}`: {message}");
        let error = match self.operation_sites.get(owner) {
            Some((file, pos)) => CompileError::at(message, file, *pos),
            None => CompileError::new(message),
        };
        self.errors.push(error);
    }

    /// The services of `api` in the order the SDL declares them: by file, then
    /// by position in it.
    ///
    /// The IR lists them sorted by name, and a check that refuses the second of
    /// two clashing declarations must meet them in the reader's order to know
    /// which one that is — `USERS` sorts before `Users` whichever came first.
    fn services_in_order<'s>(&self, api: &'s Api) -> Vec<&'s Service> {
        let mut services: Vec<&Service> = api.services.iter().collect();
        services.sort_by_key(|s| {
            self.declared
                .get(&s.name)
                .map(|(file, pos)| (file.clone(), pos.line, pos.column))
        });
        services
    }

    /// Every operation of `api` with its service, in the order the SDL
    /// declares them (see [`Self::services_in_order`]).
    fn operations_in_order<'s>(&self, api: &'s Api) -> Vec<(&'s Service, &'s Operation)> {
        let mut operations: Vec<(&Service, &Operation)> = api
            .services
            .iter()
            .flat_map(|s| s.operations.iter().map(move |op| (s, op)))
            .collect();
        operations.sort_by_key(|(s, op)| {
            self.operation_sites
                .get(&format!("{}.{}", s.name, op.name))
                .map(|(file, pos)| (file.clone(), pos.line, pos.column))
        });
        operations
    }

    /// An error about the type `name`, or about a field or member of it,
    /// reported where the type is declared: the IR keeps no positions of its
    /// own, and the type's is the nearest one a check over the IR can name.
    fn type_error(&mut self, name: &str, message: String) {
        let location = self
            .declared
            .get(name)
            .map(|(file, pos)| format!("{file}:{}:{}", pos.line, pos.column));
        self.errors.push(CompileError { message, location });
    }

    /// Reject two values of one enum that serialise to the same string.
    ///
    /// A tag that collides with another value's tag — or with a plain value's
    /// own name — would make the wire ambiguous in one direction and lossy in
    /// the other, which no target can encode its way out of.
    fn check_enum_wire_names(&mut self, file: &Path, owner: &str, values: &[EnumValue], pos: Pos) {
        let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
        for v in values {
            let previous = seen.insert(v.wire_name(), &v.name);
            if let Some(other) = previous {
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
            let disagree = held_by.iter().any(|(_, name)| name != first);
            if disagree {
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
                let previous = tags.insert(member.tag.clone(), member.name.clone());
                if let Some(other) = previous {
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
        let compiled = regex::Regex::new(pattern);
        if let Err(e) = compiled {
            self.errors.push(CompileError::at(
                format!("@pattern `{pattern}` is not a valid regex: {e}"),
                file,
                pos,
            ));
            return false;
        }
        let incompatible = js_incompatible(pattern);
        if let Some((construct, fix)) = incompatible {
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
                    Some(v) => {
                        let usable = self.check_pattern(file, d.position, &v);
                        if usable {
                            c.pattern = Some(v);
                        }
                    }
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
            let first = seen.insert(d.name.as_str());
            if !first {
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
                let first = args.insert(name);
                if !first {
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
        // One path may carry several operations as long as their methods differ:
        // `GET /items/{id}` beside `DELETE /items/{id}`. `ANY` takes every
        // method, so it shares its path with nothing.
        let mut seen: BTreeMap<&str, Vec<(&'static str, String)>> = BTreeMap::new();
        for (s, op) in self.operations_in_order(api) {
            let owner = format!("{}.{}", s.name, op.name);
            let method = op.http_method();
            let taken = seen.entry(&op.path).or_default();
            let clash = taken
                .iter()
                .find(|(other, _)| *other == method || *other == "ANY" || method == "ANY");
            if let Some((_, prev)) = clash {
                let message = format!(
                    "duplicate RPC path `{}`: used by `{prev}` and `{owner}`",
                    op.path
                );
                self.operation_error(&owner, message);
            }
            taken.push((method, owner));
        }
    }

    /// Two routes naming the parameter in one position differently.
    ///
    /// The router keeps one name per parameter position, so `/items/{id}` and
    /// `/items/{item_id}` cannot both be registered — whatever their methods,
    /// and whatever follows the parameter — and building it panics at startup.
    /// [`Self::check_paths`] compares whole strings and sees two paths.
    /// Positions are compared by the segments before them, a parameter there
    /// written as `{}`, which is how the router tells them apart too.
    fn check_path_parameter_names(&mut self, api: &Api) {
        // Shape of the prefix up to a parameter -> the name and who used it.
        let mut seen: BTreeMap<String, (&str, String)> = BTreeMap::new();
        for (s, op) in self.operations_in_order(api) {
            let owner = format!("{}.{}", s.name, op.name);
            let mut shape = String::new();
            for segment in op.path.split('/').skip(1) {
                shape.push('/');
                let name = segment.strip_prefix('{').and_then(|n| n.strip_suffix('}'));
                let Some(name) = name else {
                    shape.push_str(segment);
                    continue;
                };
                shape.push_str("{}");
                let (prev_name, prev) = seen
                    .entry(shape.clone())
                    .or_insert_with(|| (name, owner.clone()));
                if *prev_name != name {
                    self.errors.push(CompileError::new(format!(
                            "path parameter `{{{name}}}` of `{owner}` (`{}`) sits where `{prev}` names \
                             `{{{prev_name}}}`; one route cannot name it twice, so use one name",
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
        for (s, op) in self.operations_in_order(api) {
            let owner = format!("{}.{}", s.name, op.name);
            let previous = seen.insert(&op.operation_id, owner.clone());
            if let Some(prev) = previous {
                let message = format!(
                    "duplicate operationId `{}`: used by `{prev}` and `{owner}`",
                    op.operation_id
                );
                self.operation_error(&owner, message);
            }
        }
    }

    /// An SDL type spelt like a Rust item the generated code declares itself,
    /// or like a prelude name it writes unqualified.
    ///
    /// Every type the Rust targets emit is an item of the same name beside the
    /// generator's own: the `Rt` alias, `Failure`, `FromRequestContext`,
    /// `ApiClient`, the `operations` and `wire` modules, a service's
    /// `<S>Service` trait and `<S>Operations` client group, an operation's
    /// `<S><Op>Codes` set, an error group's `<Area>Failure`. A type of one of
    /// those names is a duplicate item in
    /// the module declaring both, and in a module glob-importing the types it
    /// is silently the generator's item instead of the SDL's. The prelude
    /// names are the same hazard the other way round: an SDL `Result` would
    /// take the prelude's place in every signature of the file. `From` is among
    /// them because the derives the generated code applies write it unqualified
    /// — `treat`'s `ApiErrorCode` and `error_set!` both — and `Into` because the
    /// client's constructors take `impl Into<String>`.
    ///
    /// The runtime's items are not among these: the generated code names them
    /// by their absolute paths and re-exports one only when no type is spelt
    /// like it, so an input called `Request` or an object called `RawBody` is
    /// fine. A scalar mapped to a Rust type declares nothing and is not
    /// checked; a newtype scalar is.
    fn check_rust_item_names(&mut self, api: &Api) {
        let mut reserved: BTreeMap<String, String> = BTreeMap::new();
        for (name, what) in [
            ("Rt", "the server's runtime alias"),
            ("Failure", "the types module's `Failure` alias"),
            ("FromRequestContext", "the server's context trait"),
            ("ApiClient", "the Rust client"),
            ("operations", "the `operations` module"),
            ("wire", "the `wire` module"),
        ] {
            reserved.insert(name.to_string(), what.to_string());
        }
        for name in [
            "Option", "Some", "None", "Result", "Ok", "Err", "Vec", "Box", "String", "Clone",
            "Send", "Sync", "Sized", "From", "Into",
        ] {
            reserved.insert(
                name.to_string(),
                format!("Rust's prelude `{name}`, which the generated code names"),
            );
        }
        for s in &api.services {
            reserved.insert(
                format!("{}Service", s.name),
                format!("the trait of service `{}`", s.name),
            );
            reserved.insert(
                format!("{}Operations", pascal_case(&s.name)),
                format!("the Rust client's group for service `{}`", s.name),
            );
            for op in &s.operations {
                reserved.insert(
                    codes_type_name(&s.name, &op.name),
                    format!("the codes type of `{}.{}`", s.name, op.name),
                );
            }
        }
        for group in api.error_groups() {
            if let Some(failure) = group.failure_name() {
                reserved.insert(
                    failure,
                    format!("the failure type of error group `{}`", group.name),
                );
            }
        }

        for ty in &api.types {
            let emitted = match ty {
                ApiType::Scalar(s) => s.rust_newtype,
                _ => true,
            };
            if !emitted {
                continue;
            }
            let Some(what) = reserved.get(ty.name()) else {
                continue;
            };
            let location = self
                .declared
                .get(ty.name())
                .map(|(file, pos)| format!("{file}:{}:{}", pos.line, pos.column));
            self.errors.push(CompileError {
                message: format!(
                    "type `{}` is spelt like {what}, which the generated Rust declares or \
                     names itself; rename the type",
                    ty.name()
                ),
                location,
            });
        }
    }

    /// Two services, or two operations, that the targets would give one name.
    ///
    /// Every target derives its identifiers by recasing the SDL names, and the
    /// recasing loses what told them apart. `Users.getAll` and `UsersGet.all`
    /// are both the server's handler `users_get_all`, both the `wire` statics
    /// `USERS_GET_ALL_*` and both `UsersGetAllCodes`; `Users` and `USERS` are
    /// both the Rust module and client accessor `users` and both the key
    /// `users` of TypeScript's `createApi`. Rust refuses the duplicate item —
    /// in generated code, where the author cannot fix it — and a TypeScript
    /// object literal keeps the second entry and drops the first without a
    /// word. Neither path, operationId nor type name catches this, since none
    /// of those is recased the same way.
    ///
    /// Reported where the later of the two is declared, naming the earlier
    /// one's declaration and every name the pair shares.
    fn check_generated_names(&mut self, api: &Api) {
        let mut taken: BTreeMap<(&'static str, String), Claimant> = BTreeMap::new();
        for s in self.services_in_order(api) {
            let claimant = Claimant {
                label: format!("service `{}`", s.name),
                site: self
                    .declared
                    .get(&s.name)
                    .map(|(file, pos)| format!("{file}:{}:{}", pos.line, pos.column)),
            };
            let names = [
                (
                    "the Rust module and client accessor",
                    snake_case_raw(&s.name),
                ),
                ("the TypeScript `createApi` key", s.name.to_lowercase()),
                (
                    "the Rust client type",
                    format!("{}Operations", pascal_case(&s.name)),
                ),
            ];
            self.refuse_shared_names(&mut taken, claimant, names);
        }
        for (s, op) in self.operations_in_order(api) {
            let owner = format!("{}.{}", s.name, op.name);
            let claimant = Claimant {
                site: self
                    .operation_sites
                    .get(&owner)
                    .map(|(file, pos)| format!("{}:{}:{}", file.display(), pos.line, pos.column)),
                label: format!("`{owner}`"),
            };
            let names = [
                (
                    "the Rust handler",
                    format!("{}_{}", snake_case_raw(&s.name), snake_case_raw(&op.name)),
                ),
                (
                    "the `wire` statics",
                    format!("{}_*", fields_static_name(&s.name, &op.name)),
                ),
                ("the codes type", codes_type_name(&s.name, &op.name)),
            ];
            self.refuse_shared_names(&mut taken, claimant, names);
        }
    }

    /// Claim `names` for `claimant` in `taken`, and report one error per
    /// earlier claimant already holding any of them, listing every name the two
    /// share.
    ///
    /// Each name is keyed by what kind of name it is, so that a module and a
    /// type that happen to be spelt alike are not taken for a clash.
    fn refuse_shared_names<const N: usize>(
        &mut self,
        taken: &mut BTreeMap<(&'static str, String), Claimant>,
        claimant: Claimant,
        names: [(&'static str, String); N],
    ) {
        // The earlier claimant -> what this one shares with it, in `names` order.
        let mut shared: BTreeMap<String, (Claimant, Vec<String>)> = BTreeMap::new();
        for (kind, name) in names {
            let shown = format!("{kind} `{name}`");
            match taken.entry((kind, name)) {
                Entry::Vacant(slot) => {
                    slot.insert(claimant.clone());
                }
                Entry::Occupied(slot) => {
                    let earlier = slot.get();
                    shared
                        .entry(earlier.label.clone())
                        .or_insert_with(|| (earlier.clone(), Vec::new()))
                        .1
                        .push(shown);
                }
            }
        }
        for (earlier, names) in shared.into_values() {
            let earlier_at = earlier
                .site
                .map(|at| format!(" (declared at {at})"))
                .unwrap_or_default();
            self.errors.push(CompileError {
                message: format!(
                    "{} and {}{earlier_at} generate the same names — {}; rename one of them",
                    claimant.label,
                    earlier.label,
                    names.join(", ")
                ),
                location: claimant.site.clone(),
            });
        }
    }

    /// Every `@error` code must name a value of the registry enum, which is
    /// what keeps codes from being invented per type.
    fn check_error_code_registry(&mut self, api: &Api) {
        let registry: Option<BTreeSet<&str>> = match api.find_type(ERROR_CODE_ENUM) {
            Some(ApiType::Enum(e)) => Some(e.values.iter().map(|v| v.name.as_str()).collect()),
            Some(_) => {
                self.type_error(
                    ERROR_CODE_ENUM,
                    format!("`{ERROR_CODE_ENUM}` must be an enum: it is the error code registry"),
                );
                return;
            }
            None => {
                // Demanded as soon as there is an operation, `@throws` or not:
                // every operation can answer `invalid_body` and
                // `internal_error` on its own, the generated server and client
                // are typed over the registry those codes belong to, and
                // without it neither compiles. An API of types alone has no
                // operation to fail and needs none.
                let first = api
                    .services
                    .iter()
                    .find_map(|s| s.operations.first().map(|op| (s, op)));
                let Some((service, op)) = first else { return };
                let listed = RUNTIME_ERROR_CODES
                    .iter()
                    .map(|code| format!("`{code}`"))
                    .collect::<Vec<_>>()
                    .join(" and ");
                self.operation_error(
                    &format!("{}.{}", service.name, op.name),
                    format!(
                        "an API with operations must declare `enum {ERROR_CODE_ENUM}`, the \
                         error code registry, listing at least {listed} — the runtime \
                         answers them for any operation — and every code @throws names"
                    ),
                );
                return;
            }
        };
        let Some(registry) = registry else { return };

        for value in &RUNTIME_ERROR_CODES {
            if !registry.contains(value) {
                self.type_error(
                    ERROR_CODE_ENUM,
                    format!("`{ERROR_CODE_ENUM}` must list `{value}`: the runtime emits it"),
                );
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

        // The set's codes are checked here rather than through the operations
        // that list them, so a misspelt code is caught before any operation
        // names a permission.
        if let Some(permissions) = &api.permissions {
            for code in &permissions.codes {
                if !registry.contains(code.as_str()) {
                    self.type_error(
                        &permissions.enum_name,
                        format!(
                            "enum `{}`: @permissions(codes:) names `{code}`, which is not listed \
                             in `{ERROR_CODE_ENUM}`",
                            permissions.enum_name
                        ),
                    );
                }
            }
        }

        for s in &api.services {
            for op in &s.operations {
                let owner = format!("{}.{}", s.name, op.name);
                let mut seen: BTreeSet<&str> = BTreeSet::new();
                for code in &op.errors {
                    let first = seen.insert(code);
                    if !first {
                        self.operation_error(&owner, format!("@throws lists `{code}` twice"));
                    }
                    if !registry.contains(code.as_str()) {
                        self.operation_error(
                            &owner,
                            format!(
                                "@throws names `{code}`, which is not listed in `{ERROR_CODE_ENUM}`"
                            ),
                        );
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
                    let listed = op.errors.iter().any(|c| c == code);
                    if !listed {
                        self.operation_error(
                            &owner,
                            format!(
                                "@auth({argument}:) is checked before the handler, so \
                                 @throws(codes:) must list `{code}`"
                            ),
                        );
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
            let shadowed = o.fields.iter().any(|f| f.name == tag);
            if shadowed {
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
                            Some(ApiType::Object(_)) => self.type_error(
                                &i.name,
                                format!(
                                    "`{}.{}` references output object `{}`; inputs may only use input objects, enums and scalars",
                                    i.name, f.name, f.ty.base_name()
                                ),
                            ),
                            Some(ApiType::Union(_)) => self.type_error(
                                &i.name,
                                format!(
                                    "`{}.{}` references union `{}`; use an @oneOf input instead",
                                    i.name, f.name, f.ty.base_name()
                                ),
                            ),
                            _ => {}
                        }
                    }
                }
                ApiType::Object(o) => {
                    for f in &o.fields {
                        if let Some(ApiType::InputObject(_)) = api.find_type(f.ty.base_name()) {
                            self.type_error(
                                &o.name,
                                format!(
                                    "`{}.{}` references input object `{}` in output position",
                                    o.name,
                                    f.name,
                                    f.ty.base_name()
                                ),
                            );
                        }
                    }
                }
                _ => {}
            }
        }

        for s in &api.services {
            for op in &s.operations {
                let owner = format!("{}.{}", s.name, op.name);
                if let Some(input) = &op.input {
                    if let Some(ApiType::Object(_)) = api.find_type(input.base_name()) {
                        self.operation_error(
                            &owner,
                            format!(
                                "input `{}` must be an input object, not an output type",
                                input.base_name()
                            ),
                        );
                    }
                }
                if let Some(ApiType::InputObject(_)) = api.find_type(op.output.base_name()) {
                    self.operation_error(
                        &owner,
                        format!(
                            "output `{}` must not be an input object",
                            op.output.base_name()
                        ),
                    );
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
    let mut rest: &[char] = &chars;
    let mut in_class = false;

    while let [first, tail @ ..] = rest {
        let mut next = tail;
        match (first, tail) {
            // An escape and the character it escapes are one unit, so neither
            // opens a group nor a class.
            ('\\', [c @ ('p' | 'P'), '{', ..]) => {
                return Some((
                    format!("\\{c}{{"),
                    "Spell the class out; `\\p` needs the `u` flag, which the literal does \
                     not carry.",
                ));
            }
            ('\\', ['A', ..]) => return Some(("\\A".to_string(), "Use `^`.")),
            ('\\', ['z', ..]) => return Some(("\\z".to_string(), "Use `$`.")),
            ('\\', [_, after @ ..]) => next = after,
            ('[', ['[', ':', ..]) if !in_class => {
                return Some((
                    "[[:".to_string(),
                    "Spell the POSIX class out, e.g. `[a-zA-Z]` for `[[:alpha:]]`.",
                ));
            }
            ('[', _) if !in_class => in_class = true,
            (']', _) if in_class => in_class = false,
            // `(?:` is the portable non-capturing group and `(?<name>` the
            // portable named one. Every other `(?` the Rust crate accepts sets
            // inline flags, which a JavaScript literal has no syntax for.
            ('(', ['?', after @ ..]) if !in_class => match after {
                [':' | '<', after @ ..] => next = after,
                ['P', ..] => return Some(("(?P<".to_string(), "Write `(?<name>...)`.")),
                _ => {
                    let flags: String = after
                        .iter()
                        .take_while(|c| **c != ')' && **c != ':')
                        .collect();
                    return Some((format!("(?{flags}"), FLAGS));
                }
            },
            _ => {}
        }
        rest = next;
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

/// The `{name}` parameters of an `@http` path, in order, or `None` when the
/// path is not one: it must start with `/`, a parameter must fill a whole
/// segment and be a snake_case name, and the rest is held to [`is_route`].
///
/// A `.` or `..` segment is refused, and so is an empty one but a trailing
/// slash: URL parsing drops the first two and a client's would never reach
/// the route as mounted, and `//` is a route no caller writes on purpose.
fn path_parameters(path: &str) -> Option<Vec<String>> {
    let rest = path.strip_prefix('/')?;
    let mut params = Vec::new();
    let segments: Vec<&str> = rest.split('/').collect();
    let inner = segments
        .split_last()
        .map(|(_, inner)| inner)
        .unwrap_or_default();
    let empty_segment = inner.iter().any(|segment| segment.is_empty());
    if empty_segment {
        return None;
    }
    for segment in segments {
        if matches!(segment, "." | "..") {
            return None;
        }
        let name = segment.strip_prefix('{').and_then(|s| s.strip_suffix('}'));
        match name {
            Some(name) => {
                let valid = !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
                if !valid {
                    return None;
                }
                params.push(name.to_string());
            }
            None => {
                let route = is_route(&format!("/{segment}"));
                if !route {
                    return None;
                }
            }
        }
    }
    Some(params)
}

fn describe_args(args: &[InputValue<'static, String>]) -> String {
    if args.is_empty() {
        return "no arguments".to_string();
    }
    let names: Vec<&str> = args.iter().map(|a| a.name.as_str()).collect();
    format!("`{}`", names.join("`, `"))
}

/// Names of the scalars the compiler maps without an `@scalar` directive.
/// Every group `start` includes, directly or through another, in the order a
/// depth-first walk of `includes:` meets them; never `start` itself, even
/// when the includes close into a cycle back to it. A name that is not a group
/// is skipped: it is reported where it is named.
fn included_groups(direct: &BTreeMap<&str, &[String]>, start: &str) -> Vec<String> {
    fn walk(direct: &BTreeMap<&str, &[String]>, start: &str, name: &str, found: &mut Vec<String>) {
        let includes = direct.get(name).copied().unwrap_or_default();
        for included in includes {
            let known = direct.contains_key(included.as_str());
            let seen = included == start || found.contains(included);
            if known && !seen {
                found.push(included.clone());
                walk(direct, start, included, found);
            }
        }
    }
    let mut found: Vec<String> = Vec::new();
    walk(direct, start, start, &mut found);
    found
}

/// The way round from `start` back to itself through `includes:`, as the
/// groups it passes — `A`, `B`, `A` — when there is one.
fn include_cycle(direct: &BTreeMap<&str, &[String]>, start: &str) -> Option<Vec<String>> {
    // Depth-first, one path at a time: each entry is a group and how many of
    // its includes have been walked.
    let mut path: Vec<(&str, usize)> = vec![(start, 0)];
    let mut visited: BTreeSet<&str> = BTreeSet::new();
    while let Some((name, next)) = path.last_mut() {
        let includes = direct.get(*name).copied().unwrap_or_default();
        let Some(included) = includes.get(*next) else {
            path.pop();
            continue;
        };
        *next = next.saturating_add(1);
        if included == start {
            let mut cycle: Vec<String> = path.iter().map(|(n, _)| (*n).to_string()).collect();
            cycle.push(start.to_string());
            return Some(cycle);
        }
        let first = visited.insert(included.as_str());
        if first {
            path.push((included, 0));
        }
    }
    None
}

fn builtin_names() -> Vec<String> {
    builtin_scalars()
        .iter()
        .map(|t| t.name().to_string())
        .collect()
}

/// The SDL names of the values of the enum `name`, in declaration order; none
/// when no such enum exists, which is reported where the name is declared.
fn enum_values(types: &[ApiType], name: &str) -> Vec<String> {
    types
        .iter()
        .find_map(|t| match t {
            ApiType::Enum(e) if e.name == name => {
                Some(e.values.iter().map(|v| v.name.clone()).collect())
            }
            _ => None,
        })
        .unwrap_or_default()
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

/// Whether the directive names `name` at all, whatever its value.
fn has_arg(d: &Directive<'static, String>, name: &str) -> bool {
    d.arguments.iter().any(|(n, _)| n == name)
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
