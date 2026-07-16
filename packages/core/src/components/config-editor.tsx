"use client";

import { useState } from "react";

import type { ServerConfig } from "@any-converter/shared";
import { redactJson } from "@any-converter/shared";
import { Button, Card, CardContent, CardHeader, CardTitle, Textarea } from "@any-converter/ui";

interface ConfigEditorProps {
  config: ServerConfig;
  raw: string;
  loading: boolean;
  saved: boolean;
  onSave: (raw: string) => void;
}

export function ConfigEditor({ config, raw, loading, saved, onSave }: ConfigEditorProps) {
  const [editRaw, setEditRaw] = useState(raw);

  return (
    <div className="grid gap-6">
      <Card>
        <CardHeader>
          <CardTitle>Structured View</CardTitle>
        </CardHeader>
        <CardContent className="grid gap-4">
          <Section title="Server" data={config.server ?? {}} />
          <Section title="Providers" data={redactJson(config.providers ?? [])} />
          <Section title="Model Routes" data={config.model_routes ?? []} />
          <Section title="Legacy Routes" data={config.routes ?? []} />
          <Section title="Logging" data={redactJson(config.logging ?? {})} />
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Edit TOML</CardTitle>
        </CardHeader>
        <CardContent className="grid gap-4">
          <Textarea value={editRaw} onChange={(e) => setEditRaw(e.target.value)} className="min-h-[400px] font-mono" />
          <div className="flex items-center gap-4">
            <Button onClick={() => onSave(editRaw)} disabled={loading}>
              {loading ? "Saving..." : "Save Config"}
            </Button>
            {saved && (
              <p className="text-sm text-green-600">Saved. Please restart any-converter-server to apply changes.</p>
            )}
          </div>
        </CardContent>
      </Card>
    </div>
  );
}

function Section({ title, data }: { title: string; data: unknown }) {
  return (
    <div className="grid gap-2">
      <h3 className="font-semibold">{title}</h3>
      <pre className="overflow-auto rounded-md bg-muted p-3 text-xs">{JSON.stringify(data, null, 2)}</pre>
    </div>
  );
}
