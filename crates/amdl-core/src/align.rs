//! Forced lyric alignment — generate *synced* lyrics from *plain* ones by
//! listening to the track. No separate service to run: point `[lyrics]
//! whisper_url` at any OpenAI-compatible whisper.cpp transcription endpoint and
//! alignment happens in-process.
//!
//! The endpoint gives us a word stream — each spoken word with an absolute
//! `start` time. We diff that stream against the *known* lyric words (the
//! `difflib.SequenceMatcher` matching-block algorithm) to map each lyric line to
//! a timestamp, interpolate lines whisper never heard, and force the result
//! monotonic. Onset accuracy is ~0.7 s (the "Generated" tier).
//!
//! We consume the endpoint's native `segments[].words[]` array (verbose_json +
//! word timestamps) directly, so each word carries a real `start`/`end` — no
//! per-word server tricks needed.
use std::collections::HashMap;
use std::path::Path;

/// A whisper transcription endpoint used for alignment. `url` is the base
/// (e.g. `http://192.168.1.6:8080`); we append `/v1/audio/transcriptions`.
#[derive(Clone, Debug)]
pub struct Whisper {
    pub url: String,
    /// Model id the endpoint exposes, e.g. `whisper-large-v3-turbo`.
    pub model: String,
    /// Optional bearer token — sent as `Authorization: Bearer <key>` when set.
    /// Unset for an open LAN endpoint (the common self-hosted case).
    pub key: Option<String>,
}

/// The default whisper model id (whisper.cpp large-v3-turbo, as fronted by
/// llama-swap). Used when the user doesn't pin `[lyrics] whisper_model`.
pub const DEFAULT_MODEL: &str = "whisper-large-v3-turbo";

/// Query `{url}/v1/models` and return the ids that look like whisper/speech-to-text
/// models (case-insensitive `whisper`/`stt`/`transcrib`/`speech` in the id).
/// Empty on any failure or when the endpoint mixes in only non-STT models — the
/// caller then falls back to letting the user type a model id. `key`, if set, is
/// sent as a bearer token (some endpoints gate even `/v1/models`).
pub fn whisper_models(url: &str, key: Option<&str>) -> Vec<String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(10))
        .timeout_read(std::time::Duration::from_secs(30))
        .build();
    let mut req = agent.get(&format!("{}/v1/models", url.trim_end_matches('/')));
    if let Some(k) = key {
        req = req.set("Authorization", &format!("Bearer {k}"));
    }
    let Ok(resp) = req.call() else { return Vec::new() };
    let Ok(text) = resp.into_string() else { return Vec::new() };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return Vec::new() };
    let Some(data) = v.get("data").and_then(|d| d.as_array()) else { return Vec::new() };
    data.iter()
        .filter_map(|m| m.get("id").and_then(|i| i.as_str()))
        .filter(|id| {
            let l = id.to_lowercase();
            ["whisper", "stt", "transcrib", "speech"].iter().any(|k| l.contains(k))
        })
        .map(|s| s.to_string())
        .collect()
}

/// One aligned lyric line: its original index/text and the timings we resolved.
#[derive(Debug, Clone)]
pub struct AlignLine {
    pub i: usize,
    pub text: String,
    pub start: f64,
    pub end: f64,
    pub conf: f64,
}

/// The result of aligning a whole lyric to a track.
#[derive(Debug, Clone)]
pub struct AlignResult {
    pub lines: Vec<AlignLine>,
    pub overall_conf: f64,
}

/// Align known plain-lyric `lines` to `audio_path` by transcribing the audio on
/// `w` and diffing the word stream against the lyrics. Returns per-line timings
/// + overall confidence, or `None` if transcription fails or nothing matched.
///
/// Language is always auto-detected — a real library mixes languages track to
/// track, so there's nothing useful a single hint could say.
pub fn align(w: &Whisper, audio_path: &Path, lines: &[&str]) -> Option<AlignResult> {
    let words = transcribe(w, audio_path)?;
    align_words(lines, &words)
}

/// The pure half of [`align`]: map lyric `lines` onto an already-transcribed
/// `(start, word)` stream. Split out so it's unit-testable without a network.
fn align_words(lines: &[&str], words: &[(f64, String)]) -> Option<AlignResult> {
    // Flatten known lyric lines into (line_index, normalized word) pairs.
    let mut known: Vec<(usize, String)> = Vec::new();
    for (li, raw) in lines.iter().enumerate() {
        for word in norm(raw).split_whitespace() {
            known.push((li, word.to_string()));
        }
    }
    if known.is_empty() || words.is_empty() {
        return None;
    }

    let known_words: Vec<&str> = known.iter().map(|(_, w)| w.as_str()).collect();
    let heard_words: Vec<&str> = words.iter().map(|(_, w)| w.as_str()).collect();

    let mut starts: HashMap<usize, f64> = HashMap::new();
    let mut ends: HashMap<usize, f64> = HashMap::new();
    let mut matched: HashMap<usize, usize> = HashMap::new();
    let mut total: HashMap<usize, usize> = HashMap::new();
    for (li, _) in &known {
        *total.entry(*li).or_insert(0) += 1;
    }
    for (a, b, size) in matching_blocks(&known_words, &heard_words) {
        for k in 0..size {
            let li = known[a + k].0;
            let t = words[b + k].0;
            let s = starts.entry(li).or_insert(f64::INFINITY);
            *s = s.min(t);
            let e = ends.entry(li).or_insert(0.0);
            *e = e.max(t);
            *matched.entry(li).or_insert(0) += 1;
        }
    }

    // One row per non-empty line; `start`/`end` = None until timed/interpolated.
    let mut rows: Vec<Row> = Vec::new();
    for (li, raw) in lines.iter().enumerate() {
        if norm(raw).is_empty() {
            continue;
        }
        if let Some(&st) = starts.get(&li) {
            let conf = matched.get(&li).copied().unwrap_or(0) as f64
                / total.get(&li).copied().unwrap_or(1).max(1) as f64;
            rows.push(Row {
                i: li,
                text: raw.trim().to_string(),
                start: Some(st),
                end: Some(ends.get(&li).copied().unwrap_or(st)),
                conf: round3(conf),
            });
        } else {
            rows.push(Row { i: li, text: raw.trim().to_string(), start: None, end: None, conf: 0.0 });
        }
    }
    interpolate(&mut rows);
    monotonic(&mut rows);

    let lines_out: Vec<AlignLine> = rows
        .iter()
        .filter_map(|r| {
            let start = r.start?;
            Some(AlignLine { i: r.i, text: r.text.clone(), start: round2(start), end: round2(r.end.unwrap_or(start)), conf: r.conf })
        })
        .collect();
    let confs: Vec<f64> = rows.iter().filter(|r| r.start.is_some()).map(|r| r.conf).collect();
    let overall_conf = if confs.is_empty() { 0.0 } else { round3(confs.iter().sum::<f64>() / confs.len() as f64) };
    Some(AlignResult { lines: lines_out, overall_conf })
}

/// Working row during alignment (mirrors the Python list `[i, text, start, end, conf]`).
struct Row {
    i: usize,
    text: String,
    start: Option<f64>,
    end: Option<f64>,
    conf: f64,
}

/// Normalize lyric/word text for matching: drop `[…]` tags, lowercase, turn every
/// non-alphanumeric, non-space char into a space, then collapse whitespace. Port
/// of the container's `_norm` (`re.sub(r"\[[^\]]*\]", " ", s).lower()` + filter).
fn norm(s: &str) -> String {
    let stripped = strip_brackets(s).to_lowercase();
    let spaced: String = stripped
        .chars()
        .map(|c| if c.is_alphanumeric() || c.is_whitespace() { c } else { ' ' })
        .collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Replace every complete `[…]` group with a single space (matches Python's
/// `re.sub(r"\[[^\]]*\]", " ", s)`). A lone unmatched `[` is left as-is — `norm`
/// turns it into a space at the non-alphanumeric step, same as Python.
fn strip_brackets(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '[' {
            if let Some(rel) = chars[i + 1..].iter().position(|&c| c == ']') {
                out.push(' ');
                i += rel + 2; // skip "[…]" including the closing ']'
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Longest-matching-block decomposition of two token slices, equivalent to
/// Python `difflib.SequenceMatcher(None, a, b, autojunk=False).get_matching_blocks()`
/// (minus the trailing zero-length sentinel, which contributes no mappings).
/// Returns `(a_index, b_index, len)` blocks; overlapping index runs are exactly
/// the pairs the container mapped word→time over.
fn matching_blocks(a: &[&str], b: &[&str]) -> Vec<(usize, usize, usize)> {
    // b2j: for each token value, the sorted indices where it appears in `b`.
    let mut b2j: HashMap<&str, Vec<usize>> = HashMap::new();
    for (j, &tok) in b.iter().enumerate() {
        b2j.entry(tok).or_default().push(j);
    }

    let mut blocks = Vec::new();
    // Explicit stack of (alo, ahi, blo, bhi) regions still to decompose.
    let mut queue = vec![(0usize, a.len(), 0usize, b.len())];
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let (i, j, k) = longest_match(a, &b2j, alo, ahi, blo, bhi);
        if k > 0 {
            blocks.push((i, j, k));
            if alo < i && blo < j {
                queue.push((alo, i, blo, j));
            }
            if i + k < ahi && j + k < bhi {
                queue.push((i + k, ahi, j + k, bhi));
            }
        }
    }
    blocks.sort_unstable();
    blocks
}

/// `find_longest_match` from difflib (no junk, `autojunk=False`): the longest
/// block `a[i..i+k] == b[j..j+k]` within `a[alo..ahi]` × `b[blo..bhi]`, preferring
/// the earliest `i`, then earliest `j`.
fn longest_match(
    a: &[&str],
    b2j: &HashMap<&str, Vec<usize>>,
    alo: usize,
    ahi: usize,
    blo: usize,
    bhi: usize,
) -> (usize, usize, usize) {
    let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
    // j2len[j] = length of the match ending at a[i-1], b[j-1]. Rebuilt per i.
    let mut j2len: HashMap<usize, usize> = HashMap::new();
    for (i, &ai) in a.iter().enumerate().take(ahi).skip(alo) {
        let mut newj2len: HashMap<usize, usize> = HashMap::new();
        if let Some(js) = b2j.get(ai) {
            for &j in js {
                if j < blo {
                    continue;
                }
                if j >= bhi {
                    break;
                }
                let k = j.checked_sub(1).and_then(|jm| j2len.get(&jm)).copied().unwrap_or(0) + 1;
                newj2len.insert(j, k);
                if k > bestsize {
                    besti = i + 1 - k;
                    bestj = j + 1 - k;
                    bestsize = k;
                }
            }
        }
        j2len = newj2len;
    }
    (besti, bestj, bestsize)
}

/// Fill untimed rows by linear interpolation between the nearest timed neighbors,
/// clamping to the single neighbor at the ends and 0.0 if nothing is timed.
/// Port of the container's `_interpolate`.
fn interpolate(rows: &mut [Row]) {
    let n = rows.len();
    for idx in 0..n {
        if rows[idx].start.is_some() {
            continue;
        }
        let lo = (0..idx).rev().find(|&j| rows[j].start.is_some());
        let hi = (idx + 1..n).find(|&j| rows[j].start.is_some());
        let start = match (lo, hi) {
            (Some(lo), Some(hi)) => {
                let frac = (idx - lo) as f64 / (hi - lo) as f64;
                rows[lo].start.unwrap() + frac * (rows[hi].start.unwrap() - rows[lo].start.unwrap())
            }
            (Some(lo), None) => rows[lo].start.unwrap(),
            (None, Some(hi)) => rows[hi].start.unwrap(),
            (None, None) => 0.0,
        };
        rows[idx].start = Some(start);
        rows[idx].end = Some(start);
    }
}

/// Force non-decreasing starts and `end >= start`. Port of `_monotonic`.
fn monotonic(rows: &mut [Row]) {
    let mut last = 0.0f64;
    for r in rows.iter_mut() {
        let mut s = r.start.unwrap_or(last);
        if s < last {
            s = last;
        }
        last = s;
        let e = r.end.unwrap_or(s).max(s);
        r.start = Some(s);
        r.end = Some(e);
    }
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}
fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// Transcribe `audio_path` on the whisper endpoint and return the `(start, word)`
/// stream (words normalized, empties dropped). `None` on any transport/parse
/// failure. Sends the audio bytes as-is — whisper.cpp's `--convert` accepts
/// opus/m4a/mp3, so no client-side WAV conversion is needed.
fn transcribe(w: &Whisper, audio_path: &Path) -> Option<Vec<(f64, String)>> {
    let audio = std::fs::read(audio_path).ok()?;
    let filename = audio_path.file_name()?.to_string_lossy().to_string();
    let boundary = "amdlwhisper7c3f9b2boundary";

    // Hand-built multipart body: the audio file plus the OpenAI transcription
    // fields. `timestamp_granularities[]=word` is REQUIRED — without it the
    // response carries segment text only, no per-word `words[]` array.
    let mut body = Vec::new();
    // File part first (raw bytes, so written directly).
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(&audio);
    body.extend_from_slice(b"\r\n");
    for (name, value) in [
        ("model", w.model.as_str()),
        ("response_format", "verbose_json"),
        ("timestamp_granularities[]", "word"),
        ("temperature", "0"),
    ] {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes(),
        );
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

    // First call may warm the model (llama-swap lazy-loads it) — allow a generous
    // read. Connect stays short so an unreachable host fails fast.
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(10))
        .timeout_read(std::time::Duration::from_secs(600))
        .build();
    let endpoint = format!("{}/v1/audio/transcriptions", w.url.trim_end_matches('/'));
    let mut req = agent
        .post(&endpoint)
        .set("Content-Type", &format!("multipart/form-data; boundary={boundary}"));
    if let Some(key) = &w.key {
        req = req.set("Authorization", &format!("Bearer {key}"));
    }
    let resp = req.send_bytes(&body).ok()?;
    let text = resp.into_string().ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;

    // Words live under segments[].words[] (verbose_json + word granularity).
    // Fall back to a top-level words[] if a provider puts them there instead.
    let mut out = Vec::new();
    let collect = |arr: &serde_json::Value, out: &mut Vec<(f64, String)>| {
        if let Some(ws) = arr.as_array() {
            for word in ws {
                let start = word.get("start").and_then(|x| x.as_f64());
                let text = word.get("word").and_then(|x| x.as_str());
                if let (Some(start), Some(text)) = (start, text) {
                    let n = norm(text);
                    if !n.is_empty() {
                        out.push((start, n));
                    }
                }
            }
        }
    };
    if let Some(segs) = v.get("segments").and_then(|s| s.as_array()) {
        for seg in segs {
            if let Some(ws) = seg.get("words") {
                collect(ws, &mut out);
            }
        }
    }
    if out.is_empty() {
        if let Some(ws) = v.get("words") {
            collect(ws, &mut out);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_strips_tags_punct_and_case() {
        assert_eq!(norm("You'll say, \"We've got nothin'\""), "you ll say we ve got nothin");
        assert_eq!(norm("[Chorus] Hello"), "hello");
        assert_eq!(norm("  MiXeD   CASE\tand\ntabs "), "mixed case and tabs");
        // lone '[' with no ']' degrades to a space, like Python's regex leaves it
        // then the non-alnum filter blanks it.
        assert_eq!(norm("a [ b"), "a b");
        assert_eq!(norm("[strip all]"), "");
    }

    #[test]
    fn matching_blocks_maps_common_runs() {
        // Classic difflib example: abcd vs bcde → the "bcd" run at a[1..4]/b[0..3].
        let a = ["a", "b", "c", "d"];
        let b = ["b", "c", "d", "e"];
        assert_eq!(matching_blocks(&a, &b), vec![(1, 0, 3)]);
        // Two separated runs are both found.
        let a = ["x", "one", "y", "two"];
        let b = ["one", "z", "two"];
        let mut got = matching_blocks(&a, &b);
        got.sort_unstable();
        assert_eq!(got, vec![(1, 0, 1), (3, 2, 1)]);
    }

    #[test]
    fn align_words_times_lines_and_interpolates() {
        // Two lyric lines; whisper heard line 0's words but not line 1's, so
        // line 1 must be interpolated (clamped to line 0 here — it's the tail).
        let lines = ["hello world", "unheard line"];
        let words = vec![(1.0_f64, "hello".to_string()), (2.0, "world".to_string())];
        let r = align_words(&lines, &words).unwrap();
        assert_eq!(r.lines.len(), 2);
        assert_eq!(r.lines[0].i, 0);
        assert_eq!(r.lines[0].start, 1.0);
        assert_eq!(r.lines[0].end, 2.0);
        assert!((r.lines[0].conf - 1.0).abs() < 1e-9);
        // Interpolated tail line: only a preceding timed neighbor, so it clamps
        // to that neighbor's *start* (1.0), matching Python's `_interpolate`.
        assert_eq!(r.lines[1].start, 1.0);
        assert!(r.lines[1].conf.abs() < 1e-9);
    }

    #[test]
    fn align_words_enforces_monotonic_starts() {
        // Whisper heard "b" before "a" (out of lyric order); starts must not go
        // backwards after monotonic().
        let lines = ["a", "b"];
        let words = vec![(5.0_f64, "b".to_string()), (9.0, "a".to_string())];
        let r = align_words(&lines, &words).unwrap();
        let starts: Vec<f64> = r.lines.iter().map(|l| l.start).collect();
        assert!(starts.windows(2).all(|w| w[0] <= w[1]), "starts not monotonic: {starts:?}");
    }

    #[test]
    fn align_words_empty_inputs_return_none() {
        assert!(align_words(&["hello"], &[]).is_none());
        assert!(align_words(&[], &[(1.0, "hello".to_string())]).is_none());
        assert!(align_words(&["[tag only]"], &[(1.0, "hello".to_string())]).is_none());
    }
}
