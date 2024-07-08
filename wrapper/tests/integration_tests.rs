use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

use tempfile::{tempdir, TempDir};

const WRAPPER_PATH: &str = env!("CARGO_BIN_EXE_wrapper");

// Simple case where the wrapper shouldn't do anything.
#[test]
fn build_crate_with_no_deps() {
    let cache_dir = CacheDir::new();
    let package = Package::new(&cache_dir);
    package.build();
}

// One dependency, without any build script.
#[test]
fn build_crate_with_simple_dep() {
    let cache_dir = CacheDir::new();
    let package = Package::new(&cache_dir);
    package.add("anyhow@1.0.0");
    package.build();

    // TODO: Make a structured log about what happened
    // during the build, and assert stuff about that.
}

//
// Test helpers
//

// Wrapper struct to make it super-obvious what we're talking about,
// because there are other paths floating around here, too.
// Also helps with making a new random one for each test!
struct CacheDir {
    dir: TempDir,
}

impl CacheDir {
    fn new() -> Self {
        Self {
            dir: tempdir().unwrap(),
        }
    }
}

struct Package {
    dir: TempDir,
    cache_dir: PathBuf,
}

impl Package {
    fn new(cache_dir: &CacheDir) -> Self {
        let package = Self {
            dir: tempdir().unwrap(),
            cache_dir: cache_dir.dir.path().to_owned(),
        };
        package.init();
        package
    }

    // Default to using the wrapper to exercise it as much as possible;
    // if we want to do things without it, we can have a version of this
    // that is explicitly "without wrapper".
    fn cargo(&self) -> Command {
        let mut command = Command::new("cargo");
        command.env("RUSTC_WRAPPER", WRAPPER_PATH);
        // Pass through the cache dir we're using for this test.
        command.env("WRAPPER_HAX_CACHE_DIR", self.cache_dir.to_str().unwrap());
        // REVISIT: Maybe we should forward this via `println!`
        // instead so that it gets shown if tests fail.
        //
        // See <https://github.com/rust-lang/rust/issues/92370>.
        command.stdout(Stdio::null());
        command.stderr(Stdio::null());
        command
    }

    fn init(&self) {
        assert!(self
            .cargo()
            .args(["init", "--name", "foo"])
            .current_dir(self.dir.path())
            .status()
            .unwrap()
            .success());
    }

    fn add(&self, dep: &str) {
        assert!(self
            .cargo()
            .args(["add", dep])
            .current_dir(self.dir.path())
            .status()
            .unwrap()
            .success());
    }

    fn build(&self) {
        assert!(self
            .cargo()
            .arg("build")
            .current_dir(self.dir.path())
            .status()
            .unwrap()
            .success());
    }
}
