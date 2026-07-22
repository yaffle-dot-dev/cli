#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  printf 'usage: %s VERSION RELEASE_BASE_URL DIST_DIR\n' "$0" >&2
  exit 2
fi

version="$1"
release_base_url="${2%/}"
dist_dir="$3"

checksum() {
  local archive_path="$dist_dir/$1"
  if [[ ! -f "$archive_path" ]]; then
    printf 'missing release archive: %s\n' "$archive_path" >&2
    exit 1
  fi
  shasum -a 256 "$archive_path" | cut -d ' ' -f 1
}

linux_archive="yaffle-${version}-x86_64-unknown-linux-musl.tar.gz"
macos_intel_archive="yaffle-${version}-x86_64-apple-darwin.tar.gz"
macos_arm_archive="yaffle-${version}-aarch64-apple-darwin.tar.gz"

cat <<FORMULA
class Yaffle < Formula
  desc "Environment orchestration for Terraform and OpenTofu"
  homepage "https://yaffle.dev"
  version "${version}"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "${release_base_url}/${macos_arm_archive}"
      sha256 "$(checksum "$macos_arm_archive")"
    else
      url "${release_base_url}/${macos_intel_archive}"
      sha256 "$(checksum "$macos_intel_archive")"
    end
  end

  on_linux do
    on_intel do
      url "${release_base_url}/${linux_archive}"
      sha256 "$(checksum "$linux_archive")"
    end
  end

  def install
    bin.install "yaffle"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/yaffle --version")
  end
end
FORMULA
