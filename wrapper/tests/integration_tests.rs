use std::{
    path::Path,
    process::{Command, Stdio},
};

use tempfile::tempdir;

const WRAPPER_PATH: &str = env!("CARGO_BIN_EXE_wrapper");

// Default to using the wrapper to exercise it as much as possible;
// if we want to do things without it, we can have a version of this
// that is explicitly "without wrapper".
fn cargo() -> Command {
    let mut command = Command::new("cargo");
    command.env("RUSTC_WRAPPER", WRAPPER_PATH);
    // REVISIT: Maybe we should forward this via `println!`
    // instead so that it gets shown if tests fail.
    command.stdout(Stdio::null());
    command.stderr(Stdio::null());
    command
}

fn cargo_init(path: &Path) {
    assert!(cargo()
        .args(["init", "--name", "foo"])
        .current_dir(path)
        .status()
        .unwrap()
        .success());
}

// Simple case where the wrapper shouldn't do anything.
#[test]
fn build_crate_with_no_deps() {
    let dir = tempdir().unwrap();
    cargo_init(dir.path());
}
