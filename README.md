# Yaffle CLI

The Yaffle CLI is the open-source Rust environment engine for Terraform and OpenTofu repositories.
It discovers the workspace graph in `yaffle.toml` and provides deterministic local operations for
named and transient environments.

## Install

Download the archive for your platform from the GitHub release and verify its adjacent `.sha256`
file before placing `yaffle` on your `PATH`. Linux x86-64 and macOS x86-64/ARM64 releases are
supported. Repository administrators must keep GitHub immutable releases enabled; release automation
creates each tagged release and its complete asset set in one operation and cannot update it later.

You can also build directly from source:

```bash
cargo install --git https://github.com/yaffle-dot-dev/cli --locked yaffle-cli
```

Homebrew installs the same checksummed GitHub release archives:

```bash
brew install yaffle-dot-dev/tap/yaffle
```

Nix builds are supported directly and are the canonical release builder:

```bash
nix run github:yaffle-dot-dev/cli -- --help
nix build github:yaffle-dot-dev/cli#yaffle
```

The v0.1 CLI requires a system OpenTofu version in the range `>=1.8.0,<2.0.0`. Set
`YAFFLE_TOFU_PATH` to use an explicit compatible binary instead of `tofu` from `PATH`.

CLI startup and local environment operations do not require Bun, Node.js, a Yaffle source checkout,
developer environment variables, or an internal feature token. Cloud login is a public-client flow
and likewise has no client secret.

## Use

Run `yaffle` or `yaffle --help` to inspect the command surface.

```bash
yaffle doctor --json
yaffle graph --env pr-42
yaffle converge --env main
yaffle status --env main --json
yaffle outputs --env main --workspace infra/shared
yaffle destroy --env pr-42
```

Local operations read repository configuration and invoke OpenTofu on the local machine. Cloud
login uses an OAuth public-client flow with PKCE and stores the resulting credential under
`~/.yaffle/auth/`. See [Privacy and data transfers](PRIVACY.md) before using cloud operations.

## Automation

Commands with `--json` emit one versioned JSON document on stdout. Stable output shapes and process
exit codes are documented in [the CLI contract](docs/cli-contract.md).

## Development

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --release --locked
```

See [CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md) before contributing or
reporting a vulnerability.
