#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

use agent_wiring::backup::Snapshot;
use tempfile::tempdir;

fn backup_path(file: &Path) -> PathBuf {
    PathBuf::from(format!("{}.before-any-converter", file.display()))
}

fn marker_path(file: &Path) -> PathBuf {
    PathBuf::from(format!("{}.before-any-converter.absent", file.display()))
}

#[test]
fn take_copies_exact_bytes() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("settings.json");
    fs::write(&file, b"{\"keep\": true}\n").unwrap();
    let snapshot = Snapshot::new(vec![file.clone()]);

    snapshot.take().unwrap();

    assert!(snapshot.has_backup(&file));
    assert_eq!(
        fs::read(backup_path(&file)).unwrap(),
        b"{\"keep\": true}\n"
    );
    assert_eq!(
        snapshot.read_backup(&file).unwrap(),
        Some(b"{\"keep\": true}\n".to_vec())
    );
    assert!(!marker_path(&file).exists());
}

#[test]
fn second_take_never_overwrites_pristine_backup() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("settings.json");
    fs::write(&file, b"pristine").unwrap();
    let snapshot = Snapshot::new(vec![file.clone()]);
    snapshot.take().unwrap();

    fs::write(&file, b"modified during connect").unwrap();
    snapshot.take().unwrap();

    assert_eq!(
        snapshot.read_backup(&file).unwrap(),
        Some(b"pristine".to_vec())
    );
}

#[test]
fn absent_file_gets_marker_and_clear_removes_both_kinds() {
    let dir = tempdir().unwrap();
    let existing = dir.path().join("exists.json");
    let missing = dir.path().join("missing.json");
    fs::write(&existing, b"bytes").unwrap();
    let snapshot = Snapshot::new(vec![existing.clone(), missing.clone()]);

    snapshot.take().unwrap();

    assert!(backup_path(&existing).exists());
    assert!(marker_path(&missing).exists());
    assert!(!backup_path(&missing).exists());

    snapshot.clear(&existing);
    snapshot.clear(&missing);

    assert!(!backup_path(&existing).exists());
    assert!(!marker_path(&missing).exists());
    assert!(!marker_path(&existing).exists());
}

#[test]
fn read_backup_is_none_without_take() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("settings.json");
    fs::write(&file, b"bytes").unwrap();
    let snapshot = Snapshot::new(vec![file.clone()]);
    assert_eq!(snapshot.read_backup(&file).unwrap(), None);
    assert!(!snapshot.has_backup(&file));
}

#[test]
fn preserve_bad_moves_without_losing_content() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("settings.json");
    fs::write(backup_path(&file), b"corrupt bytes").unwrap();
    let snapshot = Snapshot::new(vec![file.clone()]);

    let moved = snapshot.preserve_bad(&file);

    assert!(!backup_path(&file).exists());
    assert_eq!(fs::read(&moved).unwrap(), b"corrupt bytes");
    assert!(
        moved
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains(".bad-")
    );
}

#[test]
fn two_preserve_bad_calls_use_distinct_names() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("settings.json");
    let snapshot = Snapshot::new(vec![file.clone()]);

    fs::write(backup_path(&file), b"first corrupt").unwrap();
    let first = snapshot.preserve_bad(&file);
    fs::write(backup_path(&file), b"second corrupt").unwrap();
    let second = snapshot.preserve_bad(&file);

    assert_ne!(first, second);
    assert_eq!(fs::read(&first).unwrap(), b"first corrupt");
    assert_eq!(fs::read(&second).unwrap(), b"second corrupt");
}
