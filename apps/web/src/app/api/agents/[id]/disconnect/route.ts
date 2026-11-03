import { proxyAgentRequest } from "../../proxy";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

/** `POST /api/agents/{id}/disconnect` → `POST /v1/agents/{id}/disconnect` (three-step restore). */
export async function POST(request: Request, { params }: { params: { id: string } }) {
  return proxyAgentRequest(`/v1/agents/${encodeURIComponent(params.id)}/disconnect`, request);
}
