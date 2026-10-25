#!/bin/bash
# Append GitHub's contributor credit and changelog tail to a release body:
#
#   Contributors: @a, @b          plain mentions, which is what fills the
#                                 Contributors avatars on the release page
#                                 (the hand-written `[@a](url)` links do not)
#   ## New Contributors …         GitHub's own tail, verbatim, ending with
#   **Full Changelog**: …/compare/<prev>...<tag>
#
# Runs in `publish`, the only job with `contents: write`, which
# `releases/generate-notes` requires. Every failure is fatal and stops the
# release before it is created: v2.2.0 through v2.10.0 ran this call under a
# read-only token with its errors discarded, and eleven releases shipped with
# no range and no credit.
#
# Usage: release_credit.sh <release-notes.md>
# Env: GITHUB_REF_NAME (tag), GITHUB_REPOSITORY (owner/repo), GH_TOKEN.
# Needs the tag history (a checkout with fetch-depth: 0).
set -euo pipefail

MD="${1:?release-notes.md path required}"
TAG="${GITHUB_REF_NAME:?GITHUB_REF_NAME (tag) required}"
REPO="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY (owner/repo) required}"
: "${GH_TOKEN:?GH_TOKEN required}"
[[ -s "$MD" ]] || { echo "error: $MD is missing or empty" >&2; exit 1; }
OWNER="${REPO%%/*}"

# Named explicitly: left out, GitHub picks the previous release itself.
PREV="$(git describe --tags --abbrev=0 "${TAG}^")"
AUTO="$(gh api "repos/${REPO}/releases/generate-notes" \
  -f tag_name="$TAG" -f previous_tag_name="$PREV" --jq .body)"

TAIL="$(printf '%s\n' "$AUTO" | awk '/^## New Contributors/{f=1} f')"
[[ -n "$TAIL" ]] || TAIL="$(printf '%s\n' "$AUTO" | grep '^\*\*Full Changelog' || true)"
grep -q '^\*\*Full Changelog' <<< "$TAIL" || {
  echo "error: generate-notes returned no Full Changelog line for $PREV...$TAG" >&2
  exit 1
}

# PR authors ("by @login" in GitHub's notes) plus everyone the hand-written
# body thanks as `[@login](url)`, e.g. the reporter of a fixed issue; the
# owner and bots (GitHub writes them as `@name[bot]`) excluded.
CONTRIB="$( { printf '%s\n' "$AUTO" | grep -oE 'by @[A-Za-z0-9_-]+(\[bot\])?' \
    | grep -v '\[bot\]$' | sed 's/^by //'
  grep -oE '\[@[A-Za-z0-9_-]+\]' "$MD" | tr -d '[]'; } \
  | sort -fu | grep -vix "@${OWNER}" || true)"

{
  if [[ -n "$CONTRIB" ]]; then
    printf '\nContributors: %s\n' "$(printf '%s\n' "$CONTRIB" | paste -sd, - | sed 's/,/, /g')"
  fi
  printf '\n%s\n' "$TAIL"
} >> "$MD"

echo "previous tag: $PREV"
cat "$MD"
