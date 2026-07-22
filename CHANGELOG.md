# Changelog

## 0.1.0

- Publish the Rust workspace as the canonical standalone Yaffle CLI.
- Support local converge, destroy, status, wait, outputs, graph, doctor, Terraform login, and cloud
  authentication commands.
- Define versioned JSON output and stable process exit codes.
- Support system OpenTofu versions `>=1.8.0,<2.0.0`.
- Build release binaries through the locked Nix flake and publish a Homebrew tap formula.
