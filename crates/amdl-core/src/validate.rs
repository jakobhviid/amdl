//! Decode-test a file with ffmpeg. Fails on the classic "stripped-but-still-
//! encrypted payload" signatures — the bug the Python tool's library-repair
//! incident was built around.
use crate::ui;
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::process::Command;

const BAD_SIGNATURES: &[&str] = &[
    "Invalid data found when processing input",
    "Prediction not allowed in AAC-LC",
    "channel element",
    "exceeds limit",
];

/// Decode-check `tracks` in parallel behind a progress bar; return the bad ones.
pub fn probe_bad(tracks: &[PathBuf]) -> Vec<PathBuf> {
    if tracks.is_empty() {
        return Vec::new();
    }
    let pb = ui::bar(tracks.len() as u64, "Validating");
    let bad = tracks
        .par_iter()
        .filter_map(|t| {
            let ok = probe_ok(t);
            pb.inc(1);
            (!ok).then(|| t.clone())
        })
        .collect();
    pb.finish_and_clear();
    bad
}

/// True if the file decodes cleanly (no error output, no bad signatures).
pub fn probe_ok(path: &Path) -> bool {
    match Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "null", "-"])
        .output()
    {
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            o.status.success() && !BAD_SIGNATURES.iter().any(|s| stderr.contains(s))
        }
        Err(_) => false,
    }
}
