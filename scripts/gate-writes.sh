#!/bin/sh
# Casper-owned filesystem mutation is restricted to named modules.
# OAuth storage replaces private credential records under a locked directory and
# only unlinks its own uncommitted temporary file. Logout writes a tombstone.
set -eu
ROOT="${GATE_ROOT:-src}"

# May write. Adding to this list is a deliberate act; the point is that it cannot happen by
# accident.
WRITERS='src/acknowledged.rs src/noted.rs src/scratch.rs src/serving.rs src/jail/temporary.rs src/jail/guard.rs src/grammars/building.rs src/source_worker.rs src/files/locking.rs src/files/registry.rs src/files/worker/staging.rs src/mcp_client/oauth/store.rs'

# May remove. The scratch helper owns what it removes; the socket door clears a dead daemon's file;
# the guard removes what a jailed command left at a protected path.
REMOVERS='src/scratch.rs src/serving.rs src/jail/temporary.rs src/jail/guard.rs src/files/worker/staging.rs src/mcp_client/oauth/store.rs'

# What counts as reaching for the filesystem to change it. `File::create` and `OpenOptions` are
# here because both truncate or create without the word `write` appearing anywhere.
#
# The pattern was checked against a wider net over the whole tree. The allowlist held — those three
# files are still all that writes — but four names got through this pattern and not the net, and
# each is a way to change what is on disk: `set_permissions`, `set_times`, `symlink` and
# `File::options`, which is `OpenOptions` under another spelling. `create_new` matched only as a
# substring of `File::create`, and is spelled out here rather than left to that.
WRITING='fs::(write|create_dir|create_dir_all|copy|rename|hard_link|soft_link|symlink|set_permissions|set_times|renameat|renameat_with|linkat|mkdirat|symlinkat|chmod|fchmod|fchown)\b|File::(create|create_new|options)|OpenOptions|DirBuilder::|OFlags::(CREATE|TRUNC|TMPFILE)'
REMOVING='fs::(remove_file|remove_dir|remove_dir_all|unlinkat)\b'

fail=0

# Comments stripped, and `#[cfg(test)]` items with them: a module documenting the rule by naming
# the call is describing it, not breaking it, and a test may reach for anything. The same reading
# `gate-independent.sh` and `gate-spawn.sh` already use.
without_tests() {
  awk '
    /^[ \t]*#\[cfg\(test\)\]/ { skipping = 1; depth = 0; started = 0 }
    skipping {
      if (!started && /^[ \t]*mod [a-zA-Z_][a-zA-Z_0-9]*;[ \t]*$/) { skipping = 0; next }
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
  # A `tests.rs` or `*_tests.rs` is a `#[cfg(test)]` module kept in a file of its own.
  find "$ROOT" -name '*.rs' -not -path '*/target/*' -not -name 'tests.rs' -not -name '*_tests.rs' |
    sort | while IFS= read -r file; do
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

worker_writes=$(without_tests src/source_worker.rs | grep -nE "$WRITING" || true)
if [ "$(printf '%s\n' "$worker_writes" | grep -c .)" -ne 1 ] ||
   ! printf '%s\n' "$worker_writes" | grep -Eq '^[0-9]+:    let mut file = std::fs::OpenOptions::new\(\).write\(true\).open\(path\)\?;$' ||
   ! without_tests src/source_worker.rs | grep -Fq 'if crate::jail::Jail::from_env().on()'; then
  echo 'gate-writes: source worker may only open an existing replacement target after its jail guard' >&2
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

oauth=src/mcp_client/oauth/store.rs
oauth_removes=$(without_tests "$oauth" | grep -nE "$REMOVING" || true)
if [ "$(printf '%s\n' "$oauth_removes" | grep -c .)" -ne 1 ] ||
   ! printf '%s\n' "$oauth_removes" | grep -Fq 'fs::unlinkat(&self.directory, temporary.as_str(), AtFlags::empty())' ||
   ! without_tests "$oauth" | grep -Fq 'OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC' ||
   ! without_tests "$oauth" | grep -Fq 'self.lock_valid(&lock)?;' ||
   ! without_tests "$oauth" | grep -Fq 'self.directory.sync_all()'; then
  echo 'gate-writes: OAuth storage requires private atomic replacement; only its own temporary may be unlinked' >&2
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
