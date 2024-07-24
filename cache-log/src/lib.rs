use std::{
    fs::File,
    io::{BufRead, BufReader, BufWriter, Write as _},
    path::Path,
};

use anyhow::Context;
use chrono::Utc;
use fd_lock::RwLock;
use serde::{Deserialize, Serialize};

const LOG_FILE_NAME: &str = "hope-log.jsonl";

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum CacheLogLine {
    PulledCrateOutputs(PullCrateOutputsEvent),
    PushedCrateOutputs(PushCrateOutputsEvent),
    RanBuildScript(BuildScriptRunEvent),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PullCrateOutputsEvent {
    pub crate_unit_name: String,
    pub copied_at: chrono::DateTime<Utc>,
    // Free-form description of where it came from;
    // may differ depending on the cache implementation.
    pub copied_from: String,
    // How long did it take to copy from cache?
    pub duration_secs: f64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PushCrateOutputsEvent {
    pub crate_unit_name: String,
    pub copied_at: chrono::DateTime<Utc>,
    // Free-form description of where it went to;
    // may differ depending on the cache implementation.
    pub copied_from: String,
    // How long did it take to copy to cache?
    pub duration_secs: f64,
}

// TODO: The existence of this kinda suggests that this log
// should probably not be associated with a specific cache,
// but be global by default (with ability to override).
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BuildScriptRunEvent {
    // TODO: Lots of other details
    pub ran_at: chrono::DateTime<Utc>,
}

pub fn write_log_line(cache_dir: &Path, log_line: CacheLogLine) -> anyhow::Result<()> {
    let file = File::options()
        .create(true)
        .append(true)
        .open(cache_dir.join(LOG_FILE_NAME))?;
    let mut file = RwLock::new(file);
    let mut write_guard = file.write()?;
    let mut writer = BufWriter::new(&mut *write_guard);
    serde_json::to_writer(&mut writer, &log_line)?;
    writeln!(&mut writer)?;
    writer.flush()?;

    Ok(())
}

pub fn read_log(cache_dir: &Path) -> anyhow::Result<Vec<CacheLogLine>> {
    let mut log = Vec::new();
    let file = File::open(cache_dir.join(LOG_FILE_NAME))?;
    let mut file = RwLock::new(file);
    let mut read_guard = file.write()?;
    let reader = BufReader::new(&mut *read_guard);

    for line in reader.lines() {
        let line = line?;
        log.push(
            serde_json::from_str(&line)
                .with_context(|| format!("Failed to deserialize log line:\n{line}"))?,
        );
    }
    Ok(log)
}
