# Moving lyric alignment into `amdl` (retire the `amdl-aligner` container)

**Status:** ✅ **IMPLEMENTED** (was: Recommended — MOVE IT). Alignment now lives in
`amdl-core::align`; config uses `[lyrics] whisper_url`/`whisper_model`/`whisper_key`.
The design below is kept as the rationale/record. Next: cut over config and retire
the container on Nous per §9.
**Date:** 2026-08-01
**Scope:** Replace the dedicated `amdl-aligner` HTTP service (Nous host `:8790`, `POST /align`)
with alignment logic living **inside `amdl`**, driven by the unified **llama-swap whisper
STT** endpoint (`http://192.168.1.6:8080/v1/audio/transcriptions`, model
`whisper-large-v3-turbo`). The aligner container is then retired.

---

## 1. Verdict & justification

**Move it.** The move is feasible and a net simplification, and it hinges on one empirical
question that came back **YES**:

> **Does the unified llama-swap `/v1/audio/transcriptions` return word-level timestamps?**
> **Yes — and richer than what the current aligner actually uses.**

The current aligner does *not* consume a word-timestamp array at all. It starts its own
`whisper-server` with `-ml 1 -sow` (max-segment-length = 1 word, split-on-word) so that each
**segment** collapses to a single word, then reads `segment.start` + `segment.text`
(`aligner.py` `transcribe_words`: `return [(s["start"], _norm(s["text"])) for s in segs …]`).
The unified endpoint gives us the *real* thing: a native `words[]` array with per-word
`start`, `end`, and `probability`. So the data we need is not merely present — it is
**strictly better** than the segment-start hack the container relies on today.

Because the alignment algorithm (difflib `SequenceMatcher` of *known lyric words* against the
*whisper word stream*) is pure post-processing on `(time, word)` pairs, it ports cleanly into
`amdl`. `amdl` already speaks HTTP + multipart + JSON here (`align_track` in
`crates/amdl-core/src/lyrics.rs`) and already depends on `ureq` and `serde_json`.

**Dependency shape is unchanged.** `amdl` already requires a *reachable service URL*
(`[lyrics] aligner_url`, today `http://192.168.1.6:8790`). After the move it requires a
reachable *whisper endpoint* (`http://192.168.1.6:8080`). Same "point at a box on the LAN"
story — we just repoint and move the ~40 lines of alignment math into Rust.

**What retiring the container buys us on Nous:** removes a second, always-installed
`whisper-server` process and a **duplicate `ggml-large-v3-turbo` GPU/RAM footprint** that
competes with the unified server for the exact same model. One whisper, one model, one place
to manage flags/VAD/model choice.

**What it costs:** the alignment math now lives in `amdl` (must be ported + tested), and a
user of `amdl` can no longer just `podman run` a self-contained aligner — they must have an
OpenAI-compatible whisper.cpp transcription endpoint that returns word timestamps (documented
in §7).

---

## 2. Evidence (what was actually observed)

### 2.1 The endpoints, live
- `GET http://127.0.0.1:8790/health` →
  `{"status":"ok","engine":"whisper.cpp","backend":"vulkan","model":"ggml-large-v3-turbo.bin",…}`
  — the aligner runs **large-v3-turbo**.
- `GET http://127.0.0.1:8080/v1/models` lists whisper via llama-swap (alias `whisper-1`).
- The unified endpoint uses **the same engine (whisper.cpp, Vulkan) and the same model
  (large-v3-turbo)** — so moving is **model-parity, no accuracy regression**.

### 2.2 Word timestamps ARE exposed through `:8080` (the linchpin)
Request (test fixture `/var/home/jakob/ai/aligner-test/dbs.opus`, 257 s):
```
curl http://127.0.0.1:8080/v1/audio/transcriptions \
  -F file=@dbs.wav \
  -F model=whisper-large-v3-turbo \
  -F response_format=verbose_json \
  -F 'timestamp_granularities[]=word' \
  -F 'timestamp_granularities[]=segment' \
  -F temperature=0
```
Response shape (observed):
```json
{
  "task": "transcribe", "language": "english", "duration": 257.43,
  "text": " You'll say we've got nothing in common …",
  "segments": [
    {
      "id": 0, "text": " You'll say we've got nothing in common, no common ground to",
      "start": 0.0, "end": 17.16, "temperature": 0, "avg_logprob": …, "no_speech_prob": …,
      "tokens": [ … ],
      "words": [
        {"word": " You",  "start": 0.45, "end": 2.0,  "t_dtw": -1, "probability": 0.573},
        {"word": "'ll",   "start": 2.0,  "end": 5.7,  "t_dtw": -1, "probability": 0.937},
        {"word": " say",  "start": 5.7,  "end": 9.42, "t_dtw": -1, "probability": 0.965}
        …
      ]
    },
    …
  ]
}
```
- **546 words** returned for the test track, each with absolute `start`/`end` (seconds) and a
  `probability`. This is exactly the `(time, word)` stream the SequenceMatcher needs.
- `t_dtw` is `-1` (DTW disabled) — same as the container default (its `WHISPER_DTW` env is
  empty). No regression.
- **Opus/m4a/mp3 upload works directly** — sending `dbs.opus` (no client-side WAV conversion)
  still returned 546 words. The in-image `--convert` ffmpeg handles non-WAV input, so `amdl`
  can POST the audio file as-is (it currently reads the raw file bytes and POSTs them — no
  code change needed there).
- **No auth** was required (plain `curl` from Nous succeeded).
- `model=whisper-1` (the alias) also works.
- `language=en` is honored and passes through to whisper.

### 2.3 The current algorithm (from the container source)
`podman exec systemd-amdl-aligner cat /app/app/aligner.py /app/app/main.py`:

1. `ffmpeg -i <audio> -ar 16000 -ac 1 a.wav` (16 kHz mono).
2. POST to its private `whisper-server` `/inference` with
   `response_format=verbose_json, temperature=0, language=<hint or "auto">`.
   The server was started with `-ml 1 -sow -sns -t 8` → one word per segment.
3. `_norm(s)`: strip `[...]` bracket tags, lowercase, keep alnum+space, collapse whitespace.
4. Build `known = [(line_index, word)]` from the caller's lyric lines (normalized, split).
5. Build `words = [(start_time, word)]` from the whisper segments.
6. `SequenceMatcher(None, [known words], [whisper words], autojunk=False)`; for every matching
   block, map each matched known-word's `line_index` to the whisper time → per line track
   `start = min(times)`, `end = max(times)`, and `matched/total` word ratio = `conf`.
7. `_interpolate` unmatched lines from neighbors; `_monotonic` enforces non-decreasing starts.
8. Return `{lines:[{i,text,start,end,conf}], overall_conf, engine, backend, device, warm, model}`.

### 2.4 How `amdl` consumes it today
`crates/amdl-core/src/lyrics.rs`:
- `align_track(url, audio_path, plain)` — builds a multipart body (`file`, `lyrics`), POSTs to
  `"{url}/align"` with `ureq` (10 s connect, 600 s read timeout), parses JSON.
- Rejects if `overall_conf < ALIGN_MIN_CONF (0.5)` or `lines` empty.
- Takes each `{start, text}`, sorts by `start`, emits an LRC prefixed with
  `ALIGN_MARKER = "[re:amdl-align]"` via `fmt_ts` (`mm:ss.xx`).
- Called from `backfill(...)` stage 1b when `opts.align` and the settled lyric text isn't
  already synced. Endpoint URL comes from config `[lyrics] aligner_url`
  (`crates/amdl-core/src/config.rs`).

---

## 3. Design of the moved logic

Everything the container's `aligner.py` does moves into `amdl-core`. Nothing about *when*
alignment runs changes (`backfill` stage 1b, `ALIGN_MIN_CONF`, LRC marker, undo journaling).
Only the innards of `align_track` change.

### 3.1 New module: `crates/amdl-core/src/align.rs`
Port of `aligner.py`. Public entry:
```rust
/// Align known plain-lyric `lines` to `audio_path` using a whisper transcription
/// endpoint. Returns per-line timings + overall confidence, or None on failure.
pub fn align(
    whisper_url: &str,      // e.g. "http://192.168.1.6:8080"
    model: &str,            // e.g. "whisper-large-v3-turbo"
    audio_path: &Path,
    lines: &[&str],
    language: Option<&str>, // ISO hint or None = auto
) -> Option<AlignResult>;

pub struct AlignLine { pub i: usize, pub text: String, pub start: f64, pub end: f64, pub conf: f64 }
pub struct AlignResult { pub lines: Vec<AlignLine>, pub overall_conf: f64 }
```

### 3.2 The exact HTTP call (replaces `POST /align`)
```
POST {whisper_url}/v1/audio/transcriptions          (multipart/form-data)
  file:                        <raw audio bytes, original filename/ext — opus/m4a/mp3 OK>
  model:                       whisper-large-v3-turbo
  response_format:             verbose_json
  timestamp_granularities[]:   word          # REQUIRED — without this, no words[] array
  temperature:                 0
  language:                    <hint>         # OMIT the field entirely for auto-detect
```
Notes:
- The `timestamp_granularities[]=word` field **must** be sent (repeated form field with the
  literal `[]` in the name) or you get segment-level text only. Verified: present → `words[]`
  populated; the OpenAI-style repeated-field encoding is what whisper.cpp's server expects.
- Keep `ureq` (already a dependency). Reuse `align_track`'s hand-built multipart writer; just
  add the extra text fields and change the path. Keep the 600 s read timeout (first call warms
  the model via llama-swap's lazy load, which can take seconds).
- Parse with `serde_json`. Flatten every `segments[].words[]` into `Vec<(f64 start, String word)>`.

### 3.3 Port of `_norm`
```rust
fn norm(s: &str) -> String {
    // 1) drop [ ... ] bracket tags, 2) lowercase,
    // 3) replace every non-alphanumeric, non-space char with a space,
    // 4) collapse runs of whitespace, trim.
}
```
Use a small regex (`regex` crate) or a hand rolled char scan for the bracket strip; the rest is
`char::is_alphanumeric` / `is_whitespace`. Unicode-lowercase to match Python's `.lower()`
closely enough for lyric text.

### 3.4 Port of the SequenceMatcher alignment
Rust std has no `difflib`. Use the **`similar`** crate (`similar = "2"`), which provides
matching-block extraction equivalent to `get_matching_blocks`:
```rust
use similar::{capture_diff_slices, Algorithm};
let known_words: Vec<&str> = /* flatten lines→norm→split, remember line index per word */;
let whisper_words: Vec<&str> = /* norm+split each word-token's text */;
let ops = capture_diff_slices(Algorithm::Myers, &known_words_str, &whisper_words_str);
for op in ops {
    // for each Equal{ old_index, new_index, len }:
    //   for k in 0..len:
    //     let li = known_line_of[old_index + k];
    //     let t  = whisper_time[new_index + k];
    //     starts[li] = starts[li].min(t); ends[li] = ends[li].max(t);
    //     matched[li] += 1;
}
```
Then replicate: `conf[li] = matched[li] / total_words[li]`, `_interpolate` (linear between
nearest timed neighbors; clamp at ends), `_monotonic` (non-decreasing starts, `end >= start`),
drop lines that normalize to empty, `overall_conf = mean(conf over timed lines)`.

> **Fidelity caveat:** Python's `SequenceMatcher` is Ratcliff–Obershelp (recursive
> longest-matching-block); `similar`'s Myers/LCS produce a slightly different but, for this
> "map word index → time" purpose, equivalent set of Equal blocks. Validate against the
> fixtures in §6. If block choice ever matters, `similar::algorithms::lcs` is closest in spirit.

### 3.5 Rewire `align_track`
`align_track` becomes a thin wrapper: call `align::align(...)`, apply the existing
`ALIGN_MIN_CONF` gate and the existing `{start,text}` → LRC assembly (`ALIGN_MARKER`, `fmt_ts`,
sort by start). No change to `backfill`, undo, embedding, or counters.

### 3.6 What each old response field maps to
| old `/align` field | new source |
|---|---|
| `lines[].{i,text,start,end,conf}` | computed locally in `align.rs` (as today) |
| `overall_conf` | computed locally; still gates via `ALIGN_MIN_CONF` |
| `engine/backend/device/warm/model` | dropped (they were diagnostics of the container; not used by `amdl`) |

---

## 4. Config changes in `amdl`

Today: one key, `[lyrics] aligner_url = "http://192.168.1.6:8790"`
(`config.rs`: `lyrics.aligner_url`, plumbed through `set/get/unset/render`, keys list, and the
annotated template).

Proposed (keep it minimal, keep back-compat behavior of "one URL enables alignment"):

```toml
[lyrics]
# Whisper transcription endpoint (OpenAI-compatible, whisper.cpp) used to GENERATE
# synced lyrics from plain ones. Must return word timestamps
# (verbose_json + timestamp_granularities[]=word). Setting this enables alignment.
whisper_url   = "http://192.168.1.6:8080"        # base URL; amdl appends /v1/audio/transcriptions
whisper_model = "whisper-large-v3-turbo"          # optional; this is the default
# whisper_language = "en"                          # optional ISO hint; omit for auto-detect
```

Migration for the existing key (do at least the first, ideally all):
1. **Rename** `aligner_url` → `whisper_url` in `config.rs` (struct field, key strings in the
   `set/get/unset/keys/render` match arms, and the template text/URL example).
2. Add optional `whisper_model` (default `whisper-large-v3-turbo`) and optional
   `whisper_language`.
3. **Back-compat shim (recommended):** if `whisper_url` is unset but the deprecated
   `aligner_url` is present in an old `config.toml`, read it as `whisper_url` and warn once.
   This avoids silently breaking users who already set `aligner_url`.
   (If you'd rather not carry the shim: bump the config version and print a one-line upgrade
   note pointing here.)
4. Update the `hints` nudge string in `crates/amdl/src/main.rs` (currently points at the
   `amdl-aligner` GitHub repo) to describe pointing at a whisper endpoint instead.
5. Update `README.md` and `WORKFLOWS.md` (they reference `amdl-aligner`, `:8790`, and the
   external repo — see §7).

---

## 5. Model, language, VAD, confidence — parity notes

- **Model choice.** The container already runs **large-v3-turbo**; so does `:8080`. Moving is
  neutral. General tradeoff worth recording for *the server's* whisper config: **turbo** has a
  4-layer decoder (vs 32 for **large-v3**), which makes it faster but slightly less robust on
  timestamp precision and more prone to end-of-track repetition/hallucination on sparse audio.
  For force-alignment we match against *known* lyrics, so hallucinated tails mostly fail to
  match and are harmless — the current production quality (~0.7 s median onset) is already on
  turbo. **If we ever want sharper onsets**, the levers are (a) offer a `large-v3` (non-turbo)
  whisper model on `:8080` and set `whisper_model` to it, and/or (b) enable whisper.cpp DTW
  (`-dtw large.v3.turbo`) on the server for `t_dtw` token timestamps. Neither is required for
  parity; call them out as future accuracy knobs.
- **Language.** Container passes `language or "auto"`; `:8080` honors the OpenAI `language`
  field and auto-detects when omitted. Parity — plumb `whisper_language` (omit for auto).
- **VAD.** Neither path uses VAD today. No change.
- **`-ml 1 -sow` (one-word segments).** *Not needed anymore* — we consume the native `words[]`
  array, which is the real word segmentation. This was only a trick to make the container's
  segment-start usable; dropping it is an improvement (we get true per-word `start` *and*
  `end`).
- **`-sns` (suppress non-speech tokens).** Minor. It suppressed `[music]`-style tokens at the
  server. We can't pass it per-request to `:8080`. Impact is negligible because (a) `_norm`
  already strips `[...]` tags before matching and (b) non-speech tokens don't match known
  lyric words. If it ever matters, add `-sns`/`--suppress-nst` to the server's whisper flags
  (helps all whisper consumers, not just alignment).
- **Confidence.** `conf` is a pure function of the SequenceMatcher matched/total ratio — ports
  exactly. Optionally enrich with the now-available per-word `probability`, but keep the
  matched-ratio for behavior parity and the existing `ALIGN_MIN_CONF = 0.5` gate.

---

## 6. Verification plan (before retiring anything)

Fixtures: `/var/home/jakob/ai/aligner-test/` — `dbs.opus`, `dbs.txt` (lyrics), `dbs.lrc`
(expected-ish output), `a1.json`/`a2.json` (sample `/align` responses).

1. **Golden reference:** run the *live container* once for the fixture and save its `/align`
   JSON (or reuse `a1.json`/`a2.json` / `dbs.lrc`).
2. **New path:** run the ported `align::align` against `:8080` on the same audio+lyrics.
3. **Compare:** per-line `start` should agree within a small tolerance (target ≤ ~0.5–1.0 s;
   the container itself claims ~0.7 s median onset). `overall_conf` should land in the same
   ballpark and clear `ALIGN_MIN_CONF`. Confirm the emitted `.lrc` (marker + `mm:ss.xx`) is
   byte-similar to `dbs.lrc`.
4. Add a unit test for `norm`, `_interpolate`, `_monotonic`, and a small hand-built
   SequenceMatcher case with known expected blocks.
5. Keep the container running in parallel until the new path passes on a batch of real tracks.

---

## 7. What to reference on "Nous" going forward

- **Endpoint:** `http://192.168.1.6:8080/v1/audio/transcriptions` (LAN),
  `http://127.0.0.1:8080/...` on Nous itself.
- **Model:** `whisper-large-v3-turbo` (alias `whisper-1` also works).
- **Auth:** none observed (open on the LAN). If auth is later added to llama-swap, `amdl` will
  need an `Authorization: Bearer …` knob — not required today.
- **Required params for alignment:** `response_format=verbose_json` **and**
  `timestamp_granularities[]=word`. Optional `temperature=0`, `language=<iso>`.
- Docs to update in this repo: `README.md` (the `lyrics` row mentions `amdl-aligner` +
  the external repo), `WORKFLOWS.md` (uses `aligner_url` + `:8790` + the external repo link),
  and `crates/amdl-core/src/config.rs` template/comments.

---

## 8. User configuration (a user can no longer just pull a container)

`amdl` alignment now needs **an OpenAI-compatible whisper.cpp transcription endpoint that
returns word timestamps**. Document this for users:

**If you have Nous (or any llama-swap/whisper.cpp box) on your LAN:**
```
amdl configure set lyrics whisper_url http://192.168.1.6:8080
# optional:
amdl configure set lyrics whisper_model whisper-large-v3-turbo
amdl lyrics /music/lib      # fetch + upgrade + align the residue
```

**If you have nothing, run your own whisper.cpp server (GPU recommended):**
```bash
# Build whisper.cpp with the server (pick your backend; Vulkan shown)
git clone https://github.com/ggerganov/whisper.cpp && cd whisper.cpp
cmake -B build -DWHISPER_VULKAN=ON        # or -DGGML_CUDA=ON, -DWHISPER_METAL=ON, or CPU
cmake --build build -j --config Release

# Download the model
./models/download-ggml-model.sh large-v3-turbo     # ~1.6 GB
#   (or large-v3 for slightly sharper timestamps)

# Run the OpenAI-compatible server. --convert lets it accept opus/m4a/mp3 directly.
./build/bin/whisper-server \
  -m models/ggml-large-v3-turbo.bin \
  --host 0.0.0.0 --port 8080 \
  --convert
```
Then:
```
amdl configure set lyrics whisper_url http://<that-host>:8080
```
**Requirements the endpoint must meet:** OpenAI route
`POST /v1/audio/transcriptions`, accepts `response_format=verbose_json` +
`timestamp_granularities[]=word`, and returns `segments[].words[]` with `start`/`end`. Any
whisper.cpp `whisper-server` (which is what Nous fronts via llama-swap) satisfies this. A
non-whisper.cpp OpenAI provider works too *iff* it returns word timestamps.

> Note the whisper.cpp server's native path is `/inference`; the OpenAI-compatible alias is
> `/v1/audio/transcriptions`. `amdl` uses the OpenAI path. `--convert` (in-image ffmpeg) means
> `amdl` can send `.opus`/`.m4a`/`.mp3` directly — no client-side WAV conversion.

---

## 9. Migration steps & safe retirement of `amdl-aligner` on Nous

1. **Implement** §3–§4 in `amdl`; land behind the renamed `whisper_url` (+ back-compat shim).
2. **Verify** per §6 with the container still up. Do a real batch run and diff generated LRCs.
3. **Cut over** config: `amdl configure set lyrics whisper_url http://192.168.1.6:8080`
   (or rely on the `aligner_url`→`whisper_url` shim). Run `amdl lyrics …`; confirm
   `[re:amdl-align]` LRCs are produced and pass the confidence gate.
4. **Stop the container** (reversible first):
   ```
   systemctl --user stop amdl-aligner        # or: systemctl stop  (whichever unit type)
   ```
   Leave it stopped for a bit; if anything regresses, `start` it and repoint `whisper_url`
   back to `:8790` (the old `/align` path still exists while the image is present).
5. **Retire for good** once confident:
   ```
   systemctl --user disable amdl-aligner
   podman rm -f systemd-amdl-aligner
   podman rmi localhost/amdl-aligner:latest
   # remove the quadlet/unit file (e.g. ~/.config/containers/systemd/amdl-aligner.container) and daemon-reload
   ```
   Frees the duplicate `ggml-large-v3-turbo` GPU/RAM footprint. The unified `:8080` whisper
   is the single remaining whisper on Nous.
6. **Archive** the `amdl-aligner` GitHub repo (or add a README pointing here), since `amdl`
   no longer references it.

---

## 10. Risk summary

| | |
|---|---|
| **Gained** | one whisper/model on Nous (no duplicate GPU load); alignment self-contained in `amdl`; richer per-word `start`+`end`; no separate service to build/run/monitor. |
| **Lost / cost** | must port + test SequenceMatcher in Rust (`similar`); `amdl` users need a reachable whisper endpoint (they already needed a reachable aligner — same shape); can't pass server-only flags (`-sns`) per request (negligible). |
| **Neutral** | model (large-v3-turbo both sides), language handling, DTW-off, confidence formula, no-VAD. |
| **Watch** | Myers-vs-Ratcliff block differences (validate on fixtures); llama-swap first-call warm-up latency (600 s read timeout already covers it); if llama-swap ever gains auth, add a token knob. |
