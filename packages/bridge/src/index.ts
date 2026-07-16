import { spawn } from "node:child_process";
import { existsSync } from "node:fs";

import type { ConvertApiRequest, ConvertApiResponse } from "@any-converter/shared";

interface NativeModule {
  convertRequestString: (input: string, from: string, to: string) => string;
  convertResponseString: (input: string, from: string, to: string) => string;
}

let nativeModule: NativeModule | null = null;

function loadNativeModule(): NativeModule | null {
  if (nativeModule) return nativeModule;

  try {
    const generatedIndex = require.resolve("@any-converter/bridge/index.js");
    if (existsSync(generatedIndex)) {
      // eslint-disable-next-line @typescript-eslint/no-require-imports
      nativeModule = require("@any-converter/bridge/index.js") as NativeModule;
      return nativeModule;
    }
  } catch {
    // ignore
  }

  return null;
}

function toCliFormat(format: string): string {
  return format.replace(/_/g, "-");
}

function runCliConvert(args: string[], input: string): Promise<{ output: string; error?: string }> {
  return new Promise((resolve) => {
    const child = spawn("cargo", ["run", "-q", "-p", "any-converter", "--", "convert", ...args], {
      stdio: ["pipe", "pipe", "pipe"],
    });

    let stdout = "";
    let stderr = "";

    child.stdout.on("data", (data: Buffer) => {
      stdout += data.toString();
    });

    child.stderr.on("data", (data: Buffer) => {
      stderr += data.toString();
    });

    child.on("close", (code: number | null) => {
      if (code !== 0) {
        resolve({ output: "", error: stderr.trim() || "conversion failed" });
        return;
      }
      resolve({ output: stdout.trim() });
    });

    child.stdin.write(input);
    child.stdin.end();
  });
}

export async function convertRequest({ input, from, to }: ConvertApiRequest): Promise<ConvertApiResponse> {
  const native = loadNativeModule();
  if (native) {
    try {
      const output = native.convertRequestString(input, from, to);
      return { output };
    } catch (error) {
      return { output: "", error: error instanceof Error ? error.message : String(error) };
    }
  }

  const { output, error } = await runCliConvert(
    ["--from", toCliFormat(from), "--to", toCliFormat(to), "--stdin"],
    input,
  );
  return { output, error };
}

export async function convertResponse({ input, from, to }: ConvertApiRequest): Promise<ConvertApiResponse> {
  const native = loadNativeModule();
  if (native) {
    try {
      const output = native.convertResponseString(input, from, to);
      return { output };
    } catch (error) {
      return { output: "", error: error instanceof Error ? error.message : String(error) };
    }
  }

  const { output, error } = await runCliConvert(
    ["--from", toCliFormat(from), "--to", toCliFormat(to), "--response", "--stdin"],
    input,
  );
  return { output, error };
}

export function convert({ input, from, to, mode }: ConvertApiRequest): Promise<ConvertApiResponse> {
  return mode === "response" ? convertResponse({ input, from, to, mode }) : convertRequest({ input, from, to, mode });
}
