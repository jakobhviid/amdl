//! lyrics — LRCLIB backfill (spec §4). For each audio file in the output library
//! missing a sibling `.lrc`, look up synced (preferred) or plain lyrics and write
//! the `.lrc` next to it. **State-only by construction:** we operate on the
//! output library, so the read-only source is never touched. Skip-existing,
//! per-file error isolation, parallel. Large `not_found` counts are normal for
//! niche/Danish catalogs — that's not a failure.
use crate::{tags, ui};
use rayon::prelude::*;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

const UA: &str = concat!("amdl/", env!("CARGO_PKG_VERSION"), " (https://github.com/jakobhviid/amdl)");

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub ok_synced: usize,
    pub ok_plain: usize,
    pub not_found: usize,
    pub instrumental: usize,
    pub no_meta: usize,
    pub skipped: usize,
}

enum Fetched {
    Synced(String),
    Plain(String),
    Instrumental,
    NotFound,
}

pub fn backfill(output: &Path, jobs: usize) -> Report {
    let files = list_audio(output);
    if files.is_empty() {
        return Report::default();
    }
    let pb = ui::bar(files.len() as u64, "Lyrics");
    let c = Counters::default();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(jobs.clamp(1, 10))
        .build()
        .expect("thread pool");
    pool.install(|| {
        files.par_iter().for_each(|f| {
            let lrc = f.with_extension("lrc");
            if lrc.exists() {
                c.skipped.fetch_add(1, Ordering::Relaxed);
                pb.inc(1);
                return;
            }
            let b = tags::read_basic(f);
            let (Some(artist), Some(title)) = (b.artist.clone(), b.title.clone()) else {
                c.no_meta.fetch_add(1, Ordering::Relaxed);
                pb.inc(1);
                return;
            };
            match fetch(&artist, &title, b.album.as_deref(), tags::duration_secs(f)) {
                Fetched::Synced(s) => {
                    if write_lrc(&lrc, &s) {
                        c.ok_synced.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Fetched::Plain(s) => {
                    if write_lrc(&lrc, &s) {
                        c.ok_plain.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Fetched::Instrumental => {
                    c.instrumental.fetch_add(1, Ordering::Relaxed);
                }
                Fetched::NotFound => {
                    c.not_found.fetch_add(1, Ordering::Relaxed);
                }
            }
            pb.inc(1);
        });
    });
    pb.finish_and_clear();
    c.into_report()
}

fn write_lrc(path: &Path, content: &str) -> bool {
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    std::fs::write(path, content).is_ok()
}

/// LRCLIB: try the exact `get` (artist/title/album/duration), then fall back to
/// `search`. Prefers synced lyrics; skips instrumentals.
fn fetch(artist: &str, title: &str, album: Option<&str>, dur: Option<u64>) -> Fetched {
    let mut req = ureq::get("https://lrclib.net/api/get")
        .set("User-Agent", UA)
        .query("artist_name", artist)
        .query("track_name", title);
    if let Some(al) = album {
        req = req.query("album_name", al);
    }
    if let Some(d) = dur {
        req = req.query("duration", &d.to_string());
    }
    if let Some(f) = req.call().ok().and_then(parse_one) {
        return f;
    }

    // Fallback: fuzzy search, take the first usable hit.
    let search = ureq::get("https://lrclib.net/api/search")
        .set("User-Agent", UA)
        .query("track_name", title)
        .query("artist_name", artist)
        .call();
    if let Ok(resp) = search {
        if let Ok(text) = resp.into_string() {
            if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(&text) {
                for v in arr {
                    match classify(&v) {
                        Fetched::NotFound => continue,
                        other => return other,
                    }
                }
            }
        }
    }
    Fetched::NotFound
}

fn parse_one(resp: ureq::Response) -> Option<Fetched> {
    let text = resp.into_string().ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(classify(&v))
}

fn classify(v: &serde_json::Value) -> Fetched {
    if v.get("instrumental").and_then(|x| x.as_bool()).unwrap_or(false) {
        return Fetched::Instrumental;
    }
    if let Some(s) = v.get("syncedLyrics").and_then(|x| x.as_str()) {
        if !s.trim().is_empty() {
            return Fetched::Synced(s.to_string());
        }
    }
    if let Some(s) = v.get("plainLyrics").and_then(|x| x.as_str()) {
        if !s.trim().is_empty() {
            return Fetched::Plain(s.to_string());
        }
    }
    Fetched::NotFound
}

#[derive(Default)]
struct Counters {
    ok_synced: AtomicUsize,
    ok_plain: AtomicUsize,
    not_found: AtomicUsize,
    instrumental: AtomicUsize,
    no_meta: AtomicUsize,
    skipped: AtomicUsize,
}
impl Counters {
    fn into_report(self) -> Report {
        Report {
            ok_synced: self.ok_synced.into_inner(),
            ok_plain: self.ok_plain.into_inner(),
            not_found: self.not_found.into_inner(),
            instrumental: self.instrumental.into_inner(),
            no_meta: self.no_meta.into_inner(),
            skipped: self.skipped.into_inner(),
        }
    }
}

fn list_audio(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.retain(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("opus") | Some("m4a") | Some("mp3")));
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
