use std::{
    fs::File,
    io::{BufWriter, Write as _},
    path::Path,
};

use anyhow::Context;
use chrono::Utc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub enum CacheLogLine {
    Pulled(PullEvent),
    Pushed(PushEvent),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PullEvent {
    pub crate_unit_name: String,
    pub copied_at: chrono::DateTime<Utc>,
    // Free-form description of where it came from;
    // may differ depending on the cache implementation.
    pub copied_from: String,
    // Were modifications made to the file during pull?
    pub did_mangle_on_pull: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PushEvent {
    pub crate_unit_name: String,
    pub copied_at: chrono::DateTime<Utc>,
    // Free-form description of where it went to;
    // may differ depending on the cache implementation.
    pub copied_from: String,
    // Were modifications made to the file during push?
    pub did_mangle_on_push: bool,
}

pub fn write_log_line(out_dir: &Path, log_line: CacheLogLine) -> anyhow::Result<()> {
    let file = File::options()
        .create(true)
        .append(true)
        .open(out_dir.join("cache-hacks.log"))?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer(&mut writer, &log_line)?;
    writeln!(&mut writer)?;
    writer.flush()?;

    Ok(())
}

pub fn read_log(out_dir: &Path) -> anyhow::Result<Vec<CacheLogLine>> {
    let mut log = Vec::new();
    for line in std::fs::read_to_string(out_dir.join("cache-hacks.log"))
        .context("Failed to read log file")?
        .lines()
    {
        log.push(
            serde_json::from_str(line)
                .with_context(|| format!("Failed to deserialize log line:\n{line}"))?,
        );
    }
    Ok(log)
}
