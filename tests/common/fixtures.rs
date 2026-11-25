//! The SDL under `tests/fixtures/`, listed once for every test that runs all of
//! it: `golden` snapshots each fixture's targets and `ts_check` type-checks its
//! TypeScript. `golden` also checks this list against the directory, so a
//! fixture added there and forgotten here fails a test rather than going
//! unchecked.

/// One fixture SDL, by its file's stem.
pub struct Fixture {
    /// `tests/fixtures/<name>.graphql`; also the prefix of its golden files
    /// and its directory under `target/ts-check/fixtures/`.
    pub name: &'static str,
    pub sdl: &'static str,
    /// `rpcgen.toml` lines the golden files are generated with, on top of the
    /// split layout every fixture gets; empty for the defaults.
    pub config: &'static str,
}

/// Every fixture under `tests/fixtures/`.
pub const FIXTURES: &[Fixture] = &[
    Fixture {
        name: "http_wire",
        sdl: include_str!("../fixtures/http_wire.graphql"),
        config: "",
    },
    Fixture {
        name: "mcp_version",
        sdl: include_str!("../fixtures/mcp_version.graphql"),
        config: r#"
[openapi]
title = "Catalog API"
version = "2.4.0"
description = "The catalog, versioned per service and per operation."
servers = ["https://api.example.com", "https://staging.example.com/base"]
"#,
    },
    Fixture {
        name: "oneof_wire",
        sdl: include_str!("../fixtures/oneof_wire.graphql"),
        config: "",
    },
    Fixture {
        name: "raw_wire",
        sdl: include_str!("../fixtures/raw_wire.graphql"),
        config: "",
    },
    Fixture {
        name: "raw_wire_permissions",
        sdl: include_str!("../fixtures/raw_wire_permissions.graphql"),
        config: "",
    },
    Fixture {
        name: "snapshots",
        sdl: include_str!("../fixtures/snapshots.graphql"),
        config: "",
    },
    Fixture {
        name: "snapshots_http",
        sdl: include_str!("../fixtures/snapshots_http.graphql"),
        config: "",
    },
];
