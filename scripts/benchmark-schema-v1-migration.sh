#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 4 ]]; then
  echo "usage: $0 SOURCE_SQLITE_URL TARGET_URL TIMEZONE OUTPUT_DIR" >&2
  exit 64
fi

source_url=$1
target_url=$2
plant_timezone=$3
output_dir=$4

if [[ $source_url != sqlite://* ]]; then
  echo "SOURCE_SQLITE_URL must start with sqlite://" >&2
  exit 64
fi

source_path=${source_url#sqlite://}
if [[ ! -f $source_path ]]; then
  echo "source does not exist: $source_path" >&2
  exit 66
fi

mkdir -p "$output_dir"
source_hash_file="$output_dir/source.sha256"
sha256sum "$source_path" > "$source_hash_file"

cargo build --release --locked -p smalog
binary=target/release/smalog

"$binary" migrate-sbfspot \
  --source "$source_url" \
  --target "$target_url" \
  --timezone "$plant_timezone" \
  --dry-run > "$output_dir/preflight.json"
sha256sum --check "$source_hash_file"

/usr/bin/time -v -o "$output_dir/migration.time.txt" \
  "$binary" migrate-sbfspot \
  --source "$source_url" \
  --target "$target_url" \
  --timezone "$plant_timezone" \
  > "$output_dir/migration.json"
sha256sum --check "$source_hash_file"

"$binary" migrate-sbfspot \
  --source "$source_url" \
  --target "$target_url" \
  --timezone "$plant_timezone" \
  --verify-only > "$output_dir/verify.json"
sha256sum --check "$source_hash_file"

echo "reports written to $output_dir"
