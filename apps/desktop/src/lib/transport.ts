import { invoke } from "@tauri-apps/api/core";

import type { ServerProxyRequest } from "../types";
import { isDesktopApp } from "./platform";

/** Thrown rejection, shaped like the Rust `ProxyError` so existing
 * `describeError`/`errorMessage` call sites render it unchanged. */
export interface ServerRequestFailure {
  kind: "http" | "not_running" | "bad_request";
  message: string;
}

/**
 * The one dispatch point for every management/data call the SPA makes.
 *
 * Throwing a plain object (not `Error`) is deliberate: `useServerRequest`'s
 * `describeError` switches on `kind`, and `errorMessage()` reads `.message`.
 */
export async function requestServer<T>(request: ServerProxyRequest): Promise<T> {
  if (isDesktopApp()) {
    // Tauri IPC: the Rust side dials the embedded server with the
    // management bearer. Keep the invoke payload shape exactly.
    return (await invoke("server_request", { args: request })) as T;
  }

  // Browser: same-origin fetch with the session cookie. Pages pass constant
  // paths; an absolute URL must never reach `fetch` here. Resolving against
  // the page origin also rejects `//host/x` and `/\host/x`, which a leading
  // slash check alone would let through.
  if (!isSameOriginPath(request.path)) {
    throw { kind: "bad_request", message: "Only same-origin request paths are allowed" };
  }

  const headers: Record<string, string> = {};
  if (request.body !== undefined) {
    headers["Content-Type"] = "application/json";
  }
  if (request.ifMatch) {
    headers["If-Match"] = request.ifMatch;
  }

  let response: Response;
  try {
    response = await fetch(request.path, {
      method: request.method,
      credentials: "include",
      headers,
      body: request.body,
    });
  } catch {
    throw { kind: "not_running", message: "The gateway is not reachable" };
  }

  if (response.status === 204) {
    return undefined as T;
  }

  let text: string;
  try {
    text = await response.text();
  } catch {
    throw { kind: "not_running", message: "The gateway is not reachable" };
  }

  if (!response.ok) {
    if (response.status === 401) {
      // The page session died (cookie expired or the gateway rotated its
      // key). Reload so the gate asks for the key again instead of every
      // page surfacing its own auth error. This branch is browser-only:
      // the desktop rides IPC and never loses its session.
      window.location.reload();
      // The page is unloading; a never-settling promise beats a brief
      // flash of an auth error nobody can read.
      return new Promise<T>(() => {});
    }
    throw { kind: "http", message: describeHttpError(text, response.status, response.statusText) };
  }

  if (text === "") {
    return undefined as T;
  }

  const contentType = response.headers.get("content-type") ?? "";
  if (contentType.includes("application/json")) {
    try {
      return JSON.parse(text) as T;
    } catch {
      // A truncated body (gateway killed mid-response) must still reject
      // with the transport's error shape, not a raw SyntaxError.
      throw { kind: "http", message: "The gateway returned a malformed response" };
    }
  }
  // Non-JSON success bodies (the ledger CSV) resolve to the raw text.
  return text as T;
}

/** Absolute path that resolves to the page's own origin. */
function isSameOriginPath(path: string): boolean {
  if (!path.startsWith("/")) {
    return false;
  }
  try {
    return new URL(path, window.location.origin).origin === window.location.origin;
  } catch {
    return false;
  }
}

/** The server's error envelope, never the path or a key. */
function describeHttpError(text: string, status: number, statusText: string): string {
  try {
    const body = JSON.parse(text) as { error?: unknown };
    if (typeof body.error === "string" && body.error !== "") {
      // The management data API: `{"error": "<message>"}`.
      return body.error;
    }
    if (body.error && typeof body.error === "object") {
      // The rest of `/v1/admin`: `{"error": {"message": "...", "hint": "..."}}`.
      const parts = [
        (body.error as { message?: unknown }).message,
        (body.error as { hint?: unknown }).hint,
      ].filter((part): part is string => typeof part === "string" && part !== "");
      if (parts.length > 0) {
        return parts.join(" — ");
      }
    }
  } catch {
    // Not JSON; fall through to the status line.
  }
  return `HTTP ${status} ${statusText}`;
}
