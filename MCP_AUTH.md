# HTTP tool-server authentication

Casper owns OAuth credentials for configured HTTP MCP servers. This implementation
targets the MCP 2026-07-28 authorization subset: pre-registered public clients or
user-hosted HTTPS client metadata documents, PKCE S256, and manual browser consent.
There is no dynamic registration, client secret, automatic browser launch, or
credential import from another application.

## Configuration

Configure `auth` inside a trusted `mcp_servers` HTTP entry:

```json
{
  "remote": {
    "url": "https://tools.example/mcp",
    "auth": {
      "issuer": "https://identity.example/tenant",
      "resource": "https://tools.example/mcp",
      "client_id": "registered-public-client",
      "scopes": ["read"],
      "callback_port": 0,
      "timeout_secs": 120
    }
  }
}
```

`client_id` can instead be a user-hosted HTTPS metadata-document URL when the
authorization server advertises support. Casper does not host that document.
The callback is `http://127.0.0.1:PORT/oauth/callback`; port zero selects an
available loopback port. Register the applicable redirect policy with the issuer.

Resource and issuer origins are admitted; additional HTTPS control endpoint
origins require explicit `origins` entries. Private/reserved destinations require
an exact `private_origins` entry as well. HTTP is permitted only for explicitly
allowed literal-loopback origins. These settings do not replace the existing
process network grant. DNS results are checked and pinned; redirects and proxy
environment settings are disabled.

Explicit `Authorization` headers cannot coexist with OAuth. Stdio servers cannot
use this OAuth configuration. Unknown configuration fields are rejected.

## Commands

```sh
casper mcp-auth remote
casper mcp-status remote
casper mcp-logout remote
```

The explicit auth command prints a manual authorization link on stderr, validates
callback state and issuer, exchanges the code, and persists the credential. A
successful result is emitted only after the private store commit succeeds.
Status and logout require no network. Commands return the normal family reply
shape, with `--json` or `--cbor`; credential material is never a status field.
These commands are CLI-only, not socket verbs or model-facing tools.

Normal tool discovery does not open a browser or wait for consent. Missing
credentials report `authentication_required`. Expired credentials with refresh
material use a bounded, noninteractive refresh.

## Bounds and storage

- Each control exchange has a ten-second absolute timeout and 64 KiB body cap.
  Discovery has a total deadline of at most 60 seconds; consent uses the configured
  1–300 second operation deadline. Challenges are limited to 8 KiB.
- Callback listeners bind only IPv4 loopback, with 8 KiB headers and a five-second
  connected-client deadline inside the operation deadline. Cancellation drops
  the listener. Authorization codes and error descriptions are not echoed.
- Credentials live below `$XDG_STATE_HOME/casper/mcp-auth`, falling back to
  `$HOME/.local/state/casper/mcp-auth`. Directory mode is 0700, files 0600.
  Keys bind endpoint, canonical resource, exact issuer and client identity.
  Native jails protect both the selected and default OAuth store roots, including
  aliases and overlapping workspace or explicit grants.
- Descriptor-relative no-follow operations, ownership/link checks, bounded
  cross-process locking, atomic replacement and file/directory synchronization
  protect updates. Logout writes a generation tombstone; stale updates fail.
- A separate cross-process lock serializes refresh without holding the record lock
  during network requests. Refresh preserves the login epoch; login and logout
  replace it. Failed or cancelled refresh attempts retain credential material and
  enforce a 30-second retry delay. Total refresh time is at most 30 seconds.
- Local logout signals matching transports without waiting for the session-pool
  mutex. An owned monitor checks for external logout once per second; changed
  epochs stop active reads and SSE streams. At most 256 auth handles are admitted.
- Dropping login or refresh fences queued store mutations. Writers check cancellation
  and absolute deadlines during lock waits and before atomic replacement. An atomic
  replacement already admitted may finish; cancellation is not a rollback guarantee.
- Login uses one absolute deadline from store opening through consent and commit.
  Initial login reads and refresh snapshots share the operation's cancellation
  fence, including record-lock waits.
- Normal credential attachment has one deadline of at most 30 seconds, including
  initial store reads. Stopped or dropped monitors cancel their blocking readers.
- Record and refresh leases explicitly unlock when their owner releases them;
  duplicated descriptors do not retain those locks after owner teardown.
- Pool shutdown closes admission and signals the current pool generation before
  taking the session mutex. Preflight, initialization, discovery and calls stop
  without waiting for their network deadlines. Admission reopens after teardown;
  callers queued under the previous generation remain cancelled.
- Token-response merge preserves an omitted refresh token; explicit null clears
  it, and a supplied replacement rotates it. Missing expiry remains unknown;
  zero or overflowing expiry and unsupported token types are refused.
- Authenticated HTTP transport errors omit endpoint URLs, rejected content types
  and parser details. Malformed authenticated SSE messages stop the auth handle;
  valid JSON-RPC payloads and resumption cursors are preserved. Empty priming and
  ignored control-event data do not enter the JSON-RPC decoder.
- Authenticated session IDs and SSE resumption cursors are encrypted before the
  SDK receives them. A transport-local AES-256-GCM key and shared monotonic nonce
  sequence bind aliases to the transport and identifier kind. Cloned clients
  share that state; older cursors remain recoverable without an eviction table.
  Only HTTP header construction restores the original values, marking them
  sensitive. Raw, foreign, tampered, oversized and wrong-kind aliases fail closed.
  Empty cursor resets, valid JSON-RPC data and unauthenticated behavior remain
  unchanged. No SDK fork or logging suppression is used.

## Current acceptance boundary

The2026-10-29 final-image family gate passes8,132records with no failures or
leaked processes; thirteen explicit editor/visual/measurement cases are skipped
in that invocation. The separate selected-editor gate passes all seven editor
cases. Deterministic family acceptance passes15/15. Final Casper SHA256 is
`ff2a06bf44608063945b76ddc0a93ebc92e6d6288103ba2fa47838b9f224b9f5`.
Receipts under `target/roadmap/11/family-candidate.abuXbcuD` in the family checkout:
`target/family/run-VE6tVqb8`, `target/acceptance/run-VvDpo1T4` and
`target/roadmap/11/selected-editor-workflows.FGs0csXN`.
The earlier failures described below remain historical evidence, not current
failures. The native family gate includes both abrupt Landlock owner-death cases.

Local native fixtures cover explicit login, resource discovery, PKCE verification,
credential persistence, authenticated tool discovery, status and logout. They do
not constitute real-server consent acceptance.

Local fixtures also cover four-process single-flight refresh, rotation, failure
backoff, logout fencing, and cancellation of stalled reads and open SSE streams.
Native response-loss coverage records one completed remote action, a failed call
with an explicit uncertain-effects warning, and no replay or refresh. Local TLS,
absolute body-drip deadlines and oversized/malformed token cases exercise the
production control transport and preserve existing credentials on failure.
Native CLI fixtures also cover initial lock deadlines/cancellation, post-exchange
commit deadlines and both interrupt/terminate signals during a blocked commit.
Test launchers apply kernel faults before executing the selected native binary;
stage creation, file synchronization and rename failures return errors without
replacing old credentials or leaking temporary files, and a later login recovers.
Native fixtures honor `CASPER_TEST_BINARY` when supplied, including fault-launcher
exec, and check the running image through `/proc`. An invalid explicit selection
fails without falling back to Cargo's binary. Library shutdown workers remain
separate test processes rather than pretending to be the selected CLI.
Isolated owner-process fixtures cover shutdown during refresh, calls, initialization,
discovery and an initial record-lock wait, plus local and external native logout
during refresh and calls. They check peer closure, no replay or credential
resurrection, unrelated session reuse, successor recovery and credential-descriptor
cleanup. Contended monitor/attachment readers release ownership before the fixture
unlocks. The final-image family gate includes these fixtures and the isolated
unfiltered SDK diagnostic checks described below.
Real SDK fixtures cover eight concurrent interrupted SSE calls, exact original
cursor resumption, session cleanup and malformed-stream failure without replay.
Another fixture expires credentials while two interrupted streams are open: their
resumption shares one refresh, keeps the original session/cursors, executes each
tool once and commits rotated credentials without changing the login epoch.
Header fixtures check sensitive Debug formatting and invalid-value refusal.
Earlier full owner verification passed 1,860 reported records across 78 reports with
zero failures or ignored records, including auth/MCP/web, hermetic, release and
configured dependency checks. After the selected-image fixture correction, focused
auth verification passes 69 reported records across 8 reports, Clippy and all ten
architectural gates against a preserved Casper release copy.
The actual SDK resume, malformed-stream and refresh fixtures now install an
unfiltered global TRACE subscriber in isolated processes. Event fields, new span
fields and later span records share a one-MiB capture bound; overflow fails.
Each fixture requires actual RMCP TRACE activity and encrypted identifier
diagnostics, then rejects its secret canaries. A positive-control test verifies
capture of span/event canaries and exact-bound overflow detection.
The preceding focused auth verification passed70reported records across8reports,
and owner verification passed1,700records across62reports, zero failed/ignored.
The dev-only direct tracing0.1.44 edge adds no package version; the production
Casper binary is byte-identical to the preceding selected image. These fixtures
do not replace full-family containment and recovery acceptance.
Owner fixtures inject actual kernel write/file-sync/directory-sync failures and
verify errors, temporary-file cleanup and recovery. Failed directory sync can leave
a visible replacement; no durable success is reported for that uncertain outcome.
No ambiguous tool request is automatically replayed to obtain authentication.

### Historical process-lifetime regressions

The initial MCP process-group cleanup correction passed the full Casper owner
gate and focused MCP/auth suites, alongside Magi's selected-native event tests
(1,924 combined reported records across 97 reports, zero failed/ignored). Native
stdio fixtures cover normal completion, deadline and server-leader exit in both
jails; their descendants cannot survive the completed owned-session shutdown.
Admitted daemon MCP calls now capture their caller's connection. Full peer
disconnect invalidates the affected pooled session, cancels its transport and
stops its owned stdio process group. Concurrent calls on that same session can
also fail; unrelated sessions are not stopped. Half-close preserves the response
path. An abandoned request is never replayed; a later explicit call can create a
fresh session. Cancellation also covers authentication attachment, initialization,
discovery and pool-lock waits. Pending and completed startup tasks retain the
server cleanup guard until the session takes ownership.

The selected-native disconnect/recovery tests pass under both jails, and the
subsequent Casper owner verification passes. The expanded native suite still
fails two tests: abrupt SIGTERM/SIGKILL of Casper leaves a Landlock descendant
alive. Bubblewrap passes those abrupt-exit cases. The failing receipt is retained
as `mcp-peer-recovery.log`. The subsequent `mcp-startup-owner.log` again passes
Casper owner verification and adds selected stdio initialization/discovery cleanup
in both jails, cancellation of a native pool waiter before the held lock is
released, fresh-call recovery, and discarded-launch child-reaping tests. Its native
MCP binary reports11passed/2failed: the same two abrupt Landlock failures remain.
Full native/family acceptance is not complete.

Cancelled admission after session publication now retires only newly created or
replaced instances. Reused and unrelated sessions remain intact. Actual HTTP
fixtures verify shared-call cancellation, unrelated useful work and fresh-session
recovery without replay. `mcp-publication-isolated.log` passes owner verification;
the native matrix still reports11passed/2failed on the same Landlock abrupt exits.

Run `oslo make test-mcp-auth`, `oslo make test-mcp` and `oslo make test-web` in the
Casper checkout. Full acceptance additionally requires the owner and family gates.

### Landlock stdio supervision

Landlock MCP servers run beneath a Casper-owned subreaper created before their
signal-scoped confinement. The server cannot signal that supervisor. A private
lease and owner pidfd trigger cleanup on session release or Casper exit; server
exit also initiates cleanup. The supervisor adopts and reaps descendants even if
they create another session. Bubblewrap retains its own containment path.

The supervisor closes inherited application/control descriptors, preserves the
server's stdio and exit status, and uses fixed4KiB scan storage. Each cleanup round
reads at most16chunks plus11bytes to complete a PID. Reaping waits until scan EOF
so buffered IDs cannot be reused mid-scan. No universal deadline for uninterruptible
kernel work or total-process RSS guarantee is claimed.

Selected native fixtures cover owner TERM/KILL, peer disconnect, startup/discovery
cancellation, descriptor exclusion and recovery on both backends. Landlock
fixtures additionally require denied STOP/KILL attempts against the supervisor.
Unit fixtures cover stdio/status, changed sessions, double-forked descendants and
failed exec. The existing nested native-read and ACP cases remain enabled.
