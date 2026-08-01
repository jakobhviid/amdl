//! Live end-to-end check for forced alignment against a real whisper endpoint.
//! `#[ignore]`d because it needs a reachable box + a local audio fixture — run it
//! deliberately:
//!
//!   AMDL_WHISPER_URL=http://192.168.1.6:8080 \
//!   AMDL_ALIGN_FIXTURE_DIR=/var/home/jakob/ai/aligner-test \
//!   cargo test -p amdl-core --test align_live -- --ignored --nocapture
//!
//! It transcribes `dbs.opus`, aligns it to `dbs.txt`, and asserts the result
//! clears the confidence gate with monotonic, in-range timings.
use amdl_core::align::{self, Whisper};
use std::path::PathBuf;

#[test]
#[ignore = "hits a live whisper endpoint + needs the audio fixture"]
fn aligns_fixture_against_live_endpoint() {
    let url = std::env::var("AMDL_WHISPER_URL").unwrap_or_else(|_| "http://192.168.1.6:8080".into());
    let dir = PathBuf::from(
        std::env::var("AMDL_ALIGN_FIXTURE_DIR").unwrap_or_else(|_| "/var/home/jakob/ai/aligner-test".into()),
    );
    let audio = dir.join("dbs.opus");
    let lyrics_path = dir.join("dbs.txt");
    if !audio.exists() || !lyrics_path.exists() {
        eprintln!("fixture missing at {dir:?} — skipping");
        return;
    }

    let plain = std::fs::read_to_string(&lyrics_path).unwrap();
    let lines: Vec<&str> = plain.lines().collect();
    let w = Whisper {
        url,
        model: align::DEFAULT_MODEL.to_string(),
        key: std::env::var("AMDL_WHISPER_KEY").ok(),
    };

    let r = align::align(&w, &audio, &lines).expect("alignment returned None (endpoint unreachable?)");

    eprintln!("overall_conf = {:.3}, {} timed lines", r.overall_conf, r.lines.len());
    for l in r.lines.iter().take(6) {
        eprintln!("  [{:>7.2}] ({:.2}) {}", l.start, l.conf, l.text);
    }

    assert!(!r.lines.is_empty(), "no lines timed");
    assert!(r.overall_conf >= 0.5, "overall_conf {} below the 0.5 gate", r.overall_conf);
    // Starts must be non-decreasing and within the ~257 s track.
    let mut prev = -1.0;
    for l in &r.lines {
        assert!(l.start >= prev - 1e-6, "starts not monotonic at line {}: {} < {}", l.i, l.start, prev);
        assert!(l.start >= 0.0 && l.start < 300.0, "start {} out of range", l.start);
        assert!(l.end >= l.start - 1e-6, "end {} before start {}", l.end, l.start);
        prev = l.start;
    }
}
