mod cache;
mod dep_info;

use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::{fs::File, process::Command, str::FromStr};

use anyhow::Context;
use cache::{Cache, LocalCache};
use clap::Parser;
use serde::{Deserialize, Serialize};

use dep_info::DepInfo;

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

    // Try to pull from the cache.
    let cache = LocalCache::new(cache_dir);
    if cache.pull(&crate_unit_name).is_ok() {
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
    if args.emit.iter().any(|emit| emit == "link") {
        dbg!(&args);
        // panic!("wheee");

        'tryadopt: {
            // TODO: Lots of this is copypasta. Do a big ol' refactor.

            let out_dir = args
                .out_dir
                .clone()
                .context("Missing out-dir; don't know how to find build artifacts")?;
            let out_dir_pb =
                PathBuf::from_str(&out_dir).context("Invalid path in out-dir argument")?;

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

            let wrapper_log_file_name = format!("{crate_unit_name}.wrapper-log");
            let wrapper_log_path = out_dir_pb.join(wrapper_log_file_name);
            let mut log_file = File::create(wrapper_log_path).context("Failed to open log file")?;

            let build_manifests_dir = Path::new("/tmp/build-manifests");
            if !build_manifests_dir.exists() {
                break 'tryadopt;
            }
            let manifest_file_name = format!("{crate_unit_name}.manifest.json");
            let manifest_file_path = build_manifests_dir.join(manifest_file_name);
            if !manifest_file_path.exists() {
                break 'tryadopt;
            }
            let build_manifest_json = std::fs::read_to_string(manifest_file_path)
                .context("Failed to load build manifest")?;
            let build_manifest: BuildManifest = serde_json::from_str(&build_manifest_json)
                .context("Failed to parse build manifest JSON file")?;

            // TODO: This is a bit dodgy. We can't actually assume this is true.
            let mut target_dir = out_dir_pb.clone();
            while !target_dir.ends_with("target") {
                target_dir.pop();
            }
            let target_dir_str = target_dir
                .as_os_str()
                .to_str()
                .context("Bad UTF-8 in target dir")?;

            // Looks like we can use it! Copy the output files into our own build dir.
            // First check that all the source files exist before we even start trying
            // to copy anything.
            for out_file_path in &build_manifest.out_file_paths {
                let out_file_pb =
                    PathBuf::from_str(out_file_path).context("Failed to parse out file path")?;
                if !out_file_pb.exists() {
                    writeln!(log_file, "Missing file {out_file_path:?}")?;

                    break 'tryadopt;
                }
            }
            for out_file_path in build_manifest.out_file_paths {
                let out_file_pb =
                    PathBuf::from_str(&out_file_path).context("Failed to parse out file path")?;
                let dest_path =
                    out_dir_pb.join(out_file_pb.file_name().context("Bad out file name")?);
                let dest_dir = dest_path
                    .parent()
                    .context("Failed to pop to parent dir of dest path")?;
                if !dest_dir.exists() {
                    std::fs::create_dir_all(dest_dir).context("Failed to create dest dir")?;
                }

                writeln!(log_file, "Copying file {out_file_path:?} to {dest_path:?}")?;

                std::fs::copy(out_file_pb, &dest_path)
                    .with_context(|| format!("Failed to copy out file {out_file_path:?} to {dest_path:?}; do source file and dest dir exist?"))?;
            }

            return Ok(());
        }
    }

    run_real_rustc(&rustc_path, pass_through_args)?;

    if args
        .codegen_options
        .iter()
        .filter_map(|codegen_option| {
            if let FlagOrKvPair::KvPair(kv_pair) = codegen_option {
                Some(kv_pair)
            } else {
                None
            }
        })
        .any(|kv_pair| kv_pair.key == "incremental")
    {
        // We can't cache incremental builds because there is no "invoked.timestamp" file.
        // (TODO: It might actually not be missing because it's incremental; it might be
        // missing for some other reason related to being the top-level crate.)
        return Ok(());
    }

    if !args.emit.iter().any(|emit| emit == "link") {
        // We're probably not producing the metadata we expect.
        // E.g. calling rustc for some other reason.
        // TODO: Make this... correct. :)
        return Ok(());
    }

    let out_dir = args
        .out_dir
        .context("Missing out-dir; don't know how to find build artifacts")?;
    let out_dir_pb = PathBuf::from_str(&out_dir).context("Invalid path in out-dir argument")?;

    // Check if any of the sources files were changed since the build started.
    // (We can't use this build if anything changed, because we don't know
    // which changes if any actually affected the output of the build!)

    let crate_name = args.crate_name.context("Missing crate name argument")?;
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
    // NOTE: We can't just transform crate name from snake case to kebab,
    // because package names are allowed to use whichever they want.
    let package_name =
        std::env::var("CARGO_PKG_NAME").context("Missing 'CARGO_PKG_NAME' env var")?;
    let package_unit_name = format!("{package_name}{extra_filename}");

    // Load deps file
    let unit_deps_file_name = format!("{crate_unit_name}.d");
    let unit_deps_path = out_dir_pb.join(unit_deps_file_name);
    let dep_info = DepInfo::load(&unit_deps_path)
        .with_context(|| format!("Failed to load dep info file {:?}", unit_deps_path))?;

    // TODO: This is a bit dodgy. We can't actually assume this is true.
    let mut target_dir = out_dir_pb.clone();
    while !target_dir.ends_with("target") {
        target_dir.pop();
    }

    // TODO: Ha, we don't need fingerprints or _anything_. Wheeeee.
    // But we can use cargo to find the output directory. Or...
    // just use the command line flags. Maybe we don't actually need
    // to know much about what Cargo is doing except the fact that it
    // came from an immutable source!!!!!!!!!!
    let fingerprint_dir = target_dir.join("debug").join(".fingerprint");
    let invoked_timestamp_path = fingerprint_dir
        .join(package_unit_name)
        .join("invoked.timestamp");
    let invoked_ts_file_metadata = std::fs::metadata(&invoked_timestamp_path)
        .with_context(|| format!("Couldn't get metadata for {:?}", invoked_timestamp_path))?;
    let invoked_ts = invoked_ts_file_metadata
        .modified()
        .context("Missing mtime for 'invoked.timestamp' file")?;

    // Write out a summary of what we did as we go, so that we can manually inspect it.
    // (If I eventually write a cargo subcommand for this, I guess I'll also
    // have it read that file.)
    //
    // TODO: Make this structured.
    let wrapper_log_file_name = format!("{crate_unit_name}.wrapper-log");
    let wrapper_log_path = out_dir_pb.join(wrapper_log_file_name);
    let mut log_file = File::create(wrapper_log_path).context("Failed to open log file")?;

    writeln!(log_file, "Build invoked: {:?}", invoked_ts)?;

    // TODO: not this noise.
    //
    // Build a manifest of the input data: crate name, 'metadata' value, and a hash of all files that went into it.
    // TODO: Figure out exactly what Cargo provides in the `-C metadata` value, and avoid repeating that work.
    // It at least can't know _all_ source files, because those aren't known until after rustc does
    // macro expansion etc. -- it only knows '.rs' files before this happens.

    let mut out_file_paths = Vec::new();

    // TODO: do we actually need the dep_info? I don't think so...
    // I think I was only using that because I wanted to hash all the
    // input files. The rest should be created with formulaic names.
    for file_path in dep_info.files.keys() {
        // TODO: I would have thought that this would catch deps from _other_ crates. But for some reason
        // I'm not seeing them in the simple ".d" files I've inspected so far. Need to understand that.
        // I should probably construct these file paths myself rather than just checking what ends up in the ".d" file.
        //
        // TODO: Also... OMG hacks. I really need to start documenting (with citations)
        // the logic that Cargo and rustc use for naming these things.
        if file_path.ends_with(".d")
            || file_path.ends_with(".rlib")
            || file_path.ends_with(".rmeta")
            || file_path.ends_with(&format!("/{crate_unit_name}"))
        {
            out_file_paths.push(file_path.to_owned());
        }
    }

    let build_manifest = BuildManifest {
        crate_unit_name: crate_unit_name.clone(),
        out_dir,
        out_file_paths,
    };

    // Check whether we can actually cache this result.
    // NOTE: This MUST happen last, before computing anything else
    // that we might cache based on source file content,
    // else we might be caching lies.
    let target_dir_str = target_dir
        .as_os_str()
        .to_str()
        .context("Bad UTF-8 in target dir")?;
    for path in dep_info.files.keys() {
        if path.starts_with(target_dir_str) {
            // This file was created by a build script, so its mtime will be after the start
            // of the build. (TODO: Check what the "build start time" is set to -- is it the
            // start of the `cargo build`? I guess it must be, or the output of the build script
            // would not have a greater mtime than the start of the crate that consumes it.)
            writeln!(
                log_file,
                "Ignoring {:?} because it is inside the target dir",
                path
            )?;
            continue;
        }
    }

    // TODO: Nah, not this. We actually need to copy it to a cache directory somewhere.
    // And I think ideally compress it!!!!

    // Dump the manifest somewhere people can find it.
    let build_manifests_dir = Path::new("/tmp/build-manifests");
    if !build_manifests_dir.exists() {
        std::fs::create_dir(build_manifests_dir).context("Failed to create build-manifests dir")?;
    }
    let build_manifest_json = serde_json::to_string_pretty(&build_manifest)
        .context("Failed to serialize build manifest")?;
    let manifest_file_name = format!("{crate_unit_name}.manifest.json");
    let manifest_file_path = build_manifests_dir.join(manifest_file_name);
    std::fs::write(manifest_file_path, build_manifest_json)
        .context("Failed to write build manifest file")?;

    Ok(())
}

#[derive(Serialize, Deserialize)]
struct BuildManifest {
    crate_unit_name: String,
    out_dir: String,
    // Differs depending on the kind of thing being built
    out_file_paths: Vec<String>,
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
