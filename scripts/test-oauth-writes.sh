#!/bin/sh
set -eu
root=$(pwd)
temporary=$(mktemp -d)
trap 'rm -rf -- "$temporary"' EXIT HUP INT TERM
cp -R src "$temporary/src"
cd "$temporary"
gate() { sh "$root/scripts/gate-writes.sh" > "$temporary/result" 2>&1; }
gate || { cat "$temporary/result"; exit 1; }
store=src/mcp_client/oauth/store.rs
cp "$store" "$temporary/original"
printf '\nfn invalid() { std::fs::remove_file("unowned"); }\n' >> "$store"
if gate; then echo 'OAuth arbitrary deletion escaped the gate' >&2; exit 1; fi
cp "$temporary/original" "$store"
sed 's/OFlags::EXCL | OFlags::NOFOLLOW/OFlags::EXCL/' "$store" > "$temporary/changed"
cp "$temporary/changed" "$store"
if gate; then echo 'OAuth no-follow removal escaped the gate' >&2; exit 1; fi
cp "$temporary/original" "$store"
printf 'fn invalid() { std::fs::write("unowned", "data"); }\n' > src/oauth_unowned.rs
if gate; then echo 'An unapproved writer escaped the gate' >&2; exit 1; fi
printf '%s\n' 'OAuth store mutation gates reject unowned writes, deletes and missing no-follow.'
