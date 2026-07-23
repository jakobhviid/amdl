//! Decode-test a file with ffmpeg. Fails on the classic "stripped-but-still-
//! encrypted payload" signatures — the bug the Python tool's library-repair
//! incident was built around.
use std::path::Path;
use std::process::Command;

const BAD_SIGNATURES: &[&str] = &[
    "Invalid data found when processing input",
    "Prediction not allowed in AAC-LC",
    "channel element",
    "exceeds limit",
];

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
