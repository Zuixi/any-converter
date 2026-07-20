# Frontend Views Package

> **Parent:** [`../AGENTS.md`](../AGENTS.md)
>
> **Scope:** Shared page-level view assemblies such as Playground, Logs, Usage, Status, and Config.

## Read This When

- You are changing `PlaygroundView`, `LogsView`, `UsageView`, `StatusView`, or `ConfigView`
- You are changing how shared components are composed into a screen-level experience

## Boundary Rules

1. Views compose behavior from `@any-converter/core` and presentation from `@any-converter/ui`.
2. Keep low-level logic out of this package; if a view needs a reusable hook or component, move that logic down into `core`.
3. App-specific navigation, layout chrome, and route definitions stay in `apps/`.
