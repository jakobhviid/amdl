//! stats — a read-only overview of a music library: formats, bitrate/sample-rate/
//! channel distributions, cover + tag completeness, and the state of lyrics at
//! every level (sidecar `.lrc` and embedded `LYRICS` tag, split into plain /
//! generated / synced tiers). One pass over the tree, nothing is written.
//!
//! It scans **all** audio it recognises (not just Opus), so it works on a source
//! library or a derived one. Pairs with `doctor` (which finds *problems*); this
//! answers "what's in here and what shape is it in".
use crate::{lyrics, tags, ui};
use rayon::prelude::*;
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Extensions counted as audio tracks. Everything else in the tree (`.lrc`,
/// artwork, playlists, …) is ignored.
const AUDIO_EXTS: &[&str] = &["opus", "m4a", "mp3", "flac", "ogg", "oga", "aac", "wav", "aiff", "aif", "wma", "m4b", "alac"];

/// A `name → count` pair; the JSON-friendly shape for every distribution.
#[derive(Debug, Serialize)]
pub struct Count {
    pub name: String,
    pub count: usize,
}

/// Lyric coverage by tier for one channel (sidecar or embedded).
#[derive(Debug, Default, Serialize)]
pub struct TierCounts {
    /// No lyrics present in this channel.
    pub none: usize,
    /// Plain (untimed) lyrics.
    pub plain: usize,
    /// Synced lyrics we generated ourselves (marked `[re:amdl-align]`).
    pub generated: usize,
    /// Synced lyrics from a trusted source (timestamps, no generated marker).
    pub synced: usize,
}

#[derive(Debug, Default, Serialize)]
pub struct LyricStats {
    /// Tier breakdown of `.lrc` sidecars next to each track.
    pub sidecar: TierCounts,
    /// Tier breakdown of the embedded `LYRICS` tag inside each track.
    pub embedded: TierCounts,
    /// Tracks with timed lyrics (synced or generated) in *either* channel.
    pub timed_anywhere: usize,
    /// Tracks that have only plain lyrics — nothing timed anywhere.
    pub plain_only: usize,
    /// Tracks with no lyrics at all, in either channel.
    pub none_anywhere: usize,
}

#[derive(Debug, Default, Serialize)]
pub struct BitrateStats {
    /// Histogram over nominal buckets (in bucket order), known bitrates only.
    pub distribution: Vec<Count>,
    /// kbps min / mean / max over tracks with a known bitrate.
    pub min: u32,
    pub avg: u32,
    pub max: u32,
    /// Tracks whose bitrate couldn't be read.
    pub unknown: usize,
}

#[derive(Debug, Serialize)]
pub struct Stats {
    pub root: String,
    /// Total audio tracks found.
    pub tracks: usize,
    /// Tracks that couldn't be probed (corrupt / unsupported).
    pub unreadable: usize,
    pub total_bytes: u64,
    pub total_duration_secs: u64,
    /// Count per container/extension, most common first.
    pub formats: Vec<Count>,
    pub bitrate_kbps: BitrateStats,
    /// Count per sample rate (Hz), most common first.
    pub sample_rates: Vec<Count>,
    /// Count per channel layout (mono / stereo / N ch), most common first.
    pub channels: Vec<Count>,
    pub with_cover: usize,
    pub without_cover: usize,
    pub missing_title: usize,
    pub missing_artist: usize,
    pub missing_album: usize,
    /// Tracks with title + artist + album all present.
    pub fully_tagged: usize,
    /// Distinct artists (by artist tag).
    pub artists: usize,
    /// Distinct albums (by album-artist/artist + album).
    pub albums: usize,
    pub lyrics: LyricStats,
}

/// Lyric quality tiers, ordered `None < Plain < Generated < Synced` so the two
/// channels can be combined by taking the better one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Tier {
    None,
    Plain,
    Generated,
    Synced,
}

fn classify(bytes: &[u8]) -> Tier {
    match std::str::from_utf8(bytes) {
        Err(_) => Tier::Plain, // non-UTF8 but present → treat as (plain) content
        Ok(s) if s.trim().is_empty() => Tier::None,
        Ok(s) if lyrics::is_generated(s) => Tier::Generated,
        Ok(s) if lyrics::is_synced(s.as_bytes()) => Tier::Synced,
        Ok(_) => Tier::Plain,
    }
}

fn tally_tier(c: &mut TierCounts, t: Tier) {
    match t {
        Tier::None => c.none += 1,
        Tier::Plain => c.plain += 1,
        Tier::Generated => c.generated += 1,
        Tier::Synced => c.synced += 1,
    }
}

/// Per-file scan result, aggregated in [`collect`].
struct Record {
    ext: String,
    bytes: u64,
    readable: bool,
    duration: u64,
    bitrate: Option<u32>,
    sample_rate: Option<u32>,
    channels: Option<u8>,
    has_cover: bool,
    missing_title: bool,
    missing_artist: bool,
    missing_album: bool,
    artist_key: Option<String>,
    album_key: Option<(String, String)>,
    sidecar: Tier,
    embedded: Tier,
}

fn probe(path: &Path) -> Record {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    // Sidecar lyrics live next to the track as `<name>.lrc`.
    let sidecar = match std::fs::read(path.with_extension("lrc")) {
        Ok(b) => classify(&b),
        Err(_) => Tier::None,
    };
    match tags::read_meta(path) {
        None => Record {
            ext,
            bytes,
            readable: false,
            duration: 0,
            bitrate: None,
            sample_rate: None,
            channels: None,
            has_cover: false,
            missing_title: true,
            missing_artist: true,
            missing_album: true,
            artist_key: None,
            album_key: None,
            sidecar,
            embedded: Tier::None,
        },
        Some(m) => {
            let embedded = m.lyrics.as_deref().map(|s| classify(s.as_bytes())).unwrap_or(Tier::None);
            let artist_key = m.artist.as_ref().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
            let album_key = m.album.as_ref().and_then(|al| {
                let al = al.trim();
                if al.is_empty() {
                    return None;
                }
                // Group by album-artist when present (so a compilation is one album),
                // else fall back to the track artist.
                let owner = m
                    .album_artist
                    .as_ref()
                    .or(m.artist.as_ref())
                    .map(|s| s.trim().to_lowercase())
                    .unwrap_or_default();
                Some((owner, al.to_lowercase()))
            });
            Record {
                ext,
                bytes,
                readable: true,
                duration: m.duration_secs,
                bitrate: m.bitrate_kbps,
                sample_rate: m.sample_rate,
                channels: m.channels,
                has_cover: m.has_cover,
                missing_title: m.title.as_deref().map(str::trim).unwrap_or("").is_empty(),
                missing_artist: m.artist.as_deref().map(str::trim).unwrap_or("").is_empty(),
                missing_album: m.album.as_deref().map(str::trim).unwrap_or("").is_empty(),
                artist_key,
                album_key,
                sidecar,
                embedded,
            }
        }
    }
}

/// Nominal bitrate bucket for `kbps` (VBR encodes land near their target).
fn bitrate_bucket(kbps: u32) -> &'static str {
    match kbps {
        0..=112 => "≤112k",
        113..=144 => "128k",
        145..=176 => "160k",
        177..=224 => "192k",
        225..=288 => "256k",
        289..=352 => "320k",
        _ => ">320k",
    }
}
const BITRATE_BUCKETS: &[&str] = &["≤112k", "128k", "160k", "192k", "256k", "320k", ">320k"];

fn channel_label(ch: u8) -> String {
    match ch {
        1 => "mono".into(),
        2 => "stereo".into(),
        n => format!("{n} ch"),
    }
}

/// Turn a `key → count` map into a `Vec<Count>` sorted by count desc, then name.
fn ranked<K: ToString>(map: std::collections::HashMap<K, usize>) -> Vec<Count> {
    let mut v: Vec<Count> = map.into_iter().map(|(k, count)| Count { name: k.to_string(), count }).collect();
    v.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    v
}

/// Scan `root` and aggregate a full [`Stats`] overview. Read-only.
pub fn collect(root: &Path) -> Stats {
    let files = list_audio(root);
    let pb = ui::bar(files.len() as u64, "Scanning");
    let records: Vec<Record> = files
        .par_iter()
        .map(|p| {
            let r = probe(p);
            pb.inc(1);
            r
        })
        .collect();
    ui::finish_done(&pb);

    use std::collections::HashMap;
    let mut formats: HashMap<String, usize> = HashMap::new();
    let mut sample_rates: HashMap<u32, usize> = HashMap::new();
    let mut channels: HashMap<String, usize> = HashMap::new();
    let mut bitrate_hist: HashMap<&'static str, usize> = HashMap::new();
    let mut artists: HashSet<String> = HashSet::new();
    let mut albums: HashSet<(String, String)> = HashSet::new();

    let mut s = Stats {
        root: root.display().to_string(),
        tracks: records.len(),
        unreadable: 0,
        total_bytes: 0,
        total_duration_secs: 0,
        formats: Vec::new(),
        bitrate_kbps: BitrateStats::default(),
        sample_rates: Vec::new(),
        channels: Vec::new(),
        with_cover: 0,
        without_cover: 0,
        missing_title: 0,
        missing_artist: 0,
        missing_album: 0,
        fully_tagged: 0,
        artists: 0,
        albums: 0,
        lyrics: LyricStats::default(),
    };

    let (mut br_sum, mut br_n, mut br_min, mut br_max) = (0u64, 0u64, u32::MAX, 0u32);

    for r in &records {
        *formats.entry(r.ext.clone()).or_default() += 1;
        s.total_bytes += r.bytes;
        s.total_duration_secs += r.duration;

        if !r.readable {
            s.unreadable += 1;
        }
        if r.has_cover {
            s.with_cover += 1;
        } else {
            s.without_cover += 1;
        }
        if r.missing_title {
            s.missing_title += 1;
        }
        if r.missing_artist {
            s.missing_artist += 1;
        }
        if r.missing_album {
            s.missing_album += 1;
        }
        if !r.missing_title && !r.missing_artist && !r.missing_album {
            s.fully_tagged += 1;
        }
        if let Some(a) = &r.artist_key {
            artists.insert(a.clone());
        }
        if let Some(k) = &r.album_key {
            albums.insert(k.clone());
        }

        match r.bitrate {
            Some(k) => {
                *bitrate_hist.entry(bitrate_bucket(k)).or_default() += 1;
                br_sum += k as u64;
                br_n += 1;
                br_min = br_min.min(k);
                br_max = br_max.max(k);
            }
            None => s.bitrate_kbps.unknown += 1,
        }
        match r.sample_rate {
            Some(sr) => *sample_rates.entry(sr).or_default() += 1,
            None => *sample_rates.entry(0).or_default() += 1,
        }
        match r.channels {
            Some(c) => *channels.entry(channel_label(c)).or_default() += 1,
            None => *channels.entry("unknown".into()).or_default() += 1,
        }

        tally_tier(&mut s.lyrics.sidecar, r.sidecar);
        tally_tier(&mut s.lyrics.embedded, r.embedded);
        match r.sidecar.max(r.embedded) {
            Tier::Synced | Tier::Generated => s.lyrics.timed_anywhere += 1,
            Tier::Plain => s.lyrics.plain_only += 1,
            Tier::None => s.lyrics.none_anywhere += 1,
        }
    }

    s.formats = ranked(formats);
    s.sample_rates = ranked(sample_rates.into_iter().map(|(k, v)| (if k == 0 { "unknown".to_string() } else { k.to_string() }, v)).collect());
    s.channels = ranked(channels);
    // Bitrate histogram in nominal order (skip empty buckets).
    s.bitrate_kbps.distribution = BITRATE_BUCKETS
        .iter()
        .filter_map(|b| bitrate_hist.get(b).map(|&count| Count { name: (*b).to_string(), count }))
        .collect();
    if let Some(avg) = br_sum.checked_div(br_n) {
        s.bitrate_kbps.min = br_min;
        s.bitrate_kbps.max = br_max;
        s.bitrate_kbps.avg = avg as u32;
    }
    s.artists = artists.len();
    s.albums = albums.len();
    s
}

fn list_audio(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, &mut out);
    out.retain(|p| {
        p.extension()
            .and_then(|e| e.to_str())
            .map(|e| AUDIO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
            .unwrap_or(false)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_recognises_every_tier() {
        assert!(matches!(classify(b""), Tier::None));
        assert!(matches!(classify(b"   \n  "), Tier::None));
        assert!(matches!(classify(b"just some words\nno timestamps"), Tier::Plain));
        assert!(matches!(classify(b"[00:12.30]a line\n[00:15.00]next"), Tier::Synced));
        assert!(matches!(classify(b"[re:amdl-align]\n[00:12.30]a line"), Tier::Generated));
    }

    #[test]
    fn tier_ordering_picks_the_better_channel() {
        assert!(Tier::Synced > Tier::Generated);
        assert!(Tier::Generated > Tier::Plain);
        assert!(Tier::Plain > Tier::None);
        assert_eq!(Tier::Plain.max(Tier::Synced), Tier::Synced);
    }

    #[test]
    fn bitrate_buckets_map_vbr_near_nominal() {
        assert_eq!(bitrate_bucket(96), "≤112k");
        assert_eq!(bitrate_bucket(129), "128k");
        assert_eq!(bitrate_bucket(190), "192k");
        assert_eq!(bitrate_bucket(256), "256k");
        assert_eq!(bitrate_bucket(1000), ">320k");
    }
}
