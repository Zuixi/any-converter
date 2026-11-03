//! Claude sign-in by delegating to Claude Code's own `claude auth login`
//! (P15, magpie `internal/provider/claude_signin.go`).
//!
//! any-converter runs no OAuth of its own: it runs `claude auth login
//! --claudeai` in a throwaway config directory and takes the link Claude
//! Code would open (it is the browser Claude Code is given, via the CLI's
//! browser shim), hands it to its window, and keeps the account Claude
//! Code signed in to once it is done. Two flows finish a sign-in:
//!
//! 1. The CLI calls its browser (the any-converter executable) with the
//!    page URL; the shim writes it into a file this sign-in polls, and
//!    [`ClaudeSignIn::submit`] later hands the browser's final address
//!    back to the CLI's local callback server.
//! 2. The CLI prints the URL itself (a page that shows a code): after a
//!    short grace without a browser call, that printed link is the one; the code the page shows is pasted
//!    into the CLI's stdin as `code#state`.
//!
//! [`ClaudeSignIn::wait`] waits for the CLI to exit, reads the sign-in it
//! kept (`.credentials.json`, the same `claudeAiOauth` shape as
//! [`super::agent_import::read_claude`]) and the account identity
//! (`claude auth status --json` plus the `.claude.json` profile), then
//! removes the temp directory — whatever the outcome.
//!
//! Tests run a fake CLI script (`tests/fixtures/fake-claude-cli.sh`)
//! through [`ClaudeSignIn::with_program`]; nothing here ever needs the
//! real `claude` CLI.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::task::JoinHandle;

use super::gemini::SignInError;

/// The environment variable the browser shim (the any-converter CLI run
/// with a page URL) writes the page into; magpie's `MAGPIE_OPENED_URL`,
/// named for this project.
pub const CLAUDE_OPENED_URL_ENV: &str = "ANY_CONVERTER_OPENED_URL";

/// What to say when no `claude` command line can be found (magpie's
/// "install Claude Code first..." rewritten for any-converter).
pub const CLAUDE_INSTALL_HINT: &str = "install Claude Code first, the `claude` command line \
(the Claude desktop app is not it): any-converter signs in to Claude through it; \
see https://docs.anthropic.com/en/docs/claude-code/setup";

/// How long `start` waits for a link before failing (magpie `linkWait`).
const LINK_WAIT: Duration = Duration::from_secs(30);
/// Once the CLI prints a link, how long the browser call may still take
/// (magpie's 3-second grace after a printed link).
const LINK_GRACE: Duration = Duration::from_secs(3);
/// How long `claude auth status --json` may take.
const STATUS_TIMEOUT: Duration = Duration::from_secs(15);
/// How long `submit` gives the CLI's local callback server (magpie's 30s).
const PASTE_HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// Poll interval while `start` waits for the link.
const POLL: Duration = Duration::from_millis(50);
/// How long `wait` gives the line readers to drain after the CLI exited.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

/// The environment entries a Claude Code running a sign-in must not
/// inherit: anything pointing it at another config or browser (magpie
/// `claudeSignInEnv` / `withoutClaudeWiring`).
const STRIPPED_ENV: &[&str] = &[
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_SECURESTORAGE_CONFIG_DIR",
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "BROWSER",
    CLAUDE_OPENED_URL_ENV,
];

/// The `claude` command line a sign-in runs (magpie `claudeCLI`).
#[derive(Debug, Clone)]
pub struct ClaudeCli {
    pub path: PathBuf,
}

/// Find `claude` on `PATH` (magpie `proc.FindTool("claude")`).
pub fn find_claude_cli(env: impl Fn(&str) -> Option<String>) -> Option<ClaudeCli> {
    let path = env("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        });
        if is_executable_file(&candidate) {
            return Some(ClaudeCli { path: candidate });
        }
    }
    None
}

/// Whether `path` is a file this process may execute.
fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// What [`ClaudeSignIn::start`] produced: the page's link plus how the
/// flow finishes.
#[derive(Debug, Clone)]
pub struct ClaudeStarted {
    /// The page the sign-in opens (what the CLI printed, or asked its
    /// browser to open).
    pub url: String,
    /// The browser was never called: the page shows a code the user
    /// pastes back through [`ClaudeSignIn::submit`] as `code#state`.
    pub paste_code: bool,
}

/// A fully signed-in Claude account, read back from what `claude auth
/// login` kept (magpie `claudeLogin`).
#[derive(Debug, Clone)]
pub struct ClaudeAccount {
    /// Display name: the email; a team/enterprise seat adds `" · <Org>"`
    /// (magpie `claudeUser`).
    pub user: String,
    /// The subscription plan (`pro`, `max`, `team`, `enterprise`, ...).
    pub plan: String,
    /// The organization name (empty for personal accounts).
    pub org: String,
    pub access_token: String,
    pub refresh_token: String,
    /// The access token's expiry in unix seconds (`.credentials.json`
    /// keeps it in milliseconds).
    pub expires_at_unix: Option<i64>,
}

impl ClaudeAccount {
    /// The account id [`super::AccountResolver`] stores this account under.
    pub fn account_id_key(&self) -> String {
        format!("claude:{}", self.user)
    }
}

/// What the running `claude auth login` and its readers share.
struct Shared {
    /// The CLI's stdin: where `submit` types the page's code.
    stdin: Mutex<Option<tokio::process::ChildStdin>>,
    /// The loopback callback port of the opened link (set once the
    /// browser was called).
    port: Mutex<Option<u16>>,
    /// The last non-empty line the CLI printed (what a failure reports).
    last_line: Mutex<String>,
    /// The first `https://...` the CLI printed (the `paste_code` link).
    printed: Mutex<Option<String>>,
}

/// One `claude auth login` running in a temp config dir (magpie
/// `claudeCLISignIn`).
struct Running {
    /// The temp config dir (`.credentials.json` and friends live here);
    /// removed when the flow ends, whatever the outcome.
    dir: PathBuf,
    shared: Arc<Shared>,
    child: tokio::process::Child,
    /// The line readers of the CLI's merged stdout/stderr.
    readers: Vec<JoinHandle<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        // Whatever ends the flow, the CLI must not outlive it, and the
        // temp dir (the sign-in it may hold) must go.
        let _ = self.child.start_kill();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Drives one Claude sign-in by running Claude Code's own `claude auth
/// login` (magpie `startClaudeSignIn` / `claudePaste` / `claudeSignedIn`).
///
/// Usage: `start()` → the user opens the URL → `submit(...)` with the
/// page's code or the browser's final address → `wait(timeout)` collects
/// the account once the CLI exits. `wait` consumes the sign-in; the task
/// registry's background wait shares the instance through `wait_inner`.
pub struct ClaudeSignIn {
    cli: ClaudeCli,
    /// Where `<root>/claude-signin-<pid>-<n>` temp config dirs are made
    /// (the app's config dir in production; tests inject a tempdir).
    root: PathBuf,
    /// What the CLI is told is its browser (`BROWSER`): this executable in
    /// production (the CLI's shim records the page); the fake CLI in
    /// tests, whose shim role does the same.
    browser: PathBuf,
    /// Loopback HTTP client for `submit`'s URL branch: never follows
    /// redirects, never leaves the machine.
    http: reqwest::Client,
    /// Extra env entries for the spawned CLI (tests pick the fake CLI's
    /// mode through `FAKE_CLAUDE`).
    extra_env: Vec<(String, String)>,
    /// The running login, set by `start`, taken by `wait`.
    running: Arc<Mutex<Option<Running>>>,
    /// Why the flow ended before a link could be had; `wait` reports it
    /// again once `start` returned it.
    terminal: Arc<Mutex<Option<String>>>,
}

impl ClaudeSignIn {
    /// Create a sign-in for this machine's `claude` CLI.
    pub fn new(cli: ClaudeCli) -> Self {
        Self::build(
            cli.clone(),
            default_root(),
            std::env::current_exe().unwrap_or(cli.path),
        )
    }

    /// Test injection: the executable to run (a fake CLI) and the root its
    /// `claude-signin-*` temp dirs are made in. The same executable plays
    /// the browser (`BROWSER`), the way the real CLI shim does.
    pub fn with_program(program: PathBuf, root: PathBuf) -> Self {
        Self::build(
            ClaudeCli {
                path: program.clone(),
            },
            root,
            program,
        )
    }

    /// Test injection: one extra environment entry handed to the spawned
    /// CLI (the fake CLI picks its mode from `FAKE_CLAUDE`).
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_env.push((key.into(), value.into()));
        self
    }

    fn build(cli: ClaudeCli, root: PathBuf, browser: PathBuf) -> Self {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .unwrap_or_default();
        Self {
            cli,
            root,
            browser,
            http,
            extra_env: Vec::new(),
            running: Arc::new(Mutex::new(None)),
            terminal: Arc::new(Mutex::new(None)),
        }
    }

    /// Start `claude auth login --claudeai` in a fresh temp config dir
    /// and wait for the link: the page the CLI handed its browser (fast
    /// path), or the first `https://...` it printed once the browser was
    /// never called. Fails when the CLI exits first, or gives no link in
    /// [`LINK_WAIT`] (killed, its last line reported).
    pub async fn start(&self) -> Result<ClaudeStarted, SignInError> {
        let dir = sign_in_dir(&self.root)?;
        let opened = dir.join(".opened-url");
        let running = match spawn_login(&self.cli, &dir, &self.browser, &opened, &self.extra_env) {
            Ok(running) => running,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(error);
            }
        };
        let shared = running.shared.clone();
        {
            let mut slot = self
                .running
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if slot.is_some() {
                drop(running); // its Drop kills the CLI and removes the dir
                return Err(SignInError::Vendor(
                    "a Claude sign-in is already running".to_string(),
                ));
            }
            *slot = Some(running);
        }

        let deadline = Instant::now() + LINK_WAIT;
        let mut printed_deadline: Option<Instant> = None;
        loop {
            // (a) The browser was called: the page is in .opened-url.
            if let Ok(bytes) = std::fs::read(&opened) {
                if !bytes.is_empty() {
                    let url = String::from_utf8_lossy(&bytes).trim().to_string();
                    let port = callback_port(&url);
                    *shared
                        .port
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()) = port;
                    return Ok(ClaudeStarted {
                        url,
                        paste_code: false,
                    });
                }
            }

            // (b) The CLI printed a link: give the browser call a short
            // grace (it may be on its way), then fall back to the printed
            // link (magpie: the page + code flow).
            let printed = shared
                .printed
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            if printed.is_some() && printed_deadline.is_none() {
                printed_deadline = Some(Instant::now() + LINK_GRACE);
            }
            if let (Some(at), Some(url)) = (printed_deadline, printed) {
                if Instant::now() >= at {
                    return Ok(ClaudeStarted {
                        url,
                        paste_code: true,
                    });
                }
            }

            // The CLI went away before any link: report its last line.
            let exited = {
                let mut slot = self
                    .running
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                match slot.as_mut() {
                    Some(running) => running.child.try_wait()?,
                    None => {
                        return Err(SignInError::Vendor(
                            "the sign-in is over; start it again".to_string(),
                        ));
                    }
                }
            };
            if exited.is_some() {
                let message = self.settle_dead_start(&shared).await;
                return Err(SignInError::Vendor(message));
            }

            // No link at all within LINK_WAIT: kill the CLI and say what
            // it last printed (or that it printed nothing).
            if Instant::now() >= deadline && printed_deadline.is_none() {
                let message = self.settle_no_link(&shared).await;
                return Err(SignInError::Vendor(message));
            }

            tokio::time::sleep(POLL).await;
        }
    }

    /// The CLI exited during `start` without handing out a link: drain its
    /// output, remember the failure (for `wait`) and clean up.
    async fn settle_dead_start(&self, shared: &Arc<Shared>) -> String {
        // The running CLI is taken out from under the lock and dropped
        // before anything is awaited: a guard alive across an await makes
        // this future (and the handlers awaiting it) non-Send.
        let Some(mut running) = self
            .running
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        else {
            return "the sign-in is over; start it again".to_string();
        };
        drain_readers(&mut running.readers).await;
        let last = last_line(shared);
        let message = if last.is_empty() {
            "claude auth login didn't finish".to_string()
        } else {
            last
        };
        *self
            .terminal
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(message.clone());
        message
    }

    /// The CLI gave no link in [`LINK_WAIT`]: kill it, drain its output,
    /// remember the failure (for `wait`) and clean up.
    async fn settle_no_link(&self, shared: &Arc<Shared>) -> String {
        let Some(mut running) = self
            .running
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        else {
            return "the sign-in is over; start it again".to_string();
        };
        let _ = running.child.start_kill();
        drain_readers(&mut running.readers).await;
        let last = last_line(shared);
        let message = {
            if !last.is_empty() {
                format!("claude auth login gave no link to open: {last}")
            } else {
                format!(
                    "claude auth login gave no link to open: {} printed nothing in {}s; \
                     run `{} auth login` in a terminal to see what it does",
                    self.cli.path.display(),
                    LINK_WAIT.as_secs(),
                    self.cli.path.display()
                )
            }
        };
        *self
            .terminal
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(message.clone());
        message
    }

    /// Wait for the CLI to exit (within `timeout`, else it is killed),
    /// read the account it signed in to and remove the temp dir. A
    /// non-zero exit fails with the CLI's last printed line. Consumes the
    /// sign-in; [`Self::wait_inner`] shares the instance with the task
    /// registry's background wait.
    pub async fn wait(self, timeout: Duration) -> Result<ClaudeAccount, SignInError> {
        self.wait_inner(timeout).await
    }

    /// [`Self::wait`] without consuming the sign-in (see [`Self::wait`]).
    pub(crate) async fn wait_inner(&self, timeout: Duration) -> Result<ClaudeAccount, SignInError> {
        // A flow that failed before handing out a link reports that
        // failure again, and holds nothing.
        if let Some(message) = self
            .terminal
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
        {
            let _ = self
                .running
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take();
            return Err(SignInError::Vendor(message));
        }
        let mut running = self
            .running
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .ok_or(SignInError::NotStarted)?;

        let status = match tokio::time::timeout(timeout, running.child.wait()).await {
            Err(_) => {
                let _ = running.child.start_kill();
                let _ = running.child.wait().await;
                return Err(SignInError::Vendor(format!(
                    "Claude Code didn't finish signing in within {}s",
                    timeout.as_secs()
                )));
            }
            Ok(Err(error)) => return Err(SignInError::Io(error)),
            Ok(Ok(status)) => status,
        };

        // The readers saw EOF when the CLI exited: drain them so the last
        // printed line (what a failure reports) is complete.
        drain_readers(&mut running.readers).await;

        if !status.success() {
            let last = last_line(&running.shared);
            let message = if last.is_empty() {
                "claude auth login didn't finish".to_string()
            } else {
                last
            };
            return Err(SignInError::Vendor(message));
        }

        collect_account(&self.cli, &running, &self.extra_env).await
        // `running` drops here: the temp dir is removed either way.
    }

    /// Paste what Claude's page left: the code it shows (typed into the
    /// CLI's stdin as `code#state`), or the address the browser ended on,
    /// handed to the CLI's local callback server (magpie `claudePaste`).
    pub async fn submit(&self, input: &str) -> Result<(), SignInError> {
        let shared = self
            .running
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|running| running.shared.clone())
            .ok_or_else(|| {
                SignInError::Vendor("the sign-in is over; start it again".to_string())
            })?;
        let raw = input.trim();

        if let Ok(url) = reqwest::Url::parse(raw) {
            if url.scheme() == "http" || url.scheme() == "https" {
                let port = {
                    let port = shared
                        .port
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    match *port {
                        Some(port) => port,
                        None => {
                            return Err(SignInError::Vendor(
                                "that address isn't from this sign-in: paste the one its \
                                 browser tab ended on"
                                    .to_string(),
                            ));
                        }
                    }
                };
                let has_code = url
                    .query_pairs()
                    .any(|(key, value)| key == "code" && !value.is_empty());
                if url.port() != Some(port)
                    || !is_loopback(url.host_str().unwrap_or(""))
                    || !has_code
                {
                    return Err(SignInError::Vendor(
                        "that address isn't from this sign-in: paste the one its browser tab \
                         ended on"
                            .to_string(),
                    ));
                }
                // Feed the CLI's local callback server exactly what the
                // browser would have sent (no redirects, nothing else).
                let request_uri = match url.query() {
                    Some(query) => format!("{}?{query}", url.path()),
                    None => url.path().to_string(),
                };
                let target = format!("http://localhost:{port}{request_uri}");
                let request = self.http.get(&target).build().map_err(|error| {
                    SignInError::Vendor(format!("Claude Code didn't take that address: {error}"))
                })?;
                let outcome =
                    tokio::time::timeout(PASTE_HTTP_TIMEOUT, self.http.execute(request)).await;
                return match outcome {
                    Ok(Ok(_response)) => Ok(()),
                    Ok(Err(error)) => Err(SignInError::Vendor(format!(
                        "Claude Code didn't take that address: {error}"
                    ))),
                    Err(_) => Err(SignInError::Vendor(
                        "Claude Code didn't take that address: the callback server did not \
                         answer in time"
                            .to_string(),
                    )),
                };
            }
        }

        // `code#state`, as Claude's page shows it, typed into the CLI.
        if let Some((code, state)) = raw.split_once('#') {
            if !code.is_empty() && !state.is_empty() && !raw.contains([' ', '\n']) {
                // The stdin handle is taken out from under the lock rather
                // than borrowed through it: a `MutexGuard` alive across an
                // await would make this future (and every handler awaiting
                // it) non-Send.
                let stdin = shared
                    .stdin
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take();
                let Some(mut stdin) = stdin else {
                    return Err(SignInError::Vendor(
                        "the sign-in is over; start it again".to_string(),
                    ));
                };
                let written: std::io::Result<()> = async {
                    use tokio::io::AsyncWriteExt;
                    stdin.write_all(format!("{raw}\n").as_bytes()).await?;
                    stdin.flush().await
                }
                .await;
                // Put it back so a corrected paste can still be typed.
                *shared
                    .stdin
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(stdin);
                return written.map_err(|_| {
                    SignInError::Vendor("the sign-in is over; start it again".to_string())
                });
            }
        }

        Err(SignInError::Vendor(
            "paste the code Claude's page shows, or the whole address the browser ended on"
                .to_string(),
        ))
    }
}

/// The account `claude auth login` signed in to in config directory `dir`,
/// as kept here (magpie `claudeSignedIn`): the sign-in Claude Code kept,
/// and who it says that is.
async fn collect_account(
    cli: &ClaudeCli,
    running: &Running,
    extra_env: &[(String, String)],
) -> Result<ClaudeAccount, SignInError> {
    let credentials = read_credentials(&running.dir).ok_or_else(|| {
        SignInError::Vendor(
            "Claude Code signed in, but kept no sign-in any-converter can read".to_string(),
        )
    })?;
    let status = auth_status(cli, &running.dir, extra_env).await;
    let profile = read_profile(&running.dir);

    // The profile (`oauthAccount`) wins; `claude auth status` fills what
    // it lacks (magpie `claudeSignedIn`).
    let mut email = profile.email_address.unwrap_or_default();
    if email.trim().is_empty() {
        email = status.email;
    }
    let mut org = profile.organization_name.unwrap_or_default();
    if org.trim().is_empty() {
        org = status.org_name;
    }
    let email = email.trim().to_string();
    if email.is_empty() {
        return Err(SignInError::Vendor(
            "Claude didn't say which account signed in".to_string(),
        ));
    }
    let plan = credentials
        .subscription_type
        .filter(|plan| !plan.is_empty())
        .unwrap_or(status.subscription_type);
    let user = claude_user(&email, &plan, &org);

    Ok(ClaudeAccount {
        user,
        plan,
        org: org.trim().to_string(),
        access_token: credentials.access_token.unwrap_or_default(),
        refresh_token: credentials.refresh_token.unwrap_or_default(),
        // `expiresAt` is milliseconds since the Unix epoch.
        expires_at_unix: credentials.expires_at.map(|ms| ms / 1000),
    })
}

/// The environment for a sign-in's Claude Code: the process environment
/// minus the wiring that would point it elsewhere, plus `CLAUDE_CONFIG_DIR`
/// (magpie `claudeSignInEnv`). Blank values count as unset.
fn clean_claude_env() -> Vec<(String, String)> {
    std::env::vars()
        .filter(|(key, _)| !STRIPPED_ENV.contains(&key.as_str()))
        .collect()
}

/// Replace-or-append `key=value` so it appears exactly once.
fn set_env(env: &mut Vec<(String, String)>, key: &str, value: &str) {
    env.retain(|(existing, _)| existing != key);
    env.push((key.to_string(), value.to_string()));
}

/// Spawn `claude auth login --claudeai` in `dir` (its working directory
/// and `CLAUDE_CONFIG_DIR`), with `browser` as its browser and `opened` as
/// the file the browser shim writes the page into; stderr is read into the
/// same line scanner as stdout (magpie `cmd.Stderr = cmd.Stdout`).
fn spawn_login(
    cli: &ClaudeCli,
    dir: &Path,
    browser: &Path,
    opened: &Path,
    extra_env: &[(String, String)],
) -> Result<Running, SignInError> {
    let mut env = clean_claude_env();
    env.extend(extra_env.iter().cloned());
    set_env(&mut env, "CLAUDE_CONFIG_DIR", &dir.to_string_lossy());
    set_env(&mut env, "BROWSER", &browser.to_string_lossy());
    set_env(&mut env, CLAUDE_OPENED_URL_ENV, &opened.to_string_lossy());

    let mut cmd = tokio::process::Command::new(&cli.path);
    cmd.args(["auth", "login", "--claudeai"])
        .current_dir(dir)
        .env_clear()
        .envs(env)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn()?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| SignInError::Io(std::io::Error::other("claude auth login has no stdin")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| SignInError::Io(std::io::Error::other("claude auth login has no stdout")))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| SignInError::Io(std::io::Error::other("claude auth login has no stderr")))?;

    let shared = Arc::new(Shared {
        stdin: Mutex::new(Some(stdin)),
        port: Mutex::new(None),
        last_line: Mutex::new(String::new()),
        printed: Mutex::new(None),
    });
    let readers = vec![
        spawn_line_reader(shared.clone(), stdout),
        spawn_line_reader(shared.clone(), stderr),
    ];
    Ok(Running {
        dir: dir.to_path_buf(),
        shared,
        child,
        readers,
    })
}

/// One line scanner feeding [`Shared`]: the first `https://...` printed
/// and the last non-empty line.
fn spawn_line_reader(
    shared: Arc<Shared>,
    stream: impl tokio::io::AsyncRead + Unpin + Send + 'static,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt;
        let reader = tokio::io::BufReader::new(stream);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            observe(&shared, &line);
        }
    })
}

/// Take one printed line in: the link it may carry, and the last non-empty
/// line (magpie's tail / `lastSaid`, the prompt prefix stripped).
fn observe(shared: &Shared, line: &str) {
    let line = strip_ansi(line);
    if let Some(url) = find_https_url(&line) {
        let mut printed = shared
            .printed
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if printed.is_none() {
            *printed = Some(url);
        }
    }
    let line = line.trim();
    let line = match line.strip_prefix("Paste code here if prompted >") {
        Some(rest) => rest.trim(),
        None => line,
    };
    if !line.is_empty() {
        *shared
            .last_line
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = line.to_string();
    }
}

/// The last line the CLI printed ("" when it printed nothing).
fn last_line(shared: &Shared) -> String {
    shared
        .last_line
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

/// Join the line readers (they finish at EOF, which the CLI's exit
/// brings), so nothing they saw is lost.
async fn drain_readers(readers: &mut Vec<JoinHandle<()>>) {
    for reader in readers.drain(..) {
        let _ = tokio::time::timeout(DRAIN_TIMEOUT, reader).await;
    }
}

/// The first `https://\S+` of a line (magpie `cursorLoginURL`).
fn find_https_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let rest = &line[start..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let url = &rest[..end];
    // `\S+` requires at least one non-space character past the scheme.
    (url.len() > "https://".len()).then(|| url.to_string())
}

/// The loopback callback port a Claude sign-in page sends the browser back
/// to, from the link's `redirect_uri`; `None` when it sends it elsewhere
/// or names no port (magpie `claudeCallbackPort`).
fn callback_port(link: &str) -> Option<u16> {
    let url = reqwest::Url::parse(link).ok()?;
    let redirect = url.query_pairs().find(|(key, _)| key == "redirect_uri")?.1;
    let redirect = reqwest::Url::parse(&redirect).ok()?;
    if !is_loopback(redirect.host_str().unwrap_or("")) {
        return None;
    }
    redirect.port()
}

fn is_loopback(host: &str) -> bool {
    matches!(
        host.to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "::1"
    )
}

/// Name a Claude account: its email, and for a seat on a Team or
/// Enterprise the organization too, so it reads apart from a personal
/// subscription of the same email (magpie `claudeUser`).
fn claude_user(email: &str, plan: &str, org: &str) -> String {
    if email.is_empty() || (plan != "team" && plan != "enterprise") {
        return email.to_string();
    }
    let org = org.trim();
    let label = if org.is_empty() || org.contains(email) {
        plan_label(plan)
    } else {
        org.to_string()
    };
    format!("{email} · {label}")
}

/// `"team"` → `"Team"`, `"enterprise"` → `"Enterprise"`.
fn plan_label(plan: &str) -> String {
    let mut chars = plan.chars();
    match chars.next() {
        Some(head) => head.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

/// ANSI escape sequences out of a line, so colors never cut a URL in two
/// (magpie strips them before looking at the line).
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('[') => {
                // CSI: parameters/intermediates until the final byte.
                for next in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                // OSC: until BEL or ST (ESC \).
                let mut previous = '\0';
                for next in chars.by_ref() {
                    if next == '\x07' || (previous == '\x1b' && next == '\\') {
                        break;
                    }
                    previous = next;
                }
            }
            _ => {}
        }
    }
    out
}

/// `<root>/claude-signin-<pid>-<n>`, created `0700` (magpie MkdirTemp
/// under `claudeDirsRoot`, the root itself made as needed).
fn sign_in_dir(root: &Path) -> std::io::Result<PathBuf> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    std::fs::create_dir_all(root)?;
    for _ in 0..16 {
        let dir = root.join(format!(
            "claude-signin-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::create_dir(&dir) {
            Ok(()) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
                }
                return Ok(dir);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "no free claude-signin directory",
    ))
}

/// The app config dir the temp sign-in dirs live under (where secrets.json
/// is kept); the system temp dir when that cannot be told.
fn default_root() -> PathBuf {
    crate::config::secrets_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir)
}

/// The sign-in Claude Code kept: `<dir>/.credentials.json`'s `claudeAiOauth`
/// (the same shape [`super::agent_import::read_claude`] reads; the sign-in
/// dir holds the file directly, where Claude Code writes it, not under a
/// `.claude/` home, so the import helper itself cannot be reused).
fn read_credentials(dir: &Path) -> Option<ClaudeOAuth> {
    let bytes = std::fs::read(dir.join(".credentials.json")).ok()?;
    let credentials: ClaudeCredentials = serde_json::from_slice(&bytes).ok()?;
    let oauth = credentials.claude_ai_oauth?;
    if oauth.access_token.as_deref().is_none_or(str::is_empty) {
        return None;
    }
    Some(oauth)
}

/// `claude auth status --json` in the sign-in's config dir, 15s timeout
/// (magpie `claudeSignedIn`'s probe); a failure answers nothing, the
/// profile still can.
async fn auth_status(cli: &ClaudeCli, dir: &Path, extra_env: &[(String, String)]) -> ClaudeStatus {
    let mut env = clean_claude_env();
    env.extend(extra_env.iter().cloned());
    set_env(&mut env, "CLAUDE_CONFIG_DIR", &dir.to_string_lossy());

    let mut cmd = tokio::process::Command::new(&cli.path);
    cmd.args(["auth", "status", "--json"])
        .current_dir(dir)
        .env_clear()
        .envs(env)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = match tokio::time::timeout(STATUS_TIMEOUT, cmd.output()).await {
        Ok(Ok(output)) => output,
        _ => return ClaudeStatus::default(),
    };
    serde_json::from_slice(&output.stdout).unwrap_or_default()
}

/// The account identity `<dir>/.claude.json` keeps (`oauthAccount`).
fn read_profile(dir: &Path) -> ClaudeProfileAccount {
    let Ok(bytes) = std::fs::read(dir.join(".claude.json")) else {
        return ClaudeProfileAccount::default();
    };
    let profile: ClaudeProfile = serde_json::from_slice(&bytes).unwrap_or_default();
    profile.oauth_account.unwrap_or_default()
}

/// `~/.claude/.credentials.json` (magpie `claudeCredentials`), as the
/// sign-in dir keeps it.
#[derive(serde::Deserialize)]
struct ClaudeCredentials {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<ClaudeOAuth>,
}

#[derive(serde::Deserialize)]
struct ClaudeOAuth {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
    #[serde(rename = "refreshToken")]
    refresh_token: Option<String>,
    /// Milliseconds since the Unix epoch.
    #[serde(rename = "expiresAt")]
    expires_at: Option<i64>,
    #[serde(rename = "subscriptionType")]
    subscription_type: Option<String>,
}

/// `claude auth status --json`.
#[derive(Default, serde::Deserialize)]
struct ClaudeStatus {
    #[serde(default)]
    email: String,
    #[serde(default, rename = "orgName")]
    org_name: String,
    #[serde(default, rename = "subscriptionType")]
    subscription_type: String,
}

/// `oauthAccount` of the sign-in's `.claude.json`.
#[derive(Default, serde::Deserialize)]
struct ClaudeProfile {
    #[serde(rename = "oauthAccount")]
    oauth_account: Option<ClaudeProfileAccount>,
}

#[derive(Default, serde::Deserialize)]
struct ClaudeProfileAccount {
    #[serde(rename = "emailAddress")]
    email_address: Option<String>,
    #[serde(rename = "organizationName")]
    organization_name: Option<String>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn https_url_is_the_first_non_space_run() {
        assert_eq!(
            find_https_url("go to https://claude.example/auth?a=b now").as_deref(),
            Some("https://claude.example/auth?a=b")
        );
        assert_eq!(
            find_https_url("https://a https://b").as_deref(),
            Some("https://a")
        );
        assert_eq!(find_https_url("nothing here"), None);
        // `\S+`: a bare scheme with nothing after it is no link.
        assert_eq!(find_https_url("see https://"), None);
    }

    #[test]
    fn callback_port_comes_from_the_redirect_uri() {
        let link = "https://claude.example/auth?redirect_uri=http%3A%2F%2Flocalhost%3A34567%2Fcallback&code=abc";
        assert_eq!(callback_port(link), Some(34567));
        // No explicit port, no port.
        assert_eq!(
            callback_port(
                "https://claude.example/auth?redirect_uri=http%3A%2F%2Flocalhost%2Fcallback"
            ),
            None
        );
        // A redirect elsewhere is no callback on this machine.
        assert_eq!(
            callback_port(
                "https://claude.example/auth?redirect_uri=https%3A%2F%2Fexample.com%3A80%2Fcb"
            ),
            None
        );
        // No redirect_uri at all.
        assert_eq!(callback_port("https://claude.example/auth?code=abc"), None);
    }

    #[test]
    fn loopback_hosts_are_recognized() {
        assert!(is_loopback("localhost"));
        assert!(is_loopback("127.0.0.1"));
        assert!(is_loopback("::1"));
        assert!(is_loopback("LOCALHOST"));
        assert!(!is_loopback("example.com"));
        assert!(!is_loopback("localhost.evil.test"));
    }

    #[test]
    fn claude_user_names_team_and_enterprise_seats() {
        // Personal plans keep the bare email.
        assert_eq!(
            claude_user("me@example.com", "pro", "Acme"),
            "me@example.com"
        );
        assert_eq!(claude_user("me@example.com", "", "Acme"), "me@example.com");
        // A seat is named with its organization, or the plan when the
        // organization is missing or just repeats the email.
        assert_eq!(
            claude_user("me@example.com", "team", "Acme"),
            "me@example.com · Acme"
        );
        assert_eq!(
            claude_user("me@example.com", "enterprise", ""),
            "me@example.com · Enterprise"
        );
        assert_eq!(
            claude_user("me@example.com", "team", "me@example.com"),
            "me@example.com · Team"
        );
    }

    #[test]
    fn ansi_sequences_never_split_lines() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("a\x1b]0;title\x07b"), "ab");
    }
}
