use std::{
    env,
    path::PathBuf,
    process::{Command, Stdio},
    sync::LazyLock,
};

use hope_cache_log::{
    BuildScriptRunEvent, BuildScriptWrapperRunEvent, CacheLogLine, PullCrateOutputsEvent,
    PushCrateOutputsEvent,
};
use tempfile::{tempdir, TempDir};

const WRAPPER_PATH: &str = env!("CARGO_BIN_EXE_hope");

struct DepSpec {
    name: String,
    version: String,
    has_build_script: bool,
}

impl DepSpec {
    pub fn new(name: &str, version: &str, has_build_script: bool) -> Self {
        Self {
            name: name.to_owned(),
            version: version.to_owned(),
            has_build_script,
        }
    }
}

static TEST_DEPS: LazyLock<Vec<DepSpec>> = LazyLock::new(|| {
    vec![
        DepSpec::new("anyhow", "1.0.0", true),
        DepSpec::new("serde_derive", "1.0.0", false),
        DepSpec::new("typenum", "1.17.0", true),
        DepSpec::new("ring", "0.17.8", true),
    ]
});

#[test]
fn build_lots_of_deps() {
    let cache_dir = CacheDir::new();

    let package_a = Package::new(&cache_dir);
    for dep in &*TEST_DEPS {
        package_a.add(&format!("{}@{}", dep.name, dep.version));
    }
    package_a.build();

    // It should have written all direct deps to the cache.
    let log = cache_dir.read_log().unwrap();
    // TODO: Make some helpers for saying "this, but for all deps in the list"
    // so that if it fails, it can summarise all the failures.
    for dep in &*TEST_DEPS {
        let push_events = filter_push_crate_outputs_events(&log, &dep.name);
        assert_eq!(push_events.len(), 1);

        if dep.has_build_script {
            // The build script wrapper should have run.
            let build_script_wrapper_run_events =
                filter_ran_build_script_wrapper_events(&log, &dep.name);
            assert_eq!(build_script_wrapper_run_events.len(), 1);

            // The real build script should have run.
            let build_script_run_events = filter_ran_build_script_events(&log, &dep.name);
            assert_eq!(build_script_run_events.len(), 1);
        }
    }

    // Build the same package again, and make sure it doesn't
    // have to push to the cache again _or_ pull from the cache.
    package_a.build();

    // It should not have needed to build again (i.e. neither real build, nor "pull from cache" build).
    for dep in &*TEST_DEPS {
        let push_events = filter_push_crate_outputs_events(&log, &dep.name);
        assert_eq!(push_events.len(), 1);
        let pull_events = filter_pull_crate_outputs_events(&log, &dep.name);
        assert_eq!(pull_events.len(), 0);

        if dep.has_build_script {
            // The build script wrapper should NOT have run again.
            let build_script_wrapper_run_events =
                filter_ran_build_script_wrapper_events(&log, &dep.name);
            assert_eq!(build_script_wrapper_run_events.len(), 1);

            // The real build script should NOT have run.
            let build_script_run_events = filter_ran_build_script_events(&log, &dep.name);
            assert_eq!(build_script_run_events.len(), 1);
        }
    }

    let package_b = Package::new(&cache_dir);
    for dep in &*TEST_DEPS {
        package_b.add(&format!("{}@{}", dep.name, dep.version));
    }
    package_b.build();

    // Build for package b should have _pulled_ everything from the cache.
    let log = cache_dir.read_log().unwrap();
    for dep in &*TEST_DEPS {
        let pull_events = filter_pull_crate_outputs_events(&log, &dep.name);
        assert_eq!(pull_events.len(), 1);

        if dep.has_build_script {
            // The build script wrapper should have run again.
            let build_script_wrapper_run_events =
                filter_ran_build_script_wrapper_events(&log, &dep.name);
            assert_eq!(build_script_wrapper_run_events.len(), 2);

            // BUT, the real build script should NOT have run again.
            // (We should have set up possibly deferred execution of the build script,
            // and then later decided to not actually run it.)
            let build_script_run_events = filter_ran_build_script_events(&log, &dep.name);
            assert_eq!(build_script_run_events.len(), 1);
        }
    }
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
        hope_cache_log::read_log(self.dir.path())
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

        if env::var("HOPE_TEST_OFFLINE") == Ok("1".to_string()) {
            command.arg("--offline");
        }

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

fn filter_ran_build_script_events(
    log: &[CacheLogLine],
    crate_name: &str,
) -> Vec<BuildScriptRunEvent> {
    log.iter()
        .filter_map(|line| match line {
            CacheLogLine::RanBuildScript(ran_build_script_event) => {
                if ran_build_script_event.crate_name.starts_with(crate_name) {
                    Some(ran_build_script_event)
                } else {
                    None
                }
            }
            _ => None,
        })
        .cloned()
        .collect()
}

fn filter_ran_build_script_wrapper_events(
    log: &[CacheLogLine],
    crate_name: &str,
) -> Vec<BuildScriptWrapperRunEvent> {
    log.iter()
        .filter_map(|line| match line {
            CacheLogLine::RanBuildScriptWrapper(ran_build_script_wrapper_event) => {
                if ran_build_script_wrapper_event
                    .crate_name
                    .starts_with(crate_name)
                {
                    Some(ran_build_script_wrapper_event)
                } else {
                    None
                }
            }
            _ => None,
        })
        .cloned()
        .collect()
}
