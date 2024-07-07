use std::path::PathBuf;

pub trait Cache {
    fn pull(&self, crate_unit_name: &str) -> anyhow::Result<()>;
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
    fn pull(&self, crate_unit_name: &str) -> anyhow::Result<()> {
        anyhow::bail!("TODO; returning an error here should allow the old impl to keep going");
        // todo!()

        // TODO: We will need to rewrite dep info to fix up absolute paths.
        // NOTE: `--remap-path-prefix` is not adequate for this; we actually
        // need to modify the output files. But maybe we should _also_ remap path
        // prefixes when we might write to the cache so that we don't produce
        // subtly misleading output (different paths on different people's machines).
    }
}
