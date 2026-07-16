"use client";

import { Card, CardContent, CardHeader, CardTitle } from "@any-converter/ui";
import { UsageChart, useUsage } from "@any-converter/core";

export function UsageView() {
  const { data, loading, error } = useUsage();

  return (
    <div className="grid gap-6">
      <div className="grid gap-2">
        <h1 className="text-3xl font-bold">Usage Dashboard</h1>
        <p className="text-muted-foreground">
          Aggregate token usage, request volume, and latency from the proxy server.
        </p>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Metrics</CardTitle>
        </CardHeader>
        <CardContent>
          {loading && <p className="text-muted-foreground">Loading usage...</p>}
          {error && <p className="text-destructive">{error}</p>}
          {!loading && !error && <UsageChart data={data} />}
        </CardContent>
      </Card>
    </div>
  );
}
