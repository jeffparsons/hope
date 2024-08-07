//! Pretending to be a crate's build script

use core::str;
use std::{
    collections::HashMap,
    env,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    str::FromStr,
};

use anyhow::Context;
use chrono::Utc;
use hope_cache_log::{
    write_log_line, BuildScriptRunEvent, BuildScriptWrapperRunEvent, CacheLogLine,
};
use serde::{Deserialize, Serialize};

use crate::cache::{Cache, LocalCache};

pub const BUILD_SCRIPT_INVOCATION_INFO_FILE_NAME: &str = "build-script-invocation-info.json";

pub fn run(called_as: &Path) -> anyhow::Result<()> {
    // Figure out where the real build script is.
    let build_script_build_dir = called_as
        .parent()
        .context("Build script didn't have parent dir")?;
    // TODO: See comments where this is created about wanting to not do "real-build-script" symlink.
    let real_build_script_symlink_path = build_script_build_dir.join("real-build-script");

    // By convention, Cargo puts out dirs for build scripts under "target/debug/build/cratename-{metadata_hash}/out".
    // (This is a private implementation detail, but in practice the Cargo maintainers have been very conservative
    // about changing details like this, so it wouldn't be a big deal to adapt if they do occasionally change it.)
    // We want the build script execution metadata hash.
    let out_dir =
        env::var("OUT_DIR").context("Missing 'OUT_DIR' env var for build script execution")?;
    let out_dir =
        PathBuf::from_str(&out_dir).context("'OUT_DIR' env var contained invalid path")?;
    let (crate_name, run_metadata_hash) = out_dir
        .parent()
        .context("Missing parent on out dir")?
        .file_name()
        .context("Missing file name on build dir")?
        .to_str()
        .context("Invalid UTF-8 in build dir name")?
        .rsplit_once('-')
        .context("Couldn't find '-' in build dir")?;

    let cache_dir =
        LocalCache::dir_from_env().context("Failed to get local cache dir from environment")?;
    write_log_line(
        &cache_dir,
        CacheLogLine::RanBuildScriptWrapper(BuildScriptWrapperRunEvent {
            crate_name: crate_name.to_owned(),
            ran_at: Utc::now(),
        }),
    )?;

    // Can we find the stdout of this build script execution in cache?
    let cache = LocalCache::from_env()?;
    if let Ok(build_script_stdout) = cache.get_build_script_stdout(run_metadata_hash) {
        let build_script_stdout = str::from_utf8(&build_script_stdout)
            .context("Cached build script output contained invalid UTF-8")?;
        // We found the build script output in cache. We need to emit a copy of its output
        // so that Cargo knows what flags to use when invoking `rustc` for building the main crate.
        // (Most of them don't matter, but some things get a bit wonky if we don't emit the same thing
        // that the real build script does.)
        for line in build_script_stdout.lines() {
            if line.starts_with("cargo:rerun-if-") {
                // Skip output lines that would cause Cargo to consider
                // the build script as dirty just because we don't actually run it.
                //
                // (We store the full output in the cache because it's easier for debugging
                // and tweaking the rules here if we do it on the way _out_.)
                continue;
            }

            // TODO: See if there are any lines in the stdout that need to have, e.g., paths mangled.

            println!("{}", line);
        }

        // Don't bother printing the real stderr; it isn't used by Cargo.
        // Instead, print something to help people if they end up debugging
        // problems caused by Hope — just to hint at what's going on.
        eprintln!("Fake build script by Hope; real build script not run because we intend to pull the main crate output from cache.");

        // We also need to store some information about how this process was invoked so that
        // we can run the real build script later just before building the main crate if we discover
        // then that it's actually needed.
        // (We don't want to run it if it turns out that the final crate output can be pulled from cache!)
        let invocation_info = BuildScriptInvocationInfo {
            real_build_script_path: real_build_script_symlink_path
                .read_link()
                .context("Failed to read symlink to real build script")?,
            env_vars: env::vars().collect(),
            work_dir: env::current_dir().context("Couldn't get working dir")?,
        };
        let invocation_info_file =
            File::create(out_dir.join(BUILD_SCRIPT_INVOCATION_INFO_FILE_NAME))
                .context("Failed to create build script invocation info file")?;
        serde_json::to_writer(invocation_info_file, &invocation_info)
            .context("Failed to write build script invocation info file")?;
    } else {
        // TODO: Care about the specific error.

        // We couldn't find the build script output in cache, so we need to run it eagerly ourselves.
        let output = Command::new(&real_build_script_symlink_path)
            .output()
            .with_context(|| {
                format!(
                    "Failed to start real build script at {:?}",
                    real_build_script_symlink_path
                )
            })?;
        if !output.status.success() {
            std::process::exit(
                output
                    .status
                    .code()
                    .context("Child build script process was terminated by a signal")?,
            );
        }

        write_log_line(
            &cache_dir,
            CacheLogLine::RanBuildScript(BuildScriptRunEvent {
                crate_name: crate_name.to_string(),
                ran_at: Utc::now(),
            }),
        )?;

        // Forward child process stdout and stderr.
        // (We need to emit them, to instruct Cargo what to do for the main crate.)
        std::io::stdout().write_all(&output.stdout)?;
        std::io::stdout().write_all(&output.stderr)?;

        // Finally, we need to store the build script output for other builds to find!
        cache
            .put_build_script_stdout(run_metadata_hash, &output.stdout)
            .context("Failed to store build script output")?;
    }

    Ok(())
}

pub fn append_moved_build_script_suffix(build_script_path: &Path) -> anyhow::Result<PathBuf> {
    let build_script_file_name = build_script_path
        .file_name()
        .context("Missing file name for build script")?;
    let mut moved_build_script_file_name = build_script_file_name.to_owned();
    moved_build_script_file_name.push("-moved-by-hope");
    Ok(build_script_path.with_file_name(moved_build_script_file_name))
}

/// NOTE: We don't need to mangle anything here to tweak paths,
/// because they are only used within the target directory
/// of a single project — i.e. they don't get sent to the cache.
///
/// We don't bother storing command line arguments, because Cargo
/// doesn't provide any to build script.
#[derive(Serialize, Deserialize)]
pub struct BuildScriptInvocationInfo {
    pub real_build_script_path: PathBuf,
    pub env_vars: HashMap<String, String>,
    pub work_dir: PathBuf,
}

impl BuildScriptInvocationInfo {
    /// Get the invoked timestamp for when Cargo originally
    /// attempted to run the build script.
    ///
    /// See comments on `get_invoked_timestamp_for_crate_build_unit` for more detail.
    pub fn get_invoked_timestamp(&self) -> anyhow::Result<filetime::FileTime> {
        let out_dir = self.out_dir()?;
        let build_script_invocation_build_dir = out_dir
            .parent()
            .context("Out dir missing parent; can't find invoked timestamp for build script run")?;
        // Now read the mtime of the "invoked.timestamp" file for this build script execution unit.
        let invoked_timestamp_path = build_script_invocation_build_dir.join("invoked.timestamp");
        let invoked_timestamp_file_metadata = std::fs::metadata(invoked_timestamp_path).context(
            "Failed to get metadata for \"invoked.timestamp\" file; maybe it doesn't exist?",
        )?;
        Ok(filetime::FileTime::from_last_modification_time(
            &invoked_timestamp_file_metadata,
        ))
    }

    pub fn out_dir(&self) -> anyhow::Result<PathBuf> {
        let out_dir = self
            .env_vars
            .get("OUT_DIR")
            .context("Missing 'OUT_DIR' env var in build script invocation info")?;
        PathBuf::from_str(out_dir)
            .context("Build script invocation info 'OUT_DIR' env var contained invalid path")
    }
}
