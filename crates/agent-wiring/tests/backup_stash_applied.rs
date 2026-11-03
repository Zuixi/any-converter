#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use agent_wiring::backup::{Applied, Stash};
use tempfile::tempdir;

fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn kv(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn bad_files(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.to_string_lossy().contains(".bad-"))
        .collect()
}

fn temp_residue(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.ends_with(".tmp").then_some(name)
        })
        .collect()
}

#[test]
fn stash_keeps_namespaces_isolated() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("agent_stash.json");
    let mut stash = Stash::load(&path);
    stash.record("claude-code", map(&[("A", "1")]));
    stash.record("codex", map(&[("B", "2")]));
    stash.save(&path).unwrap();

    let loaded = Stash::load(&path);

    assert_eq!(loaded.old_values("claude-code"), map(&[("A", "1")]));
    assert_eq!(loaded.old_values("codex"), map(&[("B", "2")]));
    assert!(loaded.old_values("gemini").is_empty());
}

#[test]
fn stash_record_and_take_namespace_roundtrip() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("agent_stash.json");
    let mut stash = Stash::load(&path);
    stash.record("pi", map(&[("model", "old"), ("provider", "native")]));

    let taken = stash.take_namespace("pi");

    assert_eq!(taken, map(&[("model", "old"), ("provider", "native")]));
    assert!(stash.old_values("pi").is_empty());
    assert!(stash.take_namespace("pi").is_empty());
}

#[test]
fn stash_empty_value_removes_key() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("agent_stash.json");
    let mut stash = Stash::load(&path);
    stash.record("codex", map(&[("keep", "yes"), ("drop", "old")]));

    stash.record("codex", map(&[("drop", "")]));

    assert_eq!(stash.old_values("codex"), map(&[("keep", "yes")]));
}

#[test]
fn stash_corrupt_file_loads_empty() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("agent_stash.json");
    fs::write(&path, b"this is not json").unwrap();

    let stash = Stash::load(&path);

    assert!(stash.old_values("claude-code").is_empty());
}

#[test]
fn stash_save_moves_corrupt_file_aside() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("agent_stash.json");
    fs::write(&path, b"this is not json").unwrap();
    let mut stash = Stash::load(&path);
    stash.record("claude-code", map(&[("A", "1")]));

    stash.save(&path).unwrap();

    let bad = bad_files(dir.path());
    assert_eq!(bad.len(), 1);
    assert_eq!(fs::read(&bad[0]).unwrap(), b"this is not json");
    assert_eq!(
        Stash::load(&path).old_values("claude-code"),
        map(&[("A", "1")])
    );
    assert_eq!(temp_residue(dir.path()), Vec::<String>::new());
}

#[test]
fn stash_save_roundtrips_values() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("agent_stash.json");
    let mut stash = Stash::load(&path);
    stash.record("gemini", map(&[("GOOGLE_API_KEY", "legacy")]));

    stash.save(&path).unwrap();

    assert_eq!(
        Stash::load(&path).old_values("gemini"),
        map(&[("GOOGLE_API_KEY", "legacy")])
    );
    assert_eq!(temp_residue(dir.path()), Vec::<String>::new());
}

#[test]
fn applied_record_and_drift_branches() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("agent_applied.json");
    let mut applied = Applied::load(&path);
    applied.record("claude-code", &kv(&[("A", "1"), ("B", "2"), ("C", "3")]));

    assert_eq!(
        applied.recorded("claude-code"),
        map(&[("A", "1"), ("B", "2"), ("C", "3")])
    );

    let unchanged = map(&[("A", "1"), ("B", "2"), ("C", "3"), ("EXTRA", "x")]);
    assert!(applied.drifted_keys("claude-code", &unchanged).is_empty());

    let changed = map(&[("A", "edited"), ("C", "3")]);
    assert_eq!(
        applied.drifted_keys("claude-code", &changed),
        vec!["A".to_owned(), "B".to_owned()]
    );
}

#[test]
fn applied_forget_drops_namespace() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("agent_applied.json");
    let mut applied = Applied::load(&path);
    applied.record("codex", &kv(&[("model", "ours")]));

    applied.forget("codex");

    assert!(applied.recorded("codex").is_empty());
    assert!(
        applied
            .drifted_keys("codex", &map(&[("model", "ours")]))
            .is_empty()
    );
}

#[test]
fn applied_empty_value_removes_key() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("agent_applied.json");
    let mut applied = Applied::load(&path);
    applied.record("pi", &kv(&[("keep", "yes"), ("drop", "old")]));

    applied.record("pi", &kv(&[("drop", "")]));

    assert_eq!(applied.recorded("pi"), map(&[("keep", "yes")]));
}

#[test]
fn applied_corrupt_file_loads_empty() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("agent_applied.json");
    fs::write(&path, b"not an audit").unwrap();

    let applied = Applied::load(&path);

    assert!(applied.recorded("claude-code").is_empty());
}

#[test]
fn applied_save_moves_corrupt_file_aside() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("agent_applied.json");
    fs::write(&path, b"not an audit").unwrap();
    let mut applied = Applied::load(&path);
    applied.record("codex", &kv(&[("model", "ours")]));

    applied.save(&path).unwrap();

    let bad = bad_files(dir.path());
    assert_eq!(bad.len(), 1);
    assert_eq!(fs::read(&bad[0]).unwrap(), b"not an audit");
    assert_eq!(
        Applied::load(&path).recorded("codex"),
        map(&[("model", "ours")])
    );
    assert_eq!(temp_residue(dir.path()), Vec::<String>::new());
}

#[test]
fn applied_save_roundtrips_values() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("agent_applied.json");
    let mut applied = Applied::load(&path);
    applied.record("gemini", &kv(&[("GEMINI_MODEL", "model-x")]));

    applied.save(&path).unwrap();

    assert_eq!(
        Applied::load(&path).recorded("gemini"),
        map(&[("GEMINI_MODEL", "model-x")])
    );
    assert_eq!(temp_residue(dir.path()), Vec::<String>::new());
}
