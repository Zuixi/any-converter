import type React from "react";
import { Card } from "@any-converter/ui";

export function Table({
  headers,
  rows,
  emptyText,
}: {
  headers: string[];
  rows: React.ReactNode[][];
  emptyText?: string;
}) {
  return (
    <Card className="overflow-x-auto p-0">
      <table className="w-full border-collapse">
        <thead>
          <tr>
            {headers.map((header, index) => (
              <th
                key={index}
                className="border-b px-4 py-3 text-left text-xs font-semibold uppercase text-muted-foreground"
              >
                {header}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.length === 0 && emptyText ? (
            <tr>
              <td className="px-4 py-6 text-sm text-muted-foreground" colSpan={headers.length}>
                {emptyText}
              </td>
            </tr>
          ) : (
            rows.map((row, index) => (
              <tr key={index}>
                {row.map((cell, cellIndex) => (
                  <td key={cellIndex} className="border-b px-4 py-3 align-top text-sm last:border-b-0">
                    {cell}
                  </td>
                ))}
              </tr>
            ))
          )}
        </tbody>
      </table>
    </Card>
  );
}
