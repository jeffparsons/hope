mod dep_info;

use std::io::Write;
use std::path::PathBuf;
use std::{fs::File, process::Command, str::FromStr};

use anyhow::Context;
use clap::Parser;

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
#[command(version)]
struct Args {
    input: String,
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
    #[arg(short = 'W', value_delimiter = ',')]
    warn_for_lints: Vec<String>,
    #[arg(long = "force-warn", value_delimiter = ',')]
    force_warn_for_lints: Vec<String>,
    #[arg(short = 'A', value_delimiter = ',')]
    allow_lints: Vec<String>,
    #[arg(short = 'D', value_delimiter = ',')]
    deny_lints: Vec<String>,
    #[arg(short = 'F', value_delimiter = ',')]
    forbid_lints: Vec<String>,
    #[arg(short = 'Z', value_delimiter = ',')]
    unstable_options: Vec<String>,
    #[arg(long)]
    cap_lints: Option<String>,
    #[arg(short = 'C', long = "codegen", value_delimiter = ',')]
    codegen_options: Vec<FlagOrKvPair>,
    #[arg(short, long, default_value_t)]
    verbose: u8,
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

    // REVISIT: If I want to start _modifying_ arguments eventually,
    // then I'll need to reconstruct the arg vector from our parsed arguments.
    let pass_through_args: Vec<String> = args.collect();
    args_to_parse.extend(pass_through_args.iter().cloned());

    let args = Args::parse_from(args_to_parse);

    let status = Command::new(rustc_path)
        .args(pass_through_args)
        .status()
        .context("Failed to start real `rustc`")?;

    if !status.success() {
        std::process::exit(
            status
                .code()
                .context("Child process was terminated by a signal")?,
        );
    }

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
        // We can't cache incremental builds.
        return Ok(());
    }

    if !args.emit.iter().any(|emit| emit == "link") {
        // We're probably not producing the metadata we expect.
        // E.g. calling rustc for some other reason.
        // TODO: Make this... correct. :)
        return Ok(());
    }

    let out_dir = PathBuf::from_str(
        &args
            .out_dir
            .context("Missing out-dir; don't know how to find build artifacts")?,
    )
    .context("Invalid path in out-dir argument")?;

    // Check if any of the sources files were changed since the build started.
    // (We can't use this build if anything changed, because we don't know
    // which changes if any actually affected the output of the build!)

    // TODO: Assert that there's only one metadata value here; we won't know
    // what to do if there are multiple.
    let crate_name = args.crate_name.context("Missing crate name argument")?;
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

    // TODO: Better understand what these two different things actually represent. This is a bit of a guess.
    let crate_unit_name = format!("{crate_name}-{metadata_hash}");
    let out_dir_name = out_dir
        .components()
        .last()
        .context("No path components in out-dir")?
        .as_os_str()
        .to_str()
        .context("Bad UTF-8 in out-dir")?
        .to_owned();
    let package_unit_name = if out_dir_name == "deps" {
        crate_unit_name.clone()
    } else {
        out_dir_name
    };

    // Load deps file
    let unit_deps_file_name = format!("{crate_unit_name}.d");
    let unit_deps_path = out_dir.join(unit_deps_file_name);
    let dep_info = DepInfo::load(&unit_deps_path)
        .with_context(|| format!("Failed to load dep info file {:?}", unit_deps_path))?;

    // TODO: This is a bit dodgy. We can't actually assume this is true.
    let mut target_dir = out_dir.clone();
    while !target_dir.ends_with("target") {
        target_dir.pop();
    }

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
    let wrapper_log_path = out_dir.join(wrapper_log_file_name);
    let mut log_file = File::create(wrapper_log_path).context("Failed to open log file")?;

    writeln!(log_file, "Build invoked: {:?}", invoked_ts)?;

    let target_dir_str = target_dir
        .as_os_str()
        .to_str()
        .context("Bad UTF-8 in target dir")?;
    let mut can_cache = true;
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

        let file_metadata =
            std::fs::metadata(path).context("Couldn't get metadata for source file")?;
        let source_mtime = file_metadata
            .modified()
            .context("Missing mtime for source file")?;

        writeln!(log_file, "{:?} mtime: {:?}", path, source_mtime)?;

        if source_mtime > invoked_ts {
            // One of the source files was modified after we started the build;
            // we can't cache this output, because we don't know what went into it.
            writeln!(log_file, "^ MODIFIED")?;
            can_cache = false;
        }
    }

    writeln!(log_file, "Can cache? {:?}", can_cache)?;

    Ok(())
}
