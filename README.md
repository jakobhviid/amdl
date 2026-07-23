# amdl

A **music-library harness**: it validates audio, transcodes to space-efficient
**Opus**, keeps tags and cover art consistent, and repairs a broken library — so a
large collection stays clean and uniform. It's a thin wrapper around
[`gamdl`](https://github.com/glomatico/gamdl) and `ffmpeg`; **amdl's value is the
harness *around* those tools** — the validation, conversion, tagging, and
library-maintenance pipeline — not the acquisition. **No config file:** paths and
knobs are flags, and output defaults to the current folder.

> **Acquisition is [gamdl](https://github.com/glomatico/gamdl)'s job, not amdl's.**
> amdl orchestrates gamdl and processes whatever it produces into a tidy library.
> Install and use gamdl per its own documentation, terms, and your local laws —
> that part is at your own risk. amdl supplies only the library-management harness.

## Install

```sh
brew install jakobhviid/tap/amdl        # Linux; pulls gamdl + ffmpeg as deps
```

Or build from source (needs Rust):

```sh
cargo build --release                   # → ./target/release/amdl
```

**Runtime deps:** `gamdl` + `ffmpeg`/`ffprobe` (both in Homebrew). Nothing else —
`amdl` is a single self-contained binary.

## Login cookies (for gamdl)

gamdl needs a browser login session to fetch. amdl helps resolve one, in order:

1. `--cookies <file>` if you pass one,
2. gamdl's own `~/.gamdl/cookies.txt` — **if it isn't expired**,
3. **auto-extracted from an installed browser** — Chrome, Chromium, Firefox,
   Brave, or Vivaldi,
4. if the file/browser cookies look **expired**, or none are found: it warns you
   to log in again at `https://music.apple.com` in one of those browsers, then
   retries.

amdl never writes to `~/.gamdl`; browser cookies are cached under
`~/.cache/amdl/` and passed to gamdl via `--cookies-path`. Run `amdl cookies` to
check what amdl would use, without downloading anything.

### Headless / server (no browser)

With no browser to read, amdl accepts cookies you paste or pipe. The parser is
lenient — it takes a Netscape `cookies.txt` (tab- **or** space-separated, incl.
`#HttpOnly_` lines) **or** a `document.cookie` / `Cookie:`-header string
(`name=value; name=value`), keeps the `apple.com` cookies, and normalises them:

```sh
amdl download --cookies - 'https://music.apple.com/…' < cookies.txt   # pipe a file
pbpaste | amdl cookies                                                # validate a paste
```

Interactively, if no browser is found `download` offers to let you paste the
cookies right in the terminal (finish with a blank line or Ctrl-D).

## Commands

```sh
amdl <command> [options]
```

| Command | What it does |
|---------|--------------|
| `download <url>…` | Fetch via gamdl, then validate → Opus → into your library (default: CWD). |
| `convert <src> [dest]` | Transcode an existing library of `.m4a` to Opus (no login needed). |
| `recover <dir>` | *(planned)* Re-acquire broken/missing library files by their metadata. |
| `cookies` | Report which login cookies amdl would use (gamdl's file + browser auto-detect), no download. |

Common flags: `-o/--out <dir>` (default CWD), `--cookies`, `--bitrate 192k`,
`-j/--jobs`, `--storefront dk`, `--fallback us,gb`, `--work-dir`, `--keep-work`,
`--no-convert`. Also `--version`, `--help`, shell completions, man page.

## Typical use

```sh
cd ~/Music/opus
amdl download 'https://music.apple.com/dk/album/…'   # → Opus here, cookies auto-detected
amdl download 'https://music.apple.com/…' --bitrate 128k -j 12
amdl convert ~/Music/m4a ~/Music/opus                # transcode an existing library
```

## Status / roadmap

MVP: `download` + `convert` with parallel conversion, progress bars, storefront
fallback, and cookie auto-detection. **Being ported from the original Python
tool** and refined with a real test corpus:

- precise tag handling (keep Picard/MusicBrainz tags, strip iTunes junk) and cover
  art as a spec-correct `METADATA_BLOCK_PICTURE` — first cut copies metadata via
  `ffmpeg -map_metadata`;
- `recover` (metadata → re-acquire), and later `lyrics`, Navidrome delivery, and
  playlist sync.

## License

MIT
