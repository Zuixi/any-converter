# Frontend Core Package

> **Parent:** [`../AGENTS.md`](../AGENTS.md)
>
> **Scope:** Shared business-facing React hooks, API client abstraction, and reusable composed components used by both Web and Desktop surfaces.

## Read This When

- You are changing hooks under `src/hooks/*`
- You are changing reusable components such as log tables, usage charts, config editors, or the playground
- You are adjusting the shared API client contract between Web and Desktop

## Local Structure

```
src/
├── hooks/        # use-config, use-convert, use-logs, use-status, use-usage, api-client
├── components/   # reusable higher-level components
└── index.ts      # public exports
```

## Boundary Rules

1. Put cross-app behavior here, not in `apps/web` or `apps/desktop`.
2. Keep presentational primitives in [`../ui/AGENTS.md`](../ui/AGENTS.md).
3. Keep page composition in [`../views/AGENTS.md`](../views/AGENTS.md).
4. Keep shared contract types in [`../shared/AGENTS.md`](../shared/AGENTS.md).
5. Playground response examples in `conversion-playground.tsx` must stay schema-complete for every format (required fields such as OpenAI Chat `choices[].index` / `object`, Claude `type`+`role`, Responses `object`+`status`); incomplete fixtures cause convert failures that surface as opaque client errors.
