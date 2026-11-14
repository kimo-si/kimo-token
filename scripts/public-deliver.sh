#!/usr/bin/env bash
# 检查待发布提交后，将 main 推送到本项目的公开仓库。
# Usage: scripts/public-deliver.sh [--dry-run]
set -euo pipefail
cd "$(dirname "$0")/.."

REMOTE="origin"
REPOSITORY="kimo-si/kimo-token"
BRANCH="main"
DRY_RUN=0
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    *) echo "usage: $0 [--dry-run]" >&2; exit 2 ;;
  esac
done

# 校验发布目标，防止本地仍指向旧仓库或其他仓库。
remote_url=$(git remote get-url "$REMOTE")
case "$remote_url" in
  "https://github.com/${REPOSITORY}.git"|"https://github.com/${REPOSITORY}"|"git@github.com:${REPOSITORY}.git") ;;
  *) echo "${REMOTE} must point to github.com/${REPOSITORY}" >&2; exit 1 ;;
esac

# 只发布当前 main 的已提交内容，防止漏掉本地改动。
[[ "$(git branch --show-current)" == "$BRANCH" ]] || {
  echo "switch to ${BRANCH} before publishing" >&2; exit 1;
}
[[ -z "$(git status --porcelain)" ]] || {
  echo "commit local changes before publishing" >&2; exit 1;
}

# 获取远程分支并检查快进关系；失败时中止，不覆盖已有提交。
remote_sha=$(git ls-remote --heads "$REMOTE" "refs/heads/${BRANCH}" | cut -f1)
scan_range="HEAD"
if [[ -n "$remote_sha" ]]; then
  git fetch "$REMOTE" "$BRANCH"
  git merge-base --is-ancestor "$remote_sha" HEAD || {
    echo "remote ${BRANCH} has changes; integrate them before publishing" >&2; exit 1;
  }
  scan_range="${remote_sha}..HEAD"
fi

# 检查此次推送携带的内容及作者信息。
python3 scripts/scan-public.py "$scan_range"
if [[ "$DRY_RUN" == 1 ]]; then
  echo "dry run: checks passed; nothing pushed"
  exit 0
fi

# 仅推送 main；发布标签由维护者单独推送，避免触发继承的旧标签。
git push "$REMOTE" "HEAD:refs/heads/${BRANCH}"
echo "delivered: ${REPOSITORY}/${BRANCH} = $(git rev-parse --short HEAD)"
