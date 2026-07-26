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
use rayon::prelude::*;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// AcoustID allows at most ~3 requests/second per application key; exceeding it
/// earns 429s and can get the key banned. `fpcalc` fingerprinting is local CPU
/// work and safe to parallelize, so we fan that out across cores but funnel every
/// lookup through this global gate to hold the network side at the rate limit.
const ACOUSTID_MIN_INTERVAL: Duration = Duration::from_millis(350);

struct RateGate {
    last: Mutex<Option<Instant>>,
    min: Duration,
}
impl RateGate {
    fn new(min: Duration) -> Self {
        Self { last: Mutex::new(None), min }
    }
    /// Block until at least `min` has elapsed since the previous caller. Holding
    /// the lock across the sleep serializes callers at exactly the min interval.
    fn wait(&self) {
        let mut last = self.last.lock().unwrap();
        if let Some(prev) = *last {
            let elapsed = prev.elapsed();
            if elapsed < self.min {
                std::thread::sleep(self.min - elapsed);
            }
        }
        *last = Some(Instant::now());
    }
}

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

/// Per-file outcome, collected in input order then aggregated into the Report.
enum Outcome {
    SkippedTagged,
    Matched { m: Match, applied: bool, low_score: bool },
    NoMatch,
    Failed,
}

pub fn run(path: &Path, key: &str, opts: &Opts) -> Result<Report> {
    if which("fpcalc").is_none() {
        bail!("fpcalc not found — `brew install chromaprint`");
    }
    let files = list_audio(path);
    let pb = ui::bar(files.len() as u64, "Identifying");
    let gate = RateGate::new(ACOUSTID_MIN_INTERVAL);
    let jobs = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let pool = rayon::ThreadPoolBuilder::new().num_threads(jobs.max(1)).build()?;

    // Fingerprint in parallel (local CPU work); every AcoustID lookup passes
    // through `gate`, so the network side stays under the rate limit even though
    // fpcalc is fanned out across cores. `map` preserves input order.
    let outcomes: Vec<(String, Outcome)> = pool.install(|| {
        files
            .par_iter()
            .map(|f| {
                let rel = f.strip_prefix(path).unwrap_or(f).display().to_string();
                let out = classify(f, key, opts, &gate);
                pb.inc(1);
                (rel, out)
            })
            .collect()
    });
    ui::finish_done(&pb);

    let mut report = Report { total: files.len(), dry_run: opts.dry_run, ..Default::default() };
    for (rel, out) in outcomes {
        match out {
            Outcome::SkippedTagged => report.skipped_tagged += 1,
            Outcome::Matched { m, applied, low_score } => {
                report.matched += 1;
                if applied {
                    report.applied += 1;
                }
                if low_score {
                    report.skipped_low_score += 1;
                }
                report.results.push(FileResult { file: rel, matched: Some(m) });
            }
            Outcome::NoMatch => {
                report.no_match += 1;
                report.results.push(FileResult { file: rel, matched: None });
            }
            Outcome::Failed => report.failed += 1,
        }
    }
    Ok(report)
}

fn classify(f: &Path, key: &str, opts: &Opts, gate: &RateGate) -> Outcome {
    if opts.skip_tagged && has_all_tags(f) {
        return Outcome::SkippedTagged;
    }
    let Ok((duration, fp)) = fingerprint(f) else {
        return Outcome::Failed;
    };
    gate.wait();
    match lookup(key, duration, &fp) {
        Ok(Some(m)) => {
            let (mut applied, mut low_score) = (false, false);
            if opts.apply {
                if m.score < opts.min_score {
                    low_score = true;
                } else if opts.dry_run {
                    applied = true; // would apply
                } else if crate::journal::edit(f, || tags::write_fields(f, m.title.as_deref(), m.artist.as_deref(), m.album.as_deref())).is_ok() {
                    applied = true;
                }
            }
            Outcome::Matched { m, applied, low_score }
        }
        Ok(None) => Outcome::NoMatch,
        Err(_) => Outcome::Failed,
    }
}

fn has_all_tags(path: &Path) -> bool {
    let b = tags::read_basic(path);
    let ok = |o: &Option<String>| o.as_deref().map(|s| !s.is_empty()).unwrap_or(false);
    ok(&b.artist) && ok(&b.title) && ok(&b.album)
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
    let resp = crate::http::agent().post("https://api.acoustid.org/v2/lookup")
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
    crate::scan::with_exts(path, &["opus", "m4a", "mp3", "flac"])
}
fn which(bin: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).map(|d| d.join(bin)).find(|p| p.is_file())
    })
}
