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

# ---- the other half of the boundary is not here, and that is deliberate ------------------------
# A file under `site/pack/` arrived from somewhere else and runs once somebody has said it may.
# That used to be three greps in this file — `Self::Installed`, `needs_acknowledging()`,
# `acknowledged::cleared` — and each of them would have fired on an honest rename while passing
# against a defence wired to nothing. `tests/acknowledged.rs` drives the real binary through the
# whole arrangement instead: a fetched package held, the owner's own file running on sight, the
# package running once acknowledged, and held again the moment it changes. Breaking any one of
# the three turns three of those four red.
#
# What is left here is only what a test cannot reach: how many VMs there are.

[ "$fail" -eq 0 ] || { echo "gate-sandbox: failed" >&2; exit 1; }
echo "gate-sandbox: ok"
