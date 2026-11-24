import type { CSSProperties } from "react";

import { agentIconSource } from "../../lib/agent-icons";

/** One-tone outline drawn when an agent has no logo of its own. */
const GENERIC_CUBE =
  "M8 2.2 13.2 5.1v5.8L8 13.8 2.8 10.9V5.1Z M8 8v5.8 M2.8 5.1 8 8l5.2-2.9";

/** An agent's logo, or magpie's generic cube when it has none. */
export function AgentIcon({ id, className = "" }: { id: string; className?: string }) {
  const icon = agentIconSource(id);

  if (icon.kind === "generic") {
    return (
      <svg
        aria-hidden="true"
        viewBox="0 0 16 16"
        className={className}
        fill="none"
        stroke="currentColor"
        strokeWidth={1.4}
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        <path d={GENERIC_CUBE} />
      </svg>
    );
  }

  if (icon.kind === "image") {
    return (
      <img
        aria-hidden="true"
        alt=""
        src={icon.url}
        draggable={false}
        className={`object-contain ${className}`}
      />
    );
  }

  // A mask takes its colour from the tile's text tone, so a dark logo stays
  // visible in dark mode.
  const style: CSSProperties = {
    WebkitMaskImage: `url(${icon.url})`,
    maskImage: `url(${icon.url})`,
    WebkitMaskSize: "contain",
    maskSize: "contain",
    WebkitMaskRepeat: "no-repeat",
    maskRepeat: "no-repeat",
    WebkitMaskPosition: "center",
    maskPosition: "center",
  };
  return <span aria-hidden="true" className={`block bg-current ${className}`} style={style} />;
}
