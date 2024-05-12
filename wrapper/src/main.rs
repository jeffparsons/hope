use std::process::Command;

use clap::Parser;

use anyhow::Context;

// Arguments here mirror the real `rustc` arguments.
// I'm just using Clap to make it easier to inspect/modify the ones I care about.
#[derive(Parser, Debug)]
#[command(version)]
struct Args {
    input: String,
    #[arg(long)]
    cfg: Vec<String>,
    #[arg(short = 'L')]
    lib_search_paths: Vec<String>,
    #[arg(short = 'l')]
    link_to_native_libs: Vec<String>,
    #[arg(long = "crate-type")]
    crate_types: Vec<String>,
    #[arg(long)]
    crate_name: Option<String>,
    #[arg(long)]
    edition: Option<String>,
    #[arg(long)]
    emit: Vec<String>,
    #[arg(long)]
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
    #[arg(short = 'W')]
    warn_for_lints: Vec<String>,
    #[arg(long = "force-warn")]
    force_warn_for_lints: Vec<String>,
    #[arg(short = 'A')]
    allow_lints: Vec<String>,
    #[arg(short = 'D')]
    deny_lints: Vec<String>,
    #[arg(short = 'F')]
    forbid_lints: Vec<String>,
    #[arg(short = 'Z')]
    unstable_options: Vec<String>,
    #[arg(long)]
    cap_lints: Option<String>,
    #[arg(short = 'C', long = "codegen")]
    codegen_options: Vec<String>,
    #[arg(short, long, default_value_t)]
    verbose: u8,
    #[arg(long = "extern")]
    extern_: Vec<String>,
    #[arg(long)]
    sysroot: Option<String>,
    #[arg(long)]
    error_format: Option<String>,
    #[arg(long)]
    color: Option<String>,
    #[arg(long)]
    diagnostic_width: Option<u32>,
    #[arg(long = "remap-path-prefix")]
    remap_path_prefixes: Vec<String>,
    #[arg(long)]
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

    let _args = Args::parse_from(args_to_parse);

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

    Ok(())
}
