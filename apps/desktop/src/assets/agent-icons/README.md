# Agent logos

These files are copied from [magpie](https://github.com/yetone/magpie)'s
`internal/gui/assets/icons` (MIT License, Copyright (c) 2026 yetone). They
identify the coding agent a row is about; the marks themselves remain the
trademarks of their owners.

`src/lib/agent-icons.ts` maps agent ids to these files. A logo ending in
`-color`, or one of the PNGs, is drawn as a picture; every other SVG is drawn
as a one-tone mask so it follows the active theme.
