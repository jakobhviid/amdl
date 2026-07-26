//! amdl-core — the library behind the amdl CLI.
//!
//! Every capability lives here as a reusable function so scripts and agents can
//! compose them (the CLI is a thin, `--json`-emitting layer on top). Design
//! rules that hold across the crate: never write to a source directory, prefer
//! idempotent + resumable operations, and gate any fuzzy write behind a
//! confidence check (see `report`/`journal`).
pub mod config;
pub mod convert;
pub mod cookies;
pub mod covers;
pub mod dedup;
pub mod doctor;
pub mod download;
pub mod identify;
pub mod journal;
pub mod lyrics;
pub mod recover;
pub mod retag;
pub mod stats;
pub mod tags;
pub mod ui;
pub mod validate;
