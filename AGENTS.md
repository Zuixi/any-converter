# any converter

一个用 Rust 编写的 LLM API 格式互转换工具，支持CLI和Desktop，支持多种主流 LLM API 格式之间的双向转换。支持格式如下：
- OpenAI-Compatible (Chat Completion API)
- Claude Messages (Anthropic Claude API)
- OpenAI-Responses (OpenAI Response Format)
- Google Gemini (Google AI Studio API)

## ACTIVE MEMORY SYSTEM
- BEFORE MAKING ANY CHANGE, YOU MUST READ THE [project memory](./docs/memory/AGENTS.md) to check whether there is any useful tricks.
- BEFORE FINISH ANY SESSION, NECESSARY BEST-PRACTICE OR SOMETHING IMPORTTANT MUST BE DOCUMENTED IN [project-context](./docs/memory/project_context.md)

## IMPORTANT
- Any Change or Feature should be Documented at [this file](./CHANGELOG.md)
tech statck refer to [file](./docs/design/any-converter.md)

## CONSTRAINS
- Understand before coding;
- Simplicity first, Minimum code + Minimum performance impact;
- Surgical Changes, precise modifications + Change risk management
- Goal-Driven Execution, Define success criteria. Loop until verified;
- Verification Before Completion, Run verification commands and confirm output before making any success claims;
- Codex source code refer [directory](/Users/wqz/Developer/opensources/openai/codex)

Always use the OpenAI developer documentation MCP server if you need to work with the OpenAI API, ChatGPT Apps SDK, Codex,… without me having to explicitly ask.