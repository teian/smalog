#!/usr/bin/env bash
#
# Build the release artefacts locally — the same .tar.gz per target and .deb
# for the Raspberry Pi targets that the GitHub release workflow produces —
# without publishing a release or waiting for the pipeline.
#
# The packaged Debian changelog is the committed one. The workflow rewrites it
# per release; a local build deliberately does not touch the working tree.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

# target triple -> release architecture label -> Debian architecture (empty:
# no .deb, matching the workflow's matrix).
declare -A release_arch=(
  [i686-unknown-linux-gnu]=linux-x86
  [x86_64-unknown-linux-gnu]=linux-amd64
  [armv7-unknown-linux-gnueabihf]=linux-armv7
  [aarch64-unknown-linux-gnu]=linux-arm64
)
declare -A deb_arch=(
  [armv7-unknown-linux-gnueabihf]=armhf
  [aarch64-unknown-linux-gnu]=arm64
)
# The Raspberry Pi targets: the ones that ship a Debian package.
default_targets=(armv7-unknown-linux-gnueabihf aarch64-unknown-linux-gnu)
all_targets=(
  i686-unknown-linux-gnu
  x86_64-unknown-linux-gnu
  armv7-unknown-linux-gnueabihf
  aarch64-unknown-linux-gnu
)

usage() {
  cat <<'USAGE'
usage: scripts/build-packages.sh [options] [TARGET ...]

Builds a release archive per target, plus a Debian package for the Raspberry
Pi targets, into dist/ (override with --output).

Targets (default: the two that produce a .deb)
  armv7-unknown-linux-gnueabihf   linux-armv7, Debian armhf
  aarch64-unknown-linux-gnu       linux-arm64, Debian arm64
  x86_64-unknown-linux-gnu        linux-amd64
  i686-unknown-linux-gnu          linux-x86

Options
  --all              build every target listed above
  --no-ui            build without the web dashboard (no `ui` feature)
  --skip-ui-build    keep the existing src/ui/dist instead of running pnpm
  --version-label V  name the archives after V instead of the crate version
  -o, --output DIR   where the artefacts land (default: dist)
  -h, --help         this text

Requirements
  cross and cargo-deb (cargo install cross cargo-deb), a running Docker or
  Podman for anything but the host target, jq, and pnpm unless --no-ui or
  --skip-ui-build is given.

Examples
  scripts/build-packages.sh                      # both Pi packages, with UI
  scripts/build-packages.sh --no-ui aarch64-unknown-linux-gnu
  scripts/build-packages.sh --all --output /tmp/smalog-dist
USAGE
}

targets=()
output_dir=dist
with_ui=1
build_ui=1
version_label=

while [[ $# -gt 0 ]]; do
  case "$1" in
    --all) targets=("${all_targets[@]}") ;;
    --no-ui) with_ui=0 ;;
    --skip-ui-build) build_ui=0 ;;
    --version-label)
      [[ $# -ge 2 ]] || { echo "--version-label needs a value" >&2; exit 64; }
      version_label=$2
      shift
      ;;
    -o | --output)
      [[ $# -ge 2 ]] || { echo "--output needs a directory" >&2; exit 64; }
      output_dir=$2
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    -*)
      echo "unknown option: $1" >&2
      usage >&2
      exit 64
      ;;
    *)
      [[ -n "${release_arch[$1]:-}" ]] || {
        echo "unknown target: $1" >&2
        echo "known targets: ${all_targets[*]}" >&2
        exit 64
      }
      targets+=("$1")
      ;;
  esac
  shift
done

if [[ ${#targets[@]} -eq 0 ]]; then
  targets=("${default_targets[@]}")
fi

require() {
  command -v "$1" >/dev/null || {
    echo "$1 is required: $2" >&2
    exit 69
  }
}

require jq "install it from your distribution"
require cargo "install Rust from https://rustup.rs"

host_triple="$(rustc -vV | awk '/^host:/ {print $2}')"
needs_cross=0
for target in "${targets[@]}"; do
  [[ "$target" == "$host_triple" ]] || needs_cross=1
done
if [[ $needs_cross -eq 1 ]]; then
  require cross "cargo install cross --git https://github.com/cross-rs/cross.git"
  if ! docker info >/dev/null 2>&1 && ! podman info >/dev/null 2>&1; then
    echo "cross needs a running Docker or Podman for a foreign target" >&2
    exit 69
  fi
fi
for target in "${targets[@]}"; do
  if [[ -n "${deb_arch[$target]:-}" ]]; then
    require cargo-deb "cargo install cargo-deb"
    break
  fi
done

version="$(
  cargo metadata --format-version 1 --no-deps |
    jq -r '.packages[] | select(.name == "smalog") | .version'
)"
label="${version_label:-v$version}"

if [[ $with_ui -eq 1 && $build_ui -eq 1 ]]; then
  require pnpm "install it, or pass --no-ui / --skip-ui-build"
  echo "==> building the web dashboard"
  pnpm --dir src/ui install --frozen-lockfile
  pnpm --dir src/ui run build
elif [[ $with_ui -eq 1 && ! -d src/ui/dist ]]; then
  echo "src/ui/dist is missing: drop --skip-ui-build, or build with --no-ui" >&2
  exit 65
fi

features=()
[[ $with_ui -eq 1 ]] && features=(--features ui)

mkdir -p "$output_dir"
output_dir="$(cd "$output_dir" && pwd)"

for target in "${targets[@]}"; do
  arch="${release_arch[$target]}"
  echo "==> building smalog $label for $target ($arch)"

  if [[ "$target" == "$host_triple" ]]; then
    builder=(cargo)
    container_opts=""
  else
    builder=(cross)
    # ~/.cargo/config.toml is mounted into the container, so a rustc wrapper
    # configured on the host (sccache and friends) is looked up there too and
    # is not installed. An empty value reads as unset. This cannot be a
    # `--config` flag: cross takes the first argument as its subcommand and
    # silently falls back to a host cargo build.
    container_opts="-e CARGO_BUILD_RUSTC_WRAPPER="
  fi
  CROSS_CONTAINER_OPTS="$container_opts" "${builder[@]}" build \
    --release \
    --locked \
    --target "$target" \
    --package smalog \
    "${features[@]}"

  staging="$(mktemp -d)"
  trap 'rm -rf "$staging"' EXIT
  install -D -m 0755 "target/$target/release/smalog" "$staging/smalog"
  install -D -m 0644 config.example.toml "$staging/config.example.toml"
  install -D -m 0644 packaging/smalog.service "$staging/smalog.service"
  install -D -m 0644 README.md "$staging/README.md"
  install -D -m 0644 LICENSE.md "$staging/LICENSE.md"
  tar -C "$staging" -czf "$output_dir/smalog-$label-$arch.tar.gz" .
  rm -rf "$staging"
  trap - EXIT

  if [[ -n "${deb_arch[$target]:-}" ]]; then
    echo "==> packaging ${deb_arch[$target]} .deb"
    # --no-build: package the binary just built for this target. --no-strip
    # keeps cargo-deb from running the host strip on a foreign binary.
    cargo deb \
      --no-build \
      --no-strip \
      --target "$target" \
      --package smalog \
      --output "$output_dir/"
  fi
done

(cd "$output_dir" && sha256sum smalog-* smalog_* 2>/dev/null > SHA256SUMS || true)

echo
echo "artefacts in $output_dir:"
ls -1sh "$output_dir"
echo
echo "install one on the Pi with:"
echo "  scp $output_dir/smalog_${version}-1_arm64.deb pi:/tmp/"
echo "  ssh pi 'sudo apt-get install -y /tmp/smalog_${version}-1_arm64.deb'"
