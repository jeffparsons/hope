mod build_script;
mod cache;

use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Instant;
use std::{process::Command, str::FromStr};

use anyhow::Context;
use build_script::{append_moved_build_script_suffix, BUILD_SCRIPT_CRATE_METADATA_HASH_FILE_NAME};
use cache::{build_script_stdout_file_name, Cache, LocalCache};
use clap::Parser;
use tempfile::tempdir;

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

    let out_dir = args
        .out_dir
        .context("Missing out-dir; don't know where build artifacts are supposed to be")?;
    let out_dir = PathBuf::from_str(&out_dir).context("Invalid path in out-dir argument")?;

    let crate_name = args
        .crate_name
        .clone()
        .context("Missing crate name argument")?;
    let metadata_hash = args
        .codegen_options
        .iter()
        .filter_map(|codegen_option| {
            if let FlagOrKvPair::KvPair(kv_pair) = codegen_option {
                Some(kv_pair)
            } else {
                None
            }
        })
        .find(|kv_pair| kv_pair.key == "metadata")
        .context("Missing metadata codegen option")?
        .value
        .clone();
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

    let cache = LocalCache::from_env()?;

    if out_dir.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .expect("Bad string in out dir component")
            == "build"
    }) {
        // This looks like a build script.
        //
        // If the cache contains a copy of the main crate, then we don't
        // want to waste time building _or_ running the build script.
        //
        // If we _do_ have to build the main crate, then we will need a
        // way to relate the build script crate metadata hash to the main
        // crate.
        //
        // So, either way, we will put a copy of _this_ binary in place
        // of the real build script, and if we had to build the real
        // build script we will first move it out of the way and create
        // a symlink to it so we can run it from within our wrapper.

        // TODO: Cargo seems to copy, e.g., "build_script_main" to
        // "build-script-main" and run it from there. I'm just replacing
        // the former right now (on the assumption that what I replace it
        // with will get copied just fine) but I should probably understand why
        // both exist.
        //
        // TODO: Apply binary extension here if relevant.
        let build_script_path = out_dir.join(&crate_unit_name);

        if cache
            .get_build_script_stdout_by_build_script_crate_metadata_hash(&metadata_hash)
            .is_err()
        {
            // TODO: Care about the specific error! This is actually super important
            // because we need to make sure the build the real build script if
            // we might end up later needing to build the main crate!

            // Build the build script for real.
            run_real_rustc(&rustc_path, pass_through_args.clone())?;

            // Now move it out of the way so that we can put a copy
            // of this exe there as a wrapper.
            let moved_build_script_path = append_moved_build_script_suffix(&build_script_path)
                .context("Failed to append moved build script path suffix")?;
            std::fs::rename(&build_script_path, &moved_build_script_path)
                .context("Failed to move build script out of the way")?;

            // Make a symlink to the real buildscript,
            // with a predictable name.
            //
            // NOTE: We _must_ only do this if we intend to run it,
            // because right now we use its existence to decide whether
            // to run the real build script or attempt to pull from cache.
            //
            // TODO: I'd prefer to not have to do this, but I'm not sure
            // how to accurately infer the name from the kebab-case "build-script-build"
            // that we get called as.
            let real_build_script_symlink_path = out_dir.join("real-build-script");
            std::os::unix::fs::symlink(moved_build_script_path, real_build_script_symlink_path)
                .context("Failed to create symlink to the real build script")?;
        }

        // Now we unconditionally make a copy of this exe in place
        // of the build script.
        //
        // NOTE: We do not use a symlink here because otherwise Cargo
        // will copy the _target_ of the symlink, which results in the
        // mtime being older than the build attempt. This causes spurious rebuilds.
        let current_exe = std::env::current_exe().context("Failed to get path to current exe")?;
        std::fs::copy(current_exe, &build_script_path)
            .context("Failed to copy 'hope' binary to where build script would have been built")?;
        // Bump the copy's mtime. In my testing on macOS,
        // this seems to be necessary or the old mtime gets copied across
        // causing spurious rebuilds!
        filetime::set_file_mtime(&build_script_path, filetime::FileTime::now())
            .with_context(|| format!("Failed to update mtime for {build_script_path:?}."))?;

        // Make a bogus '.d' file for the build script.
        // I think this is needed for Cargo to create its fingerprint
        // files and avoid having to recompute a bunch of stuff
        // from scratch when deciding whether to build crates again.
        let dot_d_path = build_script_path.with_extension("d");
        // let mut dot_d_file =
        //     File::create_new(dot_d_path).context("Failed to create fake '.d' file")?;
        // TEMP: Allow it to overwrite the file while I'm testing getting the other stuff right.
        // (I don't want it to fail on this step all the time right now,
        // and I'm not even convinced that I don't need to allow this...)
        let mut dot_d_file = File::create(dot_d_path).context("Failed to create fake '.d' file")?;
        let build_script_path_str = build_script_path
            .to_str()
            .context("Bad UTF-8 in build script path")?;
        dot_d_file.write_all(format!("{build_script_path_str}:").as_bytes())?;

        // Whether we ran rustc or made a fake build script,
        // we don't want to cache anything. So we're done!
        return Ok(());
    }

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

    let output_defns = output_defns(&crate_types, &output_types);

    // Try to pull from the cache.
    // We first pull into a temporary directory, attempt to make any changes
    // we need to the pulled files, and then copy them into the target directory.
    // (This is partly to help with testing, and partly to make it more obvious
    // what need cleaning up if there are failures.)
    let arrival_dir = tempdir()
        .with_context(|| format!("Failed to create arrival dir for crate {crate_unit_name}."))?;
    match cache.pull_crate(&crate_unit_name, &output_defns, arrival_dir.path()) {
        Ok(_) => {
            // Modify files in the arrival dir, and then copy them over to the target dir.
            //
            // TODO: If anything in here fails, then try to clean up any files
            // that we already copied across.
            for output_defn in &output_defns {
                let file_name = output_defn.file_name(&crate_unit_name);
                let arrival_path = arrival_dir.path().join(&file_name);

                // Bump the staging copy's mtime. In my testing on macOS,
                // this seems to be necessary or the old mtime gets copied across
                // from local cache causing spurious rebuilds!
                filetime::set_file_mtime(&arrival_path, filetime::FileTime::now()).with_context(
                    || format!("Failed to update mtime for arrival file {file_name:?}."),
                )?;

                if *output_defn == OutputDefn::DepInfo {
                    // We want to remove most stuff from dep info files because the
                    // relevant files won't actually exist!
                    let dep_info_text = std::fs::read_to_string(&arrival_path)
                        .context("Failed to read received dep info file")?;
                    let mut file = File::create(&arrival_path)?;
                    for line in dep_info_text.lines() {
                        let line = line.trim();
                        if line.is_empty() || line.starts_with('#') {
                            // Write it out unmodified.
                            writeln!(file, "{}", line)?;
                            continue;
                        }

                        // TODO: Handle escaped spaces etc. in file names!
                        let (left_side, rest) = line
                            .split_once(':')
                            .with_context(|| format!("Couldn't find ':' in line: {line}"))?;

                        // TODO: Proper way to determine that it's in the build dir!
                        // We should have enough information in context,
                        // but we're not doing the absolute path replacement yet
                        // so I'm just going with this dirty hack for right now.
                        if left_side.contains("/build/") {
                            // Skip the whole line.
                            continue;
                        } else {
                            write!(file, "{left_side}:")?;
                        }

                        // There will be a space after the ':' if there are actually any deps.
                        //
                        // TODO: Handle escaped spaces etc. in file names!
                        let deps = rest
                            .trim()
                            .split(' ')
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned);

                        for dep in deps {
                            // TODO: Proper way to determine that it's in the build dir!
                            // We should have enough information in context,
                            // but we're not doing the absolute path replacement yet
                            // so I'm just going with this dirty hack for right now.
                            if !dep.contains("/build/") {
                                // It's not in the build dir, so we can depend on it
                                // without it causing Cargo to constantly rebuild.

                                // TODO: Handle re-escaping here, if we end up dealing
                                // with an unescaped value here.
                                // (I should probably split this out as a module again
                                // and actually parse the file properly.)
                                write!(file, " {dep}")?;
                            }
                        }

                        // Finish the line.
                        writeln!(file)?;
                    }

                    // TODO: Also replace placeholder paths with the relevant absolute paths
                    // for our target dir.
                }

                let path_in_out_dir = out_dir.join(&file_name);
                std::fs::copy(arrival_path, &path_in_out_dir).with_context(|| {
                    format!("Failed to copy file {file_name:?} from arrival directory to target directory.")
                })?;
            }
        }
        Err(_) => {
            // TODO: We should care about the specific error when pulling!

            // We weren't able to pull from cache, so we have to ask the real rustc to build it.
            run_real_rustc(&rustc_path, pass_through_args)?;

            // Attempt to push the result to cache, via departure dir.
            let departure_dir = tempdir().with_context(|| {
                format!("Failed to create departure dir for crate {crate_unit_name}.")
            })?;

            for output_defn in &output_defns {
                let file_name = output_defn.file_name(&crate_unit_name);
                let path_in_out_dir = out_dir.join(&file_name);
                let departure_path = departure_dir.path().join(&file_name);

                // TODO: Replace absolute paths in '.d' files with a placeholder that we can then
                // replace again when pulling.

                std::fs::copy(path_in_out_dir, departure_path).with_context(|| {
                    format!("Failed to copy file {file_name:?} from target directory to departure directory.")
                })?;
            }

            // Also copy the build script stdout if there was any.
            let maybe_build_script_crate_metadata_hash = if let Ok(out_dir) =
                std::env::var("OUT_DIR")
            {
                let out_dir = PathBuf::from_str(&out_dir).context("Bad path in OUT_DIR env")?;
                let build_dir = out_dir
                    .parent()
                    .context("Out dir missing parent directory")?;
                let build_script_stdout_path_in_build_dir = build_dir.join("output");
                let build_script_crate_metadata_hash_file_path =
                    build_dir.join(BUILD_SCRIPT_CRATE_METADATA_HASH_FILE_NAME);
                let build_script_crate_metadata_hash =
                    std::fs::read_to_string(&build_script_crate_metadata_hash_file_path)
                        .with_context(|| {
                            format!(
                                "Missing build script crate metadata hash file {:?}",
                                build_script_crate_metadata_hash_file_path
                            )
                        })?;
                // Make sure we don't bring any trailing newlines or whatever along for the ride.
                let build_script_crate_metadata_hash =
                    build_script_crate_metadata_hash.trim().to_owned();
                let build_script_stdout_file_name =
                    build_script_stdout_file_name(&build_script_crate_metadata_hash);
                let build_script_stdout_departure_path =
                    departure_dir.path().join(&build_script_stdout_file_name);

                std::fs::copy(build_script_stdout_path_in_build_dir, build_script_stdout_departure_path).with_context(|| {
                    format!("Failed to copy file {build_script_stdout_file_name:?} from target directory to departure directory.")
                })?;

                Some(build_script_crate_metadata_hash)
            } else {
                // This is okay; it just means that there was no build script.
                None
            };

            cache
                .push_crate(
                    &crate_unit_name,
                    &output_defns,
                    maybe_build_script_crate_metadata_hash,
                    departure_dir.path(),
                )
                .context("Failed to push to cache")?;
        }
    };

    Ok(())
}

fn run_real_rustc(rustc_path: &Path, pass_through_args: Vec<String>) -> anyhow::Result<()> {
    let before = Instant::now();
    // dbg!(&pass_through_args[0..usize::min(pass_through_args.len(), 3)]);

    // TODO: Yeah, I'd like an explicit event for this,
    // especially so that I can start collecting timings. :)

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
