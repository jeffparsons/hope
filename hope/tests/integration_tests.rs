use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

use cache_log::{BuildScriptRunEvent, CacheLogLine, PullCrateOutputsEvent, PushCrateOutputsEvent};
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
//
// TODO: Factor most of this out; you should probably
// just have one example that does a bunch of fun things
// with a bunch of different crates in a big loop,
// and then some more targeted unit tests.
#[test]
fn build_crate_with_simple_dep() {
    let cache_dir = CacheDir::new();
    let package_a = Package::new(&cache_dir);
    package_a.add("anyhow@1.0.0");
    package_a.build();

    // It should have written it to the cache.
    let log = cache_dir.read_log().unwrap();

    let push_events = filter_push_crate_outputs_events(&log, "anyhow");
    assert_eq!(push_events.len(), 1);

    // Build the same package again, and make sure it doesn't
    // have to push to the cache again _or_ pull from the cache.
    package_a.build();

    // It should not have needed to build again.
    let push_events = filter_push_crate_outputs_events(&log, "anyhow");
    assert_eq!(push_events.len(), 1);
    let pull_events = filter_push_crate_outputs_events(&log, "anyhow");
    assert_eq!(pull_events.len(), 1);

    let package_b = Package::new(&cache_dir);
    package_b.add("anyhow@1.0.0");
    package_b.build();

    // Build for package b should have _pulled_ it from the cache.
    let log = cache_dir.read_log().unwrap();
    let pull_events = filter_pull_crate_outputs_events(&log, "anyhow");
    assert_eq!(pull_events.len(), 1);
}

// Dep with proc macro.
#[test]
fn build_dep_with_proc_macro() {
    let cache_dir = CacheDir::new();
    let package_a = Package::new(&cache_dir);
    package_a.add("serde_derive@1.0.0");
    package_a.build();

    // It should have written it to the cache.
    let log = cache_dir.read_log().unwrap();

    let push_events = filter_push_crate_outputs_events(&log, "serde_derive");
    assert_eq!(push_events.len(), 1);

    // Build the same package again, and make sure it doesn't
    // have to push to the cache again _or_ pull from the cache.
    package_a.build();

    // It should not have needed to build again.
    let push_events = filter_push_crate_outputs_events(&log, "serde_derive");
    assert_eq!(push_events.len(), 1);
    let pull_events = filter_push_crate_outputs_events(&log, "serde_derive");
    assert_eq!(pull_events.len(), 1);

    let package_b = Package::new(&cache_dir);
    package_b.add("serde_derive@1.0.0");
    package_b.build();

    // Build for package b should have _pulled_ it from the cache.
    let log = cache_dir.read_log().unwrap();
    let pull_events = filter_pull_crate_outputs_events(&log, "serde_derive");
    assert_eq!(pull_events.len(), 1);
}

#[test]
fn build_dep_with_build_script() {
    let cache_dir = CacheDir::new();
    let package_a = Package::new(&cache_dir);
    package_a.add("typenum@1.17.0");
    package_a.build();

    // It should have written it to the cache.
    let log = cache_dir.read_log().unwrap();

    let push_events = filter_push_crate_outputs_events(&log, "typenum");
    assert_eq!(push_events.len(), 1);

    // There should have been at least one build script run.
    // TODO: Tie it back to the main crate. That's a bit fiddly/hacky, but doable.
    let ran_build_script_events = filter_ran_build_script_events(&log);
    let first_ran_build_script_event_count = ran_build_script_events.len();
    assert!(first_ran_build_script_event_count > 0);

    // Build the same package again, and make sure it doesn't
    // have to push to the cache again _or_ pull from the cache.
    package_a.build();

    // It should not have needed to build again.
    let push_events = filter_push_crate_outputs_events(&log, "typenum");
    assert_eq!(push_events.len(), 1);
    let pull_events = filter_push_crate_outputs_events(&log, "typenum");
    assert_eq!(pull_events.len(), 1);

    // There should not have been any more build script runs.
    let ran_build_script_events = filter_ran_build_script_events(&log);
    let subsequent_ran_build_script_event_count = ran_build_script_events.len();
    assert_eq!(
        subsequent_ran_build_script_event_count,
        first_ran_build_script_event_count
    );

    let package_b = Package::new(&cache_dir);
    package_b.add("typenum@1.17.0");
    package_b.build();

    // Build for package b should have _pulled_ it from the cache.
    let log = cache_dir.read_log().unwrap();
    let pull_events = filter_pull_crate_outputs_events(&log, "typenum");
    assert_eq!(pull_events.len(), 1);

    // There should not have been any more build script runs.
    let ran_build_script_events = filter_ran_build_script_events(&log);
    let subsequent_ran_build_script_event_count = ran_build_script_events.len();
    assert_eq!(
        subsequent_ran_build_script_event_count,
        first_ran_build_script_event_count
    );
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

    // It should have written it to the cache.
    let log = cache_dir.read_log().unwrap();

    let push_events = filter_push_crate_outputs_events(&log, "ring");
    assert_eq!(push_events.len(), 1);

    // There should have been at least one build script run.
    // TODO: Tie it back to the main crate. That's a bit fiddly/hacky, but doable.
    let ran_build_script_events = filter_ran_build_script_events(&log);
    let first_ran_build_script_event_count = ran_build_script_events.len();
    assert!(first_ran_build_script_event_count > 0);

    // Build the same package again, and make sure it doesn't
    // have to push to the cache again _or_ pull from the cache.
    package_a.build();

    // It should not have needed to build again.
    let push_events = filter_push_crate_outputs_events(&log, "ring");
    assert_eq!(push_events.len(), 1);
    let pull_events = filter_push_crate_outputs_events(&log, "ring");
    assert_eq!(pull_events.len(), 1);

    // There should not have been any more build script runs.
    let ran_build_script_events = filter_ran_build_script_events(&log);
    let subsequent_ran_build_script_event_count = ran_build_script_events.len();
    assert_eq!(
        subsequent_ran_build_script_event_count,
        first_ran_build_script_event_count
    );

    let package_b = Package::new(&cache_dir);
    package_b.add("ring@0.17.8");
    package_b.build();

    // Build for package b should have _pulled_ it from the cache.
    let log = cache_dir.read_log().unwrap();
    let pull_events = filter_pull_crate_outputs_events(&log, "ring");
    assert_eq!(pull_events.len(), 1);

    // There should not have been any more build script runs.
    let ran_build_script_events = filter_ran_build_script_events(&log);
    let subsequent_ran_build_script_event_count = ran_build_script_events.len();
    assert_eq!(
        subsequent_ran_build_script_event_count,
        first_ran_build_script_event_count
    );
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

    pub fn read_log(&self) -> anyhow::Result<Vec<CacheLogLine>> {
        cache_log::read_log(self.dir.path())
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
}

fn filter_push_crate_outputs_events(
    log: &[CacheLogLine],
    crate_name: &str,
) -> Vec<PushCrateOutputsEvent> {
    log.iter()
        .filter_map(|line| match line {
            CacheLogLine::PushedCrateOutputs(push_event) => {
                if push_event.crate_unit_name.starts_with(crate_name) {
                    Some(push_event)
                } else {
                    None
                }
            }
            _ => None,
        })
        .cloned()
        .collect()
}

fn filter_pull_crate_outputs_events(
    log: &[CacheLogLine],
    crate_name: &str,
) -> Vec<PullCrateOutputsEvent> {
    log.iter()
        .filter_map(|line| match line {
            CacheLogLine::PulledCrateOutputs(pull_event) => {
                if pull_event.crate_unit_name.starts_with(crate_name) {
                    Some(pull_event)
                } else {
                    None
                }
            }
            _ => None,
        })
        .cloned()
        .collect()
}

fn filter_ran_build_script_events(log: &[CacheLogLine]) -> Vec<BuildScriptRunEvent> {
    log.iter()
        .filter_map(|line| match line {
            CacheLogLine::RanBuildScript(ran_build_script_event) => Some(ran_build_script_event),
            _ => None,
        })
        .cloned()
        .collect()
}
