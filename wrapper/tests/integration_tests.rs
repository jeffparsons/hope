use std::process::{Command, Stdio};

use tempfile::{tempdir, TempDir};

const WRAPPER_PATH: &str = env!("CARGO_BIN_EXE_wrapper");

// Simple case where the wrapper shouldn't do anything.
#[test]
fn build_crate_with_no_deps() {
    let package = Package::new();
    package.build();
}

// One dependency, without any build script.
#[test]
fn build_crate_with_simple_dep() {
    let package = Package::new();
    package.add("anyhow@1.0.0");
    package.build();
}

//
// Test helpers
//

struct Package {
    dir: TempDir,
}

impl Package {
    fn new() -> Self {
        let package = Self {
            dir: tempdir().unwrap(),
        };
        package.init();
        package
    }

    fn init(&self) {
        assert!(cargo()
            .args(["init", "--name", "foo"])
            .current_dir(self.dir.path())
            .status()
            .unwrap()
            .success());
    }

    fn add(&self, dep: &str) {
        assert!(cargo()
            .args(["add", dep])
            .current_dir(self.dir.path())
            .status()
            .unwrap()
            .success());
    }

    fn build(&self) {
        assert!(cargo()
            .arg("build")
            .current_dir(self.dir.path())
            .status()
            .unwrap()
            .success());
    }
}

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
