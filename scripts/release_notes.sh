#!/bin/bash
# Assemble release notes in two formats from hand-written, version-bound sources:
#
#   release-notes.txt  — plain text. Rendered by the Sparkle update dialog
#                        and embedded in appcast.xml, so it must stay
#                        markdown-free. (The legacy Tauri latest.json is a
#                        frozen file and does not carry these notes.) Credit
#                        for external PRs is written into the source by hand.
#   release-notes.md   — GitHub-flavored markdown for the GitHub release
#                        page. `release_credit.sh` appends the contributor
#                        line and GitHub's changelog tail in `publish`.
#
# Both bodies come from `release-notes/<tag>.{txt,md}`, written by hand before
# the tag is pushed. There is no generation step and no model: earlier versions
# asked DeepSeek to write these, which produced text that differed between the
# two formats and between a local preview and the CI run.
#
# The sources are bound to the tag by filename, and a missing one is a hard
# error rather than a fallback. That is the point of this design: the previous
# scheme read an unversioned `release-notes.override.{txt,md}`, so completing a
# release correctly left the current version's notes in the repository, armed
# to be published again by the next one. That misfire was caught by manual
# inspection before v1.16.0 and v1.17.0 and never by a mechanism.
#
# Input (env): GITHUB_REF_NAME (tag). No network: both outputs are the
# sources, copied.
set -euo pipefail

TAG="${GITHUB_REF_NAME:?GITHUB_REF_NAME (tag) required}"
TXT="release-notes.txt"
MD="release-notes.md"

SRC_TXT="release-notes/${TAG}.txt"
SRC_MD="release-notes/${TAG}.md"

# --- Version-bound sources, required. ----------------------------------------
# -s, not -f: an empty file would pass an existence check and then ship. The
# appcast renderer skips its description when the text is empty (its own -s
# guard), so the release would complete with nothing to show for it.
MISSING=()
[[ -s "$SRC_TXT" ]] || MISSING+=("$SRC_TXT")
[[ -s "$SRC_MD" ]] || MISSING+=("$SRC_MD")
if (( ${#MISSING[@]} > 0 )); then
  {
    printf 'error: no release notes for %s\n' "$TAG"
    printf 'Write these before tagging (missing, or present but empty):\n'
    printf '  %s\n' "${MISSING[@]}"
    printf 'Notes are hand-written and named after the tag; there is no fallback,\n'
    printf 'so a release cannot inherit the previous version'"'"'s text.\n'
  } >&2
  exit 1
fi

cp "$SRC_TXT" "$TXT"
cp "$SRC_MD" "$MD"

echo "----- $TXT -----"
cat "$TXT"
echo "----- $MD -----"
cat "$MD"
