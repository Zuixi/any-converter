# UI Package

> **Parent:** [`../AGENTS.md`](../AGENTS.md)
>
> **Scope:** Low-level UI primitives, styling helpers, and reusable form/display controls.

## Read This When

- You are changing atoms such as buttons, inputs, badges, labels, cards, or selects
- You are changing low-level molecules such as `format-selector` or `json-editor`
- You are changing shared styling helpers or `globals.css`

## Boundary Rules

1. Keep components presentational and reusable.
2. Do not put API access, app routing, or product-specific state here.
3. If a component starts encoding product semantics, move that composition up into `packages/core` or `packages/views`.
