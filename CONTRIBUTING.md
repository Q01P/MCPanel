# Contributing to MCPanel

Thanks for pitching in. This is a small, deliberately scoped project; the bar for merging is that changes are focused and the full check suite passes clean.

## Dev setup

Prerequisites: Rust stable ≥ 1.95, Node 20+, and on Linux:

```bash
sudo apt install libwebkit2gtk-4.1-dev build-essential libxdo-dev libssl-dev \
  libayatana-appindicator3-dev librsvg2-dev
```

Then:

```bash
npm ci && npm run build        # required once before any cargo command:
                               # the Tauri build embeds dist/ at compile time
npm run tauri dev              # dev app: vite on :1420 + the Rust backend
```

## Tests and checks

Everything below must pass before a PR is ready:

```bash
# Rust
cargo fmt    --manifest-path src-tauri/Cargo.toml --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked -- -D warnings
cargo test   --manifest-path src-tauri/Cargo.toml --locked

# Frontend
npm run lint        # biome; accessibility rules are on, keep it clean
npm run typecheck   # tsc --noEmit
npm test            # vitest
```

Notes:

- **Never run `cargo test --release`.** The release profile sets `panic = "abort"`, which breaks `#[should_panic]` tests.
- Clippy runs with `-D warnings`; a single warning fails CI.
- The keyring tests are opt-in (they touch the real OS credential store and can prompt on macOS). To run them:

  ```bash
  MCPANEL_TEST_KEYRING=1 cargo test --locked --manifest-path src-tauri/Cargo.toml
  ```

  Without the variable they self-skip and pass silently.

## Pull requests

- **Scoped changes, one concern per PR.** A bug fix and a refactor are two PRs.
- Keep `Cargo.lock` / `package-lock.json` changes limited to what your change actually requires; don't add or bump dependencies incidentally.
- CI runs the suite above on Linux, macOS, and Windows, plus a Rust 1.95 (MSRV) check and a <15 MB release-binary size gate; a green local run is the fastest path to a green PR.

For security issues, don't open a PR or issue; see [SECURITY.md](SECURITY.md).

## Releasing

Release mechanics — tagging, the Homebrew tap, winget submission, and enabling code signing — are documented in [`packaging/README.md`](packaging/README.md).
