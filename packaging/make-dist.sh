#!/usr/bin/env bash
# Assemble a release tarball and its checksum.
#
#   packaging/make-dist.sh <target-triple> [path/to/mctl]
#
# The binary defaults to target/dist/mctl (cargo build --profile dist --locked -p mctl); a path overrides it.
# Writes dist/mctl-<version>-<target>.tar.gz and a .sha256 beside it. Used by the release workflow, and fine
# to run by hand.
set -euo pipefail
cd "$(dirname "$0")/.."

target="${1:?usage: make-dist.sh <target-triple> [binary]}"
bin="${2:-target/dist/mctl}"
[ -x "$bin" ] || { echo "make-dist.sh: $bin is not an executable file (build it first)" >&2; exit 1; }

version="$(awk -F'"' '/^version *=/ { print $2; exit }' Cargo.toml)"
[ -n "$version" ] || { echo "make-dist.sh: cannot read the version from Cargo.toml" >&2; exit 1; }

name="mctl-$version-$target"
stage="dist/$name"
rm -rf "$stage" "dist/$name.tar.gz" "dist/$name.tar.gz.sha256"
mkdir -p "$stage/packaging/pi"

install -m755 "$bin" "$stage/mctl"
install -m644 README.md LICENSE THIRD-PARTY-NOTICES.md CHANGELOG.md "$stage/"
install -m644 config/default.toml "$stage/default-config.toml"
install -m755 packaging/install.sh "$stage/packaging/"
install -m644 packaging/mctl.desktop packaging/mctl-pi.desktop packaging/mctl.svg "$stage/packaging/"
install -m644 packaging/pi/mctl-display.service.in "$stage/packaging/pi/"
install -m755 packaging/pi/install-display.sh "$stage/packaging/pi/"
install -m644 docs/pi.md "$stage/"

# Reproducible-ish: fixed owner, sorted names, and a modification time from the last commit when there is one.
mtime="$(git log -1 --format=%cI 2>/dev/null || date -u +%FT%TZ)"
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="$mtime" -C dist -czf "dist/$name.tar.gz" "$name"
rm -rf "$stage"
(cd dist && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
echo "dist/$name.tar.gz"
