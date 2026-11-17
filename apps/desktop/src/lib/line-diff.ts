/** One rendered line of a line diff. */
export interface DiffLine {
  kind: "same" | "add" | "remove";
  text: string;
}

/** Beyond this many lines side-by-side, show the whole file as replaced. */
const MAX_DIFF_LINES = 4000;

/**
 * Line-level diff using a longest-common-subsequence table.
 *
 * Agent configs are small; the cap keeps a pathological file from freezing
 * the webview by degrading to "all removed, all added".
 */
export function diffLines(before: string, after: string): DiffLine[] {
  const left = before.split(/\r?\n/);
  const right = after.split(/\r?\n/);

  if (left.length > MAX_DIFF_LINES || right.length > MAX_DIFF_LINES) {
    return [
      ...left.map((text) => ({ kind: "remove" as const, text })),
      ...right.map((text) => ({ kind: "add" as const, text })),
    ];
  }

  // lcs[i][j] = length of the LCS of left[i..] and right[j..].
  const lcs: number[][] = Array.from({ length: left.length + 1 }, () =>
    new Array<number>(right.length + 1).fill(0),
  );
  for (let i = left.length - 1; i >= 0; i -= 1) {
    for (let j = right.length - 1; j >= 0; j -= 1) {
      lcs[i]![j] =
        left[i] === right[j]
          ? (lcs[i + 1]![j + 1] ?? 0) + 1
          : Math.max(lcs[i + 1]![j] ?? 0, lcs[i]![j + 1] ?? 0);
    }
  }

  const lines: DiffLine[] = [];
  let i = 0;
  let j = 0;
  while (i < left.length && j < right.length) {
    if (left[i] === right[j]) {
      lines.push({ kind: "same", text: left[i]! });
      i += 1;
      j += 1;
    } else if ((lcs[i + 1]![j] ?? 0) >= (lcs[i]![j + 1] ?? 0)) {
      lines.push({ kind: "remove", text: left[i]! });
      i += 1;
    } else {
      lines.push({ kind: "add", text: right[j]! });
      j += 1;
    }
  }
  while (i < left.length) {
    lines.push({ kind: "remove", text: left[i]! });
    i += 1;
  }
  while (j < right.length) {
    lines.push({ kind: "add", text: right[j]! });
    j += 1;
  }
  return lines;
}
