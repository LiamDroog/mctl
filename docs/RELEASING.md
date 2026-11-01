# Releasing

Releases are cut from `main` by pushing a version tag. The `Release` workflow
(`.github/workflows/release.yml`) then runs the tests on that exact commit, builds x86-64 and aarch64 Linux
binaries with the `dist` profile, packages them with `packaging/make-dist.sh`, and creates the GitHub release
with checksums. Nothing is published until a tag is pushed.

## Checklist

1. **CI is green on `main`** (`.github/workflows/ci.yml`: fmt, clippy, tests, docs, MSRV, `cargo deny`, render).
2. **Update `CHANGELOG.md`:** rename `[Unreleased]` to `[X.Y.Z] - YYYY-MM-DD` and start a new empty
   `[Unreleased]` above it. The workflow takes the release notes from the `[X.Y.Z]` section and fails if it is
   missing.
3. **Bump the version** in `Cargo.toml` (`[workspace.package] version`), then `cargo check --workspace` so
   `Cargo.lock` follows. The workflow fails if the tag and this version differ.
4. **Regenerate the notices** if dependencies changed: `tools/gen-notices.sh`.
5. **Commit and push** to `main`, wait for CI.
6. **Tag and push the tag:**

   ```
   git tag -a vX.Y.Z -m "mctl X.Y.Z"
   git push origin vX.Y.Z
   ```
7. **Check the release page:** both tarballs and their `.sha256` files are there. Download one, check it with
   `sha256sum -c`, unpack it and run `./mctl --version` and `./mctl about`. On a Raspberry Pi, try the aarch64
   one with `mctl --kiosk`.

## Rolling back

A bad release is deleted and the tag removed, then fixed forward with a new patch version:

```
gh release delete vX.Y.Z --yes
git push --delete origin vX.Y.Z
git tag -d vX.Y.Z
```

## Notes

- The binaries are built on the oldest runner images offered (`ubuntu-22.04`, `ubuntu-22.04-arm`), so the
  minimum glibc is 2.35. When GitHub retires those images, move the matrix up and say so in the changelog.
- The minimum Rust version (`rust-version` in `Cargo.toml`) is checked by the `msrv` CI job; keep the job's
  toolchain in step with it.
- Third-party actions are referenced by version tag; Dependabot keeps them current. Pin them to commit
  hashes if you want stricter supply-chain hygiene.
- The licence is GPL-3.0-or-later, so a release must make the corresponding source available: the tag does
  that, and the release notes link to it.
