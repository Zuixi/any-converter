#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use agent_wiring::spec::HomeCtx;
use agent_wiring::sync::catalog::{CatalogModel, ModelCatalog};

thread_local! {
    static ENV: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

pub const GATEWAY: &str = "http://127.0.0.1:8787";
pub const AGENT_KEY: &str = "acw_test_agent_key";
pub const DEFAULT_MODEL: &str = "claude-sonnet-4-5";

pub fn env(key: &str) -> Option<String> {
    ENV.with(|values| values.borrow().get(key).cloned())
}

pub fn set_env(key: &str, value: impl Into<String>) {
    ENV.with(|values| {
        values.borrow_mut().insert(key.to_owned(), value.into());
    });
}

pub fn clear_env() {
    ENV.with(|values| values.borrow_mut().clear());
}

pub fn home(path: &Path) -> HomeCtx {
    clear_env();
    HomeCtx {
        home: path.to_path_buf(),
        env,
    }
}

pub fn home_with(path: &Path, values: &[(&str, String)]) -> HomeCtx {
    clear_env();
    for (key, value) in values {
        set_env(key, value.clone());
    }
    HomeCtx {
        home: path.to_path_buf(),
        env,
    }
}

pub fn catalog() -> ModelCatalog {
    ModelCatalog::new(vec![
        CatalogModel::new("claude-opus-4-5", "Claude Opus 4.5", true, Some(200_000)),
        CatalogModel::new(
            "claude-sonnet-4-5",
            "Claude Sonnet 4.5",
            true,
            Some(200_000),
        ),
        CatalogModel::new("claude-haiku-4-5", "Claude Haiku 4.5", false, Some(200_000)),
        CatalogModel::new("gpt-5-codex", "GPT-5 Codex", true, Some(400_000)),
        CatalogModel::new("gemini-3-pro", "Gemini 3 Pro", true, Some(1_000_000)),
    ])
}

pub fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, text).unwrap();
}

pub fn read(path: &Path) -> String {
    String::from_utf8(fs::read(path).unwrap()).unwrap()
}
