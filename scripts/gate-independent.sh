#!/bin/sh
# casper's Rust knows nothing about its siblings.
#
# The four programs in this family talk over pipes and sockets, in a shape `FAMILY.md` writes
# down and `gate-family.sh` holds each binary to. The moment one of them has a Rust dependency on
# another they are one program in two repositories, the contract stops being the interface, and
# "four independent programs that agree on a wire" becomes marketing. It is also how the wire
# types came to be duplicated on purpose rather than shared — see `gate-wire.sh`.
#
# The prose is a different matter and is left alone. `DESIGN.md` and half the module
# documentation here explain casper by contrast with magi and melchior, and they should: naming
# your counterpart is how a boundary gets described. Comments are stripped before this looks, and
# `#[cfg(test)]` items go the same way — a fixture may hold a refusal message that quotes a
# sibling's name, and `src/surface/asking.rs` does.
#
# POSIX for the same reason the others are: /bin/sh on the runner is dash.
set -eu
ROOT="${GATE_ROOT:-src tests}"

# The siblings, by the name their repository and binary carry.
SIBLINGS='magi melchior balthasar'

fail=0

# ---- no Rust file names one ------------------------------------------------------------------
# A leading word boundary and no trailing one: a bare substring reports prose as a dependency,
# and a full `-w` match stops catching `magi_proto`, which is the shape this is actually for.
found=$(
  # shellcheck disable=SC2086
  find $ROOT -name '*.rs' -not -path '*/target/*' 2>/dev/null | sort | while IFS= read -r file; do
    hit=$(awk '
      /^[ \t]*#\[cfg\(test\)\]/ { skipping = 1; depth = 0; started = 0 }
      skipping {
        n = gsub(/\{/, "{"); depth += n
        n = gsub(/\}/, "}"); depth -= n
        if (n > 0 || depth > 0) started = 1
        if (started && depth <= 0) skipping = 0
        next
      }
      { sub(/\/\/.*$/, ""); print }
    ' "$file" | grep -nE "\\b(magi|melchior|balthasar)" || true)
    [ -n "$hit" ] || continue
    printf '%s: %s\n' "$file" "$(printf '%s' "$hit" | head -1)"
  done
)
if [ -n "$found" ]; then
  echo "gate-independent: casper's Rust names a sibling:" >&2
  printf '%s\n' "$found" | sed 's/^/  /' >&2
  echo "gate-independent: the family agrees on a wire, not on a crate. See FAMILY.md" >&2
  fail=1
fi

# ---- and the manifest declares none of them ---------------------------------------------------
for name in $SIBLINGS; do
  if grep -qE "^[ \t]*$name[ \t]*=" Cargo.toml; then
    echo "gate-independent: Cargo.toml depends on $name" >&2
    fail=1
  fi
done

# A local path dependency is the other way in, and it does not have to use a sibling's name to be
# one. casper is a single crate with no workspace under it, so there is nothing on disk it has
# any business depending on: every `path =` here would be pointing out of the repository.
# `"src/` with the quote against it, so the crate's own `[lib]` and `[[bin]]` are not a finding
# and `"../magi/src/…"` still is.
paths=$(grep -n 'path[ \t]*=[ \t]*"' Cargo.toml | grep -v '"src/' || true)
if [ -n "$paths" ]; then
  echo "gate-independent: Cargo.toml has a path dependency, which can only lead out of the repo:" >&2
  printf '%s\n' "$paths" | sed 's/^/  /' >&2
  fail=1
fi

[ "$fail" -eq 0 ] || { echo "gate-independent: failed" >&2; exit 1; }
echo "gate-independent: ok"
