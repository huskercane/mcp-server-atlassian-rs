#!/usr/bin/env bash
# Refuse a release tag that disagrees with Cargo.toml.
#
# Run as a PreToolUse hook on Bash: reads the hook payload on stdin, and if the
# command creates a `vX.Y.Z` tag whose version differs from the crate version,
# denies the call. Releases used to carry three unrelated version numbers; the
# reported version now derives from Cargo.toml, and this keeps the tag honest.
#
# Exits 0 (allow) for anything that is not a tag-creating command, so it stays
# invisible during normal work.
set -uo pipefail

payload=$(cat)
command=$(printf '%s' "$payload" | jq -r '.tool_input.command // empty')
[ -n "$command" ] || exit 0

# Match only a real `git tag` invocation — at the start of the command or after
# a separator. Matching the raw text anywhere would block commands that merely
# *mention* tagging: writing these docs, grepping the log, a commit message.
if ! printf '%s' "$command" |
  grep -qE '(^|[;&|(]|&&|\|\|)[[:space:]]*git[[:space:]]+tag([[:space:]]|$)'; then
  exit 0
fi

# Listing, deleting, and verifying create nothing, so they need no check.
case "$command" in
*"git tag -l"* | *"git tag --list"* | *"git tag -d"* | *"git tag --delete"* | *"git tag -v"*)
  exit 0
  ;;
esac

# Take the first version *after* the invocation, so an unrelated version string
# earlier in the command line cannot be mistaken for the tag being created.
after=${command#*git tag}
tag=$(printf '%s' "$after" | grep -oE 'v[0-9]+\.[0-9]+\.[0-9]+' | head -1)
[ -n "$tag" ] || exit 0

repo_root=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
crate=$(grep -m1 '^version' "$repo_root/Cargo.toml" 2>/dev/null | sed -E 's/.*"(.*)".*/\1/')
[ -n "$crate" ] || exit 0

if [ "$tag" != "v$crate" ]; then
  jq -n --arg tag "$tag" --arg crate "$crate" '{
    hookSpecificOutput: {
      hookEventName: "PreToolUse",
      permissionDecision: "deny",
      permissionDecisionReason: ("Release tag \($tag) does not match Cargo.toml version \($crate). Bump `version` in Cargo.toml to \($tag|ltrimstr("v")), commit it, then tag — or tag v\($crate) instead. src/constants.rs::VERSION derives from Cargo.toml, so the tag is the only thing that can still drift.")
    }
  }'
fi
exit 0
