//! ffmpeg → Opus, in parallel, with a progress bar. Preserves the relative
//! directory layout under `dest`.
//!
//! TODO(port from the Python tool): precise tag handling (keep Picard/MusicBrainz
//! tags, strip iTunes junk like iTunSMPB/major_brand) and cover art embedded as a
//! spec-correct METADATA_BLOCK_PICTURE. This first cut copies metadata with
//! ffmpeg `-map_metadata 0`; the fine-grained tag/cover work needs a tagging lib
//! (e.g. `lofty`) and a real test corpus.
use crate::ui;
use anyhow::{bail, Result};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn convert_dir(src: &Path, dest: &Path, bitrate: &str, jobs: usize) -> Result<usize> {
    let files = list_ext(src, "m4a");
    convert_files(&files, src, dest, bitrate, jobs)
}

/// Convert `files` (all under `base`) into `dest`, mirroring their layout.
pub fn convert_files(files: &[PathBuf], base: &Path, dest: &Path, bitrate: &str, jobs: usize) -> Result<usize> {
    if which("ffmpeg").is_none() {
        bail!("ffmpeg not found on PATH — `brew install ffmpeg`");
    }
    if files.is_empty() {
        ui::warn("nothing to convert");
        return Ok(0);
    }
    let pb = ui::bar(files.len() as u64, "Converting → Opus");
    let pool = rayon::ThreadPoolBuilder::new().num_threads(jobs.max(1)).build()?;
    let done: usize = pool.install(|| {
        files
            .par_iter()
            .map(|f| {
                let rel = f.strip_prefix(base).unwrap_or(f);
                let out = dest.join(rel).with_extension("opus");
                if let Some(parent) = out.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let r = convert_one(f, &out, bitrate);
                pb.inc(1);
                match r {
                    Ok(()) => 1,
                    Err(e) => {
                        ui::err(&format!("{}: {e}", f.display()));
                        0
                    }
                }
            })
            .sum()
    });
    pb.finish_and_clear();
    Ok(done)
}

fn convert_one(inp: &Path, out: &Path, bitrate: &str) -> Result<()> {
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(inp)
        .args([
            "-c:a", "libopus", "-b:a", bitrate, "-vbr", "on",
            "-application", "audio", "-map_metadata", "0",
        ])
        .arg(out)
        .status()?;
    if !status.success() {
        bail!("ffmpeg failed");
    }
    Ok(())
}

fn list_ext(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.retain(|p| p.extension().map(|e| e == ext).unwrap_or(false));
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
