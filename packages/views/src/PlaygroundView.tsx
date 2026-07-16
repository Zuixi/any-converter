"use client";

import { Card, CardContent, CardHeader, CardTitle } from "@any-converter/ui";
import { ConversionPlayground } from "@any-converter/core";

export function PlaygroundView() {
  return (
    <div className="grid gap-6">
      <div className="grid gap-2">
        <h1 className="text-3xl font-bold">Conversion Playground</h1>
        <p className="text-muted-foreground">
          Paste a request or response payload and convert it between supported LLM API formats.
        </p>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Convert</CardTitle>
        </CardHeader>
        <CardContent>
          <ConversionPlayground />
        </CardContent>
      </Card>
    </div>
  );
}
