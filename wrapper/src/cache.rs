use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::Context;
use cache_log::{write_log_line, CacheLogLine, PullEvent, PushEvent};
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
    fn pull(
        &self,
        out_dir: &Path,
        crate_unit_name: &str,
        crate_types: &HashSet<CrateType>,
        output_types: &HashSet<OutputType>,
    ) -> anyhow::Result<()>;
    fn push(
        &self,
        out_dir: &Path,
        crate_unit_name: &str,
        crate_types: &HashSet<CrateType>,
        output_types: &HashSet<OutputType>,
    ) -> anyhow::Result<()>;
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
    fn pull(
        &self,
        out_dir: &Path,
        crate_unit_name: &str,
        crate_types: &HashSet<CrateType>,
        output_types: &HashSet<OutputType>,
    ) -> anyhow::Result<()> {
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
            CacheLogLine::Pulled(PullEvent {
                crate_unit_name: crate_unit_name.to_owned(),
                copied_at: Utc::now(),
                copied_from: "local cache".to_string(),
                did_mangle_on_pull: false,
            }),
        )?;

        Ok(())

        // TODO: We will need to rewrite dep info to fix up absolute paths.
        // NOTE: `--remap-path-prefix` is not adequate for this; we actually
        // need to modify the output files. But maybe we should _also_ remap path
        // prefixes when we might write to the cache so that we don't produce
        // subtly misleading output (different paths on different people's machines).
    }

    fn push(
        &self,
        out_dir: &Path,
        crate_unit_name: &str,
        crate_types: &HashSet<CrateType>,
        output_types: &HashSet<OutputType>,
    ) -> anyhow::Result<()> {
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
            CacheLogLine::Pushed(PushEvent {
                crate_unit_name: crate_unit_name.to_owned(),
                copied_at: Utc::now(),
                copied_from: "local cache".to_string(),
                did_mangle_on_push: false,
            }),
        )?;

        Ok(())
    }
}
