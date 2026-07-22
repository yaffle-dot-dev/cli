# Contributing

Contributions to the Yaffle CLI and its Rust engine crates are welcome.

## Before opening a pull request

Install the pinned Rust toolchain and a supported OpenTofu release, then run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Add tests through public interfaces and keep machine-readable output backward compatible within a
contract version. Do not include credentials, Terraform state, provider caches, or generated build
artifacts.

By intentionally submitting a contribution, you agree that it is licensed under this project's MIT
license.
