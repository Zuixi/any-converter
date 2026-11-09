import { NextResponse } from "next/server";

import type { ServerConfig } from "@any-converter/shared";

interface AdminConfigView {
  revision: string;
  server: {
    host: string;
    port: number;
    proxy_configured: boolean;
    inference_key_configured: boolean;
    management_key_configured: boolean;
  };
  logging: {
    dir?: string | null;
    max_disk_mb: number;
    request_log_enabled: boolean;
  };
}

function gateway() {
  const baseUrl = process.env.ANY_CONVERTER_URL ?? "http://127.0.0.1:1228";
  const managementKey = process.env.ANY_CONVERTER_MANAGEMENT_KEY;
  if (!managementKey) {
    return null;
  }
  return { baseUrl: baseUrl.replace(/\/$/, ""), managementKey };
}

export async function GET() {
  const connection = gateway();
  if (!connection) {
    return NextResponse.json(
      { config: {}, raw: "", error: "ANY_CONVERTER_MANAGEMENT_KEY is not configured" },
      { status: 503 },
    );
  }

  try {
    const response = await fetch(`${connection.baseUrl}/v1/admin/config`, {
      headers: { Authorization: `Bearer ${connection.managementKey}` },
      cache: "no-store",
    });
    if (!response.ok) {
      return NextResponse.json({ config: {}, raw: "", error: await response.text() }, { status: response.status });
    }
    const view = (await response.json()) as AdminConfigView;
    const config = {
      server: { host: view.server.host, port: view.server.port },
    } satisfies ServerConfig;
    return NextResponse.json({ config, raw: JSON.stringify(view, null, 2) });
  } catch (error) {
    const message = error instanceof Error ? error.message : "Unknown error";
    return NextResponse.json({ config: {}, raw: "", error: message }, { status: 502 });
  }
}

export async function POST(request: Request) {
  const connection = gateway();
  if (!connection) {
    return NextResponse.json({ error: "ANY_CONVERTER_MANAGEMENT_KEY is not configured" }, { status: 503 });
  }

  try {
    const body = (await request.json()) as { raw?: string };
    const view = JSON.parse(body.raw ?? "{}") as AdminConfigView;
    const response = await fetch(`${connection.baseUrl}/v1/admin/config`, {
      method: "PATCH",
      headers: {
        Authorization: `Bearer ${connection.managementKey}`,
        "Content-Type": "application/json",
        "If-Match": `"${view.revision}"`,
      },
      body: JSON.stringify({
        server: { host: view.server.host, port: view.server.port },
        logging: {
          dir: view.logging.dir ?? null,
          max_disk_mb: view.logging.max_disk_mb,
          request_log_enabled: view.logging.request_log_enabled,
        },
      }),
    });
    if (!response.ok) {
      return NextResponse.json({ error: await response.text() }, { status: response.status });
    }
    return NextResponse.json({ success: true });
  } catch (error) {
    const message = error instanceof Error ? error.message : "Unknown error";
    return NextResponse.json({ error: message }, { status: 400 });
  }
}
