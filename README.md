# Yaffle CLI

`@yaffle/cli` is the Bun-based operator CLI for Yaffle.

> Yaffle maintainers primarily work on this project from the main Yaffle monorepo. Community contributions in the public repo are welcome and are synced back into the monorepo after review.

## What it does

- open the operator TUI
- queue remote plan and apply actions
- inspect preview outputs
- manage local Yaffle API credentials

## Local development

From the monorepo root:

```bash
bun run --filter=@yaffle/cli build
bun run --filter=@yaffle/cli test
bun run --filter=@yaffle/cli typecheck
```

From a published standalone repo clone:

```bash
bun install
bun run build
bun test
bun run src/main.ts --help
```

## Nix and release binaries

The published standalone CLI repo includes its own `flake.nix` and `flake.lock` so it can be run or built directly with Nix:

```bash
nix run .#yaffle -- --help
nix build .#yaffle-cli
```

Published releases can also attach prebuilt binaries from GitHub Actions for supported platforms.

The standalone repo also maintains a rolling `edge` release on every successful push to `main`.
