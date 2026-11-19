//! The embedded ops manifest must be a byte copy of SMILE-PMS's manifest v2
//! (`packages/shared/src/ops/ops-manifest.json`). Set `PMS_OPS_MANIFEST` to that file, e.g.
//! `git -C $SMILE_PMS_REPO show origin/master:packages/shared/src/ops/ops-manifest.json`
//! saved to a file; the test is skipped when the variable is unset (no SMILE-PMS checkout).

use dl_agent_pms::ops::EMBEDDED;

#[test]
fn embedded_manifest_matches_smile_pms() {
    let Ok(path) = std::env::var("PMS_OPS_MANIFEST") else {
        eprintln!("PMS_OPS_MANIFEST is unset; skipping the drift check");
        return;
    };
    let theirs = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read PMS_OPS_MANIFEST {path}: {e}"));
    let ours = dl_json::parse(EMBEDDED).expect("embedded manifest is JSON");
    let theirs = dl_json::parse(&theirs).unwrap_or_else(|e| panic!("{path}: {e}"));
    assert!(
        ours == theirs,
        "assets/ops-manifest.json drifted from {path}: copy it again and review the op changes"
    );
}
