default: build

# ── 构建 ────────────────────────────────────────────────────────────────

build:
    cargo build

release:
    cargo build --release

# ── 质量检查 ────────────────────────────────────────────────────────────

check:
    cargo fmt -- --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo check --all-targets

fix:
    cargo fmt
    cargo clippy --all-targets --all-features --fix --allow-dirty

test:
    cargo test --workspace

coverage:
    cargo tarpaulin --out Html

docs:
    cargo doc --no-deps --open

# ── 环境初始化 ──────────────────────────────────────────────────────────

setup:
    @echo "Installing Rust..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    @echo "Done. Run: source ~/.cargo/env"
