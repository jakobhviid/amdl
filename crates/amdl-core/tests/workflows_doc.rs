//! Guard: the specific numeric constants quoted in WORKFLOWS.md must stay in
//! sync with the code. WORKFLOWS.md is embedded verbatim into `amdl --llm`
//! (see crates/amdl/src/main.rs), so a number that drifts here means the agent
//! guide silently lies — and an LLM will trust the doc over the source. If an
//! assertion below fails, fix WHICHEVER is wrong (the constant or the doc) so
//! they match again.
//!
//! Not guarded: the "~0.7 s" alignment onset figure in the lyrics section is an
//! empirical property of the whisper model, not a constant in this repo.

use amdl_core::{covers, doctor, identify, journal, lyrics, recover};

const DOC: &str = include_str!("../../../WORKFLOWS.md");

fn assert_documented(label: &str, needle: String) {
    assert!(
        DOC.contains(&needle),
        "WORKFLOWS.md is out of sync with the code: expected {label} to appear as \
         {needle:?}, but it does not. Update the constant or the doc so they match.",
    );
}

#[test]
fn workflows_doc_constants_match_code() {
    assert_documented("doctor truncated tolerance", format!("> {} s", doctor::DURATION_TOLERANCE));
    assert_documented("lyrics fuzzy-match duration tolerance", format!("(±{}s)", lyrics::DURATION_TOL));
    assert_documented("identify default min-score", format!("default {}", identify::DEFAULT_MIN_SCORE));
    assert_documented("recover duration tolerance", format!("±{} s", recover::DURATION_TOL_SECS));
    assert_documented("undo journal retention", format!("{} runs", journal::KEEP_RUNS));
    assert_documented("covers default min edge", format!("≥{}px", covers::DEFAULT_MIN_EDGE_PX));
}
