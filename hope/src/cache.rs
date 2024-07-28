use std::{
    fs::File,
    io::Write as _,
    path::{Path, PathBuf},
    str::FromStr,
    time::Instant,
};

use anyhow::Context;
use cache_log::{write_log_line, CacheLogLine, PullCrateOutputsEvent, PushCrateOutputsEvent};
use chrono::Utc;
use directories::ProjectDirs;

use crate::OutputDefn;

/// Cache implementations are not responsible for modifying
/// content to be stored/retrieved (e.g. changing paths);
/// that is the responsibility of the caller.
pub trait Cache {
    /// Unit name is of the form "{crate name}-{metadata hash}".
    ///
    /// The `arrival_dir` should be a temporary directory.
    /// Once files are placed in that directory, it is the caller's
    /// responsibility to perform any path mangling and ensure that
    /// they are copied over to the target directory kinda-atomically
    /// (at least try to clean up if you get a failure part-way through).
    fn pull_crate(
        &self,
        unit_name: &str,
        output_defns: &[OutputDefn],
        arrival_dir: &Path,
    ) -> anyhow::Result<()>;

    /// Unit name is of the form "{crate name}-{metadata hash}".
    ///
    /// TODO: List things that must be placed into this dir,
    /// and provide a helper to assert that they are there!
    fn push_crate(
        &self,
        unit_name: &str,
        output_defns: &[OutputDefn],
        departure_dir: &Path,
    ) -> anyhow::Result<()>;

    /// Get stdout of a build script execution from the cache.
    ///
    /// (We don't have a great source for the main crate name when we
    /// need to look this up, so just go by the execution's metadata hash alone.)
    ///
    /// If this is present, then we can assume that the whole crate
    /// output is cached, so we can just emit the cached stdout to control
    /// arguments to `rustc` for the build of the main crate, but without
    /// actually building or running the build script itself.
    fn get_build_script_stdout(
        &self,
        build_script_execution_metadata_hash: &str,
    ) -> anyhow::Result<Vec<u8>>;

    /// Put stdout of a build script execution into the cache.
    fn put_build_script_stdout(
        &self,
        build_script_execution_metadata_hash: &str,
        stdout: &[u8],
    ) -> anyhow::Result<()>;
}

pub struct LocalCache {
    root: PathBuf,
}

impl LocalCache {
    /// This does _not_ create the cache dir for you.
    ///
    /// If you want that, then call `from_env`, which ensures
    /// the directory exists.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn from_env() -> anyhow::Result<Self> {
        let cache_dir = Self::dir_from_env().context("Couldn't infer cache directory")?;
        if !cache_dir.exists() {
            std::fs::create_dir_all(&cache_dir).context("Failed to create cache dir")?;
        }
        Ok(Self::new(cache_dir))
    }

    pub fn dir_from_env() -> anyhow::Result<PathBuf> {
        if let Ok(dir_from_env) = std::env::var("HOPE_CACHE_DIR") {
            return PathBuf::from_str(&dir_from_env)
                .context("Invalid path in 'HOPE_CACHE_DIR' environment variable");
        }
        // Default to a directory based on OS-specific standard.
        let project_dirs =
            ProjectDirs::from("", "", "Hope").context("Couldn't get project dirs for Hope")?;
        Ok(project_dirs.cache_dir().to_owned())
    }
}

impl Cache for LocalCache {
    fn pull_crate(
        &self,
        unit_name: &str,
        output_defns: &[OutputDefn],
        arrival_dir: &Path,
    ) -> anyhow::Result<()> {
        let before = Instant::now();

        for output_defn in output_defns {
            let file_name = output_defn.file_name(unit_name);
            let from_path = self.root.join(&file_name);
            let to_path = arrival_dir.join(&file_name);
            // Copy it to from cache dir.
            std::fs::copy(from_path, &to_path)
                .with_context(|| format!("Failed to copy file {file_name:?} from local cache."))?;
        }

        // Write out a log line describing where we got the unit from.
        write_log_line(
            &self.root,
            CacheLogLine::PulledCrateOutputs(PullCrateOutputsEvent {
                crate_unit_name: unit_name.to_owned(),
                copied_at: Utc::now(),
                copied_from: "local cache".to_string(),
                duration_secs: before.elapsed().as_secs_f64(),
            }),
        )?;

        Ok(())
    }

    fn push_crate(
        &self,
        unit_name: &str,
        output_defns: &[OutputDefn],
        departure_dir: &Path,
    ) -> anyhow::Result<()> {
        let before = Instant::now();

        for output_defn in output_defns {
            let file_name = output_defn.file_name(unit_name);
            let from_path = departure_dir.join(&file_name);
            let to_path = self.root.join(&file_name);
            // Copy it to the cache dir.
            std::fs::copy(from_path, to_path)
                .with_context(|| format!("Failed to copy file {file_name:?} to local cache."))?;
        }

        // Write out a log line describing where we pushed the unit to.
        write_log_line(
            &self.root,
            CacheLogLine::PushedCrateOutputs(PushCrateOutputsEvent {
                crate_unit_name: unit_name.to_owned(),
                copied_at: Utc::now(),
                copied_from: "local cache".to_string(),
                duration_secs: before.elapsed().as_secs_f64(),
            }),
        )?;

        Ok(())
    }

    fn get_build_script_stdout(
        &self,
        build_script_execution_metadata_hash: &str,
    ) -> anyhow::Result<Vec<u8>> {
        let stdout_file_name = build_script_stdout_file_name(build_script_execution_metadata_hash);
        let stdout_path = self.root.join(&stdout_file_name);
        let content = std::fs::read_to_string(stdout_path).with_context(|| {
            format!("Failed to read build script stdout file \"{stdout_file_name}\".")
        })?;
        Ok(content.into_bytes())
    }

    fn put_build_script_stdout(
        &self,
        build_script_execution_metadata_hash: &str,
        stdout: &[u8],
    ) -> anyhow::Result<()> {
        let stdout_file_name = build_script_stdout_file_name(build_script_execution_metadata_hash);
        let stdout_path = self.root.join(stdout_file_name);

        let mut stdout_file =
            File::create(stdout_path).context("Failed to create file for build script stdout")?;
        stdout_file
            .write_all(stdout)
            .context("Failed to write build script stdout to file")?;
        Ok(())
    }
}

/// We don't have a great source for the main crate name when we
/// need to look this up, so just go by the execution's metadata hash alone.
pub fn build_script_stdout_file_name(build_script_execution_metadata_hash: &str) -> String {
    // NOTE: This is different to what Cargo calls it ("output").
    // I flip-flopped a bit on this, but ultimately decided that
    // I preferred calling it this in my own file names to clarify exactly what it is.
    // (Yeah, I know: big deal, right?)
    format!("build-script-{build_script_execution_metadata_hash}-stdout.txt")
}
