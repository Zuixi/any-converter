//! The macOS launch agent: `~/Library/LaunchAgents/com.any-converter.gateway.plist`,
//! loaded into the user's GUI domain. Started at login (`RunAtLoad`) and
//! again when it stops with a failure (`KeepAlive`), logging to
//! `~/Library/Logs/any-converter/gateway.log`. The plist holds only the
//! executable and paths — never a key or token.

// On a non-macOS build nothing but the tests uses this module.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::path::{Path, PathBuf};

use any_converter_server::home::Home;

use super::{
    DaemonError, Paths, Runner, ServiceManager, ServiceStatus, require_installed, require_success,
    write_service_file,
};

/// The launch agent's label, and the stem of its plist file name.
pub(crate) const LABEL: &str = "com.any-converter.gateway";
const PLIST_NAME: &str = "com.any-converter.gateway.plist";
const LOG_FILE: &str = "gateway.log";

/// The launchd service manager for one home.
pub(crate) struct Launchd {
    paths: Paths,
    plist: PathBuf,
}

impl Launchd {
    /// Resolve the launch agent's paths from `$HOME` and the running
    /// executable.
    pub(crate) fn discover(home: &Home) -> Result<Self, DaemonError> {
        let home_dir = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or(DaemonError::NoUserDir("HOME"))?;
        let unit_dir = home_dir.join("Library").join("LaunchAgents");
        let log_dir = home_dir.join("Library").join("Logs").join("any-converter");
        let paths = Paths::new(
            home.root().to_path_buf(),
            Paths::absolute_executable()?,
            unit_dir,
            Some(log_dir),
        )?;
        Ok(Self::new(paths))
    }

    /// Build from explicit paths (tests).
    pub(crate) fn new(paths: Paths) -> Self {
        let plist = paths.unit_dir.join(PLIST_NAME);
        Self { paths, plist }
    }

    fn log_path(&self) -> PathBuf {
        match &self.paths.log_dir {
            Some(dir) => dir.join(LOG_FILE),
            None => self.paths.home.join(LOG_FILE),
        }
    }

    /// The user's uid, as `id -u` says it: required and all digits.
    fn uid(&self, runner: &dyn Runner) -> Result<String, DaemonError> {
        let output = require_success("id", &["-u"], runner.run("id", &["-u"])?)?;
        let uid = output.stdout.trim().to_owned();
        if uid.is_empty() || !uid.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(DaemonError::Command {
                program: "id".to_owned(),
                args: "-u".to_owned(),
                detail: format!("unexpected `id -u` output: {uid:?}"),
            });
        }
        Ok(uid)
    }

    /// `gui/<uid>/<label>`, the service's full launchd name.
    fn target(&self, uid: &str) -> String {
        format!("gui/{uid}/{LABEL}")
    }

    /// `gui/<uid>`, the domain agents are bootstrapped into.
    fn domain(uid: &str) -> String {
        format!("gui/{uid}")
    }

    /// Whether launchd has the agent loaded, running or not.
    fn loaded(&self, runner: &dyn Runner, uid: &str) -> Result<bool, DaemonError> {
        let target = self.target(uid);
        Ok(runner.run("launchctl", &["print", &target])?.success)
    }
}

/// XML-escape `& < > " '`, `&` first so its own escape is not escaped
/// again.
pub(crate) fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Parse `launchctl print` output: the first `state` (its last
/// whitespace segment), the first positive `pid`, and the first
/// `last exit code`.
pub(crate) fn parse_print(text: &str) -> (String, Option<u32>, Option<i32>) {
    let mut state = String::new();
    let mut pid = None;
    let mut last_exit = None;
    for line in text.lines() {
        let Some((key, value)) = split_print(line) else {
            continue;
        };
        match key {
            "state" if state.is_empty() => {
                state = value.split_whitespace().last().unwrap_or("").to_owned();
            }
            "pid" if pid.is_none() => pid = value.parse::<u32>().ok(),
            "last exit code" if last_exit.is_none() => last_exit = value.parse::<i32>().ok(),
            _ => {}
        }
    }
    (state, pid, last_exit)
}

/// `launchctl print` lines are `key = value`; accept `key=value` too.
fn split_print(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if let Some((key, value)) = line.split_once(" = ") {
        return Some((key, value));
    }
    let (key, value) = line.split_once('=')?;
    Some((key.trim(), value.trim()))
}

impl ServiceManager for Launchd {
    fn kind(&self) -> &'static str {
        "launchd"
    }

    fn unit_path(&self) -> &Path {
        &self.plist
    }

    fn logs_hint(&self) -> String {
        self.log_path().to_string_lossy().into_owned()
    }

    fn render(&self) -> String {
        format!(
            concat!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
                "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
                "<plist version=\"1.0\">\n",
                "<dict>\n",
                "  <key>Label</key><string>{label}</string>\n",
                "  <key>ProgramArguments</key>\n",
                "  <array><string>{exe}</string><string>serve</string><string>--home</string><string>{home}</string></array>\n",
                "  <key>RunAtLoad</key><true/>\n",
                "  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>\n",
                "  <key>ThrottleInterval</key><integer>5</integer>\n",
                "  <key>ProcessType</key><string>Background</string>\n",
                "  <key>StandardOutPath</key><string>{log}</string>\n",
                "  <key>StandardErrorPath</key><string>{log}</string>\n",
                "</dict>\n",
                "</plist>\n",
            ),
            label = LABEL,
            exe = xml_escape(&self.paths.executable.to_string_lossy()),
            home = xml_escape(&self.paths.home.to_string_lossy()),
            log = xml_escape(&self.log_path().to_string_lossy()),
        )
    }

    fn is_installed(&self) -> bool {
        self.plist.exists()
    }

    fn install(&self, runner: &dyn Runner) -> Result<(), DaemonError> {
        if let Some(parent) = self.plist.parent() {
            std::fs::create_dir_all(parent).map_err(|source| DaemonError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        // The gateway's stdout (which can print an unlock URL) lands here:
        // the directory and file must not be world-readable. Pre-creating
        // the file 0600 matters because launchd opens it under the umask.
        if let Some(log_dir) = &self.paths.log_dir {
            prepare_log_dir(log_dir)?;
        }
        write_service_file(&self.plist, &self.render())?;
        let uid = self.uid(runner)?;
        let target = self.target(&uid);
        let domain = Self::domain(&uid);
        // One loaded before runs the old plist until it is taken out.
        if self.loaded(runner, &uid)? {
            require_success(
                "launchctl",
                &["bootout", &target],
                runner.run("launchctl", &["bootout", &target])?,
            )?;
        }
        let plist_path = self.plist.to_string_lossy().into_owned();
        require_success(
            "launchctl",
            &["bootstrap", &domain, &plist_path],
            runner.run("launchctl", &["bootstrap", &domain, &plist_path])?,
        )?;
        Ok(())
    }

    fn uninstall(&self, runner: &dyn Runner) -> Result<(), DaemonError> {
        if !self.plist.exists() {
            return Ok(());
        }
        let uid = self.uid(runner)?;
        let target = self.target(&uid);
        if self.loaded(runner, &uid)? {
            require_success(
                "launchctl",
                &["bootout", &target],
                runner.run("launchctl", &["bootout", &target])?,
            )?;
        }
        // The key file and the logs stay: only the agent goes.
        std::fs::remove_file(&self.plist).map_err(|source| DaemonError::Io {
            path: self.plist.clone(),
            source,
        })?;
        Ok(())
    }

    fn start(&self, runner: &dyn Runner) -> Result<(), DaemonError> {
        require_installed(self)?;
        let uid = self.uid(runner)?;
        let target = self.target(&uid);
        if self.loaded(runner, &uid)? {
            require_success(
                "launchctl",
                &["kickstart", "-k", &target],
                runner.run("launchctl", &["kickstart", "-k", &target])?,
            )?;
        } else {
            let domain = Self::domain(&uid);
            let plist_path = self.plist.to_string_lossy().into_owned();
            require_success(
                "launchctl",
                &["bootstrap", &domain, &plist_path],
                runner.run("launchctl", &["bootstrap", &domain, &plist_path])?,
            )?;
        }
        Ok(())
    }

    fn stop(&self, runner: &dyn Runner) -> Result<(), DaemonError> {
        require_installed(self)?;
        let uid = self.uid(runner)?;
        let target = self.target(&uid);
        if self.loaded(runner, &uid)? {
            // Stopped alone, KeepAlive would have it back; boot it out.
            require_success(
                "launchctl",
                &["bootout", &target],
                runner.run("launchctl", &["bootout", &target])?,
            )?;
        }
        Ok(())
    }

    fn restart(&self, runner: &dyn Runner) -> Result<(), DaemonError> {
        require_installed(self)?;
        let uid = self.uid(runner)?;
        let target = self.target(&uid);
        if self.loaded(runner, &uid)? {
            require_success(
                "launchctl",
                &["kickstart", "-k", &target],
                runner.run("launchctl", &["kickstart", "-k", &target])?,
            )?;
        } else {
            let domain = Self::domain(&uid);
            let plist_path = self.plist.to_string_lossy().into_owned();
            require_success(
                "launchctl",
                &["bootstrap", &domain, &plist_path],
                runner.run("launchctl", &["bootstrap", &domain, &plist_path])?,
            )?;
        }
        Ok(())
    }

    fn status(&self, runner: &dyn Runner) -> Result<ServiceStatus, DaemonError> {
        if !self.plist.exists() {
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
        let uid = self.uid(runner)?;
        let target = self.target(&uid);
        let output = runner.run("launchctl", &["print", &target])?;
        if !output.success {
            // A plist in LaunchAgents is loaded at every login, just not
            // right now.
            return Ok(ServiceStatus {
                installed: true,
                enabled: true,
                running: false,
                pid: None,
                state: "not loaded".to_owned(),
                last_exit: None,
                advice: None,
            });
        }
        let (state, pid, last_exit) = parse_print(&output.stdout);
        Ok(ServiceStatus {
            installed: true,
            enabled: true,
            running: state == "running",
            state,
            pid,
            last_exit,
            advice: None,
        })
    }
}

/// Create the launchd log directory 0700 and the log file 0600. launchd
/// appends to the file under the user's umask, so an existing world-mode
/// file is repaired and a missing one is pre-created private.
fn prepare_log_dir(log_dir: &std::path::Path) -> Result<(), DaemonError> {
    std::fs::create_dir_all(log_dir).map_err(|source| DaemonError::Io {
        path: log_dir.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        std::fs::set_permissions(log_dir, std::fs::Permissions::from_mode(0o700)).map_err(
            |source| DaemonError::Io {
                path: log_dir.to_path_buf(),
                source,
            },
        )?;
        let log_file = log_dir.join(LOG_FILE);
        // Opening create+append is the touch: launchd then appends to an
        // already-private file instead of creating one under the umask.
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true).mode(0o600);
        options.open(&log_file).map_err(|source| DaemonError::Io {
            path: log_file.clone(),
            source,
        })?;
        std::fs::set_permissions(&log_file, std::fs::Permissions::from_mode(0o600)).map_err(
            |source| DaemonError::Io {
                path: log_file,
                source,
            },
        )?;
    }
    #[cfg(not(unix))]
    let _ = log_dir;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::daemon::CommandOutput;
    use crate::daemon::test_support::{FakeRunner, script_uid};

    fn paths(dir: &Path) -> Paths {
        Paths::new(
            dir.to_path_buf(),
            PathBuf::from("/opt/any-converter/any-converter"),
            dir.join("LaunchAgents"),
            Some(dir.join("Logs")),
        )
        .unwrap()
    }

    const PRINT_RUNNING: &str = "com.any-converter.gateway = {\n\
         \tactive count = 1\n\
         \tstate = running\n\
         \tprogram = /opt/any-converter/any-converter\n\
         \tpid = 4242\n\
         \tlast exit code = 0\n\
         \tendpoints = {\n\
         \t\tstate = active\n\
         \t}\n\
         }\n";

    #[test]
    fn render_is_a_valid_shape_without_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Launchd::new(paths(dir.path()));
        let rendered = manager.render();
        for expected in [
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
            "<key>Label</key><string>com.any-converter.gateway</string>",
            "<array><string>/opt/any-converter/any-converter</string><string>serve</string>",
            "<string>--home</string>",
            &format!("<string>{}</string>", dir.path().display()),
            "<key>RunAtLoad</key><true/>",
            "<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>",
            "<key>ThrottleInterval</key><integer>5</integer>",
            "<key>ProcessType</key><string>Background</string>",
            &format!(
                "<key>StandardOutPath</key><string>{}</string>",
                dir.path().join("Logs").join("gateway.log").display()
            ),
            &format!(
                "<key>StandardErrorPath</key><string>{}</string>",
                dir.path().join("Logs").join("gateway.log").display()
            ),
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
    fn render_escapes_xml_specials_in_paths() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("a&b \"c\" d<e>f'g");
        let manager = Launchd::new(
            Paths::new(
                home.clone(),
                PathBuf::from("/opt/any-converter/any-converter"),
                dir.path().join("LaunchAgents"),
                Some(dir.path().join("Logs")),
            )
            .unwrap(),
        );
        let rendered = manager.render();
        let escaped = xml_escape(&home.to_string_lossy());
        assert!(
            rendered.contains(&format!("<string>{escaped}</string>")),
            "{rendered}"
        );
        assert!(!rendered.contains("a&b \"c\" d<e>f'g"), "{rendered}");
    }

    #[test]
    fn parse_print_reads_state_pid_and_exit_code() {
        let (state, pid, exit) = parse_print(PRINT_RUNNING);
        assert_eq!(state, "running");
        assert_eq!(pid, Some(4242));
        assert_eq!(exit, Some(0));
    }

    #[test]
    fn parse_print_reads_a_not_running_state() {
        let (state, pid, exit) =
            parse_print("com.x = {\n\tstate = exited\n\tlast exit code = 1\n}\n");
        assert_eq!(state, "exited");
        assert_eq!(pid, None);
        assert_eq!(exit, Some(1));
    }

    #[test]
    fn parse_print_defaults_missing_keys() {
        assert_eq!(parse_print(""), (String::new(), None, None));
        let (state, pid, exit) = parse_print("com.x = {\n\tactive count = 0\n}\n");
        assert_eq!(state, String::new());
        assert_eq!(pid, None);
        assert_eq!(exit, None);
    }

    #[test]
    fn first_install_writes_the_plist_and_bootstraps() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Launchd::new(paths(dir.path()));
        let runner = FakeRunner::default();
        script_uid(&runner, "501");
        runner.script(
            "launchctl",
            &["print", "gui/501/com.any-converter.gateway"],
            CommandOutput {
                success: false,
                stdout: String::new(),
                stderr: "Could not find service".to_owned(),
            },
        );
        manager.install(&runner).unwrap();

        assert!(manager.unit_path().exists());
        let plist_path = manager.unit_path().to_string_lossy().into_owned();
        assert_eq!(
            runner.recorded_program("launchctl"),
            vec![
                "launchctl print gui/501/com.any-converter.gateway".to_owned(),
                format!("launchctl bootstrap gui/501 {plist_path}"),
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
    fn reinstall_boots_out_before_bootstrap() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Launchd::new(paths(dir.path()));
        let runner = FakeRunner::default();
        script_uid(&runner, "501");
        runner.script(
            "launchctl",
            &["print", "gui/501/com.any-converter.gateway"],
            CommandOutput {
                success: true,
                stdout: PRINT_RUNNING.to_owned(),
                stderr: String::new(),
            },
        );
        manager.install(&runner).unwrap();

        let plist_path = manager.unit_path().to_string_lossy().into_owned();
        assert_eq!(
            runner.recorded_program("launchctl"),
            vec![
                "launchctl print gui/501/com.any-converter.gateway".to_owned(),
                "launchctl bootout gui/501/com.any-converter.gateway".to_owned(),
                format!("launchctl bootstrap gui/501 {plist_path}"),
            ]
        );
    }

    #[test]
    fn start_bootstraps_an_unloaded_service() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Launchd::new(paths(dir.path()));
        let runner = FakeRunner::default();
        script_uid(&runner, "501");
        runner.script(
            "launchctl",
            &["print", "gui/501/com.any-converter.gateway"],
            CommandOutput {
                success: false,
                stdout: String::new(),
                stderr: "Could not find service".to_owned(),
            },
        );
        std::fs::create_dir_all(manager.unit_path().parent().unwrap()).unwrap();
        std::fs::write(manager.unit_path(), "x").unwrap();
        manager.start(&runner).unwrap();

        let plist_path = manager.unit_path().to_string_lossy().into_owned();
        assert_eq!(
            runner.recorded_program("launchctl"),
            vec![
                "launchctl print gui/501/com.any-converter.gateway".to_owned(),
                format!("launchctl bootstrap gui/501 {plist_path}"),
            ]
        );
    }

    #[test]
    fn start_on_a_loaded_service_kickstarts() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Launchd::new(paths(dir.path()));
        let runner = FakeRunner::default();
        script_uid(&runner, "501");
        runner.script(
            "launchctl",
            &["print", "gui/501/com.any-converter.gateway"],
            CommandOutput {
                success: true,
                stdout: PRINT_RUNNING.to_owned(),
                stderr: String::new(),
            },
        );
        std::fs::create_dir_all(manager.unit_path().parent().unwrap()).unwrap();
        std::fs::write(manager.unit_path(), "x").unwrap();
        manager.start(&runner).unwrap();

        assert_eq!(
            runner.recorded_program("launchctl"),
            vec![
                "launchctl print gui/501/com.any-converter.gateway".to_owned(),
                "launchctl kickstart -k gui/501/com.any-converter.gateway".to_owned(),
            ]
        );
    }

    #[test]
    fn stop_on_an_unloaded_service_runs_no_launchctl() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Launchd::new(paths(dir.path()));
        let runner = FakeRunner::default();
        script_uid(&runner, "501");
        runner.script(
            "launchctl",
            &["print", "gui/501/com.any-converter.gateway"],
            CommandOutput {
                success: false,
                stdout: String::new(),
                stderr: "Could not find service".to_owned(),
            },
        );
        std::fs::create_dir_all(manager.unit_path().parent().unwrap()).unwrap();
        std::fs::write(manager.unit_path(), "x").unwrap();
        manager.stop(&runner).unwrap();

        // Only the loaded-check ran; nothing was booted out or kicked.
        assert_eq!(
            runner.recorded_program("launchctl"),
            vec!["launchctl print gui/501/com.any-converter.gateway".to_owned()]
        );
    }

    #[test]
    fn stop_on_a_loaded_service_boots_it_out() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Launchd::new(paths(dir.path()));
        let runner = FakeRunner::default();
        script_uid(&runner, "501");
        runner.script(
            "launchctl",
            &["print", "gui/501/com.any-converter.gateway"],
            CommandOutput {
                success: true,
                stdout: PRINT_RUNNING.to_owned(),
                stderr: String::new(),
            },
        );
        std::fs::create_dir_all(manager.unit_path().parent().unwrap()).unwrap();
        std::fs::write(manager.unit_path(), "x").unwrap();
        manager.stop(&runner).unwrap();

        assert_eq!(
            runner.recorded_program("launchctl"),
            vec![
                "launchctl print gui/501/com.any-converter.gateway".to_owned(),
                "launchctl bootout gui/501/com.any-converter.gateway".to_owned(),
            ]
        );
    }

    #[test]
    fn uninstall_removes_the_plist_and_leaves_the_key_file() {
        let dir = tempfile::tempdir().unwrap();
        let web_key = dir.path().join("web-key");
        std::fs::write(&web_key, "0123456789abcdef").unwrap();
        let manager = Launchd::new(paths(dir.path()));
        let runner = FakeRunner::default();
        script_uid(&runner, "501");
        manager.install(&runner).unwrap();
        manager.uninstall(&runner).unwrap();

        assert!(!manager.unit_path().exists());
        assert!(web_key.exists());
    }

    #[test]
    fn start_without_a_file_is_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Launchd::new(paths(dir.path()));
        let runner = FakeRunner::default();
        let error = manager.start(&runner).unwrap_err();
        assert!(matches!(error, DaemonError::NotInstalled), "{error:?}");
        assert!(runner.recorded().is_empty());
    }

    #[test]
    fn status_of_a_loaded_service_reports_the_system_state() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Launchd::new(paths(dir.path()));
        let runner = FakeRunner::default();
        script_uid(&runner, "501");
        runner.script(
            "launchctl",
            &["print", "gui/501/com.any-converter.gateway"],
            CommandOutput {
                success: true,
                stdout: PRINT_RUNNING.to_owned(),
                stderr: String::new(),
            },
        );
        std::fs::create_dir_all(manager.unit_path().parent().unwrap()).unwrap();
        std::fs::write(manager.unit_path(), "x").unwrap();

        let status = manager.status(&runner).unwrap();
        assert!(status.installed);
        assert!(status.enabled);
        assert!(status.running);
        assert_eq!(status.pid, Some(4242));
        assert_eq!(status.state, "running");
        assert_eq!(status.last_exit, Some(0));
    }

    #[test]
    fn status_of_an_installed_but_unloaded_service_says_not_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Launchd::new(paths(dir.path()));
        let runner = FakeRunner::default();
        script_uid(&runner, "501");
        runner.script(
            "launchctl",
            &["print", "gui/501/com.any-converter.gateway"],
            CommandOutput {
                success: false,
                stdout: String::new(),
                stderr: "Could not find service".to_owned(),
            },
        );
        std::fs::create_dir_all(manager.unit_path().parent().unwrap()).unwrap();
        std::fs::write(manager.unit_path(), "x").unwrap();

        let status = manager.status(&runner).unwrap();
        assert!(status.installed);
        assert!(status.enabled);
        assert!(!status.running);
        assert_eq!(status.state, "not loaded");
    }

    #[test]
    fn status_without_a_file_runs_no_commands() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Launchd::new(paths(dir.path()));
        let runner = FakeRunner::default();

        let status = manager.status(&runner).unwrap();
        assert!(!status.installed);
        assert_eq!(status.state, "not installed");
        assert!(runner.recorded().is_empty());
    }
}
