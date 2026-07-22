# Conservative Log Sessions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show one Logs item per reliably identified client session and keep the conversation visible at the Desktop window's minimum size.

**Architecture:** Keep explicit session headers authoritative. For records without one, reuse the existing normalized trace summaries and merge only when the current history strictly extends one uniquely longest prior history from the same client. Keep layout fixes inside the existing shared Logs components.

**Tech Stack:** Rust/Axum request logging, React, TypeScript, Tailwind CSS.

---

### Task 1: Conservative Session Grouping

**Files:**
- Modify: `packages/core/src/components/log-conversation.ts`
- Test: `packages/core/src/components/log-conversation.test.ts`

- [x] Write a failing self-contained test where a later request strictly extends an earlier trace and must produce one session.
- [x] Compile and run the test, confirming the current request-based fallback produces two sessions.
- [x] Implement uniquely-longest strict-prefix matching for records that have `client_id` but no explicit `session_id`.
- [x] Add a failing ambiguity test and keep ambiguous or identical histories as separate sessions.
- [x] Compile and run both tests.

### Task 2: Session Header Semantics

**Files:**
- Modify: `crates/server/src/handlers.rs`

- [x] Add a Rust test proving `x-request-id` is not accepted as a session identifier.
- [x] Run the focused test and confirm it fails.
- [x] Restrict session extraction to `x-session-id` and `x-conversation-id`.
- [x] Run the server test suite.

### Task 3: Responsive Logs Layout

**Files:**
- Modify: `packages/core/src/components/log-table.tsx`
- Modify: `packages/views/src/LogsView.tsx`

- [x] Replace the viewport `lg` split with a wider breakpoint that reflects the Desktop content area's real width.
- [x] Use a bounded master/detail grid and add `min-w-0`, `max-w-full`, and wrapping constraints along the message rendering chain.
- [x] Remove the outer Logs card padding so the conversation gets the available window width.
- [x] Verify type checking and the Desktop production build.
