//! doctor — health/integrity scan of a derived (Opus) library, the map the rest
//! of a repair is planned from (spec W2). In one pass over `output`:
//!   - **unreadable** (won't probe),
//!   - **missing_cover** (no embedded picture),
//!   - **missing_tags** (no artist/title/album),
//!
//! and, when a `source` tree is given for comparison:
//!   - **truncated** (decoded Opus duration vs the source's differ > ~1.5 s — the
//!     silent damage a killed run leaves behind, which skip-existing would hide),
//!   - **source_without_opus** (a source file that never produced an Opus =
//!     conversion failure / damaged original).
use crate::{tags, ui};
use anyhow::{bail, Result};
use rayon::prelude::*;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Tolerance for the truncation check (seconds).
const DURATION_TOLERANCE: f64 = 1.5;

#[derive(Debug, Default, Serialize)]
pub struct Health {
    pub total: usize,
    pub unreadable: Vec<String>,
    pub missing_cover: Vec<String>,
    pub missing_tags: Vec<String>,
    pub truncated: Vec<String>,
    /// Files that fail a full decode (`--deep`) — corruption the header/probe hides.
    pub corrupt: Vec<String>,
    pub source_without_opus: Vec<String>,
}

impl Health {
    pub fn is_clean(&self) -> bool {
        self.unreadable.is_empty()
            && self.missing_cover.is_empty()
            && self.missing_tags.is_empty()
            && self.truncated.is_empty()
            && self.corrupt.is_empty()
            && self.source_without_opus.is_empty()
    }
}

/// Decoded duration in seconds via ffprobe, if readable.
pub fn duration(path: &Path) -> Option<f64> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

fn rel_display(base: &Path, p: &Path) -> String {
    p.strip_prefix(base).unwrap_or(p).display().to_string()
}

/// Locate the source audio file (.m4a/.mp3) that would have produced `opus`.
fn source_for(opus: &Path, output: &Path, source: &Path) -> Option<PathBuf> {
    let rel = opus.strip_prefix(output).ok()?;
    for ext in ["m4a", "mp3", "flac"] {
        let cand = source.join(rel).with_extension(ext);
        if cand.exists() {
            return Some(cand);
        }
    }
    None
}

pub fn scan(output: &Path, source: Option<&Path>, deep: bool) -> Result<Health> {
    // ffprobe backs every duration/readability check — without it *every* file
    // would look `unreadable`. `--deep` also needs ffmpeg, and without it the
    // decode check silently reports zero corruption. Fail loudly, like convert.
    if which("ffprobe").is_none() {
        bail!("ffprobe not found on PATH — `brew install ffmpeg` (provides ffprobe)");
    }
    if deep && which("ffmpeg").is_none() {
        bail!("--deep needs ffmpeg for the full-decode check — `brew install ffmpeg`");
    }
    let opus = list_ext(output, "opus");
    let mut health = Health {
        total: opus.len(),
        ..Default::default()
    };

    let pb = ui::bar(opus.len() as u64, if deep { "Decoding" } else { "Scanning" });
    // Per-opus checks in parallel; collect classified findings.
    let findings: Vec<(String, Vec<Finding>)> = opus
        .par_iter()
        .map(|o| {
            let name = rel_display(output, o);
            let mut fs = Vec::new();
            let dur = duration(o);
            if dur.is_none() {
                fs.push(Finding::Unreadable);
            }
            let basic = tags::read_basic(o);
            if !basic.has_cover {
                fs.push(Finding::MissingCover);
            }
            if basic.title.is_none() || basic.artist.is_none() || basic.album.is_none() {
                fs.push(Finding::MissingTags);
            }
            // Full-decode integrity check (opt-in): catches stream corruption that a
            // metadata probe misses, and needs no source library to compare against.
            if deep && dur.is_some() && !decodes_cleanly(o) {
                fs.push(Finding::Corrupt);
            }
            if let (Some(src_root), Some(d)) = (source, dur) {
                if let Some(src) = source_for(o, output, src_root) {
                    if let Some(sd) = duration(&src) {
                        if (sd - d).abs() > DURATION_TOLERANCE {
                            fs.push(Finding::Truncated);
                        }
                    }
                }
            }
            pb.inc(1);
            (name, fs)
        })
        .collect();
    ui::finish_done(&pb);

    for (name, fs) in findings {
        for f in fs {
            match f {
                Finding::Unreadable => health.unreadable.push(name.clone()),
                Finding::MissingCover => health.missing_cover.push(name.clone()),
                Finding::MissingTags => health.missing_tags.push(name.clone()),
                Finding::Truncated => health.truncated.push(name.clone()),
                Finding::Corrupt => health.corrupt.push(name.clone()),
            }
        }
    }

    // Source files that never produced an Opus (conversion failures / damage).
    if let Some(src_root) = source {
        let srcs = {
            let mut v = Vec::new();
            walk(src_root, &mut v);
            v.retain(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("m4a") | Some("mp3") | Some("flac")));
            v
        };
        for s in &srcs {
            if let Ok(rel) = s.strip_prefix(src_root) {
                let expected = output.join(rel).with_extension("opus");
                if !expected.exists() {
                    health.source_without_opus.push(rel.display().to_string());
                }
            }
        }
    }

    health.unreadable.sort();
    health.missing_cover.sort();
    health.missing_tags.sort();
    health.truncated.sort();
    health.corrupt.sort();
    health.source_without_opus.sort();
    Ok(health)
}

fn which(bin: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).map(|d| d.join(bin)).find(|p| p.is_file())
    })
}

/// A full decode with no errors on stderr (`-v error`). ffmpeg can exit 0 while
/// still logging decode errors, so any error output means corruption.
fn decodes_cleanly(path: &Path) -> bool {
    match Command::new("ffmpeg")
        .args(["-v", "error", "-xerror", "-i"])
        .arg(path)
        .args(["-f", "null", "-"])
        .output()
    {
        Ok(out) => out.status.success() && out.stderr.is_empty(),
        Err(_) => true, // ffmpeg missing → don't cry corruption; probe already ran
    }
}

enum Finding {
    Unreadable,
    MissingCover,
    MissingTags,
    Truncated,
    Corrupt,
}

fn list_ext(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.retain(|p| p.extension().and_then(|e| e.to_str()) == Some(ext));
    out.sort();
    out
}
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.push(p);
            }
        }
    }
}
