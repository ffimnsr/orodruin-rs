#!/usr/bin/env bash
set -euo pipefail

readonly RELEASE_MANIFEST="crates/orodruin/Cargo.toml"
readonly RELEASE_PACKAGE="orodruin-cli"
readonly REMOTE_NAME="origin"
readonly CHANGELOG_FILE="CHANGELOG.md"

usage() {
  cat <<'EOF'
Usage: scripts/release.sh [options] [<version>]

Bump the crate version, refresh Cargo.lock, update CHANGELOG.md, run release
quality gates, create a release commit, create a v-prefixed git tag, and
optionally push the commit/tag and publish to crates.io.

Release version source of truth: crates/orodruin/Cargo.toml

Options:
  --major      Increment major version and reset minor/patch to zero.
  --minor      Increment minor version and reset patch to zero.
  --patch      Increment patch version.
  --skip-push  Skip pushing the release commit and tag to origin.
  --publish    Publish the crate to crates.io after tagging.
  -h, --help   Show this help message.

Examples:
  scripts/release.sh --patch
  scripts/release.sh --minor --skip-push
  scripts/release.sh 0.2.0
EOF
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

manifest_version() {
  awk '
    BEGIN { in_package = 0 }
    /^\[package\]$/ { in_package = 1; next }
    /^\[/ && $0 != "[package]" && in_package { in_package = 0 }
    in_package && /^version = "/ {
      gsub(/^version = "/, "", $0)
      gsub(/"$/, "", $0)
      print
      exit
    }
  ' "$RELEASE_MANIFEST"
}

ensure_clean_worktree() {
  git diff --quiet --exit-code || die "working tree has unstaged changes"
  git diff --cached --quiet --exit-code || die "index has staged but uncommitted changes"
}

previous_release_tag() {
  git describe --tags --abbrev=0 --match 'v[0-9]*.[0-9]*.[0-9]*' 2>/dev/null || true
}

append_section_entry() {
  local section_name="$1"
  local entry="$2"
  printf -v "$section_name" '%s- %s\n' "${!section_name}" "$entry"
}

render_changelog_group() {
  local title="$1"
  local content="$2"

  [[ -n "$content" ]] || return 0

  printf '### %s\n\n' "$title"
  printf '%s\n' "$content"
  printf '\n'
}

update_changelog() {
  local version="$1"
  local release_date="$2"
  local previous_tag="$3"
  local log_range
  local features=""
  local fixes=""
  local docs=""
  local tests=""
  local ci=""
  local maintenance=""
  local other=""
  local entry_count=0
  local conventional_commit_regex='^([[:alnum:]_-]+)(\([^)]+\))?(!)?:[[:space:]]*(.+)$'

  if [[ -n "$previous_tag" ]]; then
    log_range="${previous_tag}..HEAD"
  else
    log_range="HEAD"
  fi

  while IFS=$'\t' read -r commit_sha subject; do
    local short_sha category message commit_type

    [[ -n "$commit_sha" ]] || continue

    short_sha="$(git rev-parse --short "$commit_sha")"
    category="other"
    message="$subject"

    if [[ "$subject" =~ $conventional_commit_regex ]]; then
      commit_type="${BASH_REMATCH[1]}"
      message="${BASH_REMATCH[4]}"
      case "$commit_type" in
        feat)
          category="features"
          ;;
        fix)
          category="fixes"
          ;;
        docs)
          category="docs"
          ;;
        test)
          category="tests"
          ;;
        ci)
          category="ci"
          ;;
        build|style|refactor|perf|chore)
          category="maintenance"
          ;;
      esac
    fi

    case "$category" in
      features)
        append_section_entry features "${message} (\`${short_sha}\`)"
        ;;
      fixes)
        append_section_entry fixes "${message} (\`${short_sha}\`)"
        ;;
      docs)
        append_section_entry docs "${message} (\`${short_sha}\`)"
        ;;
      tests)
        append_section_entry tests "${message} (\`${short_sha}\`)"
        ;;
      ci)
        append_section_entry ci "${message} (\`${short_sha}\`)"
        ;;
      maintenance)
        append_section_entry maintenance "${message} (\`${short_sha}\`)"
        ;;
      *)
        append_section_entry other "${message} (\`${short_sha}\`)"
        ;;
    esac

    ((entry_count += 1))
  done < <(git log --reverse --format='%H%x09%s' "$log_range")

  ((entry_count > 0)) || die "no commits found for changelog range: ${log_range}"

  if [[ -f "$CHANGELOG_FILE" ]] && grep -Eq "^## ${version//./\\.}([[:space:]]|$)" "$CHANGELOG_FILE"; then
    die "$CHANGELOG_FILE already contains an entry for version $version"
  fi

  local tmp preface existing_releases
  tmp="$(mktemp)"
  preface=""
  existing_releases=""

  if [[ -f "$CHANGELOG_FILE" ]]; then
    preface="$(awk '
      BEGIN { seen_release = 0 }
      /^## [0-9]+\.[0-9]+\.[0-9]+([[:space:]]-|$)/ {
        seen_release = 1
        exit
      }
      { print }
    ' "$CHANGELOG_FILE")"

    existing_releases="$(awk '
      BEGIN { seen_release = 0 }
      /^## [0-9]+\.[0-9]+\.[0-9]+([[:space:]]-|$)/ {
        seen_release = 1
      }
      seen_release { print }
    ' "$CHANGELOG_FILE")"
  fi

  {
    if [[ -n "$preface" ]]; then
      printf '%s\n' "$preface"
      [[ "$preface" == *$'\n' ]] || printf '\n'
    else
      printf '# Changelog\n\n'
    fi

    printf '## %s - %s\n\n' "$version" "$release_date"
    render_changelog_group "Features" "$features"
    render_changelog_group "Fixes" "$fixes"
    render_changelog_group "Documentation" "$docs"
    render_changelog_group "Tests" "$tests"
    render_changelog_group "CI" "$ci"
    render_changelog_group "Maintenance" "$maintenance"
    render_changelog_group "Other Changes" "$other"

    if [[ -n "$existing_releases" ]]; then
      printf '%s\n' "$existing_releases"
    fi
  } >"$tmp"

  mv "$tmp" "$CHANGELOG_FILE"
}

increment_version() {
  local current="$1"
  local bump_kind="$2"
  local major minor patch

  IFS='.' read -r major minor patch <<<"$current"

  case "$bump_kind" in
    major)
      ((major += 1))
      minor=0
      patch=0
      ;;
    minor)
      ((minor += 1))
      patch=0
      ;;
    patch)
      ((patch += 1))
      ;;
    *)
      die "unsupported bump kind: $bump_kind"
      ;;
  esac

  printf '%s.%s.%s\n' "$major" "$minor" "$patch"
}

update_manifest_version() {
  local version="$1"
  local tmp

  tmp="$(mktemp)"

  awk -v version="$version" '
    BEGIN { in_package = 0; replaced = 0 }
    /^\[package\]$/ { in_package = 1 }
    /^\[/ && $0 != "[package]" && in_package { in_package = 0 }
    in_package && /^version = "/ && !replaced {
      print "version = \"" version "\""
      replaced = 1
      next
    }
    { print }
    END {
      if (!replaced) {
        exit 1
      }
    }
  ' "$RELEASE_MANIFEST" >"$tmp" || {
    rm -f "$tmp"
    die "failed to update version in $RELEASE_MANIFEST"
  }

  mv "$tmp" "$RELEASE_MANIFEST"
}

ensure_remote_exists() {
  git remote get-url "$REMOTE_NAME" >/dev/null 2>&1 || die "git remote '$REMOTE_NAME' is not configured"
}

ensure_tag_absent() {
  local tag_name="$1"

  git rev-parse --verify "refs/tags/$tag_name" >/dev/null 2>&1 &&
    die "tag '$tag_name' already exists locally"

  if git remote get-url "$REMOTE_NAME" >/dev/null 2>&1; then
    git ls-remote --exit-code --tags "$REMOTE_NAME" "refs/tags/$tag_name" >/dev/null 2>&1 &&
      die "tag '$tag_name' already exists on '$REMOTE_NAME'" || true
  fi
}

publish_crate() {
  cargo publish -p "$RELEASE_PACKAGE"
}

main() {
  local run_push=1
  local run_publish=0
  local version=""
  local bump_kind=""

  while (($# > 0)); do
    case "$1" in
      --major|--minor|--patch)
        [[ -z "$bump_kind" ]] || die "only one of --major, --minor, or --patch may be used"
        bump_kind="${1#--}"
        shift
        ;;
      --skip-push)
        run_push=0
        shift
        ;;
      --publish)
        run_publish=1
        shift
        ;;
      -h|--help)
        usage
        exit 0
        ;;
      -*)
        die "unknown option: $1"
        ;;
      *)
        [[ -z "$version" ]] || die "version may only be provided once"
        version="$1"
        shift
        ;;
    esac
  done

  if [[ -z "$version" && -z "$bump_kind" ]]; then
    usage
    exit 1
  fi

  [[ -z "$version" || -z "$bump_kind" ]] || die "pass either an explicit version or one bump flag"

  need_cmd awk
  need_cmd cargo
  need_cmd date
  need_cmd git
  need_cmd mktemp

  local repo_root
  repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" || die "must be run inside a git repository"
  cd "$repo_root"

  ensure_clean_worktree

  local old_version
  old_version="$(manifest_version)"
  [[ -n "$old_version" ]] || die "failed to read current release version"
  [[ "$old_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "current version must match x.y.z"

  if [[ -n "$bump_kind" ]]; then
    version="$(increment_version "$old_version" "$bump_kind")"
  fi

  [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "version must match x.y.z"
  [[ "$old_version" != "$version" ]] || die "version is already $version"

  local tag_name="v$version"
  local previous_tag
  local release_date

  if (( run_push )); then
    ensure_remote_exists
  fi

  ensure_tag_absent "$tag_name"

  previous_tag="$(previous_release_tag)"
  release_date="$(date +%Y-%m-%d)"

  update_manifest_version "$version"
  update_changelog "$version" "$release_date" "$previous_tag"

  cargo check --workspace --all-targets --quiet
  cargo fmt --all --check
  cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
  RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --locked
  cargo test --workspace --locked

  git add Cargo.lock "$RELEASE_MANIFEST" "$CHANGELOG_FILE"
  git commit -m "release: $tag_name"
  git tag -a "$tag_name" -m "release: $tag_name"

  if (( run_push )); then
    git push "$REMOTE_NAME" HEAD
    git push "$REMOTE_NAME" "$tag_name"
  fi

  if (( run_publish )); then
    publish_crate
  fi

  printf 'Released %s -> %s (%s)\n' "$old_version" "$version" "$tag_name"
}

main "$@"
