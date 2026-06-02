# any converter design

## 数据存储
- 数据库，~/.any-converter/any-converter.db
- 本地配置：~/.any-converter/settings.json
- 备份：~/.any-converter/自动轮换
- SKILLS: ~/.any-converter/skills 软连接到其他目录
- Skill backup: ~/.any-converter/skills-backups

## Tech Stack

- frontend
  - React and TS
  - Using Bun instead of npm
  - vitest
  - TanStack query

- backend
  - Rust
  - Tarui
  - using SQLite


