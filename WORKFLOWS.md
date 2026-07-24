# amdl workflows

End-to-end recipes for building and maintaining an Opus music library with
`amdl`. Written to be **driven by a human or an LLM/agent**: every batch command
takes `--json`, so a step's output can be inspected and fed to the next.

## Model (read this first)

- **Source** = a read-only *input* library (your originals: `.m4a`/`.mp3` +
  optional `.lrc` sidecars). amdl **never writes to the source.**
- **Output** = the *derived* library amdl produces and maintains (`.opus` +
  mirrored `.lrc`, player-visible covers). Everything is written here.
- Set defaults once so you can omit paths: `amdl config --init`, then edit
  `~/.config/amdl/config.toml` (`[paths] source=… output=…`). Flags always
  override config.
- `--json` on `convert`/`doctor`/`config` prints structured results; pipe to `jq`.

## Convention for agents

Prefer this loop: **act → `doctor --json` → decide from the counts → act again.**
`doctor` is the map; it never changes anything, so it's safe to call repeatedly.

---

## W1 — Build a library (convert with fidelity)

Transcode a source tree to Opus. Covers are re-embedded as real
`METADATA_BLOCK_PICTURE` (players see them), iTunes junk tags are stripped, and
`.lrc` sidecars are mirrored into the output. Skip-existing makes it resumable —
re-running costs nothing for already-converted files.

```sh
amdl convert /music/originals /music/lib          # explicit paths
amdl convert                                       # uses config [paths]
amdl convert /music/originals /music/lib --json    # {converted, skipped, failed, with_cover, lrc_copied}
```

Convert and `lyrics` (below) touch different outputs (`.opus` vs `.lrc`) and are
CPU- vs network-bound, so running them in parallel halves wall-clock:

```sh
amdl convert /music/originals /music/lib &        # CPU-bound (ffmpeg×N)
amdl lyrics  /music/lib &                          # network-bound (LRCLIB)
wait
```

### lyrics — LRCLIB backfill (state-only)

For every track missing a sibling `.lrc`, fetch synced (preferred) or plain
lyrics and write the `.lrc` into the library. Skip-existing, parallel; writes
only into the output, so it's safe when the source is read-only.

```sh
amdl lyrics /music/lib
amdl lyrics /music/lib --json   # {ok_synced, ok_plain, not_found, instrumental, no_meta, skipped}
```

A large `not_found` count is normal for niche/Danish catalogs — not a failure.

*Resumability (W9):* if a run dies mid-way, just run the same command again —
finished files are skipped. The one trap this creates — a half-written `.opus`
that *looks* done — is caught by `doctor` (W3).

---

## W2 — Health scan (the map you plan repairs from)

One pass over the output library; add `--source` to also compare against the
originals.

```sh
amdl doctor /music/lib --source /music/originals
amdl doctor /music/lib --source /music/originals --json
```

Reports (each is a list of relative paths in `--json`):

- `missing_cover` — Opus with no embedded picture.
- `missing_tags` — missing artist/title/album.
- `unreadable` — won't decode.
- `truncated` — decoded Opus duration differs from the source's by > 1.5 s
  (silent-disconnect damage that skip-existing would otherwise hide forever).
- `source_without_opus` — a source file that never produced an Opus (conversion
  failure or damaged original).

An agent should scan first, then scope the work from these lists.

---

## W3 — Repair truncated Opus

A killed conversion can leave half-written `.opus` files; because they *exist*,
skip-existing never retries them. The safe fix is detect-by-duration → delete →
reconvert:

```sh
amdl doctor /music/lib --source /music/originals --json > /tmp/scan.json
jq -r '.truncated[]' /tmp/scan.json | while read -r rel; do rm -f "/music/lib/$rel"; done
amdl convert /music/originals /music/lib          # skip-existing regenerates only the deleted ones
amdl doctor  /music/lib --source /music/originals # verify: truncated == []
```

Deleting derived output files is fine; **never delete source originals.**

---

## W4 — Cover backfill (funnel; cheapest+safest first)

Backfill missing Opus covers. Run as a funnel so each pass shrinks the problem
before the next. **Implemented today:** the two free, always-correct passes plus
the human straggler list; every embedded image is validated (decodes, min edge)
and square-cropped, and only *coverless* tracks are touched (a blank cover beats
a wrong one). Covers apply per normalized album (multi-disc/editions group).

```sh
# 1. copy-from-source: the source file still had art convert didn't need
amdl covers /music/lib --source /music/originals
# 2. cross-library: a sibling library already has the same album covered (free, correct)
amdl covers /music/lib --reference /music/otherlib --reference /music/third
# both passes in one go, preview first:
amdl covers /music/lib --source /music/originals --reference /music/otherlib --dry-run
amdl covers /music/lib --source /music/originals --reference /music/otherlib --json | jq '.stragglers'
```

Whatever's left is a **numbered straggler list** (album, artist, track count,
impact-sorted) — the genuine long tail. The network waterfall
(MusicBrainz/CAA → iTunes → Discogs) and the "paste a URL per number" human pass
are **planned** (below); until then, resolve stragglers with the Python tool.

## recovery, identification — planned

On the roadmap (ported from the original Python tool, adapted to source→output).
**Not yet implemented**; use the Python tool + scripts for these:

- **`covers` network passes** — MB/CAA → iTunes → Discogs auto-waterfall
  (confidence-gated, generic-title guard) + numbered "paste a URL" human tail.
- **`recover`** — detect broken/unconverted → resolve metadata (tags, else
  folder/sibling) → cross-library copy if a reference has it, else re-acquire via
  gamdl → place in output, retagged to group with its album siblings.
- **`identify`** — AcoustID acoustic fingerprint (`fpcalc`) to fix untagged /
  mis-tagged tracks by sound.

When implemented, each will be idempotent, resumable, `--json`, and logged for
`undo`. Watch this file and `amdl <command> --help`.
