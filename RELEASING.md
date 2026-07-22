# Releasing

Tags matching `v*` run `.github/workflows/release.yml`.

The workflow builds native Linux and macOS binaries exclusively through the locked Nix flake,
smoke-tests each binary, creates checksummed archives, and publishes all assets atomically with the
GitHub release. GitHub immutable releases must remain enabled for `yaffle-dot-dev/cli`.

After the release is published, the same workflow updates `Formula/yaffle.rb` in
`yaffle-dot-dev/homebrew-tap`. The CLI repository must define these Actions secrets:

- `YAFFLE_INTERNAL_GH_APP_ID`
- `YAFFLE_INTERNAL_GH_APP_PRIVATE_KEY`

The GitHub App installation must be restricted to `yaffle-dot-dev/homebrew-tap` with repository
contents write access. It does not need write access to the CLI repository.

Before creating a tag, update the workspace version and changelog, then verify:

```bash
cargo test --workspace --locked
nix flake check
nix build .#yaffle
result/bin/yaffle --version
```

Users install the release with:

```bash
brew install yaffle-dot-dev/tap/yaffle
```
