//! lyrics — LRCLIB backfill (spec §4). For each audio file in the output library
//! missing a sibling `.lrc`, look up synced (preferred) or plain lyrics and write
//! the `.lrc` next to it. **State-only by construction:** we operate on the
//! output library, so the read-only source is never touched. Skip-existing,
//! per-file error isolation, parallel. Large `not_found` counts are normal for
//! niche/Danish catalogs — that's not a failure.
//!
//! With `upgrade_synced` on, an existing *plain* (untimed) `.lrc` is re-queried
//! and replaced when LRCLIB has a synced version; the old bytes are journaled so
//! `undo` can restore them. Already-synced files are still skipped.
//!
//! With `embed` on, the sidecar's lyrics are also written into the audio file's
//! `LYRICS` tag (sidecar kept). Embedding **never downgrades**: it writes only
//! when nothing is embedded or as a plain→synced upgrade; a synced embed is left
//! untouched (and identical content is never rewritten) unless `force_embed`.
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
    /// Existing *plain* `.lrc` sidecars replaced with a synced version from LRCLIB
    /// (only counted when `upgrade_synced` is on).
    pub upgraded: usize,
    /// Tracks whose lyrics were (re)written into the audio file's `LYRICS` tag
    /// (only counted when `embed` is on).
    pub embedded: usize,
    /// Plain `.lrc` files turned into synced by the alignment service (Generated
    /// tier; only counted when `align` is on).
    pub aligned: usize,
    pub not_found: usize,
    pub instrumental: usize,
    pub no_meta: usize,
    pub skipped: usize,
}

/// What a `lyrics` run should do beyond the default sidecar backfill.
#[derive(Clone, Copy, Default)]
pub struct Options {
    /// Re-time an existing *plain* `.lrc` to synced when LRCLIB now has one.
    pub upgrade_synced: bool,
    /// Also embed the sidecar's lyrics into the audio file's `LYRICS` tag.
    pub embed: bool,
    /// When embedding, overwrite even if it isn't an upgrade — including
    /// replacing an already-*synced* embed. Off = never downgrade / never churn.
    pub force_embed: bool,
    /// Generate *synced* lyrics from plain ones via the forced-alignment service
    /// (config `[lyrics] aligner_url`) when no source has timed lyrics. Results
    /// are marked `[re:amdl-align]` (the "Generated" tier).
    pub align: bool,
}

enum Fetched {
    Synced(String),
    Plain(String),
    Instrumental,
    NotFound,
}

pub fn backfill(output: &Path, jobs: usize, opts: Options, fallback: Option<&Fallback>, aligner_url: Option<&str>) -> Report {
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
            // Stage 1: settle the sidecar (skip / fetch / upgrade), and hand back
            // its resulting lyric text so later stages can use it.
            let (mut content, pre) = settle_sidecar(f, opts, &c, fallback);
            // Stage 1b: if we ended up with *plain* lyrics and no source had a
            // synced version, generate synced ones via the alignment service
            // (Generated tier, marked). Only fires when alignment is enabled
            // (a configured aligner, not opted out) and the text isn't synced.
            if opts.align {
                if let (Some(url), Some(text)) = (aligner_url, content.clone()) {
                    if !is_synced(text.as_bytes()) {
                        if let Some(gen) = align_track(url, f, &text) {
                            let lrc = f.with_extension("lrc");
                            // If a sidecar already existed we're replacing it
                            // (plain → synced); if the lyrics only lived in the
                            // file's tag there's no sidecar yet, so this creates
                            // one (undo then deletes it rather than restoring).
                            let ok = if lrc.exists() {
                                upgrade_lrc(&lrc, text.as_bytes(), &gen)
                            } else {
                                write_lrc(&lrc, &gen)
                            };
                            if ok {
                                // Aligning it means it wasn't skipped / left as
                                // plain after all — move it out of that bucket.
                                match pre {
                                    PreCount::Skipped => {
                                        c.skipped.fetch_sub(1, Ordering::Relaxed);
                                    }
                                    PreCount::OkPlain => {
                                        c.ok_plain.fetch_sub(1, Ordering::Relaxed);
                                    }
                                    PreCount::None => {}
                                }
                                c.aligned.fetch_add(1, Ordering::Relaxed);
                                content = Some(gen);
                            }
                        }
                    }
                }
            }
            // Stage 2: optionally embed that text into the audio file's tags,
            // honoring the never-downgrade rule.
            if opts.embed {
                if let Some(text) = &content {
                    if embed_lyrics(f, text, opts.force_embed) {
                        c.embedded.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            pb.inc(1);
        });
    });
    ui::finish_done(&pb);
    c.into_report()
}

/// Which tally `settle_sidecar` charged this track to — so that if the align
/// stage then times it, we can move it out of that bucket and into `aligned`
/// (a file that gets aligned wasn't really "skipped" or left as plain).
#[derive(Clone, Copy, PartialEq)]
enum PreCount {
    None,
    Skipped,
    OkPlain,
}

/// Ensure the sidecar `.lrc` is present/upgraded, tallying the outcome, and
/// return the lyric text now on disk (for embedding) plus the bucket it was
/// charged to. `None` content means there are no usable lyrics for this track.
fn settle_sidecar(
    f: &Path,
    opts: Options,
    c: &Counters,
    fallback: Option<&Fallback>,
) -> (Option<String>, PreCount) {
    let b = tags::read_basic(f);
    // Instrumental short-circuit. A durable `AMDL_INSTRUMENTAL` mark, or an
    // explicit "(Instrumental)" in the title, means no lyrics belong here: skip
    // with no network at all. When we newly detect it from the title, stamp the
    // mark so every later run skips instantly and never re-queries — the biggest
    // source of wrong auto-matches was vocal lyrics landing on instrumentals.
    if b.instrumental_marked {
        c.instrumental.fetch_add(1, Ordering::Relaxed);
        return (None, PreCount::None);
    }
    if b.title.as_deref().map(title_says_instrumental).unwrap_or(false) {
        mark_instrumental(f);
        c.instrumental.fetch_add(1, Ordering::Relaxed);
        return (None, PreCount::None);
    }
    let dur = tags::duration_secs(f);
    let lrc = f.with_extension("lrc");
    if lrc.exists() {
        // Existing sidecar: normally skip. With `upgrade_synced`, a *plain* .lrc
        // is re-queried and replaced iff a source has a synced version.
        // A sidecar we can't read must NOT be treated as empty: doing so would
        // let the upgrade below overwrite it and journal an empty "before" (undo
        // would then restore nothing). On a read error, leave it strictly alone.
        let Ok(existing) = std::fs::read(&lrc) else {
            c.skipped.fetch_add(1, Ordering::Relaxed);
            return (None, PreCount::Skipped);
        };
        if opts.upgrade_synced && !is_synced(&existing) {
            if let Some(Fetched::Synced(s)) = fetch_meta(&b, dur, fallback) {
                if upgrade_lrc(&lrc, &existing, &s) {
                    c.upgraded.fetch_add(1, Ordering::Relaxed);
                    return (Some(s), PreCount::None);
                }
            }
        }
        // Left as-is (already synced, no synced upgrade available, or no upgrade).
        c.skipped.fetch_add(1, Ordering::Relaxed);
        return (String::from_utf8(existing).ok(), PreCount::Skipped);
    }
    match fetch_meta(&b, dur, fallback) {
        Some(Fetched::Synced(s)) => {
            if write_lrc(&lrc, &s) {
                c.ok_synced.fetch_add(1, Ordering::Relaxed);
            }
            (Some(s), PreCount::None)
        }
        Some(Fetched::Plain(s)) => {
            if write_lrc(&lrc, &s) {
                c.ok_plain.fetch_add(1, Ordering::Relaxed);
            }
            (Some(s), PreCount::OkPlain)
        }
        Some(Fetched::Instrumental) => {
            // The source flags this recording instrumental — persist that as our
            // mark so we never re-query it, and never write a lyric to it.
            mark_instrumental(f);
            c.instrumental.fetch_add(1, Ordering::Relaxed);
            (None, PreCount::None)
        }
        // No sidecar and nothing from the network. Fall back to lyrics embedded
        // in the file itself (the `LYRICS` tag) as the source — so alignment can
        // time a track whose only lyrics live inside the file. `None` here = no
        // meta to even query with.
        other => {
            if let Some(emb) = tags::read_lyrics(f) {
                if !emb.trim().is_empty() {
                    return (Some(emb), PreCount::None);
                }
            }
            match other {
                None => c.no_meta.fetch_add(1, Ordering::Relaxed),
                _ => c.not_found.fetch_add(1, Ordering::Relaxed),
            };
            (None, PreCount::None)
        }
    }
}

/// Stamp the durable instrumental mark (journaled), surfacing any write error
/// instead of silently dropping it — a failed mark means we'd re-query forever.
fn mark_instrumental(f: &Path) {
    if let Err(e) = crate::journal::edit(f, || tags::set_instrumental_mark(f)) {
        ui::warn(&format!("could not mark {} instrumental: {e}", f.display()));
    }
}

/// Fetch lyrics from already-read tags. `None` means the file lacks the
/// artist+title needed to query (the caller's `no_meta` case).
fn fetch_meta(b: &tags::Basic, dur: Option<u64>, fallback: Option<&Fallback>) -> Option<Fetched> {
    let (artist, title) = (b.artist.as_deref()?, b.title.as_deref()?);
    Some(fetch(artist, title, b.album.as_deref(), dur, fallback))
}

/// High-confidence "this is an instrumental" from the track title alone: an
/// explicit parenthesised/bracketed "(Instrumental)" / "(Instrumental Version)"
/// or a trailing "- Instrumental". Deliberately does NOT treat vague markers
/// (Intro / Interlude / Score) as instrumental — those frequently have vocals,
/// and this decision is durable (it stamps the file), so it must be conservative.
fn title_says_instrumental(title: &str) -> bool {
    let t = title.to_ascii_lowercase();
    t.contains("(instrumental")
        || t.contains("[instrumental")
        || t.ends_with("- instrumental")
        || t.ends_with("- instrumental version")
}

/// True if the `.lrc` carries at least one `[mm:ss]` timestamp tag — i.e. it's
/// synced. Metadata-only tags like `[ar:…]`/`[length:…]` start with a letter, so
/// they don't count; a plain lyric file has no bracketed timestamps at all.
pub fn is_synced(lrc: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(lrc) else { return false };
    text.lines().any(|line| {
        let Some(rest) = line.trim_start().strip_prefix('[') else { return false };
        let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        digits > 0 && rest[digits..].starts_with(':')
    })
}

/// Marker line stamped on lyrics we generated ourselves via forced alignment —
/// the "Generated" tier (between plain and a trusted external synced source). A
/// standard LRC metadata line, so players ignore it while amdl can recognise it.
pub const ALIGN_MARKER: &str = "[re:amdl-align]";

/// Whether lyric text is one we generated (carries [`ALIGN_MARKER`]).
pub fn is_generated(text: &str) -> bool {
    text.contains(ALIGN_MARKER)
}

/// Overwrite an existing plain `.lrc` with synced content, backing up the old
/// bytes for `undo`. Returns false (no change recorded) if the content is
/// identical or the write fails.
fn upgrade_lrc(path: &Path, old: &[u8], synced: &str) -> bool {
    if synced.as_bytes() == old {
        return false;
    }
    if std::fs::write(path, synced).is_err() {
        return false;
    }
    crate::journal::replaced(path, old);
    true
}

/// Embed `text` into `f`'s `LYRICS` tag when `should_embed` allows it (journaled
/// for `undo`). Returns true iff the tag was written.
fn embed_lyrics(f: &Path, text: &str, force: bool) -> bool {
    let current = tags::read_lyrics(f);
    if !should_embed(current.as_deref(), text, force) {
        return false;
    }
    crate::journal::edit(f, || tags::set_lyrics(f, text)).is_ok()
}

/// Whether to (over)write an embedded lyric. The never-downgrade rule: embed
/// when nothing is there, or only as a genuine upgrade (plain → synced).
/// Identical content is left alone; a synced embed is never replaced by plain,
/// nor churned by another synced, unless `force` overrides.
fn should_embed(current: Option<&str>, candidate: &str, force: bool) -> bool {
    match current {
        None => true,
        Some(cur) if cur == candidate => false,
        Some(_) if force => true,
        Some(cur) => !is_synced(cur.as_bytes()) && is_synced(candidate.as_bytes()),
    }
}

fn write_lrc(path: &Path, content: &str) -> bool {
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let ok = std::fs::write(path, content).is_ok();
    if ok {
        crate::journal::created(path); // undo: delete this new .lrc
    }
    ok
}

/// An LrcApi-compatible server (HisAtri/LrcApi): base URL + `Authorization` key.
#[derive(Clone)]
pub struct LrcApi {
    pub url: String,
    pub key: String,
}

/// The configured secondary source and how it's prioritized against lrclib.net.
#[derive(Clone)]
pub struct Fallback {
    pub api: LrcApi,
    /// Query the LrcApi server before lrclib.net (flip the default priority).
    pub first: bool,
}

/// Fetch lyrics for a track from the two sources in priority order. The primary
/// (lrclib.net by default, or the LrcApi server when `lrcapi_first`) is queried
/// first; a *synced* hit there short-circuits. Otherwise the secondary is
/// consulted and the better result wins — synced beats plain beats none, and the
/// primary wins ties. With no `fallback` configured, only lrclib.net is used.
fn fetch(artist: &str, title: &str, album: Option<&str>, dur: Option<u64>, fallback: Option<&Fallback>) -> Fetched {
    let Some(fb) = fallback else {
        return fetch_lrclib(artist, title, album, dur);
    };
    let api = &fb.api;
    let (primary, secondary): (Fetched, &dyn Fn() -> Fetched) = if fb.first {
        (fetch_lrcapi(api, artist, title, album), &|| fetch_lrclib(artist, title, album, dur))
    } else {
        (fetch_lrclib(artist, title, album, dur), &|| fetch_lrcapi(api, artist, title, album))
    };
    if matches!(primary, Fetched::Synced(_)) {
        return primary; // best possible from the primary — no need to consult the other
    }
    best_of(primary, secondary())
}

/// Rank a result so cross-source merges can prefer the richer one.
fn rank(f: &Fetched) -> u8 {
    match f {
        Fetched::Synced(_) => 3,
        Fetched::Plain(_) => 2,
        Fetched::Instrumental => 1,
        Fetched::NotFound => 0,
    }
}

/// The better of two results; ties keep `a` (the primary/lrclib source).
fn best_of(a: Fetched, b: Fetched) -> Fetched {
    if rank(&b) > rank(&a) {
        b
    } else {
        a
    }
}

/// lrclib.net: try the exact `get` (artist/title/album/duration), then fall back
/// to `search`. Prefers synced lyrics; reports instrumentals.
fn fetch_lrclib(artist: &str, title: &str, album: Option<&str>, dur: Option<u64>) -> Fetched {
    let mut req = crate::http::agent().get("https://lrclib.net/api/get")
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
    let search = crate::http::agent().get("https://lrclib.net/api/search")
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

/// LrcApi (HisAtri/LrcApi): `GET {url}/jsonapi?title=&artist=&album=`, the key in
/// the `Authorization` header. Returns a JSON array of candidates whose `lyrics`
/// field is LRC text; take the first synced one, else the first non-empty plain.
fn fetch_lrcapi(api: &LrcApi, artist: &str, title: &str, album: Option<&str>) -> Fetched {
    let mut req = crate::http::agent().get(&format!("{}/jsonapi", api.url.trim_end_matches('/')))
        .set("User-Agent", UA)
        .set("Authorization", &api.key)
        .query("title", title)
        .query("artist", artist);
    if let Some(al) = album {
        req = req.query("album", al);
    }
    let Ok(resp) = req.call() else { return Fetched::NotFound };
    let Ok(text) = resp.into_string() else { return Fetched::NotFound };
    parse_lrcapi(&text)
}

/// Parse an LrcApi `/jsonapi` response body: a JSON array whose entries carry a
/// `lyrics` field of LRC text. Return the first synced entry, else the first
/// non-empty plain one, else NotFound.
fn parse_lrcapi(body: &str) -> Fetched {
    let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(body) else { return Fetched::NotFound };
    let mut plain: Option<String> = None;
    for v in &arr {
        let Some(lrc) = v.get("lyrics").and_then(|x| x.as_str()) else { continue };
        let lrc = lrc.trim();
        if lrc.is_empty() {
            continue;
        }
        if is_synced(lrc.as_bytes()) {
            return Fetched::Synced(lrc.to_string());
        }
        if plain.is_none() {
            plain = Some(lrc.to_string());
        }
    }
    plain.map(Fetched::Plain).unwrap_or(Fetched::NotFound)
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
    upgraded: AtomicUsize,
    embedded: AtomicUsize,
    aligned: AtomicUsize,
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
            upgraded: self.upgraded.into_inner(),
            embedded: self.embedded.into_inner(),
            aligned: self.aligned.into_inner(),
            not_found: self.not_found.into_inner(),
            instrumental: self.instrumental.into_inner(),
            no_meta: self.no_meta.into_inner(),
            skipped: self.skipped.into_inner(),
        }
    }
}

fn is_audio(p: &Path) -> bool {
    matches!(p.extension().and_then(|e| e.to_str()), Some("opus") | Some("m4a") | Some("mp3"))
}

/// Collect audio files under `root`. `root` may be a **single audio file** (act
/// on just it) or a **directory** (recurse) — same as `tag`/`identify`, so an
/// agent can target one track (e.g. `lyrics song.opus --force-embed`).
fn list_audio(root: &Path) -> Vec<PathBuf> {
    if root.is_file() {
        return if is_audio(root) { vec![root.to_path_buf()] } else { Vec::new() };
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out.retain(|p| is_audio(p));
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

/// Minimum overall confidence to accept an aligned result — below this, leave
/// the track plain (a wrong sync is worse than none).
const ALIGN_MIN_CONF: f64 = 0.5;

/// POST the audio + plain lyric lines to the amdl-aligner service and assemble a
/// synced `.lrc` (marked `[re:amdl-align]` = Generated tier) from the returned
/// per-line timings. `None` if the service errors or confidence is too low.
fn align_track(url: &str, audio_path: &Path, plain: &str) -> Option<String> {
    let audio = std::fs::read(audio_path).ok()?;
    let filename = audio_path.file_name()?.to_string_lossy().to_string();
    let boundary = "amdlaligner7c3f9b2boundary";
    let mut body = Vec::new();
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(&audio);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"lyrics\"\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(plain.as_bytes());
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    // Alignment is slow (seconds on GPU, longer on CPU) — allow a generous read.
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(10))
        .timeout_read(std::time::Duration::from_secs(600))
        .build();
    let resp = agent
        .post(&format!("{}/align", url.trim_end_matches('/')))
        .set("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
        .send_bytes(&body)
        .ok()?;
    let text = resp.into_string().ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let conf = v.get("overall_conf").and_then(|x| x.as_f64()).unwrap_or(0.0);
    let lines = v.get("lines")?.as_array()?;
    if conf < ALIGN_MIN_CONF || lines.is_empty() {
        return None;
    }
    let mut rows: Vec<(f64, String)> = lines
        .iter()
        .filter_map(|l| Some((l.get("start")?.as_f64()?, l.get("text")?.as_str()?.to_string())))
        .collect();
    rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut out = format!("{ALIGN_MARKER}\n");
    for (start, text) in rows {
        out.push_str(&format!("[{}]{}\n", fmt_ts(start), text));
    }
    Some(out)
}

/// Seconds → LRC timestamp `mm:ss.xx`.
fn fmt_ts(sec: f64) -> String {
    let s = sec.max(0.0);
    let m = (s / 60.0) as u64;
    format!("{:02}:{:05.2}", m, s - (m as f64) * 60.0)
}

#[cfg(test)]
mod tests {
    use super::{best_of, is_synced, parse_lrcapi, should_embed, title_says_instrumental, Fetched};

    #[test]
    fn lrcapi_parse_prefers_synced_then_plain() {
        // synced entry wins even if a plain one comes first
        let body = r#"[{"lyrics":"plain words\nno timing"},{"lyrics":"[00:01.00]hi\n[00:03.00]yo"}]"#;
        assert!(matches!(parse_lrcapi(body), Fetched::Synced(_)));
        // no synced anywhere → first non-empty plain
        assert!(matches!(parse_lrcapi(r#"[{"lyrics":"just words"}]"#), Fetched::Plain(_)));
        // empty / missing / malformed → NotFound
        assert!(matches!(parse_lrcapi(r#"[]"#), Fetched::NotFound));
        assert!(matches!(parse_lrcapi(r#"[{"lyrics":"   "}]"#), Fetched::NotFound));
        assert!(matches!(parse_lrcapi("not json"), Fetched::NotFound));
    }

    #[test]
    fn fallback_merge_prefers_synced_then_keeps_primary() {
        // fallback's synced beats the primary's plain (the whole point of a fallback)
        assert!(matches!(best_of(Fetched::Plain("p".into()), Fetched::Synced("s".into())), Fetched::Synced(_)));
        // primary's synced is never displaced by a fallback plain
        assert!(matches!(best_of(Fetched::Synced("s".into()), Fetched::Plain("p".into())), Fetched::Synced(_)));
        // fallback fills a primary miss
        assert!(matches!(best_of(Fetched::NotFound, Fetched::Plain("p".into())), Fetched::Plain(_)));
        // primary is kept over a weaker/equal fallback (ties favor lrclib)
        assert!(matches!(best_of(Fetched::Plain("p".into()), Fetched::NotFound), Fetched::Plain(_)));
        match best_of(Fetched::Plain("A".into()), Fetched::Plain("B".into())) {
            Fetched::Plain(s) => assert_eq!(s, "A", "tie keeps the primary source"),
            _ => panic!("expected plain"),
        }
    }

    const PLAIN: &str = "just words\nno timing";
    const SYNCED: &str = "[00:01.00]timed line\n[00:04.00]second";
    const SYNCED2: &str = "[00:02.00]different timing\n[00:05.00]second";

    #[test]
    fn embed_when_nothing_present() {
        assert!(should_embed(None, PLAIN, false));
        assert!(should_embed(None, SYNCED, false));
    }

    #[test]
    fn embed_never_downgrades_or_churns() {
        // synced already embedded: don't replace with plain, don't churn synced.
        assert!(!should_embed(Some(SYNCED), PLAIN, false));
        assert!(!should_embed(Some(SYNCED), SYNCED2, false));
        // identical is always a no-op.
        assert!(!should_embed(Some(SYNCED), SYNCED, false));
        assert!(!should_embed(Some(PLAIN), PLAIN, false));
        // plain already embedded, candidate also plain → not an upgrade → skip.
        assert!(!should_embed(Some(PLAIN), "other plain text", false));
    }

    #[test]
    fn embed_upgrades_plain_to_synced() {
        assert!(should_embed(Some(PLAIN), SYNCED, false));
    }

    #[test]
    fn force_overrides_never_downgrade() {
        assert!(should_embed(Some(SYNCED), PLAIN, true));
        assert!(should_embed(Some(SYNCED), SYNCED2, true));
        // even force won't rewrite byte-identical content.
        assert!(!should_embed(Some(SYNCED), SYNCED, true));
    }

    #[test]
    fn synced_detects_timestamp_tags() {
        assert!(is_synced(b"[00:12.34]a line\n[00:15.00]another"));
        assert!(is_synced(b"[01:02]no centiseconds is still timed"));
        // A synced file usually carries metadata tags before the first timestamp.
        assert!(is_synced(b"[ar:Artist]\n[ti:Title]\n[00:01.00]first sung line"));
    }

    #[test]
    fn plain_and_metadata_only_are_not_synced() {
        assert!(!is_synced(b"just a plain lyric\nsecond line, no timing"));
        assert!(!is_synced(b"")); // empty
        // Metadata tags start with a letter, not digits — not a timestamp.
        assert!(!is_synced(b"[ar:Rick Astley]\n[length:03:33]\nplain words here"));
        // A leading bracket with no digit-then-colon is not a timestamp.
        assert!(!is_synced(b"[chorus]\nwords"));
    }

    #[test]
    fn title_instrumental_is_explicit_and_conservative() {
        // Explicit markers → instrumental (the wrong-lyrics failure bucket).
        for t in [
            "What The Hell (Instrumental)",
            "The Family Madrigal (Instrumental Version)",
            "Wizards in Winter (Instrumental)",
            "Some Song [Instrumental]",
            "A Tune - Instrumental",
        ] {
            assert!(title_says_instrumental(t), "{t} should read as instrumental");
        }
        // Vague / unrelated markers must NOT — these can have vocals.
        for t in [
            "Intro",
            "Waterline (intro)",
            "Nest (Contains Instrumental Excerpt)",
            "Instrumental Break Up", // a real vocal song title
            "My Favourite Things",
        ] {
            assert!(!title_says_instrumental(t), "{t} must not read as instrumental");
        }
    }
}
