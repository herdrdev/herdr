#!/usr/bin/env bash
# scripts/upstream-sync.sh
# Routine for synchronizing Shepherd with upstream herdr releases.
#
# Usage:
#   ./scripts/upstream-sync.sh [options] <tag-or-commit>
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

DRY_RUN=0
SKIP_BUILD=0
SKIP_TESTS=0
ALLOW_DIRTY=0
ALLOW_BRANCH=0
UPSTREAM_REMOTE="upstream"
TARGET=""

usage() {
    cat <<EOF
Usage: $0 [options] <tag-or-commit>

Routine for synchronizing Shepherd with new upstream herdr versions.

Arguments:
  <tag-or-commit>      Upstream git tag (e.g., v0.9.6) or commit hash to merge

Options:
  --dry-run            Print the sync steps without executing them
  --skip-build         Skip the cargo build step
  --skip-tests         Skip running the cargo test suite
  --allow-dirty        Bypass dirty working tree check (use with caution)
  --allow-branch       Bypass 'shepherd' branch check (useful for testing)
  --upstream <remote>  Remote name for upstream (default: 'upstream')
  -h, --help           Show this help message

Steps performed:
  1. Verify clean working tree and branch 'shepherd'
  2. Create temporary branch 'sync/<tag>'
  3. Fetch from upstream remote and merge <tag-or-commit>
  4. Run 'scripts/rename-from-herdr.sh' and verify with '--check'
  5. Run './cargo-fork.sh build' and test suite by area
  6. Leave the branch ready without push or merge to 'shepherd'
EOF
    exit 1
}

# Parse options
while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run)
            DRY_RUN=1
            shift
            ;;
        --skip-build)
            SKIP_BUILD=1
            shift
            ;;
        --skip-tests)
            SKIP_TESTS=1
            shift
            ;;
        --allow-dirty)
            ALLOW_DIRTY=1
            shift
            ;;
        --allow-branch)
            ALLOW_BRANCH=1
            shift
            ;;
        --upstream)
            if [ -z "${2:-}" ]; then
                echo "Error: --upstream requires an argument" >&2
                exit 1
            fi
            UPSTREAM_REMOTE="$2"
            shift 2
            ;;
        -h|--help)
            usage
            ;;
        -*)
            echo "Error: Unknown option: $1" >&2
            usage
            ;;
        *)
            if [ -n "$TARGET" ]; then
                echo "Error: Multiple targets specified: $TARGET and $1" >&2
                usage
            fi
            TARGET="$1"
            shift
            ;;
    esac
done

if [ -z "$TARGET" ]; then
    echo "Error: <tag-or-commit> argument is required." >&2
    usage
fi

CLEAN_TAG="${TARGET#refs/tags/}"
SYNC_BRANCH="sync/${CLEAN_TAG}"

if [ "$DRY_RUN" -eq 1 ]; then
    CURRENT_BRANCH="$(git branch --show-current 2>/dev/null || echo "unknown")"
    echo "=== [DRY RUN] Upstream Sync Routine for Shepherd ==="
    echo "Target upstream ref: $TARGET"
    echo "Upstream remote:     $UPSTREAM_REMOTE"
    echo "Sync branch to create: $SYNC_BRANCH"
    echo ""
    echo "Steps that would be executed:"
    echo "  (a) Check working tree is clean and current branch is 'shepherd' (current: '$CURRENT_BRANCH')"
    echo "  (b) Create and switch to temporary branch: git checkout -b \"$SYNC_BRANCH\""
    echo "  (c) Fetch upstream: git fetch \"$UPSTREAM_REMOTE\""
    echo "      Merge target:   git merge \"$TARGET\""
    echo "  (d) Re-apply automated rename: ./scripts/rename-from-herdr.sh"
    echo "      Verify rename check:       ./scripts/rename-from-herdr.sh --check"
    if [ "$SKIP_BUILD" -eq 0 ]; then
        echo "  (e) Build project:             ./cargo-fork.sh build"
    else
        echo "  (e) Build project:             [SKIPPED via --skip-build]"
    fi
    if [ "$SKIP_TESTS" -eq 0 ]; then
        echo "      Run test battery by area:  ./cargo-fork.sh test -p shepherd-core"
        echo "                                 ./cargo-fork.sh test client::shell"
        echo "                                 ./cargo-fork.sh test shepherd::"
        echo "                                 ./cargo-fork.sh test protocol::"
        echo "                                 ./cargo-fork.sh test api::"
        echo "                                 ./cargo-fork.sh test server::"
    else
        echo "      Run test battery by area:  [SKIPPED via --skip-tests]"
    fi
    echo "  (f) Leave '$SYNC_BRANCH' ready WITHOUT pushing to any remote and WITHOUT merging into 'shepherd'"
    echo "=== [DRY RUN END] ==="
    exit 0
fi

echo "==> Step (a): Verifying repository state..."
CURRENT_BRANCH="$(git branch --show-current 2>/dev/null || echo "")"
if [ "$CURRENT_BRANCH" != "shepherd" ] && [ "$ALLOW_BRANCH" -eq 0 ]; then
    echo "Error: You must be on the 'shepherd' branch to start a sync (current: '$CURRENT_BRANCH')." >&2
    echo "Pass --allow-branch to bypass this check for testing." >&2
    exit 1
fi

if [ "$ALLOW_DIRTY" -eq 0 ]; then
    if ! git diff --quiet || ! git diff --cached --quiet || [ -n "$(git status --porcelain)" ]; then
        echo "Error: Working tree is dirty. Please commit or stash your changes before syncing." >&2
        git status -s >&2
        exit 1
    fi
fi

echo "==> Step (b): Creating temporary sync branch '$SYNC_BRANCH'..."
if git show-ref --verify --quiet "refs/heads/$SYNC_BRANCH"; then
    echo "Warning: Branch '$SYNC_BRANCH' already exists. Switching to it."
    git checkout "$SYNC_BRANCH"
else
    git checkout -b "$SYNC_BRANCH"
fi

echo "==> Step (c): Fetching from remote '$UPSTREAM_REMOTE'..."
if ! git remote get-url "$UPSTREAM_REMOTE" >/dev/null 2>&1; then
    echo "Error: Remote '$UPSTREAM_REMOTE' does not exist." >&2
    exit 1
fi

git fetch "$UPSTREAM_REMOTE"

echo "==> Merging '$TARGET' into '$SYNC_BRANCH'..."
MERGE_FAILED=0
if ! git merge "$TARGET" --no-edit; then
    MERGE_FAILED=1
fi

if [ "$MERGE_FAILED" -eq 1 ]; then
    CONFLICTS="$(git diff --name-only --diff-filter=U || true)"
    echo ""
    echo "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!"
    echo "Merge conflicts detected!"
    echo "Conflicted files:"
    echo "$CONFLICTS"
    echo ""
    echo "Next steps:"
    echo "1. Resolve the conflicts above manually."
    echo "2. Stage the resolved files with 'git add <files>'."
    echo "3. Complete the merge with 'git commit'."
    echo "4. Run './scripts/rename-from-herdr.sh' and './scripts/rename-from-herdr.sh --check'."
    echo "5. Run './cargo-fork.sh build' and the test suite."
    echo "6. See docs/UPSTREAM-SYNC.md for guidance on common conflicts."
    echo "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!"
    exit 2
fi

echo "==> Step (d): Re-running automated rename script..."
if [ -x "./scripts/rename-from-herdr.sh" ]; then
    ./scripts/rename-from-herdr.sh
    echo "==> Checking rename cleanliness..."
    ./scripts/rename-from-herdr.sh --check
else
    echo "Warning: ./scripts/rename-from-herdr.sh not found or not executable. Skipping."
fi

# If changes were made by the rename script, stage and commit them on the sync branch
if [ -n "$(git status --porcelain)" ]; then
    echo "==> Committing automated rename adaptations to '$SYNC_BRANCH'..."
    git add -A
    git commit -m "chore(sync): automated rename adaptations for $TARGET"
fi

echo "==> Step (e): Building and testing..."
CARGO_CMD="./cargo-fork.sh"
if [ ! -x "$CARGO_CMD" ]; then
    if command -v cargo >/dev/null 2>&1; then
        CARGO_CMD="cargo"
    else
        echo "Notice: Neither ./cargo-fork.sh nor cargo found. Skipping build and test."
        SKIP_BUILD=1
        SKIP_TESTS=1
    fi
fi

if [ "$SKIP_BUILD" -eq 0 ]; then
    echo "--> Running build..."
    "$CARGO_CMD" build
fi

if [ "$SKIP_TESTS" -eq 0 ]; then
    echo "--> Running tests by area..."
    "$CARGO_CMD" test -p shepherd-core
    "$CARGO_CMD" test client::shell
    "$CARGO_CMD" test shepherd::
    "$CARGO_CMD" test protocol::
    "$CARGO_CMD" test api::
    "$CARGO_CMD" test server::
fi

echo ""
echo "======================================================================="
echo "==> Step (f): Sync completed successfully on branch '$SYNC_BRANCH'!"
echo ""
echo "Branch '$SYNC_BRANCH' is ready for review."
echo "CRITICAL SAFETY REMINDER:"
echo "- Do NOT push to remote (upstream push is disabled and shepherd is local)."
echo "- Do NOT merge directly into 'shepherd' without human review."
echo "- Inspect git log and diff with: git log -p shepherd..$SYNC_BRANCH"
echo "======================================================================="
