import { proxyAgentRequest } from "../../proxy";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

/**
 * `POST /api/agents/{id}/rotate` → `POST /v1/agents/{id}/key/rotate`.
 *
 * The proxy path stays `/rotate` so the WebUI vocabulary matches the button;
 * the server keeps its `/key/rotate` spelling.
 */
export async function POST(request: Request, { params }: { params: { id: string } }) {
  return proxyAgentRequest(`/v1/agents/${encodeURIComponent(params.id)}/key/rotate`, request);
}
