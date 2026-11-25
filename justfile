set export
set dotenv-load

default:
  @just --list

PWD := invocation_directory()

ARTIFACT_DIR := env_var_or_default("ARTIFACT_DIR", "./dist")
PROFILE := env_var_or_default("PROFILE", "release")
TARGET := env_var_or_default("TARGET", "x86_64-unknown-linux-gnu")

export RUSTFLAGS := ""

lint:
    cargo deny --log-level error check advisories bans sources
    cargo fmt --all --check -- --unstable-features --error-on-unformatted
    cargo check --profile "$PROFILE"
    cargo clippy --profile "$PROFILE"
    cargo sort -c -w
    cargo machete

fix:
    cargo clippy --fix --allow-dirty --allow-staged --all-features --all-targets
    cargo fmt --all -- --unstable-features --error-on-unformatted
    cargo sort -w
    cargo machete --fix

test:
    cargo test

test-one filter:
    cargo test {{filter}}

install:
    cargo install --path .

# Generates the TypeScript and zod of every fixture under tests/fixtures/, and of
# the SDL under tests/sdl/ (types spelt like the globals the client uses), into
# target/ts-check/fixtures (rewritten whole each run) and type-checks them.
# `typescript` and `zod` are pinned to the exact versions below and installed
# there with npm, which needs the network; later runs reuse them until a pin
# changes or either package goes missing. The `#[ignore]`d `ts_check` test runs
# the same tsc step on the library's output.
#
# Type-check the generated TypeScript and zod with `tsc --strict`
check-ts:
    #!/usr/bin/env bash
    set -euo pipefail
    typescript=7.0.2
    zod=4.6.7
    dir=target/ts-check
    mkdir -p "$dir"
    installed_typescript=$(jq -r .version "$dir/node_modules/typescript/package.json" 2>/dev/null || echo none)
    installed_zod=$(jq -r .version "$dir/node_modules/zod/package.json" 2>/dev/null || echo none)
    if [ ! -x "$dir/node_modules/.bin/tsc" ] || [ ! -f "$dir/node_modules/zod/package.json" ] \
        || [ "$installed_typescript" != "$typescript" ] || [ "$installed_zod" != "$zod" ]; then
        rm -rf "$dir/node_modules" "$dir/package-lock.json"
        echo '{ "private": true }' > "$dir/package.json"
        npm install --prefix "$dir" --no-audit --no-fund --save-exact \
            "typescript@$typescript" "zod@$zod"
    fi
    version=$(cargo metadata --offline --no-deps --format-version 1 \
        | jq -r '.packages[] | select(.name == "graphql-rpcgen") | .version')
    rm -rf "$dir/fixtures"
    for sdl in tests/fixtures/*.graphql tests/sdl/*.graphql; do
        name=$(basename "$sdl" .graphql)
        out="$dir/fixtures/$name"
        mkdir -p "$out/api"
        cp "$sdl" "$out/api/"
        printf 'rpcgen_version = "%s"\n\n[output]\ntypescript = "api.ts"\ntypescript_zod = "zod.ts"\n' \
            "$version" > "$out/rpcgen.toml"
        cargo run --quiet --offline --bin graphql-rpcgen -- generate --api "$out/api" --root "$out"
    done
    cat > "$dir/tsconfig.json" <<'JSON'
    {
      "compilerOptions": {
        "strict": true,
        "noEmit": true,
        "target": "es2022",
        "lib": ["es2022", "dom", "dom.iterable"],
        "module": "esnext",
        "moduleResolution": "bundler",
        "skipLibCheck": true
      },
      "include": ["fixtures/*/api.ts", "fixtures/*/zod.ts"]
    }
    JSON
    "$dir/node_modules/.bin/tsc" --version
    "$dir/node_modules/.bin/tsc" --noEmit --strict -p "$dir/tsconfig.json"
