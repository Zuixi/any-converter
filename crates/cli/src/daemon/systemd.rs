//! The Linux systemd user unit: `~/.config/systemd/user/any-converter-gateway.service`,
//! enabled through `systemctl --user`, no root anywhere. Restarts on
//! failure after 5 seconds and follows the network. The unit holds only
//! the executable and paths — never a key or token.

// On a non-Linux build nothing but the tests uses this module.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use any_converter_server::home::Home;

use super::{
    DaemonError, Paths, Runner, ServiceManager, ServiceStatus, require_installed, require_success,
    write_service_file,
};

/// The systemd user unit's file name.
pub(crate) const UNIT: &str = "any-converter-gateway.service";

/// The systemd service manager for one home.
pub(crate) struct Systemd {
    paths: Paths,
    unit: PathBuf,
}

impl Systemd {
    /// Resolve the unit's paths: `$XDG_CONFIG_HOME` when set and
    /// absolute, else `$HOME/.config`; the executable is the running
    /// binary.
    pub(crate) fn discover(home: &Home) -> Result<Self, DaemonError> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|value| !value.is_empty())
                    .map(PathBuf::from)
                    .map(|home_dir| home_dir.join(".config"))
            })
            .ok_or(DaemonError::NoUserDir("HOME"))?;
        let unit_dir = config.join("systemd").join("user");
        let paths = Paths::new(
            home.root().to_path_buf(),
            Paths::absolute_executable()?,
            unit_dir,
            None,
        )?;
        Ok(Self::new(paths))
    }

    /// Build from explicit paths (tests).
    pub(crate) fn new(paths: Paths) -> Self {
        let unit = paths.unit_dir.join(UNIT);
        Self { paths, unit }
    }
}

/// Quote `value` as one word of a unit file: systemd reads `\` and `"`
/// escaped inside quotes, `%` as the start of a specifier, and `$` as the
/// start of a variable.
pub(crate) fn exec_quote(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
    )
}

/// Parse `systemctl show` output into its `Key=Value` lines.
pub(crate) fn parse_show(text: &str) -> BTreeMap<String, String> {
    let mut properties = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        properties.insert(key.to_owned(), value.to_owned());
    }
    properties
}

/// What to run so the service starts at boot and keeps running after
/// logout, when it doesn't: systemd starts a user's units with their
/// first login and stops them with their last, unless the user lingers.
/// `None` when it does linger, or when `loginctl` can't tell.
pub(crate) fn linger_advice(runner: &dyn Runner, user: Option<&str>) -> Option<String> {
    let user = user?;
    let output = runner
        .run(
            "loginctl",
            &["show-user", user, "--property=Linger", "--value"],
        )
        .ok()?;
    if !output.success {
        return None;
    }
    let linger = output.stdout.trim();
    if linger == "no" || linger == "Linger=no" {
        Some(format!(
            "run `loginctl enable-linger {user}` so the gateway starts without a login session"
        ))
    } else {
        None
    }
}

const SHOW_PROPERTIES: &str =
    "--property=ActiveState,SubState,MainPID,UnitFileState,ExecMainStatus";

impl ServiceManager for Systemd {
    fn kind(&self) -> &'static str {
        "systemd"
    }

    fn unit_path(&self) -> &Path {
        &self.unit
    }

    fn logs_hint(&self) -> String {
        format!("journalctl --user -u {UNIT} -f")
    }

    fn render(&self) -> String {
        let executable = exec_quote(&self.paths.executable.to_string_lossy());
        let home = exec_quote(&self.paths.home.to_string_lossy());
        format!(
            "[Unit]\n\
             Description=any-converter gateway\n\
             Wants=network-online.target\n\
             After=network-online.target\n\
             \n\
             [Service]\n\
             Type=simple\n\
             ExecStart={executable} serve --home {home}\n\
             Restart=on-failure\n\
             RestartSec=5\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n"
        )
    }

    fn is_installed(&self) -> bool {
        self.unit.exists()
    }

    fn install(&self, runner: &dyn Runner) -> Result<(), DaemonError> {
        write_service_file(&self.unit, &self.render())?;
        require_success(
            "systemctl",
            &["--user", "daemon-reload"],
            runner.run("systemctl", &["--user", "daemon-reload"])?,
        )?;
        require_success(
            "systemctl",
            &["--user", "enable", UNIT],
            runner.run("systemctl", &["--user", "enable", UNIT])?,
        )?;
        // restart, not start: an install over one running takes the new
        // unit.
        require_success(
            "systemctl",
            &["--user", "restart", UNIT],
            runner.run("systemctl", &["--user", "restart", UNIT])?,
        )?;
        Ok(())
    }

    fn uninstall(&self, runner: &dyn Runner) -> Result<(), DaemonError> {
        if !self.unit.exists() {
            return Ok(());
        }
        require_success(
            "systemctl",
            &["--user", "disable", "--now", UNIT],
            runner.run("systemctl", &["--user", "disable", "--now", UNIT])?,
        )?;
        // The key file and the journal stay: only the unit goes.
        std::fs::remove_file(&self.unit).map_err(|source| DaemonError::Io {
            path: self.unit.clone(),
            source,
        })?;
        require_success(
            "systemctl",
            &["--user", "daemon-reload"],
            runner.run("systemctl", &["--user", "daemon-reload"])?,
        )?;
        Ok(())
    }

    fn start(&self, runner: &dyn Runner) -> Result<(), DaemonError> {
        require_installed(self)?;
        require_success(
            "systemctl",
            &["--user", "start", UNIT],
            runner.run("systemctl", &["--user", "start", UNIT])?,
        )?;
        Ok(())
    }

    fn stop(&self, runner: &dyn Runner) -> Result<(), DaemonError> {
        require_installed(self)?;
        require_success(
            "systemctl",
            &["--user", "stop", UNIT],
            runner.run("systemctl", &["--user", "stop", UNIT])?,
        )?;
        Ok(())
    }

    fn restart(&self, runner: &dyn Runner) -> Result<(), DaemonError> {
        require_installed(self)?;
        require_success(
            "systemctl",
            &["--user", "restart", UNIT],
            runner.run("systemctl", &["--user", "restart", UNIT])?,
        )?;
        Ok(())
    }

    fn status(&self, runner: &dyn Runner) -> Result<ServiceStatus, DaemonError> {
        if !self.unit.exists() {
            return Ok(ServiceStatus {
                installed: false,
                enabled: false,
                running: false,
                pid: None,
                state: "not installed".to_owned(),
                last_exit: None,
                advice: None,
            });
        }
        let output = require_success(
            "systemctl",
            &["--user", "show", UNIT, SHOW_PROPERTIES],
            runner.run("systemctl", &["--user", "show", UNIT, SHOW_PROPERTIES])?,
        )?;
        let properties = parse_show(&output.stdout);
        let active = properties
            .get("ActiveState")
            .map(String::as_str)
            .unwrap_or("");
        let sub = properties.get("SubState").map(String::as_str).unwrap_or("");
        Ok(ServiceStatus {
            installed: true,
            enabled: properties
                .get("UnitFileState")
                .is_some_and(|value| value.starts_with("enabled")),
            running: active == "active",
            pid: properties
                .get("MainPID")
                .and_then(|value| value.parse::<u32>().ok())
                .filter(|pid| *pid > 0),
            state: format!("{active}/{sub}"),
            last_exit: properties
                .get("ExecMainStatus")
                .and_then(|value| value.parse::<i32>().ok()),
            advice: linger_advice(
                runner,
                std::env::var_os("USER")
                    .as_deref()
                    .and_then(|user| user.to_str()),
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::daemon::CommandOutput;
    use crate::daemon::test_support::FakeRunner;

    fn paths(dir: &Path) -> Paths {
        Paths::new(
            dir.to_path_buf(),
            PathBuf::from("/opt/any-converter/any-converter"),
            dir.join("config").join("systemd").join("user"),
            None,
        )
        .unwrap()
    }

    #[test]
    fn render_quotes_paths_and_names_the_policies() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Systemd::new(
            Paths::new(
                dir.path().to_path_buf(),
                PathBuf::from("/opt/any converter/any%converter$1 \"x\""),
                dir.path().join("config").join("systemd").join("user"),
                None,
            )
            .unwrap(),
        );
        let rendered = manager.render();
        let executable = exec_quote("/opt/any converter/any%converter$1 \"x\"");
        let home = exec_quote(&dir.path().to_string_lossy());
        for expected in [
            "[Unit]\nDescription=any-converter gateway",
            "Wants=network-online.target\nAfter=network-online.target",
            "[Service]\nType=simple",
            &format!("ExecStart={executable} serve --home {home}"),
            "Restart=on-failure\nRestartSec=5",
            "[Install]\nWantedBy=default.target",
        ] {
            assert!(
                rendered.contains(expected),
                "missing {expected:?}\n{rendered}"
            );
        }
        for forbidden in ["k=", "web-key", "Bearer", "sk-"] {
            assert!(
                !rendered.contains(forbidden),
                "{forbidden} leaked:\n{rendered}"
            );
        }
    }

    #[test]
    fn parse_show_maps_key_value_lines() {
        let properties = parse_show(
            "ActiveState=active\nSubState=running\nMainPID=4242\nUnitFileState=enabled\nExecMainStatus=0\n",
        );
        assert_eq!(
            properties.get("ActiveState").map(String::as_str),
            Some("active")
        );
        assert_eq!(properties.get("MainPID").map(String::as_str), Some("4242"));
        assert_eq!(properties.len(), 5);
        assert!(parse_show("no equals sign here\n").is_empty());
    }

    #[test]
    fn install_runs_reload_enable_restart_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Systemd::new(paths(dir.path()));
        let runner = FakeRunner::default();
        manager.install(&runner).unwrap();

        assert!(manager.unit_path().exists());
        assert_eq!(
            runner.recorded_program("systemctl"),
            vec![
                "systemctl --user daemon-reload",
                "systemctl --user enable any-converter-gateway.service",
                "systemctl --user restart any-converter-gateway.service",
            ]
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(manager.unit_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn status_reports_an_active_enabled_service() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Systemd::new(paths(dir.path()));
        let runner = FakeRunner::default();
        std::fs::create_dir_all(manager.unit_path().parent().unwrap()).unwrap();
        std::fs::write(manager.unit_path(), "x").unwrap();
        runner.script(
            "systemctl",
            &["--user", "show", UNIT, SHOW_PROPERTIES],
            CommandOutput {
                success: true,
                stdout: "ActiveState=active\nSubState=running\nMainPID=4242\nUnitFileState=enabled\nExecMainStatus=0\n"
                    .to_owned(),
                stderr: String::new(),
            },
        );

        let status = manager.status(&runner).unwrap();
        assert!(status.installed);
        assert!(status.enabled);
        assert!(status.running);
        assert_eq!(status.pid, Some(4242));
        assert_eq!(status.state, "active/running");
        assert_eq!(status.last_exit, Some(0));
    }

    #[test]
    fn status_reports_a_failed_service() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Systemd::new(paths(dir.path()));
        let runner = FakeRunner::default();
        std::fs::create_dir_all(manager.unit_path().parent().unwrap()).unwrap();
        std::fs::write(manager.unit_path(), "x").unwrap();
        runner.script(
            "systemctl",
            &["--user", "show", UNIT, SHOW_PROPERTIES],
            CommandOutput {
                success: true,
                stdout: "ActiveState=failed\nSubState=failed\nMainPID=0\nUnitFileState=enabled\nExecMainStatus=1\n"
                    .to_owned(),
                stderr: String::new(),
            },
        );

        let status = manager.status(&runner).unwrap();
        assert!(!status.running);
        assert_eq!(status.pid, None);
        assert_eq!(status.state, "failed/failed");
        assert_eq!(status.last_exit, Some(1));
    }

    #[test]
    fn uninstall_disables_then_removes_then_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let web_key = dir.path().join("web-key");
        std::fs::write(&web_key, "0123456789abcdef").unwrap();
        let manager = Systemd::new(paths(dir.path()));
        let runner = FakeRunner::default();
        manager.install(&runner).unwrap();
        manager.uninstall(&runner).unwrap();

        assert!(!manager.unit_path().exists());
        assert!(web_key.exists());
        assert_eq!(
            runner.recorded_program("systemctl"),
            vec![
                "systemctl --user daemon-reload",
                "systemctl --user enable any-converter-gateway.service",
                "systemctl --user restart any-converter-gateway.service",
                "systemctl --user disable --now any-converter-gateway.service",
                "systemctl --user daemon-reload",
            ]
        );
    }

    #[test]
    fn start_without_a_file_is_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Systemd::new(paths(dir.path()));
        let runner = FakeRunner::default();
        let error = manager.start(&runner).unwrap_err();
        assert!(matches!(error, DaemonError::NotInstalled), "{error:?}");
        assert!(runner.recorded().is_empty());
    }

    #[test]
    fn status_without_a_file_runs_no_commands() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Systemd::new(paths(dir.path()));
        let runner = FakeRunner::default();

        let status = manager.status(&runner).unwrap();
        assert!(!status.installed);
        assert_eq!(status.state, "not installed");
        assert!(runner.recorded().is_empty());
    }

    #[test]
    fn linger_advice_names_enable_linger_when_off() {
        let runner = FakeRunner::default();
        runner.script(
            "loginctl",
            &["show-user", "tester", "--property=Linger", "--value"],
            CommandOutput {
                success: true,
                stdout: "no\n".to_owned(),
                stderr: String::new(),
            },
        );
        let advice = linger_advice(&runner, Some("tester")).unwrap();
        assert!(advice.contains("loginctl enable-linger tester"), "{advice}");

        runner.script(
            "loginctl",
            &["show-user", "tester", "--property=Linger", "--value"],
            CommandOutput {
                success: true,
                stdout: "Linger=no\n".to_owned(),
                stderr: String::new(),
            },
        );
        assert!(linger_advice(&runner, Some("tester")).is_some());
    }

    #[test]
    fn linger_advice_is_silent_when_lingering_or_unknown() {
        let runner = FakeRunner::default();
        // A default empty success means "yes" was not reported.
        assert!(linger_advice(&runner, Some("tester")).is_none());
        // A failing loginctl is ignored.
        runner.script(
            "loginctl",
            &["show-user", "tester", "--property=Linger", "--value"],
            CommandOutput {
                success: false,
                stdout: String::new(),
                stderr: "no loginctl".to_owned(),
            },
        );
        assert!(linger_advice(&runner, Some("tester")).is_none());
        // No user, no advice.
        assert!(linger_advice(&runner, None).is_none());
    }
}
