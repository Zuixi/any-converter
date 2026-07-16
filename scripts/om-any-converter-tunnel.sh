#!/usr/bin/env bash
set -euo pipefail

BIN="${BIN:-./target/debug/any-converter}"
CONFIG_PATH="${CONFIG_PATH:-config.toml}"
LOCAL_HOST="${LOCAL_HOST:-127.0.0.1}"
LOCAL_PORT="${LOCAL_PORT:-8080}"
REMOTE="${REMOTE:-om}"
REMOTE_HOST="${REMOTE_HOST:-127.0.0.1}"
REMOTE_PORT="${REMOTE_PORT:-18080}"

server_pid=""
tunnel_pid=""

cleanup() {
  local status=$?
  if [[ -n "${tunnel_pid}" ]] && kill -0 "${tunnel_pid}" 2>/dev/null; then
    kill "${tunnel_pid}" 2>/dev/null || true
    wait "${tunnel_pid}" 2>/dev/null || true
  fi
  if [[ -n "${server_pid}" ]] && kill -0 "${server_pid}" 2>/dev/null; then
    kill "${server_pid}" 2>/dev/null || true
    wait "${server_pid}" 2>/dev/null || true
  fi
  exit "${status}"
}

trap cleanup INT TERM EXIT

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing required command: $1" >&2
    exit 1
  fi
}

wait_for_url() {
  local label="$1"
  local url="$2"
  local i
  for i in {1..40}; do
    if curl -fsS "${url}" >/dev/null 2>&1; then
      echo "${label} is ready: ${url}"
      return 0
    fi
    sleep 0.25
  done
  echo "timed out waiting for ${label}: ${url}" >&2
  return 1
}

wait_for_remote_url() {
  local label="$1"
  local url="$2"
  local i
  for i in {1..40}; do
    if ssh "${REMOTE}" curl -fsS "${url}" >/dev/null 2>&1; then
      echo "${label} is ready on ${REMOTE}: ${url}"
      return 0
    fi
    if [[ -n "${tunnel_pid}" ]] && ! kill -0 "${tunnel_pid}" 2>/dev/null; then
      echo "ssh tunnel exited before ${label} became ready" >&2
      return 1
    fi
    sleep 0.25
  done
  echo "timed out waiting for ${label} on ${REMOTE}: ${url}" >&2
  return 1
}

require_command curl
require_command ssh

if [[ ! -x "${BIN}" ]]; then
  echo "binary not found or not executable: ${BIN}" >&2
  echo "build it first, for example: cargo build" >&2
  exit 1
fi

if [[ ! -f "${CONFIG_PATH}" ]]; then
  echo "config file not found: ${CONFIG_PATH}" >&2
  exit 1
fi

if ! grep -q 'client_format[[:space:]]*=[[:space:]]*"claude"' "${CONFIG_PATH}"; then
  echo "warning: ${CONFIG_PATH} has no Claude client route; pi Anthropic Messages tests may fail." >&2
fi

echo "starting any-converter: ${BIN} serve --config ${CONFIG_PATH}"
"${BIN}" serve --config "${CONFIG_PATH}" &
server_pid=$!

wait_for_url "local any-converter" "http://${LOCAL_HOST}:${LOCAL_PORT}/health"

echo "opening reverse tunnel: ssh -N -R ${REMOTE_HOST}:${REMOTE_PORT}:${LOCAL_HOST}:${LOCAL_PORT} ${REMOTE}"
ssh -N -R "${REMOTE_HOST}:${REMOTE_PORT}:${LOCAL_HOST}:${LOCAL_PORT}" "${REMOTE}" &
tunnel_pid=$!

wait_for_remote_url "remote tunnel" "http://${REMOTE_HOST}:${REMOTE_PORT}/health"

cat <<EOF

Tunnel is ready.

Remote base URL:
  http://${REMOTE_HOST}:${REMOTE_PORT}

pi should use Anthropic Messages / Claude format:
  baseUrl = "http://${REMOTE_HOST}:${REMOTE_PORT}"

Codex should use OpenAI Responses wire API, not chat-completions:
  base_url = "http://${REMOTE_HOST}:${REMOTE_PORT}/v1"
  wire_api = "responses"

Press Ctrl-C to stop any-converter and the SSH tunnel.
EOF

wait "${server_pid}"
