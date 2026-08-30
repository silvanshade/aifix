#!/bin/sh
# prepare-commit-msg: append the opaque Session trailer from the seat's env.
# No token in env -> no-op; commitlint then fails with the instructive message.
msg="$1"
[ -n "$GANDR_SESSION_TOKEN" ] || exit 0
grep -q '^Session:[ 	]' "$msg" && exit 0
# The trailer inserts after the last content line, so a comment/blank suffix
# (commit templates, status comments git strips at cleanup) never separates
# it from an existing trailer block. Trailer-shaped last lines take the
# Session line contiguously -- a blank inside the block would orphan the
# existing trailers from git's parser; the separator matches git's own
# horizontal-whitespace syntax (space or tab). Prose shaped like a trailer
# misclassifies toward the loud side: commitlint's trailer-leading-blank
# rule rejects the join and names the line.
lastno=$(grep -n -v -e '^#' -e '^[[:space:]]*$' "$msg" | tail -n 1 | cut -d: -f1)
if [ -z "$lastno" ]; then
  printf '\nSession: %s\n' "$GANDR_SESSION_TOKEN" >>"$msg"
  exit 0
fi
contig=0
sed -n "${lastno}p" "$msg" | grep -qE '^([A-Za-z][A-Za-z-]*|BREAKING CHANGE):[ 	]' && contig=1
# Temp file lives beside the message so the final mv is a same-filesystem
# atomic rename -- a cross-device fallback could leave the message truncated
# on a failed copy. The trap clears the temp on any exit.
tmp=$(mktemp "$(dirname "$msg")/.session-trailer.XXXXXX") || exit 1
trap 'rm -f "$tmp"' EXIT
awk -v n="$lastno" -v tok="$GANDR_SESSION_TOKEN" -v contig="$contig" '
  { print }
  NR == n {
    if (contig != 1) print ""
    print "Session: " tok
  }
' "$msg" >"$tmp" && mv "$tmp" "$msg"
