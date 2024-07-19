mod build_script;
mod cache;

use std::collections::HashSet;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Instant;
use std::{process::Command, str::FromStr};

use anyhow::Context;
use build_script::append_moved_build_script_suffix;
use cache::{Cache, LocalCache};
use clap::Parser;

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
    let mut args = std::env::args().peekable();

    let mut args_to_parse: Vec<String> = Vec::new();

    let called_as = args
        .next()
        .context("Missing argument for path to this executable")?;

    // TODO: Non-hack way to get this! :P
    if called_as.contains("/build/") && args.peek().is_none() {
        // Looks like we're being run as a build script, because we moved
        // the actual build script out of the way and replaced it with a symlink
        // to this binary.
        let called_as = PathBuf::from_str(&called_as).context("Bad path in argv[0]")?;
        return build_script::run(&called_as);
    }

    args_to_parse.push(called_as);

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
    let cache = LocalCache::from_env()?;
    if cache
        .pull_crate_outputs(&out_dir, &crate_unit_name, &crate_types, &output_types)
        // REVISIT: Care about the specific error when pulling.
        .is_err()
    {
        // We weren't able to pull from cache, so we have to ask the real rustc to build it.
        run_real_rustc(&rustc_path, pass_through_args)?;

        // Attempt to push the result to cache.
        cache
            .push_crate_outputs(&out_dir, &crate_unit_name, &crate_types, &output_types)
            .context("Failed to push to cache")?;
    }

    // TODO: Better way of checking this. ;)
    if out_dir.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .expect("Bad string in out dir component")
            == "build"
    }) {
        // This looks like a build script.
        //
        // Replace it with a symlink to this binary so that we can
        // attempt to pull the build script's output from cache, too.
        //
        // REVISIT: In future it would be nice to avoid running build
        // scripts or pulling their output altogether if we're not
        // actually going to build the main crate. But that's a little
        // bit fiddly because of build script outputs
        // (https://doc.rust-lang.org/cargo/reference/build-scripts.html#outputs-of-the-build-script)
        // so we'll come back to that later.
        //
        // TODO: Cargo seems to copy, e.g., "build_script_main" to
        // "build-script-main" and run it from there. I'm just replacing
        // the former right now (on the assumption that what I replace it
        // with will get copied just fine) but I should probably understand why
        // both exist.
        //
        // TODO: Apply binary extension here if relevant.
        let build_script_path = out_dir.join(&crate_unit_name);
        let moved_build_script_path = append_moved_build_script_suffix(&build_script_path)
            .context("Failed to append moved build script path suffix")?;
        std::fs::rename(&build_script_path, &moved_build_script_path)
            .context("Failed to move build script out of the way")?;

        // Replace it with a copy of this binary;
        // we will masquerade as a build script. Muahahaha.
        //
        // NOTE: We do not use a symlink here because otherwise Cargo
        // will copy the _target_ of the symlink, which results in the
        // mtime being older than the build attempt. This causes spurious rebuilds.
        let current_exe = std::env::current_exe().context("Failed to get path to current exe")?;
        std::fs::copy(current_exe, &build_script_path)
            .context("Failed to replace build script with a copy of 'hope'")?;
        // Bump the copy's mtime. In my testing on macOS,
        // this seems to be necessary or the old mtime gets copied across
        // causing spurious rebuilds!
        filetime::set_file_mtime(&build_script_path, filetime::FileTime::now())
            .with_context(|| format!("Failed to update mtime for {build_script_path:?}."))?;

        // Make a symlink to the real buildscript,
        // with a predictable name.
        //
        // TODO: I'd prefer to not have to do this, but I'm not sure
        // how to accurately infer the name from the kebab-case "build-script-build"
        // that we get called as.

        let real_build_script_symlink_path = out_dir.join("real-build-script");
        std::os::unix::fs::symlink(moved_build_script_path, real_build_script_symlink_path)
            .context("Failed to create symlink to the real build script")?;
    }

    Ok(())
}

fn run_real_rustc(rustc_path: &Path, pass_through_args: Vec<String>) -> anyhow::Result<()> {
    let before = Instant::now();
    // dbg!(&pass_through_args[0..usize::min(pass_through_args.len(), 3)]);

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

    // DEBUG: TODO: Put behind a verbose flag or something.
    // Or just put it in the structured log.
    // eprintln!("Real rustc took: {}", before.elapsed().as_secs_f32());
    let _ = before;

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
                    #[cfg(target_os = "linux")]
                    CrateType::ProcMacro => format!("lib{crate_unit_name}.so"),
                    #[cfg(target_os = "macos")]
                    CrateType::ProcMacro => format!("lib{crate_unit_name}.dylib"),
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
