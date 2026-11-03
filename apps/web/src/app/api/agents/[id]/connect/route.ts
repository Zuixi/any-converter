import { proxyAgentRequest } from "../../proxy";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

/** `POST /api/agents/{id}/connect` → `POST /v1/agents/{id}/connect` (body: `{ model? }`). */
export async function POST(request: Request, { params }: { params: { id: string } }) {
  return proxyAgentRequest(`/v1/agents/${encodeURIComponent(params.id)}/connect`, request);
}
