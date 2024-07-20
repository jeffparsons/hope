# Hope

A WIP `rustc` wrapper for caching build artifacts.

## Status

- Incomplete
- Only minimally tested, so probably buggy
- **May eat your data, pets, or other loved ones**

## How do I use it?

I haven't published any releases yet.
Once I do, I will add some instructions here.

## Design goals

_Hope_ only concerns itself with crates from immutable sources, e.g., crates.io.
This is because:

1. It makes caching a much easier problem to solve, because you can key things on the crate fingerprint alone rather than needing to discover all source content and hash it.
2. The vast majority of build time is typically spent on external dependencies, so this is a pragmatic place to start.

Relies on knowledge of private, unstable Cargo internals, so it may well break across Rust releases.

Currently only works for a single user on a single machine.
I plan to eventually add shared remote backends, e.g., S3.
