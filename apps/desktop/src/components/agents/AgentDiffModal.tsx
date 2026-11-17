import { useEffect, useMemo, useState } from "react";
import { useI18n } from "@any-converter/core";

import { Modal } from "../providers/Modal";
import { agentMeta } from "../../lib/agents";
import { diffLines } from "../../lib/line-diff";
import { management, type AgentFileDiff, type AgentSummary } from "../../lib/management";

/** Read-only view of the files this agent's wiring changed. */
export function AgentDiffModal({
  agent,
  onClose,
}: {
  agent: AgentSummary;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const [files, setFiles] = useState<AgentFileDiff[]>();
  const [error, setError] = useState(false);

  useEffect(() => {
    let active = true;
    management
      .agentDiff(agent.id)
      .then((result) => {
        if (active) setFiles(result);
      })
      .catch(() => {
        if (active) setError(true);
      });
    return () => {
      active = false;
    };
  }, [agent.id]);

  return (
    <Modal
      wide
      title={t("agents.diff.title").replace("{agent}", agentMeta(agent.id).label)}
      onClose={onClose}
    >
      {error && <p className="text-sm text-destructive">{t("agents.diff.error")}</p>}
      {!error && files === undefined && (
        <p className="text-sm text-muted-foreground">{t("agents.diff.loading")}</p>
      )}
      {!error && files !== undefined && files.length === 0 && (
        <p className="text-sm text-muted-foreground">{t("agents.diff.empty")}</p>
      )}
      {files?.map((file) => (
        <FileDiff key={file.path} file={file} />
      ))}
    </Modal>
  );
}

function FileDiff({ file }: { file: AgentFileDiff }) {
  const { t } = useI18n();
  const lines = useMemo(
    () => diffLines(file.before ?? "", file.after ?? ""),
    [file.before, file.after],
  );
  const unchanged = lines.every((line) => line.kind === "same");

  return (
    <section className="grid gap-2">
      <div className="min-w-0">
        <code className="block break-all font-mono text-xs text-muted-foreground">{file.path}</code>
        {file.created && <p className="text-xs text-muted-foreground">{t("agents.diff.created")}</p>}
      </div>
      {unchanged ? (
        <p className="rounded-md border bg-muted/30 px-3 py-2 text-sm text-muted-foreground">
          {t("agents.diff.noChanges")}
        </p>
      ) : (
        <div className="grid grid-cols-2 gap-2">
          <DiffPane title={t("agents.diff.before")} lines={lines} side="before" />
          <DiffPane title={t("agents.diff.after")} lines={lines} side="after" />
        </div>
      )}
    </section>
  );
}

/** One side of the diff: unchanged lines plus the removals (or additions). */
function DiffPane({
  title,
  lines,
  side,
}: {
  title: string;
  lines: ReturnType<typeof diffLines>;
  side: "before" | "after";
}) {
  const visible = lines.filter(
    (line) => line.kind === "same" || line.kind === (side === "before" ? "remove" : "add"),
  );

  return (
    <div className="grid gap-1">
      <p className="text-xs uppercase tracking-wide text-muted-foreground">{title}</p>
      <pre className="max-h-96 overflow-auto rounded-md border bg-muted/20 p-2 font-mono text-xs leading-relaxed">
        {visible.map((line, index) => (
          <div
            key={`${index}-${line.text}`}
            className={
              line.kind === "same"
                ? ""
                : side === "before"
                  ? "bg-destructive/10 text-destructive"
                  : "bg-emerald-500/10 text-emerald-700 dark:text-emerald-300"
            }
          >
            <span aria-hidden="true" className="select-none pr-1 opacity-60">
              {line.kind === "same" ? " " : side === "before" ? "-" : "+"}
            </span>
            {line.text || " "}
          </div>
        ))}
      </pre>
    </div>
  );
}
