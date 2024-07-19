//! Pretending to be a crate's build script

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    str::FromStr,
};

use anyhow::Context;

use crate::cache::{Cache, LocalCache};

pub fn run(called_as: &Path) -> anyhow::Result<()> {
    // TODO: This is copy-pasta; deduplicate it!
    let cache_dir = std::env::var("WRAPPER_HAX_CACHE_DIR")
        .context("Missing 'WRAPPER_HAX_CACHE_DIR' env var")?;
    let cache_dir =
        PathBuf::from_str(&cache_dir).context("Bad path in 'WRAPPER_HAX_CACHE_DIR' env var")?;
    if !cache_dir.exists() {
        // Only attempt to create the directory, but not any parents;
        // minimises the risk of really big oopsies.
        std::fs::create_dir(&cache_dir).context("Failed to create cargo-cache-hacks dir")?;
    }

    let out_dir = std::env::var("OUT_DIR").context("OUT_DIR env var not set")?;
    let out_dir = PathBuf::from_str(&out_dir).context("Bad path in OUT_DIR env")?;

    // TODO: Some more assertions to make sure that the dir looks right,
    // and is actually a directory, etc.!
    let crate_build_dir = out_dir
        .parent()
        .context("Out dir missing parent directory")?;
    let crate_unit_name = crate_build_dir
        .file_name()
        .context("Crate build dir missing file name")?
        .to_str()
        .context("Bad UTF-8 in crate build dir name")?;

    // Try to pull the out dir from cache.
    let cache = LocalCache::new(cache_dir);
    if cache
        .pull_build_script_out_dir(&out_dir, crate_unit_name)
        // REVISIT: Care about the specific error when pulling.
        .is_err()
    {
        // We weren't able to pull from the cache, so we need to
        // run the real build script.
        //
        // TODO: See comments where this is created about wanting to not do "real-build-script" symlink.
        let build_script_build_dir = called_as
            .parent()
            .context("Build script didn't have parent dir")?;
        let real_build_script_symlink_path = build_script_build_dir.join("real-build-script");
        let output = Command::new(&real_build_script_symlink_path)
            .args(std::env::args())
            .envs(std::env::vars())
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

        cache
            .push_build_script_out_dir(&out_dir, crate_unit_name)
            .context("Failed to push build script out dir")?;

        // Tee child process stdout and stderr to cache.
        // (We need to emit them, too, to talk to Cargo.)
        std::io::stdout().write_all(&output.stdout)?;
        cache
            .push_build_script_stdout(crate_unit_name, &output.stdout)
            .context("Failed to push build script stdout")?;
        std::io::stdout().write_all(&output.stderr)?;
        cache
            .push_build_script_stderr(crate_unit_name, &output.stderr)
            .context("Failed to push build script stderr")?;
    } else {
        let stdout = cache
            .pull_build_script_stdout(crate_unit_name)
            .context("Failed to pull build script stdout")?;
        std::io::stdout().write_all(&stdout)?;
        let stderr = cache
            .pull_build_script_stderr(crate_unit_name)
            .context("Failed to pull build script stderr")?;
        std::io::stderr().write_all(&stderr)?;
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
