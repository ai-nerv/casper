#!/bin/sh
# casper's own process writes one file and removes nothing.
#
# balthasar's `gate-no-delete.sh` says nothing a memory holds is ever deleted, against a table
# list. There is no store here and no port of that gate would mean anything — but the commitment
# underneath it does translate, and it translates into something wider than deletion.
#
# **casper is the program that runs other people's commands.** What a *tool* does to the
# filesystem is the point of the program and is the harness's business to allow. What the casper
# process does on its own account is a different question, and the answer today is: it appends to
# a log nobody turned on, and it writes one manifest. That is worth freezing, because the Lua half
# of the same boundary already is — `gate-sandbox.sh` holds `os.remove`, `os.rename` and
# `os.tmpname` out of every declaration's VM, so a declaration cannot unlink a file and the Rust
# underneath it could, with nothing to say so.
#
# The shape that gets past a reviewer is a housekeeping feature: pruning stale packages, clearing
# the remembered working directory, rotating a log, tidying a cache. Each is reasonable on its own
# and each makes casper a program that deletes your files.
#
# Three files, each with its own reason:
#
#   src/acknowledged.rs   the manifest, under the configuration directory. The one durable thing
#                         casper decides to write, and `casper acknowledge` is the verb for it.
#   src/noted.rs          `$CASPER_DEBUG_LOG`, appended to only when somebody set the variable.
#   src/scratch.rs        the test helper. In the library rather than a testkit because casper is
#                         one crate; it creates and removes its own directory and nothing else.
#
# And removal is narrower than writing: only the scratch helper may unlink, so a delete added to
# one of the other two is a finding even though that file may write.
#
# POSIX for the same reason the others are: /bin/sh on the runner is dash.
set -eu
ROOT="${GATE_ROOT:-src}"

# May write. Adding to this list is a deliberate act; the point is that it cannot happen by
# accident.
WRITERS='src/acknowledged.rs src/noted.rs src/scratch.rs'

# May remove. One file, and it is the one that owns what it removes.
REMOVERS='src/scratch.rs'

# What counts as reaching for the filesystem to change it. `File::create` and `OpenOptions` are
# here because both truncate or create without the word `write` appearing anywhere.
WRITING='fs::(write|create_dir|create_dir_all|copy|rename|hard_link|soft_link)|File::create|OpenOptions'
REMOVING='fs::(remove_file|remove_dir|remove_dir_all)'

fail=0

# Comments stripped, and `#[cfg(test)]` items with them: a module documenting the rule by naming
# the call is describing it, not breaking it, and a test may reach for anything. The same reading
# `gate-independent.sh` and `gate-spawn.sh` already use.
without_tests() {
  awk '
    /^[ \t]*#\[cfg\(test\)\]/ { skipping = 1; depth = 0; started = 0 }
    skipping {
      n = gsub(/\{/, "{"); depth += n
      n = gsub(/\}/, "}"); depth -= n
      if (n > 0 || depth > 0) started = 1
      if (started && depth <= 0) skipping = 0
      next
    }
    { sub(/\/\/.*$/, ""); print }
  ' "$1"
}

# Everything matching `$2`, outside the files listed in `$1`, one `file:line: text` per hit.
found_outside() {
  allowed=$1
  pattern=$2
  find "$ROOT" -name '*.rs' -not -path '*/target/*' | sort | while IFS= read -r file; do
    for ok in $allowed; do
      [ "$file" = "$ok" ] && continue 2
    done
    hit=$(without_tests "$file" | grep -nE "$pattern" || true)
    [ -n "$hit" ] || continue
    printf '%s\n' "$hit" | sed "s|^|$file:|"
  done
}

writes=$(found_outside "$WRITERS" "$WRITING")
if [ -n "$writes" ]; then
  echo "gate-writes: casper writes to the filesystem somewhere new:" >&2
  printf '%s\n' "$writes" | sed 's/^/  /' >&2
  echo "gate-writes: a tool may write whatever it was told to; the casper process writes a" >&2
  echo "gate-writes: manifest and a log nobody turned on. See the head of this script." >&2
  fail=1
fi

removes=$(found_outside "$REMOVERS" "$REMOVING")
if [ -n "$removes" ]; then
  echo "gate-writes: casper removes something, and only its own scratch may:" >&2
  printf '%s\n' "$removes" | sed 's/^/  /' >&2
  echo "gate-writes: a declaration cannot unlink a file — os.remove is out of the VM — and" >&2
  echo "gate-writes: the Rust under it should not be the way round that." >&2
  fail=1
fi

# The allowlist is a list of files, so a file that stopped existing would silently widen it.
for file in $WRITERS $REMOVERS; do
  if [ ! -f "$file" ]; then
    echo "gate-writes: '$file' is on the allowlist and is not there" >&2
    fail=1
  fi
done

[ "$fail" -eq 0 ] || { echo "gate-writes: failed" >&2; exit 1; }
echo "gate-writes: ok"
