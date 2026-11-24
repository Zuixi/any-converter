export type RouteStrategyValue = "priority" | "round_robin" | "weighted";

export interface ProviderLite {
  name: string;
  format: string;
}

export interface RouteMemberDraft {
  key: string;
  provider: string;
  model: string;
  weight: number;
  enabled: boolean;
  fallback: boolean;
}

/** The editable shape of one `[[model_routes]]` entry. */
export interface RouteDraft {
  pattern: string;
  members: RouteMemberDraft[];
  strategy: RouteStrategyValue;
}

/** One upstream model plus the providers that report it. */
export interface UpstreamOption {
  value: string;
  providers: string[];
}

export function draftFromRoute(route: {
  pattern: string;
  providers: string[];
  upstream_model?: string | null;
  strategy: RouteStrategyValue;
  members?: Array<{
    provider: string;
    model?: string | null;
    weight: number;
    enabled: boolean;
    fallback: boolean;
  }>;
}): RouteDraft {
  return {
    pattern: route.pattern,
    members: (route.members?.length
      ? [...route.members.filter((member) => !member.fallback), ...route.members.filter((member) => member.fallback)]
      : route.providers.map((provider) => ({
          provider,
          model: route.upstream_model,
          weight: 1,
          enabled: true,
          fallback: false,
        }))
    ).map((member, index) => ({
      key: `${member.provider}-${index}`,
      provider: member.provider,
      model: member.model ?? "",
      weight: member.weight || 1,
      enabled: member.enabled,
      fallback: member.fallback,
    })),
    strategy: route.strategy,
  };
}

export function emptyDraft(): RouteDraft {
  return { pattern: "", members: [], strategy: "priority" };
}

/** A pattern is a wildcard when it contains glob metacharacters. */
export function isWildcard(pattern: string): boolean {
  return pattern.includes("*") || pattern.includes("?");
}
