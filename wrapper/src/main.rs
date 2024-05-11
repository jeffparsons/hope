use std::process::Command;

use anyhow::Context;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args();

    let _ = args
        .next()
        .context("Missing argument for path to this executable")?;

    let rustc_path = args
        .next()
        .context("Missing argument for real `rustc` path")?;

    let status = Command::new(rustc_path)
        .args(args)
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
