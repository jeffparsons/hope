use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

use cache_log::{
    read_log, CacheLogLine, PullBuildScriptOutputsEvent, PullCrateOutputsEvent,
    PushBuildScriptOutputsEvent, PushCrateOutputsEvent,
};
use tempfile::{tempdir, TempDir};

const WRAPPER_PATH: &str = env!("CARGO_BIN_EXE_hope");

// Simple case where the wrapper shouldn't do anything.
#[test]
fn build_crate_with_no_deps() {
    let cache_dir = CacheDir::new();
    let package = Package::new(&cache_dir);
    package.build();
}

// One dependency, without any build script.
//
// TODO: I think this actually has a build script. Oops!
// Add assertions to make sure there is no build script!
#[test]
fn build_crate_with_simple_dep() {
    let cache_dir = CacheDir::new();
    let package_a = Package::new(&cache_dir);
    package_a.add("anyhow@1.0.0");
    package_a.build();

    // It should have written it to the cache.
    let log = package_a.read_log("debug").unwrap();

    let push_events = filter_push_crate_outputs_events(&log);
    assert_eq!(push_events.len(), 1);
    assert!(push_events[0].crate_unit_name.starts_with("anyhow-"));

    // Build the same package again, and make sure it doesn't
    // have to pull from the cache again.
    package_a.build();

    // It should not have needed to build again.
    let push_events = filter_push_crate_outputs_events(&log);
    assert_eq!(push_events.len(), 1);

    let package_b = Package::new(&cache_dir);
    package_b.add("anyhow@1.0.0");
    package_b.build();

    // Build for package b should have _pulled_ it from the cache.
    let log = package_b.read_log("debug").unwrap();
    let pull_events = filter_pull_crate_outputs_events(&log);
    assert_eq!(pull_events.len(), 1);
    assert!(pull_events[0].crate_unit_name.starts_with("anyhow-"));
}

// Dep with proc macro.
#[test]
fn build_dep_with_proc_macro() {
    let cache_dir = CacheDir::new();
    let package_a = Package::new(&cache_dir);
    package_a.add("serde_derive@1.0.0");
    package_a.build();

    // It should have written it to the cache.
    let log = package_a.read_log("debug").unwrap();
    let mut push_events = filter_push_crate_outputs_events(&log);
    push_events.retain(|push| push.crate_unit_name.starts_with("serde_derive-"));
    assert_eq!(push_events.len(), 1);
}

#[test]
fn build_dep_with_build_script() {
    let cache_dir = CacheDir::new();
    let package_a = Package::new(&cache_dir);
    package_a.add("typenum@1.17.0");
    package_a.build();

    // It should have written the build script output it to the cache.
    let log = package_a.read_log("debug").unwrap();
    let mut build_script_events = filter_push_build_script_outputs_events(&log);
    build_script_events.retain(|push| push.crate_unit_name.starts_with("typenum-"));
    assert_eq!(build_script_events.len(), 1);

    // Build the same package again, and make sure it doesn't
    // have to pull from the cache again.
    package_a.build();

    // It should not have needed to run the build script again.
    let push_events = filter_push_build_script_outputs_events(&log);
    assert_eq!(push_events.len(), 1);

    let package_b = Package::new(&cache_dir);
    package_b.add("typenum@1.0.0");
    package_b.build();

    // Build for package b should have _pulled_ build script outputs from the cache.
    let log = package_b.read_log("debug").unwrap();
    let pull_events = filter_pull_build_script_outputs_events(&log);
    assert_eq!(pull_events.len(), 1);
    assert!(pull_events[0].crate_unit_name.starts_with("typenum-"));
}

// Make sure we're actually handling the files and stdout
// from build scripts properly.
//
// TODO: We should probably use something like 'libc'
// that doesn't have any other dependencies with build scripts.
// Check that it's actually doing something meaningful
// (that makes the tests fail if we don't do the right thing)
// and then switch over.
#[test]
fn build_dep_with_build_script_that_builds_stuff() {
    let cache_dir = CacheDir::new();
    let package_a = Package::new(&cache_dir);
    package_a.add("ring@0.17.8");
    package_a.build();

    // It should have written the build script output it to the cache.
    let log = package_a.read_log("debug").unwrap();
    let mut push_events = filter_push_build_script_outputs_events(&log);
    push_events.retain(|push| push.crate_unit_name.starts_with("ring-"));
    assert_eq!(push_events.len(), 1);

    // Build the same package again, and make sure it doesn't
    // have to pull from the cache again.
    package_a.build();

    // It should not have needed to run the build script again.
    let mut push_events = filter_push_build_script_outputs_events(&log);
    push_events.retain(|push| push.crate_unit_name.starts_with("ring-"));
    assert_eq!(push_events.len(), 1);

    let package_b = Package::new(&cache_dir);
    package_b.add("ring@0.17.8");
    package_b.build();

    // Build for package b should have _pulled_ build script outputs from the cache.
    let log = package_b.read_log("debug").unwrap();
    let mut pull_events = filter_pull_build_script_outputs_events(&log);
    pull_events.retain(|push| push.crate_unit_name.starts_with("ring-"));
    assert_eq!(pull_events.len(), 1);
}

// TODO:
// - Multiple versions of the same dependency
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
        command.env("HOPE_CACHE_DIR", self.cache_dir.to_str().unwrap());

        if std::env::var("HOPE_VERBOSE") == Ok("true".to_string()) {
            command.arg("-v");
        } else {
            // REVISIT: Maybe we should forward this via `println!`
            // instead so that it gets shown if tests fail.
            //
            // See <https://github.com/rust-lang/rust/issues/92370>.
            command.stdout(Stdio::null());
            command.stderr(Stdio::null());
        }

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

    // TODO: Don't put it in deps; should go higher up.
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

fn filter_push_crate_outputs_events(log: &[CacheLogLine]) -> Vec<PushCrateOutputsEvent> {
    log.iter()
        .filter_map(|line| match line {
            CacheLogLine::PushedCrateOutputs(push_event) => Some(push_event),
            _ => None,
        })
        .cloned()
        .collect()
}

fn filter_pull_crate_outputs_events(log: &[CacheLogLine]) -> Vec<PullCrateOutputsEvent> {
    log.iter()
        .filter_map(|line| match line {
            CacheLogLine::PulledCrateOutputs(pull_event) => Some(pull_event),
            _ => None,
        })
        .cloned()
        .collect()
}

fn filter_push_build_script_outputs_events(
    log: &[CacheLogLine],
) -> Vec<PushBuildScriptOutputsEvent> {
    log.iter()
        .filter_map(|line| match line {
            CacheLogLine::PushedBuildScriptOutputs(push_event) => Some(push_event),
            _ => None,
        })
        .cloned()
        .collect()
}

fn filter_pull_build_script_outputs_events(
    log: &[CacheLogLine],
) -> Vec<PullBuildScriptOutputsEvent> {
    log.iter()
        .filter_map(|line| match line {
            CacheLogLine::PulledBuildScriptOutputs(pull_event) => Some(pull_event),
            _ => None,
        })
        .cloned()
        .collect()
}
