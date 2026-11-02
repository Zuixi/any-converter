//! Subprocess transport for provider plugins: ndjson JSON-RPC over the
//! child's stdin/stdout.
//!
//! Wire format — one JSON object per line:
//!
//! ```text
//! host → plugin:  {"id":1,"method":"ping","params":{...}}
//! plugin → host:  {"id":1,"result":{...}} | {"id":1,"error":{"message":"..."}}
//! ```
//!
//! Transport-only by design: spawning, request/response correlation
//! (out-of-order responses are matched by `id`), and graceful shutdown live
//! here; the [`ProviderPlugin`](super::ProviderPlugin) implementation on top
//! of [`PluginTransport`] lands in a later task. Response lines without an
//! `id` (notifications/events) are read and ignored for now.
//!
//! Runtime constraint: the workspace tokio is built without the `process`
//! and `io` features, so the child is driven with `std::process` plus a
//! dedicated reader thread; the only tokio surface is `tokio::sync`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::oneshot;

use super::PluginError;

/// Error message used whenever the plugin process is gone.
const GONE_MSG: &str = "plugin process is gone";

/// Waiters for in-flight requests, keyed by request id.
type PendingReplies = HashMap<u64, oneshot::Sender<Result<Value, String>>>;

/// How to launch a plugin subprocess.
#[derive(Debug, Clone)]
pub struct PluginLaunch {
    /// Executable to run (resolved via `PATH`).
    pub program: String,
    /// Command-line arguments passed to the program.
    pub args: Vec<String>,
    /// Extra environment variables set for the child.
    pub env: Vec<(String, String)>,
}

/// Live ndjson JSON-RPC connection to a plugin subprocess.
///
/// Responses are matched to requests by `id`, so a plugin may reply in any
/// order. Callers that need a deadline should wrap [`PluginTransport::call`]
/// in `tokio::time::timeout`; the transport itself waits indefinitely.
#[derive(Debug)]
pub struct PluginTransport {
    next_id: AtomicU64,
    /// Write end of the child's stdin; taken (dropped) on shutdown.
    stdin: Mutex<Option<ChildStdin>>,
    /// Waiters for unanswered requests, shared with the reader thread.
    pending: Arc<Mutex<PendingReplies>>,
    /// The child itself; `None` once reaped.
    child: Mutex<Option<Child>>,
    /// Reader thread draining the child's stdout; `None` once joined.
    reader: Mutex<Option<JoinHandle<()>>>,
    /// Set once the reader hit EOF (or shutdown ran): the process is gone.
    gone: Arc<AtomicBool>,
    /// OS process id of the child.
    pid: u32,
}

impl PluginTransport {
    /// Spawn the plugin described by `launch` and start reading its stdout.
    ///
    /// The child gets piped stdin/stdout and inherited stderr. Fails with
    /// [`PluginError::Rpc`] if the process cannot be spawned or stdio cannot
    /// be wired up.
    pub fn spawn(launch: PluginLaunch) -> Result<Self, PluginError> {
        let mut child = Command::new(&launch.program)
            .args(&launch.args)
            .envs(launch.env.iter().cloned())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                PluginError::Rpc(format!("failed to spawn plugin `{}`: {e}", launch.program))
            })?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let (stdin, stdout) = match (stdin, stdout) {
            (Some(stdin), Some(stdout)) => (stdin, stdout),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(PluginError::Rpc("plugin stdio was not piped".into()));
            }
        };

        let pending: Arc<Mutex<PendingReplies>> = Arc::new(Mutex::new(HashMap::new()));
        let gone = Arc::new(AtomicBool::new(false));
        let pid = child.id();
        let reader = std::thread::Builder::new()
            .name(format!("plugin-reader-{pid}"))
            .spawn({
                let pending = Arc::clone(&pending);
                let gone = Arc::clone(&gone);
                move || read_responses(stdout, &pending, &gone)
            })
            .map_err(|e| {
                let _ = child.kill();
                let _ = child.wait();
                PluginError::Rpc(format!("failed to spawn reader thread: {e}"))
            })?;

        Ok(Self {
            next_id: AtomicU64::new(0),
            stdin: Mutex::new(Some(stdin)),
            pending,
            child: Mutex::new(Some(child)),
            reader: Mutex::new(Some(reader)),
            gone,
            pid,
        })
    }

    /// The OS process id of the plugin subprocess.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Send a JSON-RPC request and await its matching response.
    ///
    /// The id is auto-incremented; responses may arrive in any order. The
    /// reply future resolves early with [`PluginError::Protocol`] if the
    /// plugin process dies while we are waiting. This method has no built-in
    /// timeout — wrap it with `tokio::time::timeout` if you need one.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, PluginError> {
        if self.gone.load(Ordering::Acquire) {
            return Err(PluginError::Protocol(GONE_MSG.into()));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let payload = serde_json::json!({"id": id, "method": method, "params": params});
        let line = serde_json::to_string(&payload)
            .map_err(|e| PluginError::Rpc(format!("failed to encode request: {e}")))?;

        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, tx);

        // Write one ndjson line; the guard is dropped at the block end so it
        // is never held across the await below.
        let written = {
            let mut guard = self.stdin.lock().unwrap_or_else(|e| e.into_inner());
            match guard.as_mut() {
                Some(stdin) => writeln!(stdin, "{line}")
                    .and_then(|()| stdin.flush())
                    .map_err(|e| {
                        PluginError::Protocol(format!("{GONE_MSG} (stdin write failed: {e})"))
                    }),
                None => Err(PluginError::Protocol(GONE_MSG.into())),
            }
        };
        if let Err(e) = written {
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(e);
        }

        match rx.await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(message)) => Err(PluginError::Rpc(message)),
            // Sender dropped without answering: EOF or shutdown drained it.
            Err(_recv) => Err(PluginError::Protocol(GONE_MSG.into())),
        }
    }

    /// Shut the plugin down, gracefully if possible.
    ///
    /// Closes the child's stdin (well-behaved plugins exit on EOF), polls
    /// `try_wait` for up to ~5s (50 × 100ms), then falls back to `kill`.
    /// Afterwards the child is reaped and the reader thread joined. Safe to
    /// call more than once.
    pub fn try_shutdown(&self) -> Result<(), PluginError> {
        // Close the write end so the plugin sees stdin EOF.
        drop(self.stdin.lock().unwrap_or_else(|e| e.into_inner()).take());

        // Give the child up to ~5s to exit on its own.
        let mut exited = false;
        for _ in 0..50 {
            if self.child_exited()? {
                exited = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        {
            let mut guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(child) = guard.as_mut() {
                if !exited {
                    child.kill().map_err(|e| {
                        PluginError::Rpc(format!("failed to kill plugin (pid {}): {e}", self.pid))
                    })?;
                }
                let _ = child.wait(); // reap; the pipes close and the reader hits EOF
            }
            *guard = None;
        }

        // The reader thread exits once stdout reaches EOF (child gone).
        if let Some(handle) = self.reader.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = handle.join();
        }

        self.gone.store(true, Ordering::Release);
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        Ok(())
    }

    /// Non-blocking check whether the child has exited. `Ok(true)` when it is
    /// gone (including already reaped).
    fn child_exited(&self) -> Result<bool, PluginError> {
        let mut guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_mut() {
            None => Ok(true),
            Some(child) => child
                .try_wait()
                .map(|status| status.is_some())
                .map_err(|e| PluginError::Rpc(format!("failed to wait for plugin: {e}"))),
        }
    }
}

/// Reader-thread body: parse each stdout line as JSON and hand responses to
/// the waiting caller by id. Lines without an `id` (events) are ignored —
/// they become interesting in a later task. On EOF the process is marked
/// gone and any remaining waiters are released with an error.
fn read_responses(
    stdout: ChildStdout,
    pending: &Arc<Mutex<PendingReplies>>,
    gone: &Arc<AtomicBool>,
) {
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            continue; // malformed line: skip, the protocol stays in sync per-line
        };
        dispatch_response(value, pending);
    }
    gone.store(true, Ordering::Release);
    // Dropping the senders makes every waiter's `rx.await` fail fast.
    pending.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// Route one parsed response line to its waiter, if any.
fn dispatch_response(value: Value, pending: &Arc<Mutex<PendingReplies>>) {
    let Some(id) = value.get("id").and_then(Value::as_u64) else {
        return; // notification/event: no waiter, ignored for now
    };
    let sender = pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&id);
    let Some(sender) = sender else {
        return; // unknown or abandoned id
    };
    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| error.to_string());
        let _ = sender.send(Err(message));
    } else {
        let _ = sender.send(Ok(value.get("result").cloned().unwrap_or(Value::Null)));
    }
}

/// Tests spawn `python3 -c <script>` as a fake plugin speaking the ndjson
/// JSON-RPC protocol. When `python3` is unavailable the subprocess tests
/// skip (print a note and return) — CI environments must provide python3,
/// or these tests should later move to a self-contained fake binary.
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

    use std::sync::OnceLock;
    use std::time::Duration;

    use serde_json::json;
    use tokio::time::timeout;

    use super::*;

    /// Echoes every request back: `result` = params, or `error` for "fail".
    const ECHO_SCRIPT: &str = r#"
import sys, json
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    if req.get("method") == "fail":
        resp = {"id": req["id"], "error": {"message": "boom"}}
    else:
        resp = {"id": req["id"], "result": req.get("params")}
    sys.stdout.write(json.dumps(resp) + "\n")
    sys.stdout.flush()
"#;

    /// Reads exactly two requests, then replies in reverse order.
    const REVERSE_SCRIPT: &str = r#"
import sys, json
reqs = []
for line in sys.stdin:
    if line.strip():
        reqs.append(json.loads(line))
    if len(reqs) == 2:
        break
for req in reversed(reqs):
    sys.stdout.write(json.dumps({"id": req["id"], "result": req.get("params")}) + "\n")
sys.stdout.flush()
"#;

    /// Reads stdin until EOF, then exits 0.
    const QUIET_SCRIPT: &str = r#"
import sys
for line in sys.stdin:
    pass
"#;

    fn python3_available() -> bool {
        static AVAILABLE: OnceLock<bool> = OnceLock::new();
        *AVAILABLE.get_or_init(|| {
            std::process::Command::new("python3")
                .arg("-c")
                .arg("pass")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        })
    }

    fn python_launch(script: &str) -> PluginLaunch {
        PluginLaunch {
            program: "python3".into(),
            args: vec!["-c".into(), script.into()],
            env: vec![],
        }
    }

    #[test]
    fn spawn_missing_program_reports_error() {
        let launch = PluginLaunch {
            program: "/nonexistent/definitely-missing-binary".into(),
            args: vec![],
            env: vec![],
        };
        let err = PluginTransport::spawn(launch).unwrap_err();
        assert!(matches!(err, PluginError::Rpc(_)));
    }

    #[tokio::test]
    async fn call_echoes_result() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let transport = PluginTransport::spawn(python_launch(ECHO_SCRIPT)).unwrap();
        let result = timeout(
            Duration::from_secs(5),
            transport.call("ping", json!({"k": "v", "n": 7})),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result, json!({"k": "v", "n": 7}));
        transport.try_shutdown().unwrap();
    }

    #[tokio::test]
    async fn error_response_maps_to_rpc_error() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let transport = PluginTransport::spawn(python_launch(ECHO_SCRIPT)).unwrap();
        let err = timeout(Duration::from_secs(5), transport.call("fail", json!({})))
            .await
            .unwrap()
            .unwrap_err();
        assert!(matches!(err, PluginError::Rpc(ref m) if m == "boom"));
        transport.try_shutdown().unwrap();
    }

    #[tokio::test]
    async fn out_of_order_responses_are_matched_by_id() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        // The fake replies to the second request first; each concurrent call
        // must still receive its own params back.
        let transport = PluginTransport::spawn(python_launch(REVERSE_SCRIPT)).unwrap();
        let (first, second) = tokio::join!(
            timeout(Duration::from_secs(5), transport.call("m", json!({"n": 1})),),
            timeout(Duration::from_secs(5), transport.call("m", json!({"n": 2})),),
        );
        assert_eq!(first.unwrap().unwrap(), json!({"n": 1}));
        assert_eq!(second.unwrap().unwrap(), json!({"n": 2}));
        transport.try_shutdown().unwrap();
    }

    #[tokio::test]
    async fn shutdown_is_graceful_and_marks_gone() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let transport = PluginTransport::spawn(python_launch(QUIET_SCRIPT)).unwrap();
        let _ = transport.pid(); // pid is exposed for logging/diagnostics
        transport.try_shutdown().unwrap();
        transport.try_shutdown().unwrap(); // idempotent

        // After shutdown the transport refuses new work.
        let err = timeout(Duration::from_secs(5), transport.call("ping", json!({})))
            .await
            .unwrap()
            .unwrap_err();
        assert!(matches!(err, PluginError::Protocol(ref m) if m.contains("gone")));
    }

    #[tokio::test]
    async fn call_after_process_death_is_protocol_error() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        // "pass": the child exits immediately without reading stdin.
        let transport = PluginTransport::spawn(python_launch("pass")).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let err = timeout(Duration::from_secs(5), transport.call("ping", json!({})))
            .await
            .unwrap()
            .unwrap_err();
        assert!(matches!(err, PluginError::Protocol(ref m) if m.contains("gone")));
        transport.try_shutdown().unwrap(); // reaps the dead child
    }
}
