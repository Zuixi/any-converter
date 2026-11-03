"use client";

import type { Heatmap } from "@any-converter/shared";
import { Card, CardContent, CardHeader, CardTitle } from "@any-converter/ui";
import { useI18n } from "../i18n";

interface UsageHeatmapProps {
  data: Heatmap;
  onSelectDay: (date: string) => void;
}

const CELL = 14;
const GAP = 3;
const DAY_MS = 24 * 60 * 60 * 1000;

/** Empty, then three levels of traffic; the thresholds are relative to the busiest day. */
const LEVEL_COLORS = ["#eef2f6", "#bfdbfe", "#60a5fa", "#1d4ed8"] as const;

type Level = 0 | 1 | 2 | 3;

/** A 53 × 7 UTC calendar, a Monday on top, shaded by each day's token traffic. */
export function UsageHeatmap({ data, onSelectDay }: UsageHeatmapProps) {
  const { t } = useI18n();
  const byDate = new Map(data.days.map((day) => [day.date, day]));
  const busiest = data.days.reduce((peak, day) => Math.max(peak, day.tokens), 0);
  const weeks = Math.max(1, data.weeks);
  // `from` is the Monday the first column starts on, so cell 0 is that Monday
  // and `index % 7` is the weekday row.
  const firstDay = Date.parse(`${data.from}T00:00:00Z`);
  const width = weeks * (CELL + GAP) - GAP;
  const height = 7 * (CELL + GAP) - GAP;

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("usage.heatmap")}</CardTitle>
      </CardHeader>
      <CardContent>
        <div className="overflow-x-auto">
          <svg className="h-auto w-full" viewBox={`0 0 ${width} ${height}`} aria-label={t("usage.heatmap")}>
            {Array.from({ length: weeks * 7 }, (_, index) => {
              const date = new Date(firstDay + index * DAY_MS).toISOString().slice(0, 10);
              const day = byDate.get(date);
              return (
                <rect
                  key={date}
                  x={Math.floor(index / 7) * (CELL + GAP)}
                  y={(index % 7) * (CELL + GAP)}
                  width={CELL}
                  height={CELL}
                  rx={2}
                  fill={LEVEL_COLORS[levelFor(day?.tokens ?? 0, busiest)]}
                  className={day === undefined ? undefined : "cursor-pointer"}
                  onClick={day === undefined ? undefined : () => onSelectDay(date)}
                >
                  {day !== undefined && (
                    <title>{`${day.date} · ${formatNumber(day.calls)} · ${formatNumber(day.tokens)}`}</title>
                  )}
                </rect>
              );
            })}
          </svg>
        </div>
      </CardContent>
    </Card>
  );
}

function levelFor(tokens: number, busiest: number): Level {
  if (tokens <= 0 || busiest <= 0) {
    return 0;
  }
  const step = busiest / 3;
  if (tokens <= step) {
    return 1;
  }
  return tokens <= step * 2 ? 2 : 3;
}

function formatNumber(value: number): string {
  return new Intl.NumberFormat().format(Math.round(value));
}
