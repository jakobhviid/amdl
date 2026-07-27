# amdl — Audio Metadata & Lyrics Maintainer

**A**udio **M**eta**d**ata & **L**yrics Maintainer — make the music library
you've piled up over the years feel good to use.

It's the maintenance layer for a real-world collection: the mixed-format,
multi-decade pile of tracks you actually own, from purchases to rips. amdl turns
decades of MP3s, m4a, and CD rips with patchy tags into one uniform, enriched
library — standardized to space-efficient **Opus**, with complete tags, cover
art, and synced lyrics — then keeps it consistent as it grows (`doctor`,
`dedup`, `recover`, `undo`), so it feels great in a self-hosted server like
[Navidrome](https://www.navidrome.org/).

**Composable by design.** All logic lives in the reusable **`amdl-core`** crate,
and every batch command can emit machine-readable **`--json`**, so scripts and
agents can chain steps and decide between them. Source directories are treated as
**read-only input**; everything is written to the **output** directory, and every
mutating run is journaled so `amdl undo` can revert it. Config is optional — a
lazy `~/.config/amdl/config.toml` only holds durable defaults (see `amdl config`).
See **[WORKFLOWS.md](WORKFLOWS.md)** for end-to-end recipes (build a library,
health-scan, covers, identify, recover, de-dup, undo).

**LLM/agent-friendly.** amdl is built to be driven by an LLM as much as a human:
every batch command takes `--json`, and **`amdl --llm`** (a documentation flag,
like `--help`) prints one self-contained guide — every command *and* the full
end-to-end workflows — so an agent can learn the whole tool from a single call and
drive it from zero. Every `--help` and the man page also carry the repo URL for
source inspection.

## Install

**Homebrew** (macOS & Linux) — pours a prebuilt bottle on macOS (both arches) and
x86_64 Linux, so no compiler/build tools are needed; pulls `gamdl` + `ffmpeg` as
deps:

```sh
brew install jakobhviid/tap/amdl
```

**Or paste one line** — no Homebrew, no compiler, no root (installs to
`~/.local/bin`). Fetches only the `amdl` binary — install `gamdl` + `ffmpeg`
separately:

```sh
curl -fsSL https://raw.githubusercontent.com/jakobhviid/amdl/main/install.sh | sh
```

Or build from source (needs Rust):

```sh
cargo build --release                   # → ./target/release/amdl
```

**Runtime deps:** `gamdl` + `ffmpeg`/`ffprobe` (both in Homebrew). Nothing else —
`amdl` is a single self-contained binary.

## Commands

```sh
amdl <command> [options]
```

| Command | What it does |
|---------|--------------|
| `convert [src] [dest]` | Transcode `.m4a`/`.mp3`/`.flac` → Opus with **fidelity**: re-embeds the source cover as a real `METADATA_BLOCK_PICTURE`, strips iTunes junk, mirrors `.lrc`, skip-existing (resumable). Paths default to config. |
| `tag <path>` | Set tags across a file/folder — `--compilation` groups a Various-Artists album (`albumartist=Various Artists` + `compilation=1`); also `--album/--artist/--album-artist`. `--dry-run`. |
| `identify <path>` | Fix untagged/mis-tagged tracks by **sound** (AcoustID fingerprint via `fpcalc`); `--apply` writes artist/title/album only at/above `--min-score` (default 0.9 — a wrong tag is worse than none). `--dry-run`, `--skip-tagged`. Needs `[keys] acoustid`. |
| `covers [output]` | Backfill missing covers — funnel: source → cross-library (`--reference`) → `--online` waterfall (MB/CAA→iTunes→Discogs) → `--paste` human tail (or `--paste-file` for scripts). Validated + square-cropped, per album, artist+album-gated. `--dry-run`. |
| `lyrics [output]` | LRCLIB backfill — write synced (preferred) or plain `.lrc` **sidecars** into the library. State-only. **By default** it fetches missing lyrics *and* re-times existing **plain** files to synced when a source has them; `--no-upgrade` does the cheap fill-gaps-only pass. `--embed` also writes lyrics into the audio file's `LYRICS` tag (sidecar kept; never downgrades a synced embed unless `--force-embed`). An optional [LrcApi](https://github.com/HisAtri/LrcApi)-compatible secondary server (`[lyrics]` in config) supplies synced lyrics lrclib.net lacks. Alignment (a self-hosted [amdl-aligner](https://github.com/jakobhviid/amdl-aligner) service, `aligner_url` in config) *generates* synced lyrics from plain ones by listening to the track, for the residue no source has timed — including tracks whose only lyrics live in the `LYRICS` tag (marked `[re:amdl-align]`); it runs **by default once `aligner_url` is set**, `--no-align` opts out. Instrumentals are skipped automatically (explicit "(Instrumental)" titles, or a source's instrumental flag) and stamped `AMDL_INSTRUMENTAL` so they're never re-queried; fallback matches are verified against the file's duration/album to avoid wrong-song matches. For an instrumental whose lyrics are wrong at the source, `--mark-instrumental <path>` strips its lyrics (sidecar + tag) and marks it so it's skipped forever (`--unmark-instrumental` reverses). All journaled for `undo`. |
| `doctor [output]` | Health/integrity scan: missing covers/tags, unreadable, and — with `--source` — **truncated** (decoded vs source duration) + **unconverted** source files. `--deep` full-decodes every Opus to catch **corruption** with no source needed. |
| `stats [library]` | Read-only overview: track/artist/album counts, total size + playtime, format / bitrate / sample-rate / channel distributions, cover + tag completeness, and lyrics state at every level (sidecar + embedded, split plain/generated/synced). Scans all audio, not just Opus. `--json`. |
| `dedup [output]` | **Surface** (never delete) redundant tracks: exact-duplicate recordings + subset editions (Standard ⊂ Deluxe), with paths to remove and which copy to keep. `--print-rm` emits `rm` lines for you to review. |
| `recover [output]` | Re-acquire tracks a source never converted: cross-library copy from `--reference`, else `--online` re-acquire via gamdl (verified on title+duration). Recovered tracks are **regrouped** to their album siblings (so they don't split out under Apple's own album tag). `--dry-run`. |
| `undo` | Revert a mutating run — deletes files amdl created, restores tags/covers it changed. Never clobbers edits you made since. On a terminal a bare `undo` opens an interactive picker (dated, newest-first); `--list`, `<run-id>`, `--dry-run`, `--json` stay non-interactive. |
| `config [--init]` | Show the config path + values; `--init` writes a starter `~/.config/amdl/config.toml`. |
| `configure <set\|unset\|get\|list\|keys>` | Read/write individual settings. Keys work as words or dotted and booleans take on/off — e.g. `configure set lyrics hints off`, `configure set lyrics.aligner_url http://…`, `configure get paths output`. Every write re-renders the full annotated `config.toml`, so its inline help is always preserved. `configure --help`/`configure keys` list every settable key. |
| `download <url>…` | *(optional acquisition)* Fetch via gamdl, then validate → Opus → into your library. See **[Acquisition](#acquisition-optional)**. |
| `cookies` | *(optional acquisition)* Report which login cookies amdl would use, no download. |

`--json` (global) makes every batch command emit a machine-readable report
for scripting. Common flags: `--bitrate 192k`, `-j/--jobs`, `-o/--out`,
`--cookies`, `--storefront dk`, `--fallback us,gb`, `--work-dir`, `--keep-work`,
`--no-convert`, and `-q/--quiet` (headline only) / `-v/--verbose` (per-item
detail). Result summaries are severity-colored — a failure never hides in a green
line — and carry next-step hints. Also `--version`, `--help`, shell completions, a man page
(`man amdl`), and **`--llm`** — a documentation flag (like `--help`) that dumps a
single machine-readable guide (commands + workflows + repo link) for an LLM/agent.
Every `--help` and the man page also carry the repo URL so an agent can inspect
the source.

## Typical use

```sh
amdl config --init                         # optional: set default source/output once
amdl configure set paths.output ~/Music/lib   # ...or set settings programmatically (scriptable)
amdl convert ~/Music/originals ~/Music/lib # m4a/mp3 → Opus (covers, lyrics, junk-stripped)
amdl doctor ~/Music/lib --source ~/Music/originals   # what still needs fixing
amdl doctor ~/Music/lib --json | jq '.truncated'     # feed a script/agent
amdl lyrics ~/Music/lib                    # backfill synced .lrc from LRCLIB
amdl undo                                            # reverted a run? undo it
```

Every mutating run is journaled, so `amdl undo` reverts the last one (and never
clobbers edits you made since). Repair a truncated Opus (silent-disconnect
damage): `doctor` finds it → delete the bad `.opus` → re-run `convert`
(skip-existing regenerates only the deleted one). Full recipes in
**[WORKFLOWS.md](WORKFLOWS.md)**.

## Acquisition (optional)

amdl is a *maintainer*, not a downloader. Its job starts **after** the files
exist. As a convenience it can also kick off a fetch through
[`gamdl`](https://github.com/glomatico/gamdl) and immediately process what comes
back, so `download` (and `recover --online`) are a single command:

```sh
amdl download 'https://music.apple.com/dk/album/…' -o ~/Music/lib   # acquire → validate → Opus
```

> **Acquisition is [gamdl](https://github.com/glomatico/gamdl)'s job, not
> amdl's.** amdl only *thin-wraps* a slice of gamdl (kick off a fetch, then
> process what it produces). Anything about the acquisition itself — codecs, DRM,
> storefronts, login quirks, download failures — is **gamdl's domain: take those
> requests and bugs to the gamdl team, not here.** Install and use gamdl per its
> own documentation, terms, and your local laws — that part is at your own risk.

### Login cookies (for gamdl)

gamdl needs a browser login session to fetch. amdl helps resolve one, in order:

1. `--cookies <file>` (or `$AMDL_COOKIES_FILE`) if set,
2. `$AMDL_COOKIES` — the raw cookie text itself (headless/CI, no file on disk),
3. gamdl's own `~/.gamdl/cookies.txt` — **if it isn't expired**,
4. **auto-extracted from an installed browser** — Chrome, Chromium, Firefox,
   Brave, Vivaldi, Edge, Arc, and (on macOS) **Safari**,
5. if the file/browser cookies look **expired**, or none are found: it warns you
   to log in again at `https://music.apple.com` (and, in a terminal, offers to
   let you paste them).

amdl never writes to `~/.gamdl`; browser cookies are cached under
`~/.cache/amdl/` and passed to gamdl via `--cookies-path`. Run `amdl cookies` to
check what amdl would use, without downloading anything.

> **macOS:** reading cookies from a Chromium browser (Chrome/Brave/Edge/Arc)
> needs the "Chrome Safe Storage" key from your login Keychain — macOS may prompt
> for access the first time. Safari and Firefox need no prompt.

#### Headless / server (no browser)

Point amdl at a cookies file, or hand it the cookie text directly via an
environment variable:

```sh
amdl download --cookies /path/cookies.txt 'https://music.apple.com/…'
cat cookies.txt | amdl download --cookies - 'https://music.apple.com/…'   # pipe via stdin
export AMDL_COOKIES_FILE=/path/cookies.txt      # same, as an env var
export AMDL_COOKIES="$(cat cookies.txt)"        # or the cookie text itself (no file)
```

`$AMDL_COOKIES` is lenient about format — a Netscape `cookies.txt` (tab- **or**
space-separated, incl. `#HttpOnly_` lines) **or** a `document.cookie` /
`Cookie:`-header string (`name=value; name=value`); amdl keeps the `apple.com`
cookies and normalises them. Run `amdl cookies` to check what it resolves.

Interactively, if no browser is found `download` also offers to let you paste the
cookies right in the terminal (finish with a blank line or Ctrl-D).

## AI disclosure

Parts of this codebase were written with the assistance of AI coding agents
(Claude Code, opencode, and others). All changes were reviewed by the maintainer.

## License

MIT
