"use client";

import { useMemo, useState } from "react";

import type { RequestLogRecord } from "@any-converter/shared";
import { Badge, Button, Card, CardContent, CardHeader, CardTitle, Input, Label } from "@any-converter/ui";
import { formatBytes, formatDuration, formatTimestamp, formatLabel } from "@any-converter/shared";

interface LogTableProps {
  records: RequestLogRecord[];
}

export function LogTable({ records }: LogTableProps) {
  const [selected, setSelected] = useState<RequestLogRecord | null>(null);
  const [filter, setFilter] = useState("");

  const filtered = useMemo(() => {
    const q = filter.toLowerCase();
    return records.filter((r) =>
      [r.provider, r.client_model, r.upstream_model, r.path, String(r.response_status)]
        .join(" ")
        .toLowerCase()
        .includes(q),
    );
  }, [records, filter]);

  return (
    <div className="grid gap-4">
      <div className="grid gap-2">
        <Label>Search</Label>
        <Input
          placeholder="Filter by provider, model, path, status..."
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
      </div>

      <div className="grid grid-cols-1 gap-4 lg:grid-cols-3">
        <div className="lg:col-span-1">
          <div className="rounded-md border">
            <table className="w-full text-sm">
              <thead className="bg-muted">
                <tr>
                  <th className="p-2 text-left">Time</th>
                  <th className="p-2 text-left">Provider</th>
                  <th className="p-2 text-left">Status</th>
                </tr>
              </thead>
              <tbody>
                {filtered.map((record) => (
                  <tr
                    key={record.request_id}
                    className="cursor-pointer border-t hover:bg-accent"
                    onClick={() => setSelected(record)}
                  >
                    <td className="p-2">{formatTimestamp(record.timestamp)}</td>
                    <td className="p-2">{record.provider}</td>
                    <td className="p-2">
                      <Badge variant={record.response_status >= 400 ? "destructive" : "default"}>
                        {record.response_status}
                      </Badge>
                    </td>
                  </tr>
                ))}
                {filtered.length === 0 && (
                  <tr>
                    <td colSpan={3} className="p-4 text-center text-muted-foreground">
                      No records found.
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
        </div>

        <div className="lg:col-span-2">
          {selected ? (
            <Card>
              <CardHeader>
                <CardTitle>Request Detail</CardTitle>
              </CardHeader>
              <CardContent className="grid gap-4">
                <dl className="grid grid-cols-[120px_1fr] gap-2 text-sm">
                  <dt className="text-muted-foreground">ID</dt>
                  <dd>{selected.request_id}</dd>
                  <dt className="text-muted-foreground">Format</dt>
                  <dd>{formatLabel(selected.client_format)}</dd>
                  <dt className="text-muted-foreground">Model</dt>
                  <dd>
                    {selected.client_model} → {selected.upstream_model}
                  </dd>
                  <dt className="text-muted-foreground">Latency</dt>
                  <dd>{formatDuration(selected.latency_ms)}</dd>
                  <dt className="text-muted-foreground">Tokens</dt>
                  <dd>
                    {selected.usage.input_tokens} in / {selected.usage.output_tokens} out
                  </dd>
                </dl>

                {selected.request_body && <BodySection title="Client Request" body={selected.request_body} />}
                {selected.upstream_request_body && (
                  <BodySection title="Upstream Request" body={selected.upstream_request_body} />
                )}
                <BodySection title="Response" body={selected.response_body} />

                <Button variant="outline" onClick={() => setSelected(null)}>
                  Close
                </Button>
              </CardContent>
            </Card>
          ) : (
            <div className="flex h-full items-center justify-center rounded-md border p-8 text-muted-foreground">
              Select a request to view details.
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function BodySection({ title, body }: { title: string; body: { text: string } | { lines: string[] } }) {
  const text = "text" in body ? body.text : body.lines.join("\n");
  const size = new Blob([text]).size;

  return (
    <div className="grid gap-2">
      <div className="flex items-center justify-between">
        <h4 className="font-semibold">{title}</h4>
        <span className="text-xs text-muted-foreground">{formatBytes(size)}</span>
      </div>
      <pre className="max-h-64 overflow-auto rounded-md bg-muted p-3 text-xs">{text}</pre>
    </div>
  );
}
