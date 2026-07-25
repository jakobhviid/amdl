//! identify — AcoustID acoustic fingerprint matching (spec §6). Identifies a
//! track by *sound* (fpcalc/Chromaprint → AcoustID), which is the only reliable
//! key for untagged/mis-tagged files where text search can't even start.
//!
//! Two hard-won details baked in:
//!   - the lookup is a **POST** (fingerprints are ~3–4 KB and blow past URL
//!     length limits — a GET returns HTTP 400);
//!   - `client` must be an AcoustID **application** key (not an account/user
//!     key, which is rejected as "invalid API key").
use crate::{tags, ui};
use anyhow::{bail, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

const UA: &str = concat!("amdl/", env!("CARGO_PKG_VERSION"), " (https://github.com/jakobhviid/amdl)");

#[derive(Debug, Clone, Serialize)]
pub struct Match {
    pub artist: Option<String>,
    pub title: Option<String>,
    pub album: Option<String>,
    pub score: f64,
}

#[derive(Debug, Serialize)]
pub struct FileResult {
    pub file: String,
    pub matched: Option<Match>,
}

/// AcoustID scores below this are never auto-applied — a wrong tag is worse than
/// none ("a blank field beats a wrong merge"), matching how covers/dedup gate.
pub const DEFAULT_MIN_SCORE: f64 = 0.9;

pub struct Opts {
    /// Write the matched tags (else report-only).
    pub apply: bool,
    /// Preview what `--apply` would write without touching any file.
    pub dry_run: bool,
    /// Only auto-apply a match at or above this AcoustID score (0.0–1.0).
    pub min_score: f64,
    /// Skip files that already have artist+title+album (makes a big untagged-folder
    /// run resumable). Opt-in: identify also *fixes* mis-tagged files, which have tags.
    pub skip_tagged: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub total: usize,
    pub matched: usize,
    pub applied: usize,
    /// Matched but below `min_score`, so left untouched.
    pub skipped_low_score: usize,
    /// Skipped because they already had tags (`--skip-tagged`).
    pub skipped_tagged: usize,
    pub no_match: usize,
    pub failed: usize,
    pub dry_run: bool,
    pub results: Vec<FileResult>,
}

pub fn run(path: &Path, key: &str, opts: &Opts) -> Result<Report> {
    if which("fpcalc").is_none() {
        bail!("fpcalc not found — `brew install chromaprint`");
    }
    let files = list_audio(path);
    let mut report = Report { total: files.len(), dry_run: opts.dry_run, ..Default::default() };
    let pb = ui::bar(files.len() as u64, "Identifying");
    for f in &files {
        let rel = f.strip_prefix(path).unwrap_or(f).display().to_string();
        if opts.skip_tagged && has_all_tags(f) {
            report.skipped_tagged += 1;
            pb.inc(1);
            continue;
        }
        match identify_one(f, key) {
            Ok(Some(m)) => {
                report.matched += 1;
                if opts.apply {
                    if m.score < opts.min_score {
                        report.skipped_low_score += 1;
                    } else if opts.dry_run {
                        report.applied += 1; // would apply
                    } else if crate::journal::edit(f, || tags::write_fields(f, m.title.as_deref(), m.artist.as_deref(), m.album.as_deref())).is_ok() {
                        report.applied += 1;
                    }
                }
                report.results.push(FileResult { file: rel, matched: Some(m) });
            }
            Ok(None) => {
                report.no_match += 1;
                report.results.push(FileResult { file: rel, matched: None });
            }
            Err(_) => report.failed += 1,
        }
        pb.inc(1);
    }
    pb.finish_and_clear();
    Ok(report)
}

fn has_all_tags(path: &Path) -> bool {
    let b = tags::read_basic(path);
    let ok = |o: &Option<String>| o.as_deref().map(|s| !s.is_empty()).unwrap_or(false);
    ok(&b.artist) && ok(&b.title) && ok(&b.album)
}

fn identify_one(path: &Path, key: &str) -> Result<Option<Match>> {
    let (duration, fp) = fingerprint(path)?;
    lookup(key, duration, &fp)
}

fn fingerprint(path: &Path) -> Result<(u64, String)> {
    let out = Command::new("fpcalc").arg("-json").arg(path).output()?;
    if !out.status.success() {
        bail!("fpcalc failed");
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let duration = v.get("duration").and_then(|x| x.as_f64()).map(|d| d.round() as u64).unwrap_or(0);
    let fp = v.get("fingerprint").and_then(|x| x.as_str()).unwrap_or("").to_string();
    if fp.is_empty() {
        bail!("no fingerprint");
    }
    Ok((duration, fp))
}

fn lookup(key: &str, duration: u64, fp: &str) -> Result<Option<Match>> {
    let resp = ureq::post("https://api.acoustid.org/v2/lookup")
        .set("User-Agent", UA)
        // POST body (not query) — the fingerprint is too big for a URL.
        .send_form(&[
            ("client", key),
            ("duration", &duration.to_string()),
            ("fingerprint", fp),
            // AcoustID separates meta types with '+'. Form-encoding turns a SPACE
            // into '+', so we pass a space here — a literal '+' would be sent as
            // %2B and AcoustID would return bare results (id+score, no metadata).
            ("meta", "recordings releasegroups"),
        ]);
    let text = match resp {
        Ok(r) => r.into_string()?,
        // AcoustID returns 400 (with a JSON error body) for a bad key; surface it.
        Err(ureq::Error::Status(_, r)) => r.into_string().unwrap_or_default(),
        Err(e) => return Err(e.into()),
    };
    let v: serde_json::Value = serde_json::from_str(&text)?;
    if v.get("status").and_then(|x| x.as_str()) != Some("ok") {
        let msg = v.get("error").and_then(|e| e.get("message")).and_then(|x| x.as_str()).unwrap_or("unknown");
        bail!("acoustid: {msg}");
    }
    let Some(results) = v.get("results").and_then(|x| x.as_array()) else {
        return Ok(None);
    };
    for res in results {
        let score = res.get("score").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let Some(rec) = res.get("recordings").and_then(|x| x.as_array()).and_then(|a| a.first()) else {
            continue;
        };
        let title = rec.get("title").and_then(|x| x.as_str()).map(String::from);
        let artist = rec
            .get("artists")
            .and_then(|a| a.as_array())
            .and_then(|a| a.first())
            .and_then(|c| c.get("name"))
            .and_then(|x| x.as_str())
            .map(String::from);
        let album = best_album(rec);
        return Ok(Some(Match { artist, title, album, score }));
    }
    Ok(None)
}

/// Prefer a studio release-group (`type=Album`, no `Compilation` secondary type)
/// over compilations, per the spec.
fn best_album(rec: &serde_json::Value) -> Option<String> {
    let rgs = rec.get("releasegroups")?.as_array()?;
    let is_studio = |rg: &&serde_json::Value| {
        rg.get("type").and_then(|x| x.as_str()) == Some("Album")
            && !rg
                .get("secondarytypes")
                .and_then(|s| s.as_array())
                .map(|s| s.iter().any(|t| t.as_str() == Some("Compilation")))
                .unwrap_or(false)
    };
    rgs.iter()
        .find(is_studio)
        .or_else(|| rgs.first())
        .and_then(|rg| rg.get("title"))
        .and_then(|x| x.as_str())
        .map(String::from)
}

fn list_audio(path: &Path) -> Vec<PathBuf> {
    if path.is_file() {
        return vec![path.to_path_buf()];
    }
    let mut out = Vec::new();
    walk(path, &mut out);
    out.retain(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("opus") | Some("m4a") | Some("mp3") | Some("flac")));
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
