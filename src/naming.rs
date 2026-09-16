//! Identifier casing shared by the generators.

/// Rust keywords that would be illegal as field names; escaped with `r#`.
const RUST_KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "static", "struct", "super", "trait", "true", "try", "type", "unsafe", "use", "where",
    "while", "async", "await", "dyn", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield",
];

/// The three keywords `r#` cannot rescue.
///
/// `r#self`, `r#crate` and `r#super` are rejected by rustc itself, so a field
/// spelled this way has no Rust name at all. The compiler refuses the SDL
/// rather than renaming it, which would move the wire key behind the author's
/// back. `Self` is here through [`snake_case`], which lowercases it first.
const UNESCAPABLE_RUST_KEYWORDS: &[&str] = &["crate", "self", "super"];

/// Whether a wire name has no Rust spelling at all. See
/// [`UNESCAPABLE_RUST_KEYWORDS`].
pub fn is_unescapable_keyword(wire_name: &str) -> bool {
    UNESCAPABLE_RUST_KEYWORDS.contains(&wire_name)
}

/// Whether `name` can stand as a bare identifier in the generated targets.
///
/// GraphQL names already satisfy this, so the only string that needs asking is
/// one the SDL supplies as a directive argument rather than as a name —
/// `@discriminator(field:)` or `(sibling:)`, which becomes a key in a TypeScript
/// object type.
pub fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `createdAt` / `CREATED_AT` -> `created_at`, escaping Rust keywords.
pub fn snake_case(input: &str) -> String {
    let out = snake_case_raw(input);
    if RUST_KEYWORDS.contains(&out.as_str()) {
        return format!("r#{out}");
    }
    out
}

/// `snake_case` without Rust's `r#` escaping.
///
/// Wire names are shared by every target, so they must stay valid JSON keys
/// rather than carry a Rust-specific escape.
pub fn snake_case_raw(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 4);
    let chars: Vec<char> = input.chars().collect();

    for (i, ch) in chars.iter().enumerate() {
        if *ch == '_' || *ch == '-' {
            out.push('_');
            continue;
        }
        if ch.is_uppercase() {
            let prev_lower = i > 0 && (chars[i - 1].is_lowercase() || chars[i - 1].is_numeric());
            // Trailing capital of an acronym followed by a word: HTTPServer.
            let next_lower = i + 1 < chars.len() && chars[i + 1].is_lowercase();
            let prev_upper = i > 0 && chars[i - 1].is_uppercase();
            if prev_lower || (prev_upper && next_lower) {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(*ch);
        }
    }

    // Collapse doubled separators produced by SCREAMING_CASE input.
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out
}

/// `created_at` / `CREATED_AT` -> `CreatedAt`.
pub fn pascal_case(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut capitalize = true;
    let all_caps = input
        .chars()
        .all(|c| c.is_uppercase() || c == '_' || c.is_numeric());

    for ch in input.chars() {
        if ch == '_' || ch == '-' {
            capitalize = true;
            continue;
        }
        if capitalize {
            out.extend(ch.to_uppercase());
            capitalize = false;
        } else if all_caps {
            // ACTIVE -> Active rather than ACTIVE.
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Name of the set of codes one operation may return:
/// `Players` + `nickSet` -> `PlayersNickSetCodes`.
///
/// The server names it in the trait and the client in its method signature, so
/// the spelling is decided once here rather than in each emitter.
pub fn codes_type_name(service: &str, operation: &str) -> String {
    format!("{}{}Codes", pascal_case(service), pascal_case(operation))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_case_handles_camel_and_screaming() {
        assert_eq!(snake_case("createdAt"), "created_at");
        assert_eq!(snake_case("id"), "id");
        assert_eq!(snake_case("avatarUrl"), "avatar_url");
        assert_eq!(snake_case("CREATED_AT"), "created_at");
        assert_eq!(snake_case("userId"), "user_id");
    }

    #[test]
    fn snake_case_escapes_keywords() {
        assert_eq!(snake_case("type"), "r#type");
        assert_eq!(snake_case("match"), "r#match");
        // Reserved since the 2018 edition, and so not usable bare.
        assert_eq!(snake_case("try"), "r#try");
    }

    /// The escape is not universal, and the compiler has to know which three
    /// names it cannot reach.
    #[test]
    fn the_three_unescapable_keywords_are_recognised() {
        for name in ["self", "crate", "super"] {
            assert!(is_unescapable_keyword(name), "{name}");
        }
        for name in ["type", "match", "try", "id", "name"] {
            assert!(!is_unescapable_keyword(name), "{name}");
        }
    }

    #[test]
    fn identifiers_are_told_from_arbitrary_strings() {
        for name in ["kind", "_kind", "kindOf", "kind_of2"] {
            assert!(is_identifier(name), "{name}");
        }
        for name in ["kind-of", "2kind", "", "kind of", "kind.of", "kind'"] {
            assert!(!is_identifier(name), "{name}");
        }
    }

    #[test]
    fn pascal_case_normalizes() {
        assert_eq!(pascal_case("ACTIVE"), "Active");
        assert_eq!(pascal_case("created_at"), "CreatedAt");
        assert_eq!(pascal_case("card"), "Card");
        assert_eq!(pascal_case("get"), "Get");
    }
}
