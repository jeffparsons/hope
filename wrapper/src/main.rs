mod cache;

use std::collections::HashSet;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::{fs::File, process::Command, str::FromStr};

use anyhow::Context;
use cache::{Cache, LocalCache};
use clap::Parser;
use serde::{Deserialize, Serialize};

// TODO: I don't like this. I'd instead like to be able to collect
// the flags and kv-pairs into a custom collection.
#[derive(Clone, Debug, PartialEq, Eq)]
enum FlagOrKvPair {
    Flag(String),
    KvPair(KeyValuePair),
}

impl FromStr for FlagOrKvPair {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some((key, value)) = s.split_once('=') {
            Ok(Self::KvPair(KeyValuePair {
                key: key.to_owned(),
                value: value.to_owned(),
            }))
        } else {
            Ok(Self::Flag(s.to_owned()))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct KeyValuePair {
    key: String,
    value: String,
}

// Arguments here mirror the real `rustc` arguments.
// I'm just using Clap to make it easier to inspect/modify the ones I care about.
#[derive(Parser, Debug)]
#[command(disable_version_flag = true, disable_help_flag = true)]
struct Args {
    // Not required if, e.g., passing `--version`.
    input: Option<String>,
    #[arg(long, value_delimiter = ',')]
    cfg: Vec<String>,
    #[arg(short = 'L', value_delimiter = ',')]
    lib_search_paths: Vec<String>,
    #[arg(short = 'l', value_delimiter = ',')]
    link_to_native_libs: Vec<String>,
    #[arg(long = "crate-type")]
    crate_types: Vec<String>,
    #[arg(long)]
    crate_name: Option<String>,
    #[arg(long)]
    edition: Option<String>,
    #[arg(long, value_delimiter = ',')]
    emit: Vec<String>,
    #[arg(long, value_delimiter = ',')]
    print: Vec<String>,
    #[arg(short = 'g')]
    include_debug_info: bool,
    #[arg(short = 'O')]
    optimize: bool,
    #[arg(short = 'o')]
    out: Option<String>,
    #[arg(long)]
    out_dir: Option<String>,
    #[arg(long)]
    explain: bool,
    #[arg(long)]
    test: bool,
    #[arg(long = "warn", short = 'W', value_delimiter = ',')]
    warn_for_lints: Vec<String>,
    #[arg(long = "force-warn", value_delimiter = ',')]
    force_warn_for_lints: Vec<String>,
    #[arg(long = "allow", short = 'A', value_delimiter = ',')]
    allow_lints: Vec<String>,
    #[arg(long = "deny", short = 'D', value_delimiter = ',')]
    deny_lints: Vec<String>,
    #[arg(long = "forbid", short = 'F', value_delimiter = ',')]
    forbid_lints: Vec<String>,
    #[arg(short = 'Z', value_delimiter = ',')]
    unstable_options: Vec<String>,
    #[arg(long)]
    cap_lints: Option<String>,
    #[arg(short = 'C', long = "codegen", value_delimiter = ',')]
    codegen_options: Vec<FlagOrKvPair>,
    #[arg(short = 'V', long)]
    version: bool,
    #[arg(short, long)]
    verbose: bool,
    #[arg(long = "extern", value_delimiter = ',')]
    extern_: Vec<String>,
    #[arg(long)]
    sysroot: Option<String>,
    #[arg(long)]
    error_format: Option<String>,
    #[arg(long)]
    color: Option<String>,
    #[arg(long)]
    diagnostic_width: Option<u32>,
    #[arg(long = "remap-path-prefix", value_delimiter = ',')]
    remap_path_prefixes: Vec<String>,
    #[arg(long, value_delimiter = ',')]
    json: Vec<String>,
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args();

    let mut args_to_parse: Vec<String> = Vec::new();

    args_to_parse.push(
        args.next()
            .context("Missing argument for path to this executable")?,
    );

    let rustc_path = args
        .next()
        .context("Missing argument for real `rustc` path")?;
    let rustc_path =
        PathBuf::from_str(&rustc_path).context("Invalid path in rustc path argument")?;

    // REVISIT: If I want to start _modifying_ arguments eventually,
    // then I'll need to reconstruct the arg vector from our parsed arguments.
    let pass_through_args: Vec<String> = args.collect();
    args_to_parse.extend(pass_through_args.iter().cloned());

    let args = Args::parse_from(args_to_parse);

    let Some(input_path) = &args.input else {
        // No input path; we're not actually building anything.
        return run_real_rustc(&rustc_path, pass_through_args);
    };
    let input_path =
        PathBuf::from_str(input_path).context("Invalid path in input path argument")?;

    if !input_path.components().any(|component| {
        component
            .as_os_str()
            .as_bytes()
            .starts_with(b"index.crates.io-")
    }) {
        // This doesn't look like a crate from crates.io;
        // don't try to interact with the cache.
        return run_real_rustc(&rustc_path, pass_through_args);
    }

    let cache_dir = std::env::var("WRAPPER_HAX_CACHE_DIR")
        .context("Missing 'WRAPPER_HAX_CACHE_DIR' env var")?;
    let cache_dir =
        PathBuf::from_str(&cache_dir).context("Bad path in 'WRAPPER_HAX_CACHE_DIR' env var")?;
    if !cache_dir.exists() {
        // Only attempt to create the directory, but not any parents;
        // minimises the risk of really big oopsies.
        std::fs::create_dir(&cache_dir).context("Failed to create cargo-cache-hacks dir")?;
    }

    let crate_name = args
        .crate_name
        .clone()
        .context("Missing crate name argument")?;
    let extra_filename = args
        .codegen_options
        .iter()
        .filter_map(|codegen_option| {
            if let FlagOrKvPair::KvPair(kv_pair) = codegen_option {
                Some(kv_pair)
            } else {
                None
            }
        })
        .find(|kv_pair| kv_pair.key == "extra-filename")
        .context("Missing extra-filename codegen option")?
        .value
        .clone();

    let crate_unit_name = format!("{crate_name}{extra_filename}");

    let mut crate_types = HashSet::new();
    for crate_type_str in &args.crate_types {
        let crate_type = CrateType::from_str(crate_type_str)
            .context("Found unexpected output type in '--crate-type' argument")?;
        crate_types.insert(crate_type);
    }

    let mut output_types = HashSet::new();
    for output_type_str in &args.emit {
        let output_type = OutputType::from_str(output_type_str)
            .context("Found unexpected output type in '--emit' argument")?;
        output_types.insert(output_type);
    }

    let out_dir = args
        .out_dir
        .context("Missing out-dir; don't know where build artifacts are supposed to be")?;
    let out_dir = PathBuf::from_str(&out_dir).context("Invalid path in out-dir argument")?;

    // Try to pull from the cache.
    let cache = LocalCache::new(cache_dir);
    if cache
        .pull(&out_dir, &crate_unit_name, &crate_types, &output_types)
        .is_ok()
    {
        // We got it from cache; we're done!
        return Ok(());
    }
    // REVISIT: Care about the specific error.

    // See if we can use copy cached output instead of calling rustc ourselves.
    // TODO: This is all hacks. We should be properly handling any combination of "--emit" options.
    //
    // TODO: We should actually be checking that we can provide _all_
    // of the output kinds that it wants, and passing through any
    // that we can't provide to the real rustc. Or something like that?
    // if args.emit.iter().any(|emit| emit == "link") {
    //     dbg!(&args);
    //     // panic!("wheee");

    //     'tryadopt: {
    //         // TODO: Lots of this is copypasta. Do a big ol' refactor.

    //         let out_dir = args
    //             .out_dir
    //             .clone()
    //             .context("Missing out-dir; don't know how to find build artifacts")?;
    //         let out_dir_pb =
    //             PathBuf::from_str(&out_dir).context("Invalid path in out-dir argument")?;

    //         let crate_name = args
    //             .crate_name
    //             .clone()
    //             .context("Missing crate name argument")?;
    //         let extra_filename = args
    //             .codegen_options
    //             .iter()
    //             .filter_map(|codegen_option| {
    //                 if let FlagOrKvPair::KvPair(kv_pair) = codegen_option {
    //                     Some(kv_pair)
    //                 } else {
    //                     None
    //                 }
    //             })
    //             .find(|kv_pair| kv_pair.key == "extra-filename")
    //             .context("Missing extra-filename codegen option")?
    //             .value
    //             .clone();

    //         let crate_unit_name = format!("{crate_name}{extra_filename}");

    //         let wrapper_log_file_name = format!("{crate_unit_name}.wrapper-log");
    //         let wrapper_log_path = out_dir_pb.join(wrapper_log_file_name);
    //         let mut log_file = File::create(wrapper_log_path).context("Failed to open log file")?;

    //         let build_manifests_dir = Path::new("/tmp/build-manifests");
    //         if !build_manifests_dir.exists() {
    //             break 'tryadopt;
    //         }
    //         let manifest_file_name = format!("{crate_unit_name}.manifest.json");
    //         let manifest_file_path = build_manifests_dir.join(manifest_file_name);
    //         if !manifest_file_path.exists() {
    //             break 'tryadopt;
    //         }
    //         let build_manifest_json = std::fs::read_to_string(manifest_file_path)
    //             .context("Failed to load build manifest")?;
    //         let build_manifest: BuildManifest = serde_json::from_str(&build_manifest_json)
    //             .context("Failed to parse build manifest JSON file")?;

    //         // TODO: This is a bit dodgy. We can't actually assume this is true.
    //         let mut target_dir = out_dir_pb.clone();
    //         while !target_dir.ends_with("target") {
    //             target_dir.pop();
    //         }
    //         let target_dir_str = target_dir
    //             .as_os_str()
    //             .to_str()
    //             .context("Bad UTF-8 in target dir")?;

    //         // Looks like we can use it! Copy the output files into our own build dir.
    //         // First check that all the source files exist before we even start trying
    //         // to copy anything.
    //         for out_file_path in &build_manifest.out_file_paths {
    //             let out_file_pb =
    //                 PathBuf::from_str(out_file_path).context("Failed to parse out file path")?;
    //             if !out_file_pb.exists() {
    //                 writeln!(log_file, "Missing file {out_file_path:?}")?;

    //                 break 'tryadopt;
    //             }
    //         }
    //         for out_file_path in build_manifest.out_file_paths {
    //             let out_file_pb =
    //                 PathBuf::from_str(&out_file_path).context("Failed to parse out file path")?;
    //             let dest_path =
    //                 out_dir_pb.join(out_file_pb.file_name().context("Bad out file name")?);
    //             let dest_dir = dest_path
    //                 .parent()
    //                 .context("Failed to pop to parent dir of dest path")?;
    //             if !dest_dir.exists() {
    //                 std::fs::create_dir_all(dest_dir).context("Failed to create dest dir")?;
    //             }

    //             writeln!(log_file, "Copying file {out_file_path:?} to {dest_path:?}")?;

    //             std::fs::copy(out_file_pb, &dest_path)
    //                 .with_context(|| format!("Failed to copy out file {out_file_path:?} to {dest_path:?}; do source file and dest dir exist?"))?;
    //         }

    //         return Ok(());
    //     }
    // }

    // DEBUG
    // dbg!(&pass_through_args);

    // We weren't able to pull from cache, so we have to ask the real rustc to build it.
    run_real_rustc(&rustc_path, pass_through_args)?;

    // Attempt to push the result to cache.
    cache
        .push(&out_dir, &crate_unit_name, &crate_types, &output_types)
        .context("Failed to push to cache")?;

    Ok(())
}

fn run_real_rustc(rustc_path: &Path, pass_through_args: Vec<String>) -> anyhow::Result<()> {
    let status = Command::new(rustc_path)
        .args(pass_through_args)
        .status()
        .context("Failed to start real `rustc`")?;
    if !status.success() {
        std::process::exit(
            status
                .code()
                .context("Child `rustc` process was terminated by a signal")?,
        );
    }
    Ok(())
}

/// Different types of crates that `rustc` can compile.
///
/// These are selected with the `--crate-type` argument.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum CrateType {
    // Assumed to be the same as rlib for now. But that's not guaranteed!
    Lib,
    Rlib,
    Staticlib,
    Dylib,
    Cdylib,
    Bin,
    ProcMacro,
}

impl FromStr for CrateType {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "lib" => Ok(Self::Lib),
            "rlib" => Ok(Self::Rlib),
            "staticlib" => Ok(Self::Staticlib),
            "dylib" => Ok(Self::Dylib),
            "cdylib" => Ok(Self::Cdylib),
            "bin" => Ok(Self::Bin),
            "proc-macro" => Ok(Self::ProcMacro),
            _ => anyhow::bail!("Unrecognised crate type \"{s}\""),
        }
    }
}

/// Different types of outputs created by `rustc`.
///
/// These are selected with the `--emit` argument.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum OutputType {
    Asm,
    LlvmBc,
    LlvmIr,
    Obj,
    Metadata,
    Link,
    DepInfo,
    Mir,
}

impl FromStr for OutputType {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "asm" => Ok(Self::Asm),
            "llvm-bc" => Ok(Self::LlvmBc),
            "llvm-ir" => Ok(Self::LlvmIr),
            "obj" => Ok(Self::Obj),
            "metadata" => Ok(Self::Metadata),
            "link" => Ok(Self::Link),
            "dep-info" => Ok(Self::DepInfo),
            "mir" => Ok(Self::Mir),
            _ => anyhow::bail!("Unrecognised output type \"{s}\""),
        }
    }
}

/// Output type with crate type for the `Link` output type.
///
/// This is enough information to generate an output file name
/// given a base name.
#[derive(Debug, PartialEq, Eq)]
enum OutputDefn {
    Asm,
    LlvmBc,
    LlvmIr,
    Obj,
    Metadata,
    Link(CrateType),
    DepInfo,
    Mir,
}

impl OutputDefn {
    fn file_name(&self, crate_unit_name: &str) -> String {
        match self {
            Self::Asm => format!("{crate_unit_name}.s"),
            Self::LlvmBc => format!("{crate_unit_name}.bc"),
            Self::LlvmIr => format!("{crate_unit_name}.ll"),
            Self::Obj => format!("{crate_unit_name}.o"),
            Self::Metadata => format!("lib{crate_unit_name}.rmeta"),
            Self::Link(crate_type) => {
                // TODO: This should depend on platform for many of these types!
                match crate_type {
                    // Assume lib is rlib for now, but that is not necessarily going
                    // to be true forever.
                    CrateType::Lib => format!("lib{crate_unit_name}.rlib"),
                    CrateType::Rlib => format!("lib{crate_unit_name}.rlib"),
                    CrateType::Staticlib => todo!(),
                    CrateType::Dylib => todo!(),
                    CrateType::Cdylib => todo!(),
                    CrateType::Bin => crate_unit_name.to_owned(),
                    CrateType::ProcMacro => todo!(),
                }
            }
            // TODO: This will need to be modified on push/pull to stop cargo from getting
            // confused and constantly trying to rebuild the crate.
            //
            // TODO: Also need tests to make sure that whatever you do here actually works!
            Self::DepInfo => format!("{crate_unit_name}.d"),
            Self::Mir => format!("{crate_unit_name}.mir"),
        }
    }
}

/// Return a list of all the outputs we should be creating,
/// based on the '--emit' and '--crate-type' flags.
fn output_defns(
    crate_types: &HashSet<CrateType>,
    output_types: &HashSet<OutputType>,
) -> Vec<OutputDefn> {
    let mut output_defns = vec![];
    for output_type in output_types {
        match output_type {
            OutputType::Asm => output_defns.push(OutputDefn::Asm),
            OutputType::LlvmBc => output_defns.push(OutputDefn::LlvmBc),
            OutputType::LlvmIr => output_defns.push(OutputDefn::LlvmIr),
            OutputType::Obj => output_defns.push(OutputDefn::Obj),
            OutputType::Metadata => output_defns.push(OutputDefn::Metadata),
            OutputType::Link => {
                for crate_type in crate_types {
                    match crate_type {
                        CrateType::Lib => output_defns.push(OutputDefn::Link(CrateType::Lib)),
                        CrateType::Rlib => output_defns.push(OutputDefn::Link(CrateType::Rlib)),
                        CrateType::Staticlib => {
                            output_defns.push(OutputDefn::Link(CrateType::Staticlib))
                        }
                        CrateType::Dylib => output_defns.push(OutputDefn::Link(CrateType::Dylib)),
                        CrateType::Cdylib => output_defns.push(OutputDefn::Link(CrateType::Cdylib)),
                        CrateType::Bin => output_defns.push(OutputDefn::Link(CrateType::Bin)),
                        CrateType::ProcMacro => {
                            output_defns.push(OutputDefn::Link(CrateType::ProcMacro))
                        }
                    }
                }
            }
            OutputType::DepInfo => output_defns.push(OutputDefn::DepInfo),
            OutputType::Mir => output_defns.push(OutputDefn::Mir),
        }
    }
    output_defns
}
