import { NextResponse } from "next/server";

import { proxyAgentRequest } from "../proxy";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

/**
 * `GET /api/agents/models` → `GET /v1/models`, reduced to the model ids the
 * connect dialog needs (`{ models: string[] }`).
 */
export async function GET(request: Request) {
  const res = await proxyAgentRequest("/v1/models", request);
  if (!res.ok) {
    return res;
  }

  const data = (await res.json()) as { data?: Array<{ id?: unknown }> };
  const models = (data.data ?? [])
    .map((entry) => entry.id)
    .filter((id): id is string => typeof id === "string" && id.length > 0)
    .sort();

  return NextResponse.json({ models });
}
