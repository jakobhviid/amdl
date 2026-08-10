# Agent guidelines

Instructions for any AI coding agent (Claude Code, opencode, Cursor, …) working
in this repository.

## Attribution — never attribute AI in the repo

- **Never** add AI/assistant attribution to commits or pull requests: no
  `Co-Authored-By: Claude` (or any other assistant) trailer, and no
  "🤖 Generated with …" line. Author every commit solely as the repository owner.
- AI assistance is disclosed **once**, in the README's "AI disclosure" section —
  that is the only place it belongs. Keep it out of the commit history entirely.
- If your tooling adds attribution by default, **turn it off at the source instead of
  fighting it per commit, and help the user do the same.** For Claude Code, set
  `includeCoAuthoredBy` to `false` in `~/.claude/settings.json` (it is on by default).
  A one-liner to hand the user (needs `jq`):

  ```sh
  f=~/.claude/settings.json; [ -f "$f" ] || printf '{}' > "$f"; \
    tmp=$(mktemp); jq '.includeCoAuthoredBy = false' "$f" > "$tmp" && mv "$tmp" "$f"
  ```

  Once it is off, no attribution is emitted at all and this rule holds effortlessly.

## Releases & versioning — auto-incremented from commit type

CI cuts a release on every push to `main`, and the version is **derived
automatically from the commit history** (Conventional Commits) — nobody bumps a
version by hand, so a forgotten manual release still versions correctly. The
commit **subject prefix** decides the bump:

- `feat: …` — a new feature → **minor** bump (1.2.0 → 1.3.0)
- `fix: …` — a bug fix / hotfix → **patch** bump (1.2.3 → 1.2.4)
- `feat!: …` (or any `type!:`, e.g. `fix!:`), or a `BREAKING CHANGE` — a breaking
  change → **major** bump (1.4.2 → 2.0.0)
- anything else (`docs:`, `chore:`, `refactor:`, …) or an un-prefixed subject →
  **patch** bump

So **pick the right commit-subject prefix for the change** and the release version
follows automatically. Never hand-edit `version` in `Cargo.toml` to release — CI
computes and stamps it.

## "Documenting the diff" — the doc failure mode, named

Updating a doc or a comment is not the same as narrating the update. The reflex is
to *edit around* the stale sentence so it describes the transition: "this **is** now
checked", "it **no longer** needs root", "skipped **instead of** re-run". Every word
can be true and the doc still be wrong, because it describes your commit rather than
the software.

It costs twice. A reader can't tell a live constraint from a dead one, so the old
state keeps steering them and they route around a problem that is already gone. And
it ages into trivia — one release on, "used to do X" is a fact about a version
nobody runs.

The tells are greppable, so grep **your own diff** before committing: `now`,
`now that`, `no longer`, `used to`, `actually`, `really`, `instead of <the old
behaviour>`, emphatic italics (`*is*` checked — arguing with a claim the reader
never saw), `(fixed in 1.2.3)`, `DONE`.

The fix, in order:

1. **Rewrite as if the new behaviour were the only one that ever existed** — present
   tense, no memory. Keep the *why* only where it is non-obvious and durable.
2. **Then ask whether the line still earns its place.** One that only made sense as
   a contrast with the old state should be *deleted*, not reworded.
3. **Put the before/after in the commit message** — the artefact built for it, kept
   by git with a date and a diff.
