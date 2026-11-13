//! `any-converter daemon …`: run the gateway as a service of the user's
//! own session — a launch agent on macOS, a systemd user unit on Linux.
//! The system starts it at login and again when it stops with a failure,
//! with no window and no desktop session. Whether it is installed and
//! running is the system's own record, read each time, never a setting of
//! any-converter's.
//!
//! Nothing here uses `sudo`, writes outside the invoking user's home, or
//! installs a system service. The page key lives only in
//! `<home>/web-key`, never in a service file.

use std::path::{Path, PathBuf};

use any_converter_server::config::{DEFAULT_PORT, ServerConfig};
use any_converter_server::config_store::ConfigStore;
use any_converter_server::home::Home;
use any_converter_server::web_auth;

use crate::agents_cmd;

pub(crate) mod launchd;
pub(crate) mod systemd;

#[cfg(target_os = "macos")]
use launchd::Launchd;
#[cfg(target_os = "linux")]
use systemd::Systemd;

/// The service-management subcommands of `any-converter daemon`.
#[derive(Debug, clap::Subcommand)]
pub(crate) enum DaemonCommand {
    /// Write the service file, load it, and start the gateway at login
    Install {
        /// Print the service file without writing or running anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Unload the service and remove its file
    Uninstall,
    /// Start the installed service now
    Start,
    /// Stop the installed service until the next login or Start
    Stop,
    /// Stop and start the installed service (picks up a new binary)
    Restart,
    /// What the system says of the service
    Status,
}

/// Everything `any-converter daemon` can fail with. Messages never include
/// the page key.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DaemonError {
    // Constructed only on platforms without a service manager.
    #[cfg_attr(any(target_os = "macos", target_os = "linux"), allow(dead_code))]
    #[error("daemon management is supported on macOS and Linux only")]
    Unsupported,
    #[error("cannot locate the current executable: {0}")]
    CurrentExe(std::io::Error),
    #[error("cannot determine the user directory; set {0}")]
    NoUserDir(&'static str),
    #[error("{program} {args} failed: {detail}")]
    Command {
        program: String,
        args: String,
        detail: String,
    },
    #[error("service file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("path cannot be stored in a service file: {0}")]
    InvalidPath(PathBuf),
    #[error("the gateway service is not installed; run `any-converter daemon install`")]
    NotInstalled,
    #[error(
        "a gateway is already running at {url}; stop it (desktop app or `serve`) before installing the daemon"
    )]
    GatewayRunning { url: String },
    #[error("web key: {0}")]
    WebKey(#[from] web_auth::WebKeyError),
    #[error("cannot load the canonical config: {0}")]
    Config(#[from] any_converter_server::config_store::ConfigStoreError),
}

/// What one service-manager tool invocation printed and whether it
/// succeeded. Never trimmed here: parsers trim what they read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Runs the service manager's tools (`launchctl`, `systemctl`, `id`,
/// `loginctl`). Tests substitute a fake that records calls and returns
/// scripted output, so no test ever runs a real `launchctl`/`systemctl`.
pub(crate) trait Runner {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, DaemonError>;
}

/// The real runner: `std::process::Command::output`. A spawn failure is a
/// [`DaemonError::Command`]; a nonzero exit is a plain
/// `Ok(CommandOutput { success: false, .. })` so callers can treat
/// "not loaded"/"not running" as state.
pub(crate) struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, DaemonError> {
        let output = std::process::Command::new(program)
            .args(args)
            .output()
            .map_err(|source| DaemonError::Command {
                program: program.to_owned(),
                args: args.join(" "),
                detail: source.to_string(),
            })?;
        Ok(CommandOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// The paths one service file is built from: the canonical home the
/// gateway serves, the executable that serves it, the directory the
/// service file goes in, and (launchd only) the directory the service
/// writes its log to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Paths {
    pub home: PathBuf,       // absolute
    pub executable: PathBuf, // absolute
    pub unit_dir: PathBuf,   // absolute
    pub log_dir: Option<PathBuf>,
}

impl Paths {
    /// Validate and assemble the service paths. A path containing `\n`,
    /// `\r`, or any other control character cannot be stored in a service
    /// file and is rejected.
    pub(crate) fn new(
        home: PathBuf,
        executable: PathBuf,
        unit_dir: PathBuf,
        log_dir: Option<PathBuf>,
    ) -> Result<Self, DaemonError> {
        for path in [&home, &executable, &unit_dir] {
            check_path(path)?;
        }
        if let Some(log_dir) = &log_dir {
            check_path(log_dir)?;
        }
        Ok(Self {
            home,
            executable,
            unit_dir,
            log_dir,
        })
    }

    /// The absolute path of the running executable: `current_exe`,
    /// canonicalized when possible (a symlinked install still points at
    /// the real binary), kept as-is when that fails. Must be absolute.
    pub(crate) fn absolute_executable() -> Result<PathBuf, DaemonError> {
        let executable = std::env::current_exe().map_err(DaemonError::CurrentExe)?;
        let executable = match executable.canonicalize() {
            Ok(real) => real,
            Err(_) => executable,
        };
        if !executable.is_absolute() {
            return Err(DaemonError::InvalidPath(executable));
        }
        Ok(executable)
    }
}

fn check_path(path: &Path) -> Result<(), DaemonError> {
    if path.to_string_lossy().chars().any(char::is_control) {
        return Err(DaemonError::InvalidPath(path.to_path_buf()));
    }
    Ok(())
}

/// What the system says of the service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServiceStatus {
    pub installed: bool,
    pub enabled: bool,
    pub running: bool,
    pub pid: Option<u32>,
    pub state: String,
    pub last_exit: Option<i32>,
    pub advice: Option<String>,
}

/// One system's way of running the service.
pub(crate) trait ServiceManager {
    /// `"launchd"` or `"systemd"`.
    fn kind(&self) -> &'static str;
    /// The service file's path.
    fn unit_path(&self) -> &Path;
    /// The log path, or the journalctl command that follows it.
    fn logs_hint(&self) -> String;
    /// The service file's contents. Never contains a key or token.
    fn render(&self) -> String;
    /// Whether the service file is on disk.
    fn is_installed(&self) -> bool;
    fn install(&self, runner: &dyn Runner) -> Result<(), DaemonError>;
    fn uninstall(&self, runner: &dyn Runner) -> Result<(), DaemonError>;
    fn start(&self, runner: &dyn Runner) -> Result<(), DaemonError>;
    fn stop(&self, runner: &dyn Runner) -> Result<(), DaemonError>;
    fn restart(&self, runner: &dyn Runner) -> Result<(), DaemonError>;
    fn status(&self, runner: &dyn Runner) -> Result<ServiceStatus, DaemonError>;
}

/// `Start`/`Stop`/`Restart` before an install.
pub(crate) fn require_installed(manager: &dyn ServiceManager) -> Result<(), DaemonError> {
    if manager.is_installed() {
        Ok(())
    } else {
        Err(DaemonError::NotInstalled)
    }
}

/// The host an operator's terminal can reach the gateway on: wildcard
/// binds (`0.0.0.0`, `::`) print `127.0.0.1`, bare IPv6 addresses are
/// bracketed, hostnames stay as given.
pub(crate) fn display_host_port(host: &str, port: u16) -> String {
    format!("{}:{port}", display_host(host))
}

fn display_host(host: &str) -> String {
    let bare = host
        .strip_prefix('[')
        .and_then(|bracketed| bracketed.strip_suffix(']'))
        .unwrap_or(host);
    match bare.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) if ip.is_unspecified() => "127.0.0.1".to_owned(),
        Ok(std::net::IpAddr::V6(ip)) if ip.is_unspecified() => "127.0.0.1".to_owned(),
        Ok(std::net::IpAddr::V4(ip)) => ip.to_string(),
        Ok(std::net::IpAddr::V6(ip)) => format!("[{ip}]"),
        Err(_) => host.to_owned(),
    }
}

/// The gateway's configured host and port. A missing canonical config
/// means the defaults; a config the daemon's own `serve` would fail to
/// parse is an error.
pub(crate) fn gateway_base(home: &Home) -> Result<(String, u16), DaemonError> {
    let config = match ConfigStore::from_home(home).load_if_exists()? {
        Some(document) => document.config().clone(),
        None => ServerConfig::default(),
    };
    Ok((config.server.host, config.server.port))
}

/// Write a service file: parent directories first, then `0600` on Unix.
pub(crate) fn write_service_file(path: &Path, contents: &str) -> Result<(), DaemonError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| DaemonError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    crate::write_private(path, contents).map_err(|source| DaemonError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(
            |source| DaemonError::Io {
                path: path.to_path_buf(),
                source,
            },
        )?;
    }
    Ok(())
}

/// A successful command's output, or the error its failure produced.
pub(crate) fn require_success(
    program: &str,
    args: &[&str],
    output: CommandOutput,
) -> Result<CommandOutput, DaemonError> {
    if output.success {
        return Ok(output);
    }
    let detail = if output.stderr.trim().is_empty() {
        "exit status indicates failure".to_owned()
    } else {
        output.stderr.trim().to_owned()
    };
    Err(DaemonError::Command {
        program: program.to_owned(),
        args: args.join(" "),
        detail,
    })
}

fn platform_manager(home: &Home) -> Result<Box<dyn ServiceManager>, DaemonError> {
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(Launchd::discover(home)?))
    }
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(Systemd::discover(home)?))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err(DaemonError::Unsupported)
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

/// Entry point from `main`: resolve the home, build the platform manager,
/// and run one `daemon` command. Everything the operator sees is printed
/// here; the rest of the module only returns values or errors.
#[allow(clippy::print_stdout)]
pub(crate) async fn run(
    home_arg: Option<PathBuf>,
    command: DaemonCommand,
) -> Result<(), Box<dyn std::error::Error>> {
    let home = match home_arg {
        Some(root) => Home::from_root(root),
        None => Home::discover()?,
    };
    let runner = SystemRunner;
    let manager = platform_manager(&home)?;
    let manager: &dyn ServiceManager = &*manager;
    match command {
        DaemonCommand::Install { dry_run: true } => {
            println!("service: {}", manager.kind());
            println!("file: {}", manager.unit_path().display());
            println!(
                "web key: {} (created on install)",
                home.web_key_path().display()
            );
            println!("logs: {}", manager.logs_hint());
            println!("--- {} ---", manager.unit_path().display());
            println!("{}", manager.render().trim_end());
            println!("--- {} ---", manager.unit_path().display());
            println!("dry run: no files written, no commands run");
            Ok(())
        }
        DaemonCommand::Install { dry_run: false } => {
            let (host, port) = gateway_base(&home)?;
            let url = format!("http://{}", display_host_port(&host, port));
            if agents_cmd::server_reachable(&url).await {
                return Err(DaemonError::GatewayRunning { url }.into());
            }
            web_auth::ensure_key_file(&home)?;
            manager.install(&runner)?;
            println!("installed {}", manager.unit_path().display());
            println!("web key {}", home.web_key_path().display());
            println!("logs {}", manager.logs_hint());
            if let Some(auth) = web_auth::read_key_file(&home)? {
                println!("ui {}", auth.url_for(&host, port));
            }
            Ok(())
        }
        DaemonCommand::Uninstall => {
            if !manager.is_installed() {
                println!("not installed");
                return Ok(());
            }
            manager.uninstall(&runner)?;
            println!("uninstalled {}", manager.unit_path().display());
            Ok(())
        }
        DaemonCommand::Start => {
            require_installed(manager)?;
            manager.start(&runner)?;
            println!("started");
            Ok(())
        }
        DaemonCommand::Stop => {
            require_installed(manager)?;
            manager.stop(&runner)?;
            println!("stopped");
            Ok(())
        }
        DaemonCommand::Restart => {
            require_installed(manager)?;
            manager.restart(&runner)?;
            println!("restarted");
            Ok(())
        }
        DaemonCommand::Status => {
            println!("service: {}", manager.kind());
            let status = manager.status(&runner)?;
            println!("installed: {}", yes_no(status.installed));
            println!("enabled: {}", yes_no(status.enabled));
            println!("running: {}", yes_no(status.running));
            if let Some(pid) = status.pid {
                println!("pid: {pid}");
            }
            println!("state: {}", status.state);
            if let Some(code) = status.last_exit {
                println!("last exit: {code}");
            }
            println!("file: {}", manager.unit_path().display());
            println!("logs: {}", manager.logs_hint());
            let mut advice = status.advice;
            let (host, port) = match gateway_base(&home) {
                Ok(base) => base,
                Err(error) => {
                    push_advice(&mut advice, format!("config: {error}"));
                    ("127.0.0.1".to_owned(), DEFAULT_PORT)
                }
            };
            match web_auth::read_key_file(&home) {
                Ok(Some(auth)) => println!("ui: {}", auth.url_for(&host, port)),
                Ok(None) => {
                    println!("ui: no page key (run `any-converter daemon install`)")
                }
                Err(error) => push_advice(&mut advice, format!("web key: {error}")),
            }
            if let Some(advice) = advice {
                println!("advice: {advice}");
            }
            Ok(())
        }
    }
}

fn push_advice(advice: &mut Option<String>, extra: String) {
    match advice {
        Some(existing) => existing.push_str(&format!("; {extra}")),
        None => *advice = Some(extra),
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use super::{CommandOutput, DaemonError, Runner};

    /// A [`Runner`] that records every call as `"program args…"` and
    /// returns output scripted for that exact call, or an empty success
    /// by default. No test ever runs a real `launchctl`/`systemctl`.
    #[derive(Debug, Default)]
    pub(crate) struct FakeRunner {
        calls: RefCell<Vec<String>>,
        outputs: RefCell<BTreeMap<String, CommandOutput>>,
    }

    impl FakeRunner {
        pub(crate) fn script(&self, program: &str, args: &[&str], output: CommandOutput) {
            self.outputs
                .borrow_mut()
                .insert(Self::key(program, args), output);
        }

        pub(crate) fn recorded(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }

        pub(crate) fn recorded_program(&self, program: &str) -> Vec<String> {
            let prefix = format!("{program} ");
            self.recorded()
                .into_iter()
                .filter(|call| call.starts_with(&prefix))
                .collect()
        }

        fn key(program: &str, args: &[&str]) -> String {
            format!("{program} {}", args.join(" "))
        }
    }

    impl Runner for FakeRunner {
        fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, DaemonError> {
            let key = Self::key(program, args);
            self.calls.borrow_mut().push(key.clone());
            Ok(self
                .outputs
                .borrow()
                .get(&key)
                .cloned()
                .unwrap_or_else(|| CommandOutput {
                    success: true,
                    stdout: String::new(),
                    stderr: String::new(),
                }))
        }
    }

    /// Script `id -u` to answer with `uid`.
    pub(crate) fn script_uid(runner: &FakeRunner, uid: &str) {
        runner.script(
            "id",
            &["-u"],
            CommandOutput {
                success: true,
                stdout: format!("{uid}\n"),
                stderr: String::new(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn display_host_port_maps_wildcards_and_brackets_ipv6() {
        assert_eq!(display_host_port("0.0.0.0", 3028), "127.0.0.1:3028");
        assert_eq!(display_host_port("::", 3028), "127.0.0.1:3028");
        assert_eq!(display_host_port("[::]", 3028), "127.0.0.1:3028");
        assert_eq!(display_host_port("::1", 3028), "[::1]:3028");
        assert_eq!(display_host_port("[::1]", 3028), "[::1]:3028");
        assert_eq!(display_host_port("127.0.0.1", 3028), "127.0.0.1:3028");
        assert_eq!(display_host_port("localhost", 80), "localhost:80");
    }

    #[test]
    fn paths_reject_control_characters() {
        let good = PathBuf::from("/tmp/any-converter");
        let control = PathBuf::from("/tmp/any-converter\nkey");
        assert!(Paths::new(control.clone(), good.clone(), good.clone(), None).is_err());
        assert!(Paths::new(good.clone(), control.clone(), good.clone(), None).is_err());
        assert!(Paths::new(good.clone(), good.clone(), control.clone(), None).is_err());
        assert!(
            Paths::new(
                good.clone(),
                good.clone(),
                good.clone(),
                Some(PathBuf::from("/tmp/logs\r\n"))
            )
            .is_err()
        );
    }

    #[test]
    fn gateway_base_defaults_without_a_config() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::from_root(dir.path());
        assert_eq!(gateway_base(&home).unwrap(), ("127.0.0.1".to_owned(), 3028));
    }

    #[test]
    fn gateway_base_reads_the_canonical_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[server]\nhost = \"0.0.0.0\"\nport = 9999\n",
        )
        .unwrap();
        let home = Home::from_root(dir.path());
        assert_eq!(gateway_base(&home).unwrap(), ("0.0.0.0".to_owned(), 9999));
    }

    #[test]
    fn gateway_base_propagates_a_broken_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "{ broken").unwrap();
        let home = Home::from_root(dir.path());
        assert!(matches!(gateway_base(&home), Err(DaemonError::Config(_))));
    }

    #[tokio::test]
    async fn server_reachable_probes_health() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let base = format!("http://{address}");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0_u8; 512];
            let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buffer).await;
            let _ = tokio::io::AsyncWriteExt::write_all(
                &mut socket,
                b"HTTP/1.1 200 OK\r\nContent-Length: 15\r\n\r\n{\"status\":\"ok\"}",
            )
            .await;
        });
        assert!(agents_cmd::server_reachable(&base).await);
        server.await.unwrap();
        // The listener is gone: the same address no longer answers.
        assert!(!agents_cmd::server_reachable(&base).await);
    }

    #[test]
    fn require_installed_checks_the_unit_file() {
        let dir = tempfile::tempdir().unwrap();
        let manager = launchd::Launchd::new(
            Paths::new(
                dir.path().to_path_buf(),
                PathBuf::from("/usr/bin/any-converter"),
                dir.path().join("LaunchAgents"),
                Some(dir.path().join("Logs")),
            )
            .unwrap(),
        );
        assert!(matches!(
            require_installed(&manager),
            Err(DaemonError::NotInstalled)
        ));
    }
}
