set shell := ["zsh", "-cu"]

default:
    @just --list

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

lint:
    cargo clippy --all-targets --all-features -- -D warnings

test:
    cargo test --all-targets --all-features

test-unit:
    cargo test --lib --all-features

test-e2e:
    cargo test --tests --all-features

check: fmt-check lint test

build:
    cargo build

run *args:
    cargo run -- {{args}}

install:
    cargo install --path . --locked
