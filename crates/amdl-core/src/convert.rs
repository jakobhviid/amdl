//! ffmpeg → Opus, in parallel, with a progress bar. Preserves the relative
//! directory layout under `dest` and — crucially — carries fidelity that plain
//! ffmpeg drops:
//!   - re-embeds the source cover as a spec-correct Opus METADATA_BLOCK_PICTURE
//!     (`tags::finalize_opus`), because `-map 0:a:0` deliberately drops the MP4
//!     cover *stream* (muxing it into Opus is the unreliable path);
//!   - strips iTunes junk atoms;
//!   - mirrors `.lrc` sidecars into the output tree;
//!   - handles `.m4a` and `.mp3`;
//!   - skip-existing by default (resumable) — an existing `.opus` is left alone.
use crate::{tags, ui};
use anyhow::{bail, Result};
use rayon::prelude::*;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub converted: usize,
    pub copied: usize,
    pub skipped: usize,
    pub failed: usize,
    pub with_cover: usize,
    pub lrc_copied: usize,
}

pub fn convert_dir(src: &Path, dest: &Path, bitrate: &str, jobs: usize) -> Result<Report> {
    let files = list_audio(src);
    convert_files(&files, src, dest, bitrate, jobs)
}

/// Convert `files` (all under `base`) into `dest`, mirroring their layout.
pub fn convert_files(
    files: &[PathBuf],
    base: &Path,
    dest: &Path,
    bitrate: &str,
    jobs: usize,
) -> Result<Report> {
    if which("ffmpeg").is_none() {
        bail!("ffmpeg not found on PATH — `brew install ffmpeg`");
    }
    if files.is_empty() {
        ui::warn("nothing to convert");
        return Ok(Report::default());
    }
    let pb = ui::bar(files.len() as u64, "Converting → Opus");
    let (conv, copied, skip, fail, cover, lrc) = (
        AtomicUsize::new(0),
        AtomicUsize::new(0),
        AtomicUsize::new(0),
        AtomicUsize::new(0),
        AtomicUsize::new(0),
        AtomicUsize::new(0),
    );
    let pool = rayon::ThreadPoolBuilder::new().num_threads(jobs.max(1)).build()?;
    pool.install(|| {
        files.par_iter().for_each(|f| {
            let rel = f.strip_prefix(base).unwrap_or(f);
            let out = dest.join(rel).with_extension("opus");
            match convert_one(f, &out, bitrate) {
                Ok(o) => {
                    if o.skipped {
                        skip.fetch_add(1, Ordering::Relaxed);
                    } else {
                        crate::journal::created(&out); // undo: delete this new .opus
                        if o.copied {
                            copied.fetch_add(1, Ordering::Relaxed);
                        } else {
                            conv.fetch_add(1, Ordering::Relaxed);
                        }
                        if o.cover {
                            cover.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    if o.lrc {
                        crate::journal::created(&out.with_extension("lrc"));
                        lrc.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(e) => {
                    ui::err(&format!("{}: {e}", f.display()));
                    fail.fetch_add(1, Ordering::Relaxed);
                }
            }
            pb.inc(1);
        });
    });
    ui::finish_done(&pb);
    Ok(Report {
        converted: conv.into_inner(),
        copied: copied.into_inner(),
        skipped: skip.into_inner(),
        failed: fail.into_inner(),
        with_cover: cover.into_inner(),
        lrc_copied: lrc.into_inner(),
    })
}

struct Outcome {
    skipped: bool,
    cover: bool,
    lrc: bool,
    copied: bool,
}

fn convert_one(inp: &Path, out: &Path, bitrate: &str) -> Result<Outcome> {
    if let Some(parent) = out.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Mirror the .lrc sidecar regardless of skip-existing (lyrics may arrive
    // after the audio was already converted).
    let lrc = mirror_lrc(inp, out);

    // Skip-existing (resumable): never re-encode a file that's already there.
    if out.exists() {
        return Ok(Outcome { skipped: true, cover: false, lrc, copied: false });
    }

    // Source is already Opus → copy verbatim. Re-encoding lossy→lossy would just
    // shed quality for no reason.
    if inp.extension().and_then(|e| e.to_str()) == Some("opus") {
        std::fs::copy(inp, out)?;
        let cover = tags::has_cover(out);
        return Ok(Outcome { skipped: false, cover, lrc, copied: true });
    }

    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(inp)
        .args([
            "-map", "0:a:0", // audio only — drop the MP4 cover *stream*; we re-embed below
            "-c:a", "libopus", "-b:a", bitrate, "-vbr", "on",
            "-application", "audio", "-compression_level", "10",
            "-map_metadata", "0",
        ])
        .arg(out)
        .status()?;
    if !status.success() {
        bail!("ffmpeg failed");
    }

    // Fidelity pass: strip junk + re-embed the source cover as METADATA_BLOCK_PICTURE.
    let cover = tags::read_cover(inp);
    let embedded = match tags::finalize_opus(out, cover.as_ref()) {
        Ok(e) => e,
        Err(e) => {
            // Transcode succeeded but the tag/cover pass didn't — the .opus is
            // playable but may keep iTunes junk or lack its cover. Say so rather
            // than silently counting it a clean conversion.
            ui::warn(&format!("tag finalize failed for {}: {e}", out.display()));
            false
        }
    };
    Ok(Outcome { skipped: false, cover: embedded, lrc, copied: false })
}

/// Copy `inp`'s sibling `.lrc` into the output tree if present and not already there.
fn mirror_lrc(inp: &Path, out: &Path) -> bool {
    let src_lrc = inp.with_extension("lrc");
    let dst_lrc = out.with_extension("lrc");
    if src_lrc.exists() && !dst_lrc.exists() {
        if let Some(p) = dst_lrc.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        return std::fs::copy(&src_lrc, &dst_lrc).is_ok();
    }
    false
}

fn list_audio(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.retain(|p| {
        matches!(p.extension().and_then(|e| e.to_str()), Some("m4a") | Some("mp3") | Some("opus"))
    });
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
fn which(bin: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).map(|d| d.join(bin)).find(|p| p.is_file())
    })
}
