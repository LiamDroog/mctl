# Contributing

Thanks for looking. mctl is a hobby project, so replies may take a while, but bug reports, fixes and ideas are
welcome.

## Before you start

- For anything bigger than a small fix, open an issue first so we can agree on the approach.
- By contributing you agree that your work is licensed under the project's licence, GPL-3.0-or-later.
- mctl shows data about real spacecraft. It must never present something as measured when it is not: a quantity
  with no source reads `NO DATA`, and estimated, planned or derived values carry a flag (`[EST]`, `[PLAN]`,
  `[TLE]`, `[SURVEY]`). Changes that blur that will be asked to change.

## Building and checking

You need Rust 1.95 or newer and, to run the GUI, a Vulkan driver (Mesa's software one, lavapipe, is enough).
`packaging/bootstrap.sh --check` tells you what a machine is missing and `packaging/bootstrap.sh` installs it.
The `astro` prototype under `spikes/` needs libclang, so the commands below leave it out.

```
cargo build --release --locked -p mctl
cargo fmt --all
cargo clippy --workspace --exclude astro --all-targets --locked -- -D warnings
cargo test --workspace --exclude astro --locked
SMOKE_SOFTWARE=1 tools/smoke.sh      # renders every page headless; drop SMOKE_SOFTWARE if you have a real GPU
```

CI runs the same, plus `cargo deny check` (licences and advisories) and a build on the minimum Rust version.
Please run them before opening a pull request.

The parsers have property tests (`crates/*/tests/robustness.rs`): they feed real fixtures with random damage
and arbitrary text, and assert that nothing panics. A new parser of outside data should get one.

Rendering has golden-image tests that compare fixed pages with `crates/mctl-gui/tests/golden/`:

```
VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json cargo test -p mctl-gui golden -- --ignored
```

After an intended change to how something looks, rewrite them with `MCTL_BLESS=1` in front of that command and
review the changed images in the diff.

## Conventions

- No network access in tests: use recorded fixtures (see `crates/*/fixtures`).
- Errors are handled explicitly and fail loudly. No silent fallbacks, and no `unwrap()` in non-test code
  (clippy denies it); `expect("...")` is for invariants, and the message says which one.
- URLs, rates and tunable constants live in the config (`config/default.toml`) or in clearly labelled
  constants, not scattered in code.
- Logging uses `tracing`.
- New dependencies must be compatible with GPL-3.0-or-later and pass `cargo deny check`; say why in the pull
  request. Prefer `rustls` to OpenSSL: the default build links only libc and the GPU stack.
- Dependencies change `THIRD-PARTY-NOTICES.md`: regenerate it with `tools/gen-notices.sh`.
- Keep commits small, with a message that says why.

## Reporting problems

A bug report is most useful with: what you ran (`mctl --version`, the flags), your OS and GPU, and the
relevant part of `~/.local/state/mctl/mctl.log`. Please do not post screenshots that show your location.
Security issues go through [SECURITY.md](SECURITY.md), not public issues.
