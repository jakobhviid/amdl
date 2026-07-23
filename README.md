# amdl

Apple Music → validated → **Opus**, into your library — a true command-line tool
around [`gamdl`](https://github.com/glomatico/gamdl) (download/decrypt) and
`ffmpeg` (validate/convert). **No config file:** paths and knobs are flags, and
the output defaults to the current folder (run it where you want the files).

> For your **own** Apple Music subscription/library. Needs your Apple Music login
> (cookies) and a subscription — `amdl` just orchestrates tools you already have.

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

## Cookies (Apple Music login)

`amdl` needs your Apple Music session. It resolves it in this order:

1. `--cookies <file>` if you pass one,
2. gamdl's own `~/.gamdl/cookies.txt`,
3. **auto-extracted from an installed browser** — Chrome, Chromium, Firefox,
   Brave, or Vivaldi,
4. if none: it asks you to log in at `https://music.apple.com` in one of those
   browsers, then retries.

## Commands

```sh
amdl <command> [options]
```

| Command | What it does |
|---------|--------------|
| `download <url>…` | gamdl download → decode-validate → Opus, into the output folder (default: CWD). |
| `convert <src> [dest]` | Transcode an existing library of `.m4a` to Opus (no Apple auth needed). |
| `recover <dir>` | *(planned)* Re-acquire broken/missing files from Apple Music by their metadata. |

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
