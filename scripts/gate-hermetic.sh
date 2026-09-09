#!/bin/sh
# The suite leaves nothing behind, and reads nothing that belongs to the machine.
#
# It used to leave a great deal: every test tidied up on its last line, and `assert!` unwinds
# straight past a trailing `remove_dir_all`. So a *failing* test always leaked, and the
# delete-then-create helpers only ever revisited their own name under their own pid, which never
# repeats. Thousands of directories accumulated across two renames of this project and nothing
# said so, because nothing looked.
#
# **The first version of this gate could only see `$TMPDIR`, which is not where casper writes.**
# The shipped `tools.lua` keeps the `shell` working directory under `$XDG_RUNTIME_DIR/casper/cwd`
# and falls back to a literal `/tmp/casper` when the variable is unset — neither of which moves
# when `TMPDIR` does. A test that wrote either was invisible here, and one was: `tests/settings.rs`
# ran the `pwd` tool against whatever the person running it had in `/run/user/<uid>`, so it was
# reporting on the machine and would have gone on passing with the path spelled any way at all.
#
# So there are three checks now, and each covers a different way out:
#
#   1. Every directory casper finds through the environment is replaced with an empty one of this
#      run's own, and each must still be empty afterwards. That covers `$TMPDIR` as before and
#      `$XDG_RUNTIME_DIR`, `$XDG_CONFIG_HOME` and `$XDG_DATA_HOME` besides.
#   2. `$XDG_CONFIG_HOME` being empty is a check in itself, not only a place to catch writes: a
#      test that needed the *installed* declarations to be there fails outright rather than
#      quietly grading this machine's `~/.config/casper` instead of the repository's `config/`.
#   3. The literal paths casper can still reach with the environment saying nothing are named and
#      watched by name, before and after. A hardcoded `/tmp/casper` does not move when `TMPDIR`
#      does, and a test that clears the variable itself falls through to exactly these.
#
# `$HOME` is deliberately left alone. cargo reads its registry out of it, and a gate that had to
# rebuild the world to run would not be run.
set -eu

# **Short, and rooted at `/tmp` rather than under whatever `$TMPDIR` already is.** A unix socket
# path may not exceed `SUN_LEN` — 108 bytes — and a nested root is how a sibling's suite came to
# fail with "path must be shorter than SUN_LEN" in the one place this gate was supposed to be
# proving something. casper binds no socket today; the short root costs nothing and the day it
# does is not the day to find out.
#
# `GATE_BASE` because `/tmp` is not always available to write in. This machine's is shared and
# under a per-user quota that other work fills, and a gate that cannot run is a gate that is not
# run. Isolation comes from the directory being ours, not from where it hangs — so an override
# gives up nothing, as long as it is short.
base="${GATE_BASE:-/tmp}"
[ -d "$base" ] && [ -w "$base" ] || base="${TMPDIR:-.}"
root=$(mktemp -d "$base/gh-XXXXXX")

# Kept rather than discarded. When this fails it is a test failing, not a leak, and the name of
# the test is the whole answer — a gate that printed only "exit 101" sent the reader back to
# `cargo test` to find out what it already knew.
out=$(mktemp "$base/gh-log-XXXXXX")

# The stamp the literal paths are compared against. Made before the suite runs, so anything the
# suite creates or rewrites under one of them is newer than it. `-newer` is POSIX; `find -printf`
# is not, which is why this is a stamp rather than a listing of sizes and times.
stamp=$(mktemp "$base/gh-stamp-XXXXXX")

trap 'rm -rf "$root" "$out" "$stamp"' EXIT HUP INT TERM

# The directories the suite is given, each empty and each its own. Named separately because the
# report has to say *which* one something was left in: "$TMPDIR is dirty" and "the runtime
# directory is dirty" are different bugs with different fixes.
for kind in tmp runtime config data; do
  mkdir -p "$root/$kind"
done

# The literal paths casper reaches when nothing in the environment points it elsewhere. Watched by
# exact name rather than by watching `/tmp` as a whole: a shared machine's `/tmp` has tens of
# thousands of entries in it from other work, and a gate that reported those would be switched off
# inside a week. These are the paths the shipped declarations actually name.
literal="/tmp/casper /var/tmp/casper ${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/casper"
literal="$literal ${HOME:-/nonexistent}/.config/casper ${HOME:-/nonexistent}/.local/share/casper"

if ! TMPDIR="$root/tmp" \
     XDG_RUNTIME_DIR="$root/runtime" \
     XDG_CONFIG_HOME="$root/config" \
     XDG_DATA_HOME="$root/data" \
     cargo test --all-targets --quiet >"$out" 2>&1; then
  cat "$out" >&2
  echo "gate-hermetic: the suite failed; nothing was checked" >&2
  echo "gate-hermetic: if it passes outside this gate, it is reading something it was given" >&2
  echo "gate-hermetic: here — an installed \`tools.lua\`, a runtime directory, a home" >&2
  exit 1
fi

failed=0

# Nothing here is entitled to leave anything: casper has no temporary directory of its own, and
# its allowlist is empty on purpose. If this starts failing, something new started writing to one
# of these and the question is whether it should.
for kind in tmp runtime config data; do
  left=$(ls -A "$root/$kind" || true)
  [ -n "$left" ] || continue
  case "$kind" in
    tmp) echo "gate-hermetic: the suite left these in its \$TMPDIR:" >&2 ;;
    runtime) echo "gate-hermetic: the suite left these in \$XDG_RUNTIME_DIR:" >&2 ;;
    config) echo "gate-hermetic: the suite wrote these into \$XDG_CONFIG_HOME:" >&2 ;;
    data) echo "gate-hermetic: the suite wrote these into \$XDG_DATA_HOME:" >&2 ;;
  esac
  # shellcheck disable=SC2086
  printf '  %s\n' $left >&2
  failed=1
done

# And the paths no variable points at. A test that hardcodes one of these, or that clears the
# variable and lets the declarations fall through to it, writes somewhere none of the above can
# see — which is the hole this half of the gate exists to close.
for path in $literal; do
  [ -e "$path" ] || continue
  touched=$(find "$path" -newer "$stamp" 2>/dev/null || true)
  [ -n "$touched" ] || continue
  echo "gate-hermetic: the suite reached a path outside what it was given:" >&2
  printf '%s\n' "$touched" | sed 's/^/  /' >&2
  echo "gate-hermetic: that is a person's own directory, not this run's" >&2
  failed=1
done

if [ "$failed" -ne 0 ]; then
  echo "gate-hermetic: failed" >&2
  exit 1
fi
echo "gate-hermetic: ok"
