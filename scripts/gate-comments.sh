#!/bin/sh
# Comments describe code. They do not argue with it.
#
# Per file, comment lines may not exceed 20% of code lines. Two allowances: a `SAFETY:` block does
# not count, and every file gets FLOOR lines whatever its size.
#
# POSIX, and executed rather than sourced: CI's /bin/sh is dash.
set -eu

ROOT="${GATE_ROOT:-src tests}"
LIMIT="${GATE_COMMENT_PCT:-20}"
FLOOR="${GATE_COMMENT_FLOOR:-3}"

over=$(
  # Deliberately unquoted: ROOT may name more than one tree ("src tests").
  # shellcheck disable=SC2086
  find $ROOT -name '*.rs' -type f -not -path '*/target/*' -not -path '*/xtra/*' | sort |
    while IFS= read -r file; do
      awk -v file="$file" -v limit="$LIMIT" -v floor="$FLOOR" '
        # A run of comment lines is one block. Held until it ends, so that a `SAFETY:` anywhere
        # in it exempts the whole note rather than the single line carrying the word.
        function settle() {
          if (!safe) comments += held
          held = 0; safe = 0
        }
        { line = $0; sub(/^[ \t]+/, "", line) }
        line ~ /^\/\// {
          held++
          if (line ~ /SAFETY:/) safe = 1
          next
        }
        { settle() }
        line == "" { next }
        { code++ }
        END {
          settle()
          allowed = code * limit / 100
          if (allowed < floor) allowed = floor
          if (comments > allowed)
            printf "%s: %d comment lines against %d of code (%d allowed)\n",
                   file, comments, code, allowed
        }
      ' "$file"
    done
)

if [ -n "$over" ]; then
  echo "gate-comments: these say more about the code than the code does:" >&2
  printf '%s\n' "$over" | sed 's/^/  /' >&2
  echo "gate-comments: describe the block; cut the argument, the history and the persuasion" >&2
  exit 1
fi
echo "gate-comments: ok"
