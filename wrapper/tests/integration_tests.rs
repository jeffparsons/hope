use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

use cache_log::{read_log, CacheLogLine};
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
    let package_a = Package::new(&cache_dir);
    package_a.add("anyhow@1.0.0");
    package_a.build();

    // It should have written it to the cache.
    let log = package_a.read_log("debug").unwrap();
    assert_eq!(log.len(), 1);
    let CacheLogLine::Pushed(push_event) = &log[0] else {
        panic!("Expected push event");
    };
    assert!(push_event.crate_unit_name.starts_with("anyhow-"));

    // Build the same package again, and make sure it doesn't
    // have to pull from the cache again.
    package_a.build();

    // It should not have needed to build again.
    let log = package_a.read_log("debug").unwrap();
    assert_eq!(log.len(), 1);

    let package_b = Package::new(&cache_dir);
    package_b.add("anyhow@1.0.0");
    package_b.build();

    // Build for package b should have _pulled_ it from the cache.
    let log = package_b.read_log("debug").unwrap();
    assert_eq!(log.len(), 1);
    let CacheLogLine::Pulled(pull_event) = &log[0] else {
        panic!("Expected pull event");
    };
    assert!(pull_event.crate_unit_name.starts_with("anyhow-"));
}

// Dep with proc macro.
#[test]
fn build_dep_with_proc_macro() {
    let cache_dir = CacheDir::new();
    let package_a = Package::new(&cache_dir);
    package_a.add("serde_derive@1.0.0");
    package_a.build();

    // It should have written it to the cache.
    let mut log = package_a.read_log("debug").unwrap();
    log.retain(|line| {
        let CacheLogLine::Pushed(push_event) = line else {
            return false;
        };
        push_event.crate_unit_name.starts_with("serde_derive-")
    });
    assert_eq!(log.len(), 1);
}

// TODO:
// - Multiple versions of the same dependency
// - Deps with build scripts
// - Deps with C dependencies
// - Deps with proc macros
// - Deps where the source mtimes are newer.
//   - Specifically, we need to make sure it doesn't keep trying to rebuild.

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
        // command.stdout(Stdio::null());
        // command.stderr(Stdio::null());
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

    fn read_log(&self, debug_or_release: &str) -> anyhow::Result<Vec<CacheLogLine>> {
        read_log(
            &self
                .dir
                .path()
                .join("target")
                .join(debug_or_release)
                .join("deps"),
        )
    }
}
