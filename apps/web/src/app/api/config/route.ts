import { NextResponse } from "next/server";
import { readFile, writeFile } from "node:fs/promises";
import { parse } from "@iarna/toml";

import type { ServerConfig } from "@any-converter/shared";

function getConfigPath(): string {
  return process.env.CONFIG_PATH ?? "config.toml";
}

export async function GET() {
  try {
    const path = getConfigPath();
    const raw = await readFile(path, "utf-8");
    const config = parse(raw) as unknown as ServerConfig;
    return NextResponse.json({ config, raw });
  } catch (error) {
    const message = error instanceof Error ? error.message : "Unknown error";
    return NextResponse.json({ config: {}, raw: "", error: message }, { status: 500 });
  }
}

export async function POST(request: Request) {
  try {
    const body = (await request.json()) as { raw?: string };
    if (!body.raw) {
      return NextResponse.json({ error: "Missing raw config" }, { status: 400 });
    }

    // Validate TOML before writing.
    parse(body.raw);

    const path = getConfigPath();
    await writeFile(path, body.raw, "utf-8");
    return NextResponse.json({ success: true });
  } catch (error) {
    const message = error instanceof Error ? error.message : "Unknown error";
    return NextResponse.json({ error: message }, { status: 500 });
  }
}
