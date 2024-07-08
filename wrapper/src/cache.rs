use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::Context;

use crate::{CrateType, OutputType};

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
        anyhow::bail!("TODO; returning an error here should allow the old impl to keep going");

        // TODO: After you get _push_ working, then you can test pull.
        // So maybe go and do that first!
        // todo!()

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
        let mut file_names = vec![];

        for output_type in output_types {
            match output_type {
                OutputType::Asm => file_names.push(format!("{crate_unit_name}.s")),
                OutputType::LlvmBc => file_names.push(format!("{crate_unit_name}.bc")),
                OutputType::LlvmIr => file_names.push(format!("{crate_unit_name}.ll")),
                OutputType::Obj => file_names.push(format!("{crate_unit_name}.o")),
                OutputType::Metadata => file_names.push(format!("lib{crate_unit_name}.rmeta")),
                OutputType::Link => {
                    // TODO: This should depend on platform for many of these types!
                    for crate_type in crate_types {
                        match crate_type {
                            // Assume lib is rlib for now, but that is not necessarily going
                            // to be true forever.
                            CrateType::Lib => file_names.push(format!("lib{crate_unit_name}.rlib")),
                            CrateType::Rlib => {
                                file_names.push(format!("lib{crate_unit_name}.rlib"))
                            }
                            CrateType::Staticlib => todo!(),
                            CrateType::Dylib => todo!(),
                            CrateType::Cdylib => todo!(),
                            CrateType::Bin => file_names.push(crate_unit_name.to_owned()),
                            CrateType::ProcMacro => todo!(),
                        }
                    }
                }
                OutputType::DepInfo => file_names.push(format!("{crate_unit_name}.d")),
                OutputType::Mir => file_names.push(format!("{crate_unit_name}.mir")),
            };
        }

        for file_name in file_names {
            let from_path = out_dir.join(&file_name);
            let to_path = self.root.join(&file_name);
            // Copy it to the cache dir.
            std::fs::copy(from_path, to_path)
                .with_context(|| format!("Failed to copy file {file_name:?} to cache."))?;
        }

        Ok(())
    }
}
