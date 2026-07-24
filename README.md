# amdl

A **music-library harness**: it validates audio, transcodes to space-efficient
**Opus**, keeps tags and cover art consistent, and repairs a broken library — so a
large collection stays clean and uniform. It's a thin wrapper around
[`gamdl`](https://github.com/glomatico/gamdl) and `ffmpeg`; **amdl's value is the
harness *around* those tools** — the validation, conversion, tagging, and
library-maintenance pipeline — not the acquisition.

**Composable by design.** All logic lives in the reusable **`amdl-core`** crate,
and every batch command can emit machine-readable **`--json`**, so scripts and
agents can chain steps and decide between them. Config is optional — a lazy
`~/.config/amdl/config.toml` only holds durable defaults (see `amdl config`).
Source directories are treated as **read-only input**; everything is written to
the **output** directory. See **[WORKFLOWS.md](WORKFLOWS.md)** for end-to-end
recipes (build a library, health-scan, repair truncation).

> **Acquisition is [gamdl](https://github.com/glomatico/gamdl)'s job, not amdl's.**
> amdl orchestrates gamdl and processes whatever it produces into a tidy library.
> Install and use gamdl per its own documentation, terms, and your local laws —
> that part is at your own risk. amdl supplies only the library-management harness.

## Install

**Homebrew** (macOS & Linux) — pours a prebuilt bottle on x86_64 Linux, so no
compiler/build tools are needed; pulls `gamdl` + `ffmpeg` as deps:

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

## Login cookies (for gamdl)

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

### Headless / server (no browser)

Point amdl at a cookies file, or hand it the cookie text directly via an
environment variable:

```sh
amdl download --cookies /path/cookies.txt 'https://music.apple.com/…'
export AMDL_COOKIES_FILE=/path/cookies.txt      # same, as an env var
export AMDL_COOKIES="$(cat cookies.txt)"        # or the cookie text itself (no file)
```

`$AMDL_COOKIES` is lenient about format — a Netscape `cookies.txt` (tab- **or**
space-separated, incl. `#HttpOnly_` lines) **or** a `document.cookie` /
`Cookie:`-header string (`name=value; name=value`); amdl keeps the `apple.com`
cookies and normalises them. Run `amdl cookies` to check what it resolves.

Interactively, if no browser is found `download` also offers to let you paste the
cookies right in the terminal (finish with a blank line or Ctrl-D).

## Commands

```sh
amdl <command> [options]
```

| Command | What it does |
|---------|--------------|
| `download <url>…` | Fetch via gamdl, then validate → Opus → into your library. |
| `convert [src] [dest]` | Transcode `.m4a`/`.mp3` → Opus with **fidelity**: re-embeds the source cover as a real `METADATA_BLOCK_PICTURE`, strips iTunes junk, mirrors `.lrc`, skip-existing (resumable). Paths default to config. |
| `doctor [output]` | Health/integrity scan: missing covers/tags, unreadable, and — with `--source` — **truncated** (decoded vs source duration) + **unconverted** source files. |
| `covers [output]` | Backfill missing covers: copy-from-source + cross-library (`--reference`), validated + square-cropped, per album; numbered straggler list for the rest. `--dry-run`. |
| `config [--init]` | Show the config path + values; `--init` writes a starter `~/.config/amdl/config.toml`. |
| `recover <dir>` | *(planned)* Re-acquire broken/missing library files by their metadata. |
| `cookies` | Report which login cookies amdl would use, no download. |

`--json` (global) makes `convert`/`doctor`/`config` emit machine-readable output
for scripting. Common flags: `--bitrate 192k`, `-j/--jobs`, `-o/--out`,
`--cookies`, `--storefront dk`, `--fallback us,gb`, `--work-dir`, `--keep-work`,
`--no-convert`. Also `--version`, `--help`, shell completions, and a man page per
command (`man amdl`, `man amdl-convert` where installed).

## Typical use

```sh
amdl config --init                         # optional: set default source/output once
amdl convert ~/Music/originals ~/Music/lib # m4a/mp3 → Opus (covers, lyrics, junk-stripped)
amdl doctor ~/Music/lib --source ~/Music/originals   # what still needs fixing
amdl doctor ~/Music/lib --json | jq '.truncated'     # feed a script/agent
amdl download 'https://music.apple.com/dk/album/…' -o ~/Music/lib   # acquire + convert
```

Repair a truncated Opus (silent-disconnect damage): `doctor` finds it → delete
the bad `.opus` → re-run `convert` (skip-existing regenerates only the deleted
one). Full recipes in **[WORKFLOWS.md](WORKFLOWS.md)**.

## Status / roadmap

**Done:** `download`; `convert` with real cover embedding + junk-strip + `.lrc`
mirroring + skip-existing; the `doctor` health/integrity scan; `covers` backfill
(copy-from-source + cross-library, validated + square-cropped, dry-run, straggler
list); `--json`; the `amdl-core` library crate; and lazy config. Parallel
throughout, animated bars.

**Planned** (ported from the original Python tool, adapted to the generic
source→output model): `covers` **network passes** (MusicBrainz/CAA → iTunes →
Discogs waterfall + numbered "paste a URL" human tail), `lyrics` (LRCLIB),
`recover` (detect → metadata fallback → cross-library copy → re-acquire),
`identify` (AcoustID), first-class `tag` ops, and Navidrome delivery. See
[WORKFLOWS.md](WORKFLOWS.md).

## License

MIT
