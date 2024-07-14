# Hope

A WIP `rustc` wrapper for caching build artifacts.

Only concerns itself with crates from immutable sources, e.g., crates.io. This is because:

1. It makes caching a much easier problem to solve, because you can key things on the crate fingerprint alone.
2. The vast majority of build time is typically spent on external dependencies.

Relies on knowledge of private, unstable Cargo internals, so it may well break across Rust releases.

## Status

- Incomplete
- Buggy
