#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::thread;
use std::time::{Duration, UNIX_EPOCH};

use agent_wiring::atomic::{read, write_atomic};
use tempfile::tempdir;

fn modified_nanos(path: &std::path::Path) -> u128 {
    fs::metadata(path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn temp_residue(dir: &std::path::Path) -> Vec<String> {
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
fn roundtrip_and_missing_read() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.txt");
    assert_eq!(read(&path).unwrap(), None);
    write_atomic(&path, b"hello").unwrap();
    assert_eq!(read(&path).unwrap(), Some(b"hello".to_vec()));
}

#[test]
fn noop_write_keeps_mtime() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.txt");
    write_atomic(&path, b"same").unwrap();
    let before = modified_nanos(&path);
    thread::sleep(Duration::from_millis(20));
    write_atomic(&path, b"same").unwrap();
    let after = modified_nanos(&path);
    assert_eq!(before, after);
}

#[test]
fn successful_write_leaves_no_temp_files() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.txt");
    write_atomic(&path, b"first").unwrap();
    write_atomic(&path, b"second").unwrap();
    assert_eq!(read(&path).unwrap(), Some(b"second".to_vec()));
    assert_eq!(temp_residue(dir.path()), Vec::<String>::new());
}

#[cfg(unix)]
#[test]
fn preserves_existing_permission_mode() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().unwrap();
    let path = dir.path().join("secret.txt");
    fs::write(&path, b"old").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

    write_atomic(&path, b"new").unwrap();

    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o640);
}

#[test]
fn overwrite_replaces_content() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("replace.txt");
    write_atomic(&path, b"a much longer original value").unwrap();
    write_atomic(&path, b"short").unwrap();
    assert_eq!(read(&path).unwrap(), Some(b"short".to_vec()));
}

#[test]
fn creates_missing_parent_directory() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("nested").join("config.txt");
    write_atomic(&path, b"nested").unwrap();
    assert_eq!(read(&path).unwrap(), Some(b"nested".to_vec()));
}
