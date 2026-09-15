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
