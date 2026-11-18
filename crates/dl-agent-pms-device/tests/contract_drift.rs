//! The copied wire contract (`tests/contract/*.json`) must match SMILE-PMS's
//! (`apps/device-agent/contract/`). Set `PMS_DEVICE_CONTRACT_DIR` to that folder to check;
//! the test is skipped when the variable is unset (no SMILE-PMS checkout).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn json_files(dir: &Path) -> BTreeSet<String> {
    std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|e| e.unwrap_or_else(|e| panic!("list {}: {e}", dir.display())))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".json"))
        .collect()
}

fn parse(path: &Path) -> dl_json::Value {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    dl_json::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn copied_contract_matches_smile_pms() {
    let Ok(theirs) = std::env::var("PMS_DEVICE_CONTRACT_DIR") else {
        eprintln!("PMS_DEVICE_CONTRACT_DIR is unset; skipping the drift check");
        return;
    };
    let theirs = PathBuf::from(theirs);
    let ours = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/contract");
    let (our_files, their_files) = (json_files(&ours), json_files(&theirs));
    assert_eq!(
        our_files,
        their_files,
        "the contract files differ from {}: copy them again and review the changes",
        theirs.display()
    );
    let drifted: Vec<&String> = our_files
        .iter()
        .filter(|f| parse(&ours.join(f)) != parse(&theirs.join(f)))
        .collect();
    assert!(
        drifted.is_empty(),
        "tests/contract drifted from {}: {drifted:?}; copy them again and review the changes",
        theirs.display()
    );
}
