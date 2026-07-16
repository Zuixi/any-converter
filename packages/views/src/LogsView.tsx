"use client";

import { Card, CardContent, CardHeader, CardTitle } from "@any-converter/ui";
import { LogTable, useLogs } from "@any-converter/core";

export function LogsView() {
  const { records, loading, error } = useLogs();

  return (
    <div className="grid gap-6">
      <div className="grid gap-2">
        <h1 className="text-3xl font-bold">Conversation Logs</h1>
        <p className="text-muted-foreground">
          Inspect client messages, tool activity, and LLM responses captured by the proxy server.
        </p>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Timeline</CardTitle>
        </CardHeader>
        <CardContent>
          {loading && <p className="text-muted-foreground">Loading logs...</p>}
          {error && <p className="text-destructive">{error}</p>}
          {!loading && !error && <LogTable records={records} />}
        </CardContent>
      </Card>
    </div>
  );
}
