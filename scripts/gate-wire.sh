#!/bin/sh
# The family speaks one way, and a sixth wire cannot invent a sixth spelling.
#
# Five wires had grown five ways to say the same thing: `say`/`heard` on melchior's pipe,
# `to`/`from` on the surface pipe, `message` on the tool-peer pipe, `said` on the streamed
# answer, and a call envelope on the sockets. Nothing anywhere said which was meant, and nothing
# could: each was internally consistent, and each was written by somebody who had not read the
# other four.
#
# **The failure is silent.** Two of those wires exist as byte-identical copies in two
# repositories -- the surface frames, and the streamed answer -- so when two spellings drift
# nothing fails and no test goes red. The surface simply stops being answered, and it presents
# much later as "that tool never draws".
#
# Two shapes only. A **call** is answered and carries a reply; an **event** is not. Every enum
# whose variants cross a process boundary is tagged `event`. The rule is written out in this
# repository's wire module.
#
# A **data** discriminator is a different thing: a tagged enum nested *inside* a message, which
# names a shape rather than an event. Those are allowed their own key, and each is listed below
# with why. The list is the point -- adding to it is a deliberate act, and a new wire that
# invents a tag is not on it.
#
# POSIX for the same reason the others are: /bin/sh on the runner is dash.
set -eu
ROOT="${GATE_ROOT:-src}"

# Data discriminators, and what each names. Nested inside a message; never a frame of its own.
allowed() {
  case "$1" in
    # a transport, a scope, an action, a body, an endpoint: shapes, named where they are used
    kind) return 0 ;;
    # a provider's own content blocks, which are not this family's to name
    type) return 0 ;;
    # how a tool result is drawn, carried inside the result
    shown) return 0 ;;
    # what a surface was told, carried inside an `answer` event
    answer) return 0 ;;
    # the journal on disk. A file is not a wire, and its records outlive every protocol here
    record) return 0 ;;
    # magi's own UI-to-daemon protocol: CBOR in an envelope, two halves of one program that
    # ship together, deliberately not the family shape -- a peer from another build is turned
    # away at the boundary rather than after its fields have been read
    command) return 0 ;;
    *) return 1 ;;
  esac
}

wrong=""
for file in $(find "$ROOT" -name '*.rs' -not -path '*/target/*' 2>/dev/null | sort); do
  # `#[serde(… tag = "x" …)]`, whatever order the attributes are written in.
  for found in $(grep -o 'tag *= *"[a-z_]*"' "$file" 2>/dev/null | sed 's/.*"\(.*\)"/\1/'); do
    [ "$found" = "event" ] && continue
    allowed "$found" && continue
    wrong="$wrong$file: tag = \"$found\"
"
  done
done

if [ -n "$wrong" ]; then
  echo "gate-wire: a tagged type is neither an \"event\" nor a listed data discriminator:" >&2
  printf '%s' "$wrong" | sed 's/^/  /' >&2
  echo "gate-wire: an event crossing a process boundary is tagged \"event\"; see the wire module" >&2
  echo "gate-wire: a shape nested inside a message goes on this gate's list, with its reason" >&2
  echo "gate-wire: failed" >&2
  exit 1
fi

# A reply says which revision it is written in. Without it a version skew arrives as a missing
# field at the point of use, which reads as the peer being broken rather than as a peer from a
# different build.
if grep -rql 'pub struct Reply' "$ROOT" 2>/dev/null; then
  if ! grep -rql 'pub family: u16' "$ROOT" 2>/dev/null; then
    echo "gate-wire: a Reply carries no \`family\`: a skew would arrive as a missing field" >&2
    echo "gate-wire: failed" >&2
    exit 1
  fi
fi

broken=""
broke() { broken="$broken  $1
"; }

# The text of function `$2` in file `$1`, from its signature to its closing brace.
body_of() {
  awk -v name="$2" '
    !inside && $0 ~ ("fn " name "[<(]") { inside = 1 }
    inside {
      print
      opened += gsub(/\{/, "{"); closed += gsub(/\}/, "}")
      if (opened > 0 && opened == closed) exit
    }
  ' "$1"
}

# File `$1` without its comments and its `#[cfg(test)]` items: the code that ships.
shipped() {
  awk '
    /^[ \t]*#\[cfg\(test\)\]/ { skipping = 1; depth = 0; started = 0 }
    skipping {
      opened = gsub(/\{/, "{"); depth += opened
      depth -= gsub(/\}/, "}")
      if (opened > 0) started = 1
      if (!started && $0 ~ /;[ \t]*$/) skipping = 0
      else if (started && depth <= 0) skipping = 0
      next
    }
    { sub(/\/\/.*$/, ""); print }
  ' "$1"
}

# ---- one frame cap, FAMILY.md's, on every read and every write -------------------------------
cap=$(sed -n 's/.*A frame is at most [0-9]* MiB (\([0-9 ]*\) bytes).*/\1/p' FAMILY.md | head -n 1 | tr -d ' ')
framer="$ROOT/framing.rs"
most=$(sed -n 's/^pub const MOST: usize = \([0-9_ *]*\);$/\1/p' "$framer" 2>/dev/null | tr -d '_')
if [ -z "$cap" ]; then
  broke "FAMILY.md names no frame cap"
elif [ -z "$most" ]; then
  broke "$framer declares no MOST"
elif [ "$(( $most ))" -ne "$cap" ]; then
  broke "$framer caps a frame at $(( $most )) bytes, and FAMILY.md at $cap"
fi
sized=$(body_of "$framer" sized)
printf '%s' "$sized" | grep -q 'len > MOST' || broke "sized() lets a frame over MOST through"
printf '%s' "$sized" | grep -q 'len == 0' || broke "sized() lets a frame of length zero through"
for fn in read_frame read_frame_by write_frame write_frame_by; do
  body_of "$framer" "$fn" | grep -q 'sized(' || broke "$fn() does not hold its frame to the cap"
done
for file in $(find "$ROOT" -name '*.rs' -not -name 'tests.rs' -not -path '*/target/*' | sort); do
  [ "$file" = "$framer" ] && continue
  shipped "$file" | grep -q 'from_be_bytes' && broke "$file reads a frame length outside the framer"
done
for file in config/clients/*.lua; do
  [ -f "$file" ] || continue
  limit=$(sed -n 's/^local MAX_FRAME = \([0-9]*\)$/\1/p' "$file")
  [ "$limit" = "$cap" ] || broke "$file caps a frame at ${limit:-nothing}, and FAMILY.md at $cap"
  grep -q '#request > MAX_FRAME' "$file" || broke "$file sends a call without the cap"
  grep -q 'size == 0 or size > MAX_FRAME' "$file" || broke "$file reads a reply without the cap"
done

# ---- a runtime directory is made by the private-directory rule -------------------------------
serving="$ROOT/serving.rs"
trusted=$(body_of "$serving" trusted)
for held in 'is_absolute()' 'uid()' '0o077'; do
  printf '%s' "$trusted" | grep -qF "$held" || broke "trusted() trusts XDG_RUNTIME_DIR without $held"
done
private=$(body_of "$serving" private)
for held in '.mode(0o700)' 'symlink_metadata' 'uid()' '0o077'; do
  printf '%s' "$private" | grep -qF "$held" || broke "private() makes or keeps a directory without $held"
done
bound=$(body_of "$serving" listening_in)
for held in 'private(root)' 'private(&dir)'; do
  printf '%s' "$bound" | grep -qF "$held" || broke "listening_in() skips $held"
done
for file in $(find "$ROOT" -name '*.rs' -not -name 'tests.rs' -not -path '*/target/*' | sort); do
  code=$(shipped "$file")
  if printf '%s' "$code" | grep -q 'XDG_RUNTIME_DIR' && [ "$file" != "$serving" ]; then
    broke "$file reads XDG_RUNTIME_DIR itself, not through trusted()"
  fi
  printf '%s' "$code" | grep -qE 'create_dir|DirBuilder' || continue
  case "$file" in
    # the rule itself
    "$serving") ;;
    # one command's own 0700 temporary directory, in the system temporary directory
    "$ROOT/jail/temporary.rs") ;;
    # the acknowledgement manifest's directory, under the data directory
    "$ROOT/acknowledged.rs") ;;
    # a test's 0700 scratch directory, in the system temporary directory
    "$ROOT/scratch.rs") ;;
    *) broke "$file makes a directory, and is not on this gate's list" ;;
  esac
done
scripted=$(
  for file in $(find config -name '*.lua' | sort); do
    grep -n 'mkdir -p "\$' "$file" | grep -v 'umask 077' | sed "s|^|$file:|; s|\$| (no umask 077)|"
    grep -n 'XDG_RUNTIME_DIR/' "$file" | grep -v 'casper_private "\$XDG_RUNTIME_DIR"' |
      sed "s|^|$file:|; s|\$| (XDG_RUNTIME_DIR not checked private)|"
  done
)
[ -z "$scripted" ] || broke "a declaration's shell makes a runtime directory loosely:
$scripted"

if [ -n "$broken" ]; then
  echo "gate-wire: the frame cap or the runtime directory rule is not what FAMILY.md says:" >&2
  printf '%s' "$broken" >&2
  echo "gate-wire: failed" >&2
  exit 1
fi
echo "gate-wire: ok"
