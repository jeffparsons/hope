# Hope

A WIP `rustc` wrapper for caching build artifacts.

Assumes the use of Cargo, and will never support non-Cargo-based workflows.

## Status

- Incomplete
- Only minimally tested, so probably buggy
- Only tested on Linux and macOS, so probably doesn't work on Windows
- **May eat your data, pets, or other loved ones**

## How do I use it?

You probably _shouldn't_ use it yet (because it is still super-buggy), but if you really want to:

```bash
cargo install hope
export RUSTC_WRAPPER=$(which hope)
# In a Cargo project...
cargo build # etc.
```

## Design goals

_Hope_ only concerns itself with crates from immutable sources, e.g., crates.io.
This is because:

1. It makes caching a much easier problem to solve, because you can key things on the crate fingerprint alone rather than needing to discover all source content and hash it.
2. The vast majority of build time is typically spent on external dependencies, so this is a pragmatic place to start.

Relies on knowledge of private, unstable Cargo internals, so it may well break across Rust releases.

Currently only works for a single user on a single machine.
I plan to eventually add shared remote backends, e.g., S3.
