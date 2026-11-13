//! Installing and removing plugin packages into [`managed::plugins_dir`]
//! with npm — the same flow the CLI's `plugin install` runs, so the GUI can
//! subscribe to a plugin provider without shipping or invoking a CLI
//! (magpie's add-sheet does the same through its own runtime).
//!
//! npm mutates `<plugins>/package.json` and `node_modules`, so every
//! mutating command holds [`NPM_LOCK`]: two installs at once would corrupt
//! the manifest the way magpie guards against with its own bun mutex.

use std::path::Path;
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::Mutex;

use super::managed::{self, InstalledPlugin, ManagedHosts};

/// How long one npm command may run before it is given up on.
const NPM_TIMEOUT: Duration = Duration::from_secs(300);

/// Serializes every npm invocation (installs and uninstalls alike): npm
/// rewrites the plugins dir's `package.json`, and two writers at once leave
/// it unreadable for the next one.
static NPM_LOCK: Mutex<()> = Mutex::const_new(());

/// A package spec npm understands: scoped or bare name, optionally pinned
/// (`@scope/name@1.2.3`, `name@latest`) or a path/git URL. Deliberately
/// conservative — it is also what keeps `cmd /C npm` on Windows from ever
/// seeing metacharacters.
pub fn valid_spec(spec: &str) -> bool {
    if spec.is_empty() || spec.len() > 200 || spec.starts_with('-') {
        return false;
    }
    spec.chars()
        .all(|c| c.is_ascii_alphanumeric() || "@/:._~-#".contains(c))
}

/// Run `npm <args…> --prefix dir`, returning its combined output on
/// failure. On Windows npm is `npm.cmd`, which `Command::new` cannot start
/// directly; it goes through `cmd /C` (the spec was validated by the
/// caller, so nothing hostile reaches the shell).
async fn run_npm(args: &[&str], dir: &Path) -> Result<(), String> {
    let mut command = if cfg!(windows) {
        let mut command = Command::new("cmd");
        command.arg("/C").arg("npm");
        command
    } else {
        Command::new("npm")
    };
    let command = command
        .args(args)
        .arg("--prefix")
        .arg(dir)
        .args(["--no-audit", "--no-fund"])
        .stdin(std::process::Stdio::null())
        .output();

    let output = tokio::time::timeout(NPM_TIMEOUT, command)
        .await
        .map_err(|_| {
            format!(
                "npm {} did not finish within {} seconds",
                args.first().copied().unwrap_or(""),
                NPM_TIMEOUT.as_secs()
            )
        })?
        .map_err(|err| {
            format!(
                "npm could not be run: {err}\nManual alternative: cd {} && npm {}",
                dir.display(),
                args.join(" ")
            )
        })?;

    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let tail: String = stderr.lines().rev().take(20).collect::<Vec<_>>().join("\n");
    Err(format!(
        "npm {} failed:\n{}\nManual alternative: cd {} && npm {}",
        args.first().copied().unwrap_or(""),
        tail,
        dir.display(),
        args.join(" ")
    ))
}

/// `<dir>/package.json`, present so `npm --prefix` has a project to add
/// to. Written `0600` on unix: not sensitive, but the plugins dir is.
fn ensure_package_json(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|err| format!("create {}: {err}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let package_json = dir.join("package.json");
    if package_json.exists() {
        return Ok(());
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&package_json)
        .map_err(|err| format!("write {}: {err}", package_json.display()))?;
    use std::io::Write;
    file.write_all(b"{\"name\":\"any-converter-plugins\",\"private\":true}\n")
        .map_err(|err| format!("write {}: {err}", package_json.display()))
}

/// The bare package name of an npm spec (`@scope/name@1.2` →
/// `@scope/name`, `name` → `name`), for npm's uninstall.
pub fn package_name(spec: &str) -> &str {
    if let Some(rest) = spec.strip_prefix('@') {
        // Scoped: a second `@` starts a version pin; without one the whole
        // spec is the name (`split('@')` would answer "" for these).
        if let Some((name, _version)) = rest.rsplit_once('@') {
            return &spec[..name.len() + 1];
        }
        return spec;
    }
    spec.split('@').next().unwrap_or(spec)
}

/// Install `spec` into the plugins dir and report what it serves. A plugin
/// that installs but cannot be loaded is not an install failure — it is
/// reported through the listing's `error`, exactly as the CLI does.
pub async fn install(hosts: &ManagedHosts, spec: &str) -> Result<InstalledPlugin, String> {
    if !valid_spec(spec) {
        return Err(format!("not a plugin package spec: {spec}"));
    }
    let dir = managed::plugins_dir();
    let _guard = NPM_LOCK.lock().await;
    ensure_package_json(&dir)?;
    run_npm(&["install", spec], &dir).await?;

    // The fresh package is listed (loading it once) rather than assumed:
    // the listing carries what it serves, or why it would not load.
    Ok(hosts
        .installed(&dir)
        .await
        .into_iter()
        .find(|plugin| plugin.spec == package_name(spec))
        .unwrap_or(InstalledPlugin {
            spec: spec.to_string(),
            providers: Vec::new(),
            error: String::new(),
        }))
}

/// Remove the package `spec` names from the plugins dir.
pub async fn uninstall(spec: &str) -> Result<(), String> {
    let name = package_name(spec);
    if !valid_spec(name) {
        return Err(format!("not a plugin package name: {name}"));
    }
    let dir = managed::plugins_dir();
    let _guard = NPM_LOCK.lock().await;
    if !dir.join("node_modules").exists() {
        return Ok(()); // nothing installed anywhere
    }
    run_npm(&["uninstall", name], &dir).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_spec_accepts_npm_shapes_and_rejects_the_rest() {
        for spec in [
            "plugin-hello",
            "@magpie-community/opencode-cursor-auth",
            "@scope/pkg@1.2.3",
            "pkg@latest",
            "./tests/fixtures/plugin-hello",
        ] {
            assert!(valid_spec(spec), "{spec} should be valid");
        }
        for spec in ["", "-evil", "a; rm -rf /", "a b", "x&whoami"] {
            assert!(!valid_spec(spec), "{spec} should be rejected");
        }
    }

    #[test]
    fn test_package_name_strips_version_pins_keeps_scope() {
        assert_eq!(package_name("pkg"), "pkg");
        assert_eq!(package_name("pkg@1.2.3"), "pkg");
        assert_eq!(package_name("pkg@latest"), "pkg");
        assert_eq!(package_name("@scope/pkg"), "@scope/pkg");
        assert_eq!(package_name("@scope/pkg@1.2.3"), "@scope/pkg");
    }
}
