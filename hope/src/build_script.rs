//! Pretending to be a crate's build script

use std::{
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    str::FromStr,
};

use anyhow::Context;
use cache_log::{write_log_line, BuildScriptRunEvent, CacheLogLine};
use chrono::Utc;

use crate::cache::{Cache, LocalCache};

pub const BUILD_SCRIPT_CRATE_METADATA_HASH_FILE_NAME: &str =
    "hope-build-script-crate-metadata-hash";

pub fn run(called_as: &Path) -> anyhow::Result<()> {
    let build_script_build_dir = called_as
        .parent()
        .context("Build script didn't have parent dir")?;
    let (_, build_script_crate_metadata_hash) = build_script_build_dir
        .to_str()
        .context("Bad UTF-8 in build dir")?
        .rsplit_once('-')
        .with_context(|| {
            format!(
                "Build script build dir {:?} had unexpected format",
                build_script_build_dir
            )
        })?;

    // TODO: See comments where this is created about wanting to not do "real-build-script" symlink.
    let real_build_script_symlink_path = build_script_build_dir.join("real-build-script");
    if real_build_script_symlink_path.exists() {
        // We previously created a symlink to the real build script,
        // which we only do if we intend to run it.
        //
        // So... run the real build script.
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

        let cache_dir =
            LocalCache::dir_from_env().context("Failed to get local cache dir from environment")?;
        write_log_line(
            &cache_dir,
            CacheLogLine::RanBuildScript(BuildScriptRunEvent { ran_at: Utc::now() }),
        )?;

        // Forward child process stdout and stderr.
        // (We need to emit them, to instruct Cargo what to do for the main crate.)
        std::io::stdout().write_all(&output.stdout)?;
        std::io::stdout().write_all(&output.stderr)?;

        // And finally we need to leave a reference to the build script crate's
        // metadata hash so that we can later detect that there's no need to build it!
        // This will get pushed to the cache after the main crate build.
        let out_dir = std::env::var("OUT_DIR").context("OUT_DIR env var not set")?;
        let out_dir = PathBuf::from_str(&out_dir).context("Bad path in OUT_DIR env")?;
        let build_dir = out_dir
            .parent()
            .context("Out dir missing parent directory")?;

        let mut build_script_crate_metadata_hash_file =
            File::create_new(build_dir.join(BUILD_SCRIPT_CRATE_METADATA_HASH_FILE_NAME))
                .context("Failed to create file for build script crate metadata hash")?;
        build_script_crate_metadata_hash_file
            .write_all(build_script_crate_metadata_hash.as_bytes())?;

        return Ok(());
    }

    // Try to get the real build script's stdout from cache.
    //
    // We have already verified at this point that we can find it,
    // and have committed to _not_ building anything ourselves,
    // so fail catastrophically if we can't get it.
    let cache = LocalCache::from_env()?;
    let build_script_stdout_bytes = cache
        .get_build_script_stdout_by_build_script_crate_metadata_hash(
            build_script_crate_metadata_hash,
        )
        .context("Failed to get build script stdout from cache")?;
    let build_script_stdout = String::from_utf8(build_script_stdout_bytes)
        .context("Build script output contained bad UTF-8")?;

    // Print what we found to stdout to make Cargo invoke rustc with
    // the right arguments for the real build. (Most of them don't matter,
    // but some things get a bit wonky if we don't emit the same thing
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

        println!("{}", line);
    }

    // Don't bother printing the real stderr; it isn't used by Cargo.
    // Instead, print something to help people if they end up debugging
    // problems caused by Hope — just to hint at what's going on.
    eprintln!("Fake build script by Hope; real build script not run because we intend to pull the main crate output from cache.");

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
