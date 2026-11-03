import { NextResponse } from "next/server";

export const runtime = "nodejs";

/** Every agent route is a live view of server state, never a build-time snapshot. */
export const AGENTS_DYNAMIC = "force-dynamic";

function getServerUrl(): string {
  return process.env.SERVER_URL ?? "http://127.0.0.1:8080";
}

/**
 * Server-side management key injected into `/v1/agents/*` calls.
 *
 * The Rust server's agent endpoints accept the global server API key only and
 * are open when no key is configured (local single-user mode), so sending
 * nothing is the correct default for local dev. Set `SERVER_API_KEY` when the
 * server has `server.api_key` configured; the key never reaches the browser.
 */
function authHeader(request?: Request): Record<string, string> {
  const incoming = request?.headers.get("authorization");
  if (incoming) {
    return { Authorization: incoming };
  }
  const serverKey = process.env.SERVER_API_KEY;
  return serverKey ? { Authorization: `Bearer ${serverKey}` } : {};
}

function unreachable(error: unknown): NextResponse {
  const detail = error instanceof Error ? error.message : "unknown error";
  return NextResponse.json(
    {
      error: {
        code: "server_unreachable",
        message: `Cannot reach the any-converter server at ${getServerUrl()} (${detail}).`,
        hint: "Start any-converter-server, or point SERVER_URL at the running server.",
      },
    },
    { status: 503 },
  );
}

/**
 * Proxies one agent-management call to the Rust server and relays its JSON body
 * (including the `{ error: { code, message, hint } }` envelope) unchanged.
 */
export async function proxyAgentRequest(path: string, request?: Request): Promise<NextResponse> {
  const method = request?.method ?? "GET";
  const body = method === "GET" || method === "HEAD" ? undefined : await request?.text();

  let res: Response;
  try {
    res = await fetch(`${getServerUrl()}${path}`, {
      method,
      headers: { "Content-Type": "application/json", ...authHeader(request) },
      body: body ? body : undefined,
      cache: "no-store",
    });
  } catch (error) {
    return unreachable(error);
  }

  const text = await res.text();
  let data: unknown;
  try {
    data = text ? JSON.parse(text) : {};
  } catch {
    data = {
      error: {
        code: "invalid_server_response",
        message: text.slice(0, 500) || `the server returned HTTP ${res.status}`,
      },
    };
  }
  return NextResponse.json(data, { status: res.status });
}
