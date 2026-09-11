#!/bin/sh
# A command in casper's jail cannot read the user's keys or reach the network. The escape suite in
# `src/jail.rs` proves it against the profile; this proves the same against a real `bwrap` on the
# machine the gate runs on, so a change that weakened the profile is caught on the way to `main`.
#
# Skipped, not failed, where there is no `bwrap`: the jail degrades to Landlock there (a later
# milestone), and a gate that failed for the absence of a tool would fail every build on such a box.
set -eu

command -v bwrap >/dev/null 2>&1 || { echo "gate-isolation: no bwrap here — nothing to prove against"; exit 0; }

work=$(mktemp -d)
home=$(mktemp -d)
trap 'rm -rf "$work" "$home"' EXIT HUP INT TERM
mkdir -p "$home/.ssh"
printf 'THE-SECRET-KEY' >"$home/.ssh/id"

out=$(bwrap \
  --ro-bind / / --dev /dev --proc /proc --tmpfs /tmp \
  --bind "$work" "$work" --tmpfs "$home/.ssh" \
  --unshare-net --unshare-pid --unshare-ipc --unshare-uts \
  --die-with-parent --new-session --chdir "$work" \
  --clearenv --setenv HOME "$home" --setenv PATH /usr/bin:/bin \
  -- sh -c 'cat ~/.ssh/id 2>/dev/null; echo --net--; cat /proc/net/dev' 2>/dev/null)

case "$out" in
  *THE-SECRET-KEY*) echo "gate-isolation: a jailed command read the user's key" >&2; exit 1;;
esac
net=${out#*--net--}
case "$net" in
  *eth*|*wlan*|*enp*|*wlp*) echo "gate-isolation: a real network interface was reachable in the jail" >&2; exit 1;;
esac
case "$net" in
  *lo:*) ;;
  *) echo "gate-isolation: the jail's /proc/net/dev was not readable at all" >&2; exit 1;;
esac
echo "gate-isolation: ok"
