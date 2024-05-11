#!/bin/bash

# TODO: Replace this with proper integration tests.

set -ex

pushd wrapper
cargo build
popd

export RUSTC_WRAPPER=$(realpath wrapper/target/debug/wrapper)

pushd fixtures
pushd twin-a
cargo clean
cargo build
popd
pushd twin-b
cargo clean
cargo build
popd
popd

fixtures/twin-a/target/debug/twin-a
fixtures/twin-b/target/debug/twin-b