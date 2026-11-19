//! `scripts/gate.sh`'s closing lines (CONTRACT.md, "The first release: The
//! scripts that make it"): when it went on past a failing step
//! (`--no-fail-fast`), it says at its end which failed, a line `== gate: npm
//! test failed` and a line `== gate: cargo test failed`, each only when that
//! step failed; its exit code is 0 only when every step passed.
//!
//! Driven as `release_gate_hosted.rs` drives `gate-hosted.sh`: the real
//! script with `cargo` and `npm` stand-ins first on the `PATH`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Duration;

use blackbox::release::{GateStandIns, Rule};

const GATE_TIMEOUT: Duration = Duration::from_secs(60);
const NPM_FAILED: &str = "== gate: npm test failed";
const CARGO_FAILED: &str = "== gate: cargo test failed";

#[test]
fn with_no_fail_fast_the_gate_says_at_its_end_which_of_npm_test_and_cargo_test_failed() {
    let cargo_fails = || {
        vec![Rule::new(
            "test --workspace",
            101,
            "error: test failed, to rerun pass `-p blackbox --test a`\n",
        )]
    };
    let npm_fails = || vec![Rule::new("test", 1, "npm test failed\n")];
    for (cargo, npm, npm_failed, cargo_failed) in [
        (vec![], vec![], false, false),
        (vec![], npm_fails(), true, false),
        (cargo_fails(), vec![], false, true),
        (cargo_fails(), npm_fails(), true, true),
    ] {
        let stand_ins = GateStandIns::new(&cargo, &npm);
        let ran = stand_ins.run("gate.sh", &["--no-fail-fast"], GATE_TIMEOUT);
        let case = format!("npm test failing: {npm_failed}, cargo test failing: {cargo_failed}");
        assert_eq!(
            ran.code == Some(0),
            !npm_failed && !cargo_failed,
            "expected the exit code to be 0 only when every step passed ({case})\n{}",
            ran.describe()
        );
        assert!(
            stand_ins
                .calls("cargo")
                .iter()
                .any(|c| c.starts_with("test --workspace")),
            "expected cargo test run whatever npm's tests did, with --no-fail-fast ({case})"
        );
        let lines: Vec<&str> = ran.stdout.lines().chain(ran.stderr.lines()).collect();
        for (line, failed) in [(NPM_FAILED, npm_failed), (CARGO_FAILED, cargo_failed)] {
            assert_eq!(
                lines.iter().any(|l| l.trim_end() == line),
                failed,
                "expected the line {line:?} {} ({case})\n{}",
                if failed {
                    "at the gate's end"
                } else {
                    "not printed"
                },
                ran.describe()
            );
        }
        // At its end: in the stream that has a closing line, nothing but
        // closing lines after it.
        for stream in [&ran.stdout, &ran.stderr] {
            let kept: Vec<&str> = stream.lines().filter(|l| !l.trim().is_empty()).collect();
            if let Some(first) = kept
                .iter()
                .position(|l| l.starts_with("== gate:") && l.ends_with("failed"))
            {
                assert!(
                    kept[first..].iter().all(|l| l.starts_with("== gate:")),
                    "expected the closing lines at the gate's end ({case}); after the first come {:?}",
                    &kept[first..]
                );
            }
        }
    }
}
