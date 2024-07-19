use std::{
    collections::HashSet,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::Context;
use cache_log::{
    write_log_line, CacheLogLine, PullBuildScriptOutputsEvent, PullCrateOutputsEvent,
    PushBuildScriptOutputsEvent, PushCrateOutputsEvent,
};
use chrono::Utc;

use crate::{CrateType, OutputType};

/// It is the responsibility of a `Cache` implementation to
/// modify files as necessary, either directly itself or by
/// using another `Cache` implementation that it knows does this.
///
/// E.g. a local cache doesn't strictly need to mangle path
/// names because all crates from crates.io will have their
/// source in the same location. But an S3-backed cache will
/// need to modify paths in '.d' files so that they work across
/// different user accounts.
///
/// (In practice I think we'll make the local cache do all this
/// because it'll make it easier to test.)
pub trait Cache {
    fn pull_crate_outputs(
        &self,
        out_dir: &Path,
        crate_unit_name: &str,
        crate_types: &HashSet<CrateType>,
        output_types: &HashSet<OutputType>,
    ) -> anyhow::Result<()>;

    fn push_crate_outputs(
        &self,
        out_dir: &Path,
        crate_unit_name: &str,
        crate_types: &HashSet<CrateType>,
        output_types: &HashSet<OutputType>,
    ) -> anyhow::Result<()>;

    fn pull_build_script_out_dir(
        &self,
        out_dir: &Path,
        crate_unit_name: &str,
    ) -> anyhow::Result<()>;

    fn push_build_script_out_dir(
        &self,
        out_dir: &Path,
        crate_unit_name: &str,
    ) -> anyhow::Result<()>;

    fn push_build_script_stdout(&self, crate_unit_name: &str, content: &[u8])
        -> anyhow::Result<()>;

    fn push_build_script_stderr(&self, crate_unit_name: &str, content: &[u8])
        -> anyhow::Result<()>;

    fn pull_build_script_stdout(&self, crate_unit_name: &str) -> anyhow::Result<Vec<u8>>;

    fn pull_build_script_stderr(&self, crate_unit_name: &str) -> anyhow::Result<Vec<u8>>;
}

pub struct LocalCache {
    root: PathBuf,
}

impl LocalCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl Cache for LocalCache {
    fn pull_crate_outputs(
        &self,
        out_dir: &Path,
        crate_unit_name: &str,
        crate_types: &HashSet<CrateType>,
        output_types: &HashSet<OutputType>,
    ) -> anyhow::Result<()> {
        let before = Instant::now();

        // TODO: If _anything_ in here fails, we should attempt to delete
        // all the files that we copied, because otherwise we might have
        // copied an 'rmeta' file without actually successfully copying
        // the lib that it refers to, and then it's hard to get out of that
        // stuck state.

        let output_defns = crate::output_defns(crate_types, output_types);
        for output_defn in output_defns {
            // TODO: '.d' files will need to be modified on push/pull to stop cargo from getting
            // confused and constantly trying to rebuild the crate. Are there any others that
            // need similar treatment?
            //
            // TODO: Also need tests to make sure that whatever you do here actually works!

            let file_name = output_defn.file_name(crate_unit_name);
            let from_path = self.root.join(&file_name);
            let to_path = out_dir.join(&file_name);
            // Copy it to from cache dir.
            std::fs::copy(from_path, &to_path)
                .with_context(|| format!("Failed to copy file {file_name:?} from cache."))?;

            // Bump the new copy's mtime. In my testing on macOS,
            // this seems to be necessary or the old mtime gets copied across
            // causing spurious rebuilds!
            filetime::set_file_mtime(to_path, filetime::FileTime::now())
                .with_context(|| format!("Failed to update mtime for {file_name:?}."))?;
        }

        // Write out a log line describing where we got the unit from.
        write_log_line(
            out_dir,
            CacheLogLine::PulledCrateOutputs(PullCrateOutputsEvent {
                crate_unit_name: crate_unit_name.to_owned(),
                copied_at: Utc::now(),
                copied_from: "local cache".to_string(),
                did_mangle_on_pull: false,
                duration_secs: before.elapsed().as_secs_f64(),
            }),
        )?;

        Ok(())

        // TODO: We will need to rewrite dep info to fix up absolute paths.
        // NOTE: `--remap-path-prefix` is not adequate for this; we actually
        // need to modify the output files. But maybe we should _also_ remap path
        // prefixes when we might write to the cache so that we don't produce
        // subtly misleading output (different paths on different people's machines).
    }

    fn push_crate_outputs(
        &self,
        out_dir: &Path,
        crate_unit_name: &str,
        crate_types: &HashSet<CrateType>,
        output_types: &HashSet<OutputType>,
    ) -> anyhow::Result<()> {
        let before = Instant::now();

        let output_defns = crate::output_defns(crate_types, output_types);
        for output_defn in output_defns {
            // TODO: '.d' files will need to be modified on push/pull to stop cargo from getting
            // confused and constantly trying to rebuild the crate. Are there any others that
            // need similar treatment?
            //
            // TODO: Also need tests to make sure that whatever you do here actually works!

            let file_name = output_defn.file_name(crate_unit_name);
            let from_path = out_dir.join(&file_name);
            let to_path = self.root.join(&file_name);
            // Copy it to the cache dir.
            std::fs::copy(from_path, to_path)
                .with_context(|| format!("Failed to copy file {file_name:?} to cache."))?;
        }

        // Write out a log line describing where we pushed the unit to.
        write_log_line(
            out_dir,
            CacheLogLine::PushedCrateOutputs(PushCrateOutputsEvent {
                crate_unit_name: crate_unit_name.to_owned(),
                copied_at: Utc::now(),
                copied_from: "local cache".to_string(),
                did_mangle_on_push: false,
                duration_secs: before.elapsed().as_secs_f64(),
            }),
        )?;

        Ok(())
    }

    fn pull_build_script_out_dir(
        &self,
        out_dir: &Path,
        crate_unit_name: &str,
    ) -> anyhow::Result<()> {
        let before = Instant::now();

        let tar_file_name = format!("{crate_unit_name}-out.tar");
        let tar_path = self.root.join(tar_file_name);
        let tar_file = File::open(tar_path).context("Failed to open \"out/\" dir tarball")?;
        let mut archive = tar::Archive::new(tar_file);
        archive
            .unpack(out_dir)
            .context("Failed to un-tar \"out/\" dir")?;

        // Write out a log line describing where we pulled the build script output from.
        //
        // TODO: Replace these nasty hacks with a better way to determine
        // the log path. (Or put it somewhere else!)
        let log_dir = out_dir
            .parent()
            .context("Missing parent of crate build/out dir")?
            .parent()
            .context("Missing parent of crate build dir")?
            .parent()
            .context("Missing parent of build dir")?
            .join("deps");
        write_log_line(
            &log_dir,
            CacheLogLine::PulledBuildScriptOutputs(PullBuildScriptOutputsEvent {
                crate_unit_name: crate_unit_name.to_owned(),
                copied_at: Utc::now(),
                copied_from: "local cache".to_string(),
                did_mangle_on_pull: false,
                duration_secs: before.elapsed().as_secs_f64(),
            }),
        )?;

        Ok(())
    }

    fn push_build_script_out_dir(
        &self,
        out_dir: &Path,
        crate_unit_name: &str,
    ) -> anyhow::Result<()> {
        let before = Instant::now();

        let tar_file_name = format!("{crate_unit_name}-out.tar");
        let tar_path = self.root.join(tar_file_name);
        let tar_file =
            File::create(tar_path).context("Failed to create \"out/\" dir file for writing")?;
        let mut tar = tar::Builder::new(tar_file);
        tar.append_dir_all(".", out_dir)
            .context("Failed to add out dir to tar file")?;
        tar.finish().context("Failed to finish writing tar file")?;

        // Write out a log line describing where we pushed the build script output to.
        //
        // TODO: Replace these nasty hacks with a better way to determine
        // the log path. (Or put it somewhere else!)
        let log_dir = out_dir
            .parent()
            .context("Missing parent of crate build/out dir")?
            .parent()
            .context("Missing parent of crate build dir")?
            .parent()
            .context("Missing parent of build dir")?
            .join("deps");

        write_log_line(
            &log_dir,
            CacheLogLine::PushedBuildScriptOutputs(PushBuildScriptOutputsEvent {
                crate_unit_name: crate_unit_name.to_owned(),
                copied_at: Utc::now(),
                copied_from: "local cache".to_string(),
                did_mangle_on_push: false,
                duration_secs: before.elapsed().as_secs_f64(),
            }),
        )?;

        Ok(())
    }

    fn push_build_script_stdout(
        &self,
        crate_unit_name: &str,
        content: &[u8],
    ) -> anyhow::Result<()> {
        let stdout_file_name = stdout_file_name(crate_unit_name);
        let stdout_path = self.root.join(&stdout_file_name);

        let mut stdout_file = File::create(stdout_path)
            .with_context(|| format!("Failed to create \"{stdout_file_name}\" file for writing"))?;
        stdout_file
            .write_all(content)
            .context("Failed to write stdout data to file")?;

        Ok(())
    }

    fn push_build_script_stderr(
        &self,
        crate_unit_name: &str,
        content: &[u8],
    ) -> anyhow::Result<()> {
        let stderr_file_name = stderr_file_name(crate_unit_name);
        let stderr_path = self.root.join(&stderr_file_name);

        let mut stderr_file = File::create(stderr_path)
            .with_context(|| format!("Failed to create \"{stderr_file_name}\" file for writing"))?;
        stderr_file
            .write_all(content)
            .context("Failed to write stderr data to file")?;

        Ok(())
    }

    fn pull_build_script_stdout(&self, crate_unit_name: &str) -> anyhow::Result<Vec<u8>> {
        let stdout_file_name = stdout_file_name(crate_unit_name);
        let stdout_path = self.root.join(&stdout_file_name);

        let content = std::fs::read_to_string(stdout_path)
            .with_context(|| format!("Failed to read stdout data file \"{stdout_file_name}\"."))?;
        Ok(content.into_bytes())
    }

    fn pull_build_script_stderr(&self, crate_unit_name: &str) -> anyhow::Result<Vec<u8>> {
        let stderr_file_name = stderr_file_name(crate_unit_name);
        let stderr_path = self.root.join(&stderr_file_name);

        let content = std::fs::read_to_string(stderr_path)
            .with_context(|| format!("Failed to read stderr data file \"{stderr_file_name}\"."))?;
        Ok(content.into_bytes())
    }
}

fn stdout_file_name(crate_unit_name: &str) -> String {
    // It doesn't really matter, but follow the Cargo convention
    // of calling this "output". (I assume they started with just stdout
    // and then added stderr later.)
    format!("{crate_unit_name}-output.txt")
}

fn stderr_file_name(crate_unit_name: &str) -> String {
    format!("{crate_unit_name}-stderr.txt")
}