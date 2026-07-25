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

### tag — compilation grouping

A Various-Artists album whose track artists differ (or are blank) scatters in the
player. Group it as one album:

```sh
amdl tag "/music/lib/Compilations/Some VA Album" --compilation   # albumartist=Various Artists, compilation=1
amdl tag "/music/lib/Artist/Album" --album "Real Album" --album-artist "Artist"   # fix fields
amdl tag <path> --compilation --dry-run                          # preview
```

Applies to every audio file under the path; existing tags are preserved.

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

## W4 — Cover backfill (the full funnel: cheapest+safest → manual)

Backfill missing Opus covers as an ordered funnel, so each pass shrinks the
problem before the next, riskier one. Every embedded image is validated
(decodes, min edge ≥250px) and square-cropped; covers apply **per normalized
album** (multi-disc/editions group) and only onto *coverless* tracks. Online
matches are gated on artist+album agreeing — **a blank cover beats a wrong one.**

```sh
# passes 1–2 (free, always-correct): source file art, then a sibling library
amdl covers /music/lib --source /music/originals --reference /music/otherlib

# pass 3 (online waterfall): MusicBrainz/CAA → iTunes → Discogs (needs [keys] for Discogs)
amdl covers /music/lib --source /music/originals --online

# preview / scriptable
amdl covers /music/lib --source /music/originals --online --dry-run
amdl covers /music/lib --online --json | jq '.stragglers'   # {n, album, artist, tracks} most-first
```

**Pass 4 — the human tail (`--paste`).** Whatever's still uncovered is listed as
a numbered, **most-tracks-first** straggler list; paste one URL per album (a
direct image URL *or* a Spotify album link, whose `og:image` is extracted) and it
is embedded across **every track of that album** (album-level — even a one-track
album):

```sh
amdl covers /music/lib --source /music/originals --online --paste
#   3 album(s) still need a cover (most tracks first):
#     1. Some Danish Comp — Various Artists (18 tracks)
#     2. …
#   cover> 1 https://open.spotify.com/album/xxxx     ← pasted; embeds across all 18
#   cover>                                           ← blank line to finish
```

## W5 — Fix untagged / mis-tagged tracks (`identify`)

Identify a track by **sound** (AcoustID fingerprint via `fpcalc`) — the only key
that works when tags are empty or wrong. Needs an AcoustID **application** key
(`[keys] acoustid` in config, or `$ACOUSTID_KEY`; create one at
<https://acoustid.org/new-application> — it is NOT your account/user key).

```sh
amdl identify /music/lib                 # report: artist — title · album (score)
amdl identify /music/lib --apply         # write the resolved artist/title/album
amdl identify /music/lib --json | jq '.results[] | select(.matched)'
```

Then hand the now-tagged album to `covers` for its art.

## W6 — Recover broken / missing tracks (`recover`)

A source file that never produced an Opus (conversion failure / damaged
original) is found by comparing source → output. Recover it cheaply first, then
fall back to re-download:

```sh
# cross-library copy: a sibling library already has the track (free, no download)
amdl recover /music/lib --source /music/originals --reference /music/otherlib
# re-acquire whatever no sibling has, via gamdl (needs cookies)
amdl recover /music/lib --source /music/originals --online
amdl recover /music/lib --source /music/originals --dry-run --json    # preview
```

Metadata comes from the file's tags, else the folder + filename. A re-acquired
track carries Apple's *own* album tag, which often differs from the
compilation/folder it belongs to — so after placing it, `recover` reads an
existing album sibling and **regroups** it (matches `album`, and
`albumartist=Various Artists`+`compilation=1` if the sibling is a compilation),
so it joins the album instead of splitting into a lone one-track album. Honors
`--dry-run`. (Deleting a damaged *derived* `.opus` and re-running `convert` is the
other repair path — W3.)

## W7 — Surface duplicates / orphan editions (`dedup`)

Find redundant tracks so you can prune them. `dedup` **never deletes** — it
reports, because removing media is your call. Detection is tag-level (a folder can
mix correctly- and mis-tagged tracks), with a cross-release guard: a song that
appears on both a studio album *and* a compilation has two different albums, so
it's **not** flagged — that membership is wanted.

```sh
amdl dedup /music/lib                 # exact-duplicate recordings + subset editions
amdl dedup /music/lib --json | jq '.subset_editions'
amdl dedup /music/lib --print-rm      # emit `rm` lines (redundant copies) to review, then run yourself
```

Two findings: **exact duplicates** (same artist + normalized album + title — keeps
the copy in the most-complete edition) and **subset editions** (a raw edition
whose tracks are a strict subset of another edition of the same release, e.g.
Standard ⊂ Deluxe; multi-disc sets are safe — their discs are disjoint). The
subset tier is heuristic and labelled as such; review before removing anything.

## Scope — acquisition vs. the harness

amdl's job starts *after* the files exist. Acquisition (codecs, DRM, storefronts,
login, download failures) is **gamdl's** domain — take those to the gamdl team.
Likewise, *serving* the finished library is your media server's job: point
Navidrome (or whatever) at the **output** Opus tree (never the read-only source)
and let it scan — amdl deliberately stays a self-contained file harness and does
not reach across the network into a server it doesn't own.

## Undo — planned

Every mutating command is idempotent and safe to re-run; a first-class `undo`
(revert the last run's writes) is still on the roadmap.
