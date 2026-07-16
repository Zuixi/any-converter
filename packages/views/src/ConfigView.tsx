"use client";

import { Card, CardContent, CardHeader, CardTitle } from "@any-converter/ui";
import { ConfigEditor, useConfig } from "@any-converter/core";

export function ConfigView() {
  const { config, raw, loading, error, saved, save } = useConfig();

  return (
    <div className="grid gap-6">
      <div className="grid gap-2">
        <h1 className="text-3xl font-bold">Configuration</h1>
        <p className="text-muted-foreground">
          View and edit the proxy configuration. Secrets are masked. Restart the server after saving.
        </p>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Config</CardTitle>
        </CardHeader>
        <CardContent>
          {loading && <p className="text-muted-foreground">Loading config...</p>}
          {error && <p className="text-destructive">{error}</p>}
          {!loading && !error && config && (
            <ConfigEditor config={config} raw={raw} loading={loading} saved={saved} onSave={save} />
          )}
        </CardContent>
      </Card>
    </div>
  );
}
