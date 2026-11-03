import { proxyAgentRequest } from "./proxy";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

/** `GET /api/agents` → `GET /v1/agents` (detect + status for every agent). */
export async function GET(request: Request) {
  return proxyAgentRequest("/v1/agents", request);
}
