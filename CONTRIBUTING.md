# Contributing to Spike

Thanks for helping improve Spike. This project follows a straightforward,
tool-enforced Rust workflow.

## Ground rules

- **Legal first**: Spike is a player and buffering engine. PRs must not add,
  hardcode, or advertise unlicensed content sources. See [LEGAL.md](LEGAL.md).
- Every change should come with tests when it touches parsing, buffering,
  scheduling, or serving logic — the engine is deliberately deterministic and
  mock-tested, keep it that way.

## Development workflow

```bash
git clone https://github.com/zoofam26/spike.git
cd spike
cargo build            # debug build
cargo test --workspace # 46 unit + integration tests must stay green
```

Before opening a PR, all three gates must pass (CI enforces them):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

If you touched the app crate, also verify the headless build still compiles
without the GUI stack (this is what ships for ARM64/Android):

```bash
cargo check -p spike-app --no-default-features
```

## Code style notes

- `spike-core` and `spike-stream` are intentionally `std`-only with zero
  external dependencies — do not add dependencies there without a strong
  portability argument (they must keep compiling for Android and musl).
- Errors are hand-rolled enums with `Display`/`Error` impls; keep the
  no-`thiserror` policy in the core crates.
- Public engine APIs take injected fetchers/clocks so tests never hit the
  network.

## Releases

Releases are fully automated: maintainers push an annotated tag
(`git tag -a v0.1.1 -m "..." && git push origin v0.1.1`) and the
`Release` workflow builds Linux x64, Linux ARM64, Windows x64 and Android
ARM64 binaries, attaches them to a GitHub Release, and publishes SHA256
checksums. Do not upload artifacts by hand.
