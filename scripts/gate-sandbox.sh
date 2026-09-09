#!/bin/sh
# A Lua VM in casper is a sandboxed one, and there is only one place that builds it.
#
# casper runs declarations it did not write. `~/.config/casper/tools.lua` is the owner's own,
# `plugin/` is whatever they dropped in there, and `site/pack/*/start/*/plugin/` is code that
# arrived by being fetched. `Lua::full()` hands a VM the whole standard library — `os.execute`,
# `io.popen`, `dofile` — and a declaration with those does not need `casper.exec` at all, which
# would make the process boundary a stylistic preference rather than the only way out.
#
# **The defence is one line in one constructor, and that is the risk.** `src/lua/sandbox.rs` has
# tests that prove the removals work; what they cannot prove is that every VM went through the
# constructor that applies them. A second `Lua::full()` written somewhere reasonable — a helper,
# a benchmark, a `configure` path that wanted its own VM — is a full standard library with no
# test anywhere going red. So the count is held at one, and its file is named.
#
# The list of removals is checked by name for the same reason `gate-untrusted.sh` checks
# balthasar's: a defence that can be deleted without anything failing is decoration. Each entry
# here is one the sandbox module documents a reason for.
#
# POSIX for the same reason the others are: /bin/sh on the runner is dash.
set -eu
ROOT="${GATE_ROOT:-src}"

ENGINE="$ROOT/lua/engine.rs"
SANDBOX="$ROOT/lua/sandbox.rs"

fail=0

# ---- one VM, in one place ---------------------------------------------------------------------
# Comments stripped first. The sandbox module's own documentation opens by naming `Lua::full()` —
# it is explaining what it takes away — and a gate that counted the explanation would fire on the
# file that is doing the defending.
built=$(
  find "$ROOT" -name '*.rs' -not -path '*/target/*' | sort | while IFS= read -r file; do
    awk '{ sub(/\/\/.*$/, ""); print }' "$file" \
      | grep -n 'Lua::full()\|Lua::new()' | sed "s|^|$file:|" || true
  done
)
count=$(printf '%s' "$built" | grep -c . || true)
if [ "$count" -ne 1 ]; then
  echo "gate-sandbox: a Lua VM is built in $count places; there is one sandbox and it is applied once:" >&2
  printf '%s\n' "$built" | sed 's/^/  /' >&2
  fail=1
elif ! printf '%s' "$built" | grep -q "^$ENGINE:"; then
  echo "gate-sandbox: the VM is no longer built in $ENGINE:" >&2
  printf '%s\n' "$built" | sed 's/^/  /' >&2
  fail=1
fi

# ---- and that place trims it -------------------------------------------------------------------
if ! grep -q 'sandbox::apply' "$ENGINE"; then
  echo "gate-sandbox: $ENGINE builds a VM and does not apply the sandbox to it" >&2
  fail=1
fi

# ---- the removals are still the removals --------------------------------------------------------
# Field by field, because losing one is losing a specific thing: `execute` and `popen` are the
# spawn, `remove`/`rename`/`tmpname` are writes that go round the `Ops` seam where path checking
# lives, `exit` ends the process from inside a config file, and `io` goes wholesale because every
# remaining member of it opens a file.
for gone in execute exit remove rename tmpname; do
  if ! grep -q "\"$gone\"" "$SANDBOX"; then
    echo "gate-sandbox: os.$gone is no longer removed from the VM" >&2
    fail=1
  fi
done
for gone in io package dofile loadfile require; do
  if ! grep -q "\"$gone\"" "$SANDBOX"; then
    echo "gate-sandbox: the global \`$gone\` is no longer removed from the VM" >&2
    fail=1
  fi
done

# ---- and fetched code still has to be let in ----------------------------------------------------
# The other half of the boundary. A file under `site/pack/` arrived from somewhere else and can
# change between one run and the next, so it runs once somebody has said it may — and casper is
# the program whose declarations name commands, which is why it is this one that bothers.
#
# Both ends are checked: the rule that says which trust needs it, and the call site that asks.
# Either alone passes with the defence gone — a `needs_acknowledging` nobody consults is a
# function, not a gate.
if ! grep -q 'Self::Installed' "$ROOT/plugins.rs"; then
  echo "gate-sandbox: nothing marks an installed package as needing acknowledgement" >&2
  fail=1
fi
if ! grep -rq 'needs_acknowledging()' "$ROOT/main.rs"; then
  echo "gate-sandbox: the loader no longer asks whether a layer has been acknowledged" >&2
  echo "gate-sandbox: fetched declarations would then run on sight, which is the whole risk" >&2
  fail=1
fi
if ! grep -rq 'acknowledged::cleared' "$ROOT/main.rs"; then
  echo "gate-sandbox: the loader asks, and nothing checks the answer against the manifest" >&2
  fail=1
fi

[ "$fail" -eq 0 ] || { echo "gate-sandbox: failed" >&2; exit 1; }
echo "gate-sandbox: ok"
