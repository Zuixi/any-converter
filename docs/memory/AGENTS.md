# ANY CONVERTER MEMORY SYSTEM

> **What will I learn?** Which memory file to read for which situation.
>
> **Prerequisites:** You should already have read [`../../AGENTS.md`](../../AGENTS.md) for task routing.

---

## When to Read Which Memory File

| Situation | Read this file | Why |
|-----------|---------------|-----|
| Before touching tool/namespacing code | [`known-gotchas.md`](./known-gotchas.md) | Namespace tool flattening/splitting rules |
| Before modifying SSE/stream logic | [`known-gotchas.md`](./known-gotchas.md) | SSE format differences, event naming, terminators |
| Before modifying message/content handling | [`known-gotchas.md`](./known-gotchas.md) | Claude alternating messages, content polymorphism |
| Before adding a new LLM API format | [`project_context.md`](./project_context.md) | IR rules, existing adapter patterns, model defaults |
| Before working on Codex CLI compatibility | [`known-gotchas.md`](./known-gotchas.md) | `output_item.done` vs `function_call_arguments.done` |
| Before touching model resolution or mapping | [`project_context.md`](./project_context.md) | `model_map`, wildcard rules, case-sensitivity |
| General state of the project | [`project_context.md`](./project_context.md) | Full 35-point living state snapshot |
| Understanding recent bug fixes | [`../../CHANGELOG.md`](../../CHANGELOG.md) | Root-cause analysis per version |

---

## Quick Links

- [`known-gotchas.md`](./known-gotchas.md) — Critical edge cases and pitfalls
- [`project_context.md`](./project_context.md) — Living project context

---

> **Where to go next?** Return to your parent/domain `AGENTS.md`, then drill down into the specific component you are changing.
