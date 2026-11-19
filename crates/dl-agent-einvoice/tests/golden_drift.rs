//! Drift guard: `testdata/golden.json` is a copy of MM-EINVOICE's
//! `rust/agent/testdata/golden.json`. When a checkout of that repo is at `$MMEI_REPO`,
//! the two must be byte-identical; otherwise the test is skipped.
//! Refresh: `cp $MMEI_REPO/rust/agent/testdata/golden.json crates/dl-agent-einvoice/testdata/`.

use std::path::Path;

#[test]
fn golden_matches_mmei_repo() {
    let Some(repo) = std::env::var_os("MMEI_REPO").filter(|v| !v.is_empty()) else {
        eprintln!("skipped: MMEI_REPO not set");
        return;
    };
    let theirs = Path::new(&repo).join("rust/agent/testdata/golden.json");
    let Ok(theirs_bytes) = std::fs::read(&theirs) else {
        eprintln!("skipped: {} not readable", theirs.display());
        return;
    };
    let ours = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/golden.json");
    let ours_bytes = std::fs::read(&ours).expect("read testdata/golden.json");
    assert!(
        ours_bytes == theirs_bytes,
        "testdata/golden.json is out of date with {}; refresh it: cp {} {}",
        theirs.display(),
        theirs.display(),
        ours.display()
    );
}
