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
  `~/.config/amdl/config.toml` (`[paths] source=… output=…`), **or** set them
  programmatically with `amdl configure set …` (see below). Flags always
  override config.
- `--json` on **every** batch command prints structured results; pipe to `jq`.
- Every mutating run is **journaled** so `amdl undo` can revert it (W8) — a safety
  net if a step goes wrong.

## Configuring settings programmatically (`configure`)

Every setting can be read/written without hand-editing the file, so config is
scriptable. Keys are dotted `section.field`; `amdl configure keys` lists them all.

```sh
amdl configure keys                                   # every settable key + a description
amdl configure set paths.output /mnt/music/library    # set (or update) a value
amdl configure set lyrics.aligner_url http://192.168.1.6:8790
amdl configure set lyrics.lrcapi_first true            # booleans take true/false
amdl configure get paths.output                        # prints the bare value (empty if unset) — for $(…)
amdl configure list --json                             # all keys → values, machine-readable
amdl configure unset lyrics.aligner_url                # delete a setting (revert to unset/default)
```

Every write **re-renders the whole `config.toml` from one template**, so the
inline help for *all* settings is preserved no matter which values are set — the
file stays self-documenting after any `configure set`. Unknown keys and bad
values (e.g. a non-boolean for a boolean key) error with a non-zero exit and
touch nothing. A `set`/`unset` refuses to run against a file it can't parse
rather than clobbering settings it didn't understand.

## Convention for agents

Prefer this loop: **act → `doctor --json` → decide from the counts → act again.**
`doctor` is the map; it never changes anything, so it's safe to call repeatedly.
If a mutating step turns out wrong, `amdl undo` reverts the last run (W8).

---

## W1 — Build a library (convert with fidelity)

Transcode a source tree to Opus. Covers are re-embedded as real
`METADATA_BLOCK_PICTURE` (players see them), iTunes junk tags are stripped, and
`.lrc` sidecars are mirrored into the output. Skip-existing makes it resumable —
re-running costs nothing for already-converted files.

```sh
amdl convert /music/originals /music/lib          # explicit paths
amdl convert                                       # uses config [paths]
amdl convert /music/originals /music/lib --json    # {converted, copied, skipped, failed, with_cover, lrc_copied}
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
lyrics and write the `.lrc` into the library. **And by default, re-time any
existing *plain* `.lrc` to synced** when a source now has a timed version — so
one command both fills gaps and upgrades. Parallel; writes only into the output,
so it's safe when the source is read-only.

```sh
amdl lyrics /music/lib             # fetch missing + upgrade plain→synced (default)
amdl lyrics /music/lib --json      # {ok_synced, ok_plain, upgraded, aligned, embedded, not_found, instrumental, no_meta, skipped}
```

A large `not_found` count is normal for niche/Danish catalogs — not a failure.

`.lrc` files are **sidecars** by default — written next to each track (`Song.opus`
→ `Song.lrc`), never folded into the audio. That's the most portable form (any
player that reads a same-name `.lrc` picks them up) and keeps writes
non-destructive. See `--embed` below for lyrics that travel *inside* the file.

**The default upgrades; `--no-upgrade` opts out.** Upgrading re-queries every
existing **plain** (untimed) `.lrc` and replaces it only when a source has a
**synced** version (already-synced files and tracks with no synced match are left
untouched; each replacement is journaled, so `amdl undo` restores the old `.lrc`).
Because it re-hits the network per plain file, pass `--no-upgrade` for the cheap,
idempotent pass that just fills gaps and skips everything already on disk — ideal
for a quick "any new tracks?" run over a large library.

```sh
amdl lyrics /music/lib                 # default: fill gaps AND upgrade plain→synced
amdl lyrics /music/lib --no-upgrade    # cheap: fill gaps only, skip all existing .lrc (no network for them)
amdl undo                              # revert a run (restores any replaced .lrc)
```

(`--upgrade-synced` is still accepted as a no-op alias, since upgrading is now the
default.)

**Embed lyrics into the file (`--embed`).** Sidecars don't travel: copy a track
to a phone/DAP/car or hand someone a single file and the `.lrc` is left behind.
`--embed` also writes each track's lyrics into the audio file's `LYRICS` tag,
**keeping the sidecar** — so both sidecar- and embed-reading players work. It
covers the whole library in one pass: every track that has an `.lrc` (already on
disk *or* freshly fetched this run) gets embedded. Journaled, so `amdl undo`
strips/restores the tag.

Embedding **never downgrades**. It writes only when the file has no embedded
lyric yet, or as a genuine **plain → synced** upgrade; an already-*synced* embed
is left untouched and identical content is never rewritten. To overwrite anyway
(e.g. replace a synced embed, or re-embed hand-edited lyrics) pass `--force-embed`.

```sh
amdl lyrics /music/lib --embed                   # fetch + upgrade + embed (upgrade is on by default)
amdl lyrics /music/lib --embed --no-upgrade      # embed existing/just-fetched, but don't re-time plain sidecars
amdl lyrics /music/lib --embed --json            # `embedded` counts the tags written
amdl lyrics /music/lib --force-embed             # overwrite even a synced embed (implies --embed)
amdl undo                                        # revert an embed run (restores the prior tag)
```

**Target one track, not the whole library.** The `lyrics` path argument accepts a
**single audio file** as well as a directory (same as `tag`/`identify`), so a
person or an agent can fix exactly one track without walking the library — the
lyrics analogue of a one-file cover fix:

```sh
amdl lyrics "/music/lib/Artist/Album/03 Song.opus"                # just this track
amdl lyrics "/music/lib/Artist/Album/03 Song.opus" --force-embed   # re-embed this track's tag, even if synced
```

Serving from **Navidrome** (or similar) reads either form off the output tree, so
embedding is about portability, not your server — sidecars alone already serve.

**Secondary lyrics source (config).** `lyrics` uses lrclib.net by default. You can
add a second, [LrcApi](https://github.com/HisAtri/LrcApi)-compatible server (e.g.
self-hosted) in the config; it's consulted **only when lrclib.net has no synced
match**, and a synced hit from *either* source always wins over a plain one — so a
track lrclib only has untimed gets timed lyrics from your server. Set it under
`[lyrics]` in `~/.config/amdl/config.toml`:

```toml
[lyrics]
lrcapi_url  = "https://lyrics.example.cloud"   # LrcApi base URL (GET {url}/jsonapi)
lrcapi_key  = "…"                               # sent verbatim as the Authorization header
# lrcapi_first = true                           # flip priority: try this server before lrclib.net
```

Both `lrcapi_url` and `lrcapi_key` are required to enable it; `lrcapi_first` (default
`false`) flips which source is primary/queried-first. This applies to plain
`lyrics` (default), `--no-upgrade`, and `--embed` alike — they all fetch through the same
source chain.

**Generate synced lyrics for tracks no source has (alignment).** As a last resort,
alignment produces *synced* lyrics from *plain* ones by listening to the track
(forced alignment) — for the residue that neither lrclib nor your LrcApi has
timed, **including tracks whose only lyrics are embedded in the file's `LYRICS`
tag** (nothing on disk, nothing online). It needs a running
[amdl-aligner](https://github.com/jakobhviid/amdl-aligner) service (GPU
recommended) set as `[lyrics] aligner_url`. Results are written at the
**Generated** tier — marked `[re:amdl-align]` so they're recognizable as
machine-made, lower quality than a real synced source (~0.7 s onset accuracy),
and auto-upgraded later if a real synced version appears. Low-confidence
alignments are dropped back to plain.

**Alignment is on by default once `aligner_url` is set** — no flag needed. Use
`--no-align` to skip it (fetch + upgrade only), or `--align` to request it
explicitly (which, if no `aligner_url` is configured, prints setup instructions
and proceeds without aligning).

```toml
[lyrics]
aligner_url = "http://192.168.1.6:8790"    # your amdl-aligner service (LAN)
```
```sh
amdl lyrics /music/lib                     # with aligner_url set: fetch + upgrade + align the residue
amdl lyrics /music/lib --no-align          # ...but skip the Generated-tier alignment
amdl lyrics /music/lib --embed             # align (default) and embed the results too
```

### tag — compilation grouping

A Various-Artists album whose track artists differ (or are blank) scatters in the
player. Group it as one album:

```sh
amdl tag "/music/lib/Compilations/Some VA Album" --compilation   # albumartist=Various Artists, compilation=1
amdl tag "/music/lib/Artist/Album" --album "Real Album" --album-artist "Artist"   # fix fields
amdl tag <path> --compilation --dry-run                          # preview
```

Applies to every audio file under the path; existing tags are preserved.

*Resumability:* if a run dies mid-way, just run the same command again —
finished files are skipped. The one trap this creates — a half-written `.opus`
that *looks* done — is caught by `doctor` (W3).

---

## W2 — Health scan (the map you plan repairs from)

One pass over the output library; add `--source` to also compare against the
originals.

```sh
amdl doctor /music/lib --source /music/originals
amdl doctor /music/lib --source /music/originals --json
amdl doctor /music/lib --deep                      # full-decode integrity, no source needed
```

Reports (each is a list of relative paths in `--json`):

- `missing_cover` — Opus with no embedded picture.
- `missing_tags` — missing artist/title/album.
- `unreadable` — won't even probe.
- `truncated` — decoded Opus duration differs from the source's by > 1.5 s
  (silent-disconnect damage that skip-existing would otherwise hide forever).
- `corrupt` — **`--deep` only**: fails a full ffmpeg decode. Catches stream
  corruption that a metadata probe (and even `unreadable`) misses, and needs *no*
  source library to compare against — the check for a library whose originals are
  gone. It decodes every file, so it's opt-in and slower.
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

**Scriptable (no TTY) — `--paste-file`.** An agent can drive the tail without a
terminal: read the numbered stragglers from `--json`, then feed `<n><TAB>url`
lines (tab- or space-separated; `#` comments and blank lines ignored) from a file
or `-` for stdin. The numbers are the same deterministic most-tracks-first order.

```sh
amdl covers /music/lib --online --json | jq -r '.stragglers[] | "\(.n)\t\(.album)"'  # discover numbers
printf '1\thttps://open.spotify.com/album/xxxx\n2\thttps://img.example/cover.jpg\n' \
  | amdl covers /music/lib --paste-file -                                             # embed, non-interactive
amdl covers /music/lib --paste-file covers.tsv                                        # or from a file
```

## W5 — Fix untagged / mis-tagged tracks (`identify`)

Identify a track by **sound** (AcoustID fingerprint via `fpcalc`) — the only key
that works when tags are empty or wrong. Needs an AcoustID **application** key
(`[keys] acoustid` in config, or `$ACOUSTID_KEY`; create one at
<https://acoustid.org/new-application> — it is NOT your account/user key).

```sh
amdl identify /music/lib                         # report only: artist — title · album (score)
amdl identify /music/lib --apply                 # write matches at/above the score gate
amdl identify /music/lib --apply --dry-run        # preview exactly what --apply would write
amdl identify /music/lib --apply --min-score 0.95 # stricter gate (default 0.9)
amdl identify /music/lib --apply --skip-tagged    # resume: skip files that already have tags
amdl identify /music/lib --json | jq '.results[] | select(.matched)'
```

`--apply` never writes a match below `--min-score` (default 0.9) — a wrong tag is
worse than none, the same rule covers/dedup follow; those show up as `low-score`
in the report. `--skip-tagged` makes a large untagged-folder run resumable (it's
opt-in because identify also *fixes* mis-tagged files, which have tags). Then hand
the now-tagged album to `covers` for its art.

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

Re-acquisition is verified before it's accepted: the iTunes candidate must match
the broken track's **title and duration** (±3 s), so a live cut/remix/wrong
version is rejected to `still_broken` rather than silently substituted — the more
so now that an accepted track is then regrouped into the album.

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

## W8 — Undo a run (`undo`)

Every mutating command is **journaled by default**, so you can revert it. Undo
deletes files amdl created (convert/lyrics/recover) and restores tags/covers it
changed (tag/identify/covers/paste/regroup/embed).

On a terminal, a bare `amdl undo` opens an **interactive picker** — recent runs
listed newest-first with a relative date, the command, and how many files each
touched; the most recent is preselected. Press Enter to revert it, type a number
to choose another, `d`/`dN` to preview a run with a dry-run first, or `q` to
cancel. When stdin isn't a terminal (scripts/pipes), or you pass a run id,
`--dry-run`, or `--json`, undo stays non-interactive and reverts directly.

```sh
amdl undo                 # terminal: interactive picker · non-terminal: revert most recent
amdl undo --list          # recent runs: date · command · #files · id  (--json for machine form)
amdl undo <run-id>        # revert a specific run (id from --list), non-interactive
amdl undo --dry-run       # preview the most recent without changing anything
<any mutating cmd> --no-undo   # don't journal this run
```

**It never clobbers your later edits.** Before reverting each change, undo checks
the file still matches what amdl *left*; anything you (or another tool) touched
since is skipped and reported, not forced. So `undo` is safe even if you've been
working in the library between the run and the undo.

The journal lives in your OS state dir (`~/.local/state/amdl/undo` on Linux,
`~/Library/Application Support/amdl/undo` on macOS; override with `$AMDL_UNDO_DIR`)
and persists across reboots. It's compact — creations store a path+hash, edits
store just the old tag values (and the old cover only when one is *replaced*) —
and the newest 25 runs are kept, older ones pruned automatically.

---

## Semantics — matching, exit codes, concurrency (read before scripting)

The behavior below is stable and worth knowing up front, so an agent can predict
results instead of discovering them by trial. It is the contract the `--json`
loop relies on.

### How tracks are matched (normalization)

Grouping and de-duplication never compare raw strings — they compare *normalized*
keys, and the rules are deliberately blunt so trivially-different spellings
collapse together:

- **Album key** strips everything inside `(...)`/`[...]` (edition/disc suffixes),
  then keeps only the remaining **alphanumeric** characters, lowercased. So
  `"Album (Deluxe Edition)"`, `"Album [Disc 2]"`, and `"album"` all key to
  `album` — which is exactly why multi-disc sets and Standard/Deluxe editions
  group as one release.
- **Artist / title key** keeps all **alphanumeric** characters, lowercased, with
  **no** bracket-stripping — so a `(feat. …)` in a *title* is significant
  (`"Song (feat. X)"` ≠ `"Song"`), while the same suffix in an *album* is not.
- Both keys are **Unicode-aware**: letters are lowercased, not ASCII-folded, so
  accents are preserved (`"Beyoncé"` → `beyoncé`). Two spellings that differ only
  by an accent are treated as different — normalize upstream if you need them to
  merge.
- Command identities built from these: **covers** groups coverless tracks by the
  album key; **dedup** exact-duplicate identity is `(artist, album, title)` keys;
  **recover** matches a broken track on `(album, title)` keys **plus** a duration
  check (below).

### Confidence gates (a wrong write is worse than no write)

- **recover** accepts a re-acquired/cross-library candidate only if the title
  keys match **and** durations agree within **±3 s**; with no known source
  duration it falls back to an **exact** normalized-title match. Live cuts,
  remixes, and wrong versions are rejected to `still_broken` rather than
  substituted.
- **identify** never auto-applies (`--apply`) a match below `--min-score`
  (default **0.9**); those are counted as `skipped_low_score` and left untouched.
- **covers** online matches are gated on artist+album agreeing; a blank cover is
  left rather than embedding a wrong one.

### Exit codes — parse `--json`, don't gate on `$?`

A command exits **non-zero only on a fatal error** (missing path, no source dir,
unreadable config). **Per-item problems never change the exit code**: a `doctor`
that finds 500 truncated files, a `convert` with `failed: 12`, or an `identify`
with zero matches all still exit `0`. So drive decisions off the `--json` counts,
never off the process exit status. `--json` output is a direct serialization of
the internal report structs, so the field set is exact and stable — read a
command's `--help`/source for the full shape rather than inferring it.

### Concurrency — what is actually safe to run in parallel

amdl takes **no file locks**. Parallelism is safe only when jobs touch *different
files*:

- `convert` (writes `.opus`) and `lyrics` (writes `.lrc`) over the same library
  are safe — disjoint file types (W1).
- Two commands that write the **same** output files (e.g. two `convert`s, or
  `convert` and `covers`, into one library) are **not** guarded — serialize them.
- Read-only commands (`doctor`, `dedup`, report-only `identify`, any `--dry-run`)
  are always safe to run concurrently with anything.

## Scope — acquisition vs. the harness

amdl's job starts *after* the files exist. Acquisition (codecs, DRM, storefronts,
login, download failures) is **gamdl's** domain — take those to the gamdl team.
Likewise, *serving* the finished library is your media server's job: point
Navidrome (or whatever) at the **output** Opus tree (never the read-only source)
and let it scan — amdl deliberately stays a self-contained file harness and does
not reach across the network into a server it doesn't own.
