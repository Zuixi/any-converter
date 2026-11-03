import { proxyAgentRequest } from "../../proxy";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

/** `POST /api/agents/{id}/sync` → `POST /v1/agents/{id}/sync` (rewrite the model catalog). */
export async function POST(request: Request, { params }: { params: { id: string } }) {
  return proxyAgentRequest(`/v1/agents/${encodeURIComponent(params.id)}/sync`, request);
}
