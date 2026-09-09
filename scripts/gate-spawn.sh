#!/bin/sh
# Every place casper starts a program is named here, and every one of them ties what it starts.
#
# casper's whole job is running programs, so it cannot have balthasar's gate — the one that says
# nothing may exec. What it can have is the inverse: exec is allowed in exactly two files, and
# both of them hand the command to `crate::tied` before it runs.
#
# **The failure this exists for is silent and expensive.** `PR_SET_PDEATHSIG` is cleared across
# `fork`, so a spawn that skips `tied` produces a process that outlives the magi that asked for
# it — a build, a fetch, an editor, still holding what it held, reparented to init. Nothing goes
# red. It presents days later as a machine with work on it nobody started, and the third file
# that spawns is exactly how it comes back: somebody adds a `Command::new` somewhere reasonable,
# and there is no reason for them to know about a two-function module called `tied`.
#
# It also holds the `unsafe` down. `unsafe_code` is denied across the crate and `src/tied.rs`
# carries the only `#[allow]`s, one per `pre_exec`, each with a `SAFETY:` note saying why its
# body is async-signal-safe. A third `pre_exec` with no note, or an `#[allow]` that drifted away
# from the thing it excuses, is the shape that turns "one audited exception" into "some unsafe".
#
# POSIX for the same reason the others are: /bin/sh on the runner is dash.
set -eu
ROOT="${GATE_ROOT:-src}"

# The two doors out, and what each is for. Adding to this list is a deliberate act; the point is
# that it cannot happen by accident.
#
#   src/lua/exec.rs   `casper.exec`, the one way a declaration runs anything
#   src/pty.rs        a `screen` tool's program, on a pty of its own
SPAWNS='src/lua/exec.rs src/pty.rs'

# The module that arms them. It names `Command` in a signature and spawns nothing itself.
TIED='src/tied.rs'

fail=0

# ---- nothing else opens a door ---------------------------------------------------------------
# Comments stripped first, and `#[cfg(test)]` items with them: a module that documents `tied` by
# naming `Command::new` is describing the rule, not breaking it, and a test may reach for
# anything. Both are the reading `gate-cycles.sh` already uses, for the same reason.
found=$(
  find "$ROOT" -name '*.rs' -not -path '*/target/*' | sort | while IFS= read -r file; do
    case " $SPAWNS $TIED " in *" $file "*) continue ;; esac
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
    ' "$file" | grep -n 'Command::new\|process::Command\|pre_exec' || true)
    [ -n "$hit" ] || continue
    printf '%s: %s\n' "$file" "$(printf '%s' "$hit" | head -1)"
  done
)
if [ -n "$found" ]; then
  echo "gate-spawn: something outside the two named doors starts a program:" >&2
  printf '%s\n' "$found" | sed 's/^/  /' >&2
  echo "gate-spawn: a spawn that skips \`crate::tied\` outlives the magi that asked for it" >&2
  fail=1
fi

# ---- and both doors tie what they start -------------------------------------------------------
for file in $SPAWNS; do
  if [ ! -f "$ROOT/${file#src/}" ] && [ ! -f "$file" ]; then
    echo "gate-spawn: $file is named here and is not there any more" >&2
    fail=1
    continue
  fi
  # Comments stripped. The first version of this grepped the file as written, and commenting the
  # call out was the way it was falsified — a one-character edit that leaves every spawn untied
  # and the gate green. The mention in the note above the call is not the call.
  if ! awk '{ sub(/\/\/.*$/, ""); print }' "$file" | grep -q 'tied::'; then
    echo "gate-spawn: $file starts a program and does not hand it to \`crate::tied\`" >&2
    fail=1
  fi
done

# ---- the tie is still the two syscalls it claims to be ----------------------------------------
# Named rather than assumed. Both landed as fixes for a leak that had already happened twice:
# the death signal is what ends the program when casper goes, and the process group is the
# handle a wrapper needs to take its own children with it. A `tied` that had quietly lost one
# would still compile and every spawn site would still call it.
#
# The open bracket is part of the pattern, and comments are stripped: this was a bare substring
# match, and renaming the call to `setpgid_REMOVED` left the gate green — a substring is still
# there in a symbol that no longer exists, and so is one in the paragraph explaining it.
for symbol in set_parent_process_death_signal setpgid; do
  if ! awk '{ sub(/\/\/.*$/, ""); print }' "$TIED" | grep -q "$symbol("; then
    echo "gate-spawn: $TIED no longer calls $symbol; the tie it advertises is not the tie" >&2
    fail=1
  fi
done

# ---- one `pre_exec`, one `#[allow]`, one `SAFETY:` --------------------------------------------
# `\.pre_exec(` and not the bare word: the prose in that module discusses `pre_exec` repeatedly,
# and a gate that counted the discussion would fire on an edit to a comment.
elsewhere=$(grep -rln '\.pre_exec(' "$ROOT" | grep -v "^$TIED$" || true)
if [ -n "$elsewhere" ]; then
  echo "gate-spawn: \`pre_exec\` runs between a fork and an exec and belongs in $TIED only:" >&2
  printf '%s\n' "$elsewhere" | sed 's/^/  /' >&2
  fail=1
fi

calls=$(grep -c '\.pre_exec(' "$TIED" || true)
allows=$(grep -c '^#\[allow(unsafe_code)\]' "$TIED" || true)
notes=$(grep -c '^// SAFETY:' "$TIED" || true)
if [ "$calls" -ne "$allows" ] || [ "$calls" -ne "$notes" ]; then
  echo "gate-spawn: $TIED has $calls pre_exec, $allows allow(unsafe_code), $notes SAFETY notes" >&2
  echo "gate-spawn: each of those windows is one audited exception, and it is audited by the note" >&2
  fail=1
fi

# The `#[allow]`s only mean something while the lint is denied.
if ! grep -q 'unsafe_code = "deny"' Cargo.toml; then
  echo "gate-spawn: unsafe_code is no longer denied, so the allows in $TIED excuse nothing" >&2
  fail=1
fi

[ "$fail" -eq 0 ] || { echo "gate-spawn: failed" >&2; exit 1; }
echo "gate-spawn: ok"
