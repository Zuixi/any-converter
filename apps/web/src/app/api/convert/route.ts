import { NextResponse } from "next/server";

import { convert } from "@any-converter/bridge";
import type { ConvertApiRequest } from "@any-converter/shared";

export async function POST(request: Request) {
  try {
    const body = (await request.json()) as ConvertApiRequest;
    const { input, from, to, mode } = body;

    if (!input || !from || !to || !mode) {
      return NextResponse.json({ error: "Missing required fields: input, from, to, mode" }, { status: 400 });
    }

    const result = await convert({ input, from, to, mode });
    return NextResponse.json(result);
  } catch (error) {
    const message = error instanceof Error ? error.message : "Unknown error";
    return NextResponse.json({ output: "", error: message }, { status: 500 });
  }
}
