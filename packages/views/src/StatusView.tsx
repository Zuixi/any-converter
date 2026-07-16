"use client";

import { Card, CardContent, CardHeader, CardTitle } from "@any-converter/ui";
import { StatusCard, useStatus } from "@any-converter/core";

export function StatusView() {
  const { status, loading, error } = useStatus();

  return (
    <div className="grid gap-6">
      <div className="grid gap-2">
        <h1 className="text-3xl font-bold">Proxy Status</h1>
        <p className="text-muted-foreground">Live health, disk usage, and recent errors from the running server.</p>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Status</CardTitle>
        </CardHeader>
        <CardContent>
          {loading && <p className="text-muted-foreground">Loading status...</p>}
          {error && <p className="text-destructive">{error}</p>}
          {!loading && !error && status && <StatusCard status={status} />}
        </CardContent>
      </Card>
    </div>
  );
}
