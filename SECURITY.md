# Security

## Reporting a vulnerability

Please report security problems privately, using GitHub's
[private vulnerability reporting](https://github.com/LiamDroog/mctl/security/advisories/new) for this
repository, not a public issue. Include what you found, how to reproduce it, and which version. I will
acknowledge it as soon as I can; this is a hobby project, so please allow some days.

## What is in scope

mctl is a desktop program that downloads public data over HTTPS and parses it. The realistic risks are in
that path, so reports about these are especially welcome:

- memory-safety or parsing bugs triggered by a malformed response (the crates forbid `unsafe` code, so these
  would be logic errors such as panics or unbounded memory use);
- the on-disk cache, configuration and recording (`~/.cache/mctl`, `~/.config/mctl`, `~/.local/share/mctl`);
- anything that sends information about you off the machine. By design, only the configured data sources are
  contacted, and the optional IP-geolocation lookup is off by default.

## Supported versions

Only the latest release is supported.
