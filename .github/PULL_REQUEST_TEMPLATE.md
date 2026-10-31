## What and why

<!-- What does this change, and why? Link the issue if there is one. -->

## Checks

- [ ] `cargo fmt --all`, `cargo clippy --workspace --exclude astro --all-targets --locked -- -D warnings` and `cargo test --workspace --exclude astro --locked` pass
- [ ] No test needs the network
- [ ] Nothing is presented as measured that is not (estimates and plans carry a flag)
- [ ] New dependencies are GPL-3.0-or-later compatible, and `THIRD-PARTY-NOTICES.md` is regenerated if they changed
- [ ] No screenshots or logs that show a real location
