#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

BLUE="\033[34m"
GREEN="\033[32m"
YELLOW="\033[33m"
RED="\033[31m"
RESET="\033[0m"

VERSION="${VERSION:-}"
CHANNEL="${CHANNEL:-stable}"
TARGETS="${TARGETS:-linux-x86_64-gnu}"
ALLOW_DIRTY="${ALLOW_DIRTY:-0}"
DRY_RUN="${DRY_RUN:-0}"
SKIP_TESTS="${SKIP_TESTS:-0}"
SKIP_TAG="${SKIP_TAG:-0}"
USE_CROSS="${USE_CROSS:-0}"
EXTRA_FEATURES="${EXTRA_FEATURES:-}"
RELEASE_DATE="${RELEASE_DATE:-$(date -u +"%Y-%m-%dT%H:%M:%SZ")}"

TMP_WORK="$(mktemp -d -t audetic-release.XXXXXX)"
trap 'rm -rf "$TMP_WORK"' EXIT

log() {
  local level="$1"; shift
  case "$level" in
    info) echo -e "${BLUE}==>${RESET} $*";;
    success) echo -e "${GREEN}✓${RESET} $*";;
    warn) echo -e "${YELLOW}!${RESET} $*";;
    error) echo -e "${RED}✗${RESET} $*";;
    *) echo "$@";;
  esac
}

die() {
  log error "$*"
  exit 1
}

ensure_command() {
  local cmd="$1"
  command -v "$cmd" >/dev/null 2>&1 || die "Missing required command: $cmd"
}

semver_re='^[0-9]+\.[0-9]+\.[0-9]+([\-+][0-9A-Za-z\.-]+)?$'

validate_version() {
  [[ -z "$VERSION" ]] && die "VERSION is required (e.g. make deploy VERSION=0.2.0)"
  [[ "$VERSION" =~ $semver_re ]] || die "VERSION must be semantic (got: $VERSION)"
  local current="0.0.0"
  if [[ -f release/cli/version ]]; then
    current="$(cat release/cli/version | tr -d ' \n')"
  fi
  local highest
  highest="$(printf '%s\n' "$current" "$VERSION" | sort -V | tail -n1)"
  [[ "$highest" == "$VERSION" ]] || die "VERSION ($VERSION) must be >= current release ($current)"
  if [[ "$VERSION" == "$current" ]]; then
    log warn "VERSION matches current release. Proceeding but remember to bump if this is a new release."
  fi
}

ensure_clean_git() {
  if [[ "$ALLOW_DIRTY" == "1" ]]; then
    log warn "Skipping git clean check (ALLOW_DIRTY=1)"
    return
  fi
  if git status --porcelain | grep -q '.'; then
    die "Working tree not clean. Commit/stash changes or set ALLOW_DIRTY=1"
  fi
}

maybe_run_tests() {
  if [[ "$SKIP_TESTS" == "1" ]]; then
    log warn "Skipping tests (SKIP_TESTS=1)"
    return
  fi
  log info "Running cargo test"
  if [[ "$DRY_RUN" == "1" ]]; then
    echo "  [dry-run] cargo test"
  else
    cargo test
  fi
}

map_rust_target() {
  case "$1" in
    linux-x86_64-gnu) echo "x86_64-unknown-linux-gnu";;
    linux-aarch64-gnu) echo "aarch64-unknown-linux-gnu";;
    macos-aarch64) echo "aarch64-apple-darwin";;
    macos-x86_64) echo "x86_64-apple-darwin";;
    *)
      die "Unknown target identifier: $1"
      ;;
  esac
}

file_size_bytes() {
  local file="$1"
  if command -v stat >/dev/null 2>&1; then
    if stat --version >/dev/null 2>&1; then
      stat -c%s "$file"
    else
      stat -f%z "$file"
    fi
  else
    python3 - <<'PY' "$file"
import os, sys
print(os.path.getsize(sys.argv[1]))
PY
  fi
}

sha256_file() {
  local file="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file" | awk '{print $1}'
  else
    die "Need sha256sum or shasum for hashing"
  fi
}

update_manifest() {
  local manifest="$1"
  local version="$2"
  local channel="$3"
  local release_date="$4"
  local target_id="$5"
  local archive_name="$6"
  local sha="$7"
  local size="$8"
  local notes_url="$9"
  python3 - <<'PY' "$manifest" "$version" "$channel" "$release_date" "$target_id" "$archive_name" "$sha" "$size" "$notes_url"
import json, sys, pathlib, datetime
manifest_path = pathlib.Path(sys.argv[1])
version, channel, release_date, target_id, archive, sha, size, notes_url = sys.argv[2:]
size = int(size)
if manifest_path.exists():
    data = json.loads(manifest_path.read_text())
else:
    data = {}
data.setdefault("targets", {})
data["version"] = version
data["channel"] = channel
data["release_date"] = release_date
data["notes_url"] = notes_url
data["targets"][target_id] = {
    "archive": archive,
    "sha256": sha,
    "sig": data["targets"].get(target_id, {}).get("sig") if target_id in data["targets"] else None,
    "size": size
}
manifest_path.write_text(json.dumps(data, indent=2) + "\n")
PY
}

create_notes_if_missing() {
  local notes_file="$1"
  if [[ -f "$notes_file" || "$DRY_RUN" == "1" ]]; then
    return
  fi
  cat > "$notes_file" <<EOF
# Audetic $VERSION

- TODO: describe highlights for this release.
EOF
}

write_version_file() {
  local version_file="release/cli/version"
  if [[ "$DRY_RUN" == "1" ]]; then
    log info "[dry-run] would update $version_file to $VERSION"
  else
    echo "$VERSION" > "$version_file"
  fi
}

strip_binary_if_possible() {
  local bin="$1"
  if command -v strip >/dev/null 2>&1; then
    strip "$bin" 2>/dev/null || true
  fi
}

build_target() {
  local target_id="$1"
  local rust_target
  rust_target="$(map_rust_target "$target_id")"
  local build_cmd=("cargo" "build" "--release" "--target" "$rust_target")
  if [[ -n "$EXTRA_FEATURES" ]]; then
    build_cmd+=("--features" "$EXTRA_FEATURES")
  fi
  if [[ "$USE_CROSS" == "1" ]]; then
    ensure_command cross
    build_cmd=("cross" "build" "--release" "--target" "$rust_target")
    if [[ -n "$EXTRA_FEATURES" ]]; then
      build_cmd+=("--features" "$EXTRA_FEATURES")
    fi
  fi

  log info "Building $target_id ($rust_target)"
  if [[ "$DRY_RUN" == "1" ]]; then
    echo "  [dry-run] ${build_cmd[*]}"
    return
  fi
  "${build_cmd[@]}"
}

package_target() {
  local target_id="$1"
  local rust_target
  rust_target="$(map_rust_target "$target_id")"
  local bin_path="target/$rust_target/release/audetic"
  [[ -f "$bin_path" ]] || die "Missing binary at $bin_path. Build step likely failed."

  local stage="$TMP_WORK/$target_id"
  mkdir -p "$stage"
  cp "$bin_path" "$stage/audetic"
  strip_binary_if_possible "$stage/audetic"
  cp audetic.service "$stage/audetic.service"
  cp example_config.toml "$stage/example_config.toml"
  cat > "$stage/README.txt" <<EOF
Audetic $VERSION ($target_id)

Files:
  audetic             - main binary
  audetic.service     - systemd user unit template
  example_config.toml - starter configuration

Installation instructions: https://install.audetic.ai/
EOF

  local release_dir="release/cli/releases/$VERSION/$target_id"
  local archive_name="audetic-$VERSION-$target_id.tar.gz"
  local archive_path="$release_dir/$archive_name"
  mkdir -p "$release_dir"
  tar -C "$stage" -czf "$archive_path" .
  local sha
  sha="$(sha256_file "$archive_path")"
  echo "$sha  $archive_name" > "${archive_path}.sha256"
  local size
  size="$(file_size_bytes "$archive_path")"

  local notes_url="https://install.audetic.ai/cli/releases/$VERSION/notes.md"
  update_manifest "$release_dir/../manifest.json" "$VERSION" "$CHANNEL" "$RELEASE_DATE" "$target_id" "$archive_name" "$sha" "$size" "$notes_url"

  ARTIFACT_SUMMARY+=("$target_id|$archive_path|$sha|$size")
}

publish_assets() {
  if [[ "$DRY_RUN" == "1" ]]; then
    log info "[dry-run] would run godeploy deploy"
    return
  fi
  if command -v godeploy >/dev/null 2>&1; then
    log info "Publishing via godeploy"
    godeploy deploy
  else
    log warn "godeploy not installed. Skipping publish."
  fi
}

tag_release() {
  if [[ "$DRY_RUN" == "1" || "$SKIP_TAG" == "1" ]]; then
    log warn "Skipping git tag (DRY_RUN or SKIP_TAG set)"
    return
  fi
  if git rev-parse "v$VERSION" >/dev/null 2>&1; then
    log warn "Tag v$VERSION already exists. Skipping tag creation."
    return
  fi
  git tag -a "v$VERSION" -m "Audetic $VERSION"
  git push origin "v$VERSION"
  log success "Created git tag v$VERSION"
}

main() {
  ensure_command cargo
  ensure_command python3
  ensure_command tar

  validate_version
  ensure_clean_git
  maybe_run_tests

  local release_root="release/cli/releases/$VERSION"
  mkdir -p "$release_root"
  create_notes_if_missing "$release_root/notes.md"

  write_version_file

  ARTIFACT_SUMMARY=()
  for target in $TARGETS; do
    build_target "$target"
    if [[ "$DRY_RUN" == "1" ]]; then
      continue
    fi
    package_target "$target"
  done

  publish_assets
  tag_release

  if [[ "${#ARTIFACT_SUMMARY[@]}" -gt 0 ]]; then
    log success "Artifacts ready:"
    for entry in "${ARTIFACT_SUMMARY[@]}"; do
      IFS="|" read -r target path sha size <<< "$entry"
      echo "  - $target -> $path (sha256: $sha, size: $size bytes)"
    done
  else
    log warn "No artifacts were produced (dry run?)."
  fi
}

main "$@"
