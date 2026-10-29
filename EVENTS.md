# Local operational events

The approved initial Phase11 policy has local invocation, native mutation and
tool-server instrumentation. Explicit native bootstrap activation is available
on run/serve; normal launches remain disabled. There is no disk spool or network
exporter.

`event-capabilities` emits fixed family JSON before configuration or useful work,
advertising native bootstrap revision1. This is a stateless compatibility query,
not authentication. Normal Magi-to-Casper launch integration remains pending.

The revision1 envelope records `program: casper`, numeric package version,
OS-generated128bit producer ID (parent-issued for native forwarding), nonzero sequence and nullable opaque correlation
IDs for run/agent/turn/request/ask/attempt/job/tool-call. Unknown fields, kinds,
revisions, owners, invalid numeric states and noncanonical IDs are rejected.
No arbitrary command, path, URL, content, header, error detail or measurement map
is accepted. Decoder input and encoded output are each capped at4096bytes.

## Owner facts

- `tool`: admitted/started/waiting/succeeded/failed/cancelled/refused/unknown outcome,
  with optional monotonic elapsed milliseconds. Unknown execution is not success.
- `mutation`: committed state carries separate durability, observation and delivery
  classifications. Refused and unknown states cannot carry those confirmation
  fields. A committed mutation may still have unconfirmed durability or reporting.
- `tool_server`: connecting/connected/close-requested/disconnected/failed/refused/
  unknown. Authentication-required remains reserved; no callsite infers it from
  error strings. This schema does not implement OAuth or enable authentication.

Mutation and tool-result observations are distinct facts, not two executions to
add together. No permission or jail authority is granted by an event or correlation
identity.

## Instrumented boundaries

Each calling-owner invocation receives a fresh opaque local tool-call identity.
Identities derive from the process nonce and a nonwrapping atomic counter through
domain-separated SHA256 truncated to128bits, with no per-call entropy I/O. Exhaustion
counts loss and suppresses correlated events; it cannot change tool behavior.
Standalone run/agent/turn correlation remains null. Native bootstrap2 can bind
run/agent/turn/ask from the admitted parent; normal durable daemons currently bind
run/agent only. A resumed question is a new local invocation; tool-call IDs alone
do not establish its relation to an earlier question.

Started means entering the local declaration, native operation or MCP dispatcher,
not authorization, OS execution or remote receipt. Revision-contract admission and
broker data validation precede this boundary. Undeclared/off/invalid guarded calls are refused
without a started event. Waiting distinguishes person and harness questions from
successful results. A callback returning no result after dispatch is unknown: it may
already have made changes. Successful and failed outcomes describe returned results,
not proof of particular side effects or rollback. Unwinding after dispatch is unknown;
forced process death need not produce a terminal event and is not fabricated cancellation.

Native mutations emit one parent-owner fact after a validated worker receipt or an
explicit refused/unknown worker outcome. Committed facts precede rendering, so a
presentation failure can coexist with a committed mutation. Directory durability and
current observation follow the receipt independently; outward delivery is unconfirmed.
A lost or invalid worker reply stays unknown even if the operation actually committed.
No second commit event is emitted by the native worker. Brokered-read invocations
present previously acquired data; their started/completed events do not mean another
physical read. Relationships between nested calls and helpers still require
separate correlation work.

## Tool-server ownership

Each fresh native MCP connection attempt receives one local opaque request identity.
Cache hits emit no new attempt or connected event. Connecting precedes transport
setup; connected means initialization and complete discovery succeeded, not that a
tool ran. Explicit network-policy refusal is refused. Setup/discovery errors are
failed, with original errors preserved outside the event. An attempt unwinding
without a native result is unknown. Invalid configuration and disabled declarations
never reach the connection boundary and emit no server attempt.

Close-requested records native cancellation being requested, once per session. A
shared session retains its trace until the last Arc owner releases it. Disconnected
records that final ownership release after owned cleanup guards; it is not proof
that an OS process was reaped or that a remote endpoint closed its socket. A ready
session released without an explicit close request still emits disconnected, not
fabricated cancellation. Forced process death can omit terminal events entirely.
Replacement closes the old identity and allocates a new one. A session opened while
disabled stays unobserved if the queue is enabled later. All unrelated correlation
fields remain null; server IDs do not establish tool-call or cross-process ancestry.

## Queue and loss

`emit` skips preparation while disabled. Explicit `enable_local` creates one
process-local receiver and a256slot nonblocking try-send queue. Native forwarding
binds that sole queue to the parent identity instead. Queued payload is at most1MiB, excluding queue metadata, concurrent
encoding buffers and consumer-owned data. Payload closures must assemble bounded
facts, not perform I/O; nonblocking queue capacity is not a hard real-time guarantee.

Full/closed/encoding/exhaustion losses saturate rather than wrap. Receiver closure
also accounts for enqueued events not handed to the consumer as abandoned, including
racing sends once producers return. Snapshots are not atomic. Sequence gaps indicate
attempts, not acknowledged delivery; this is not a durable transaction journal.
Concurrent sessions can enqueue out of sequence order. Producer/sequence identifies
an event; it does not serialize unrelated session lifecycles or establish remote
causality.

`oslo make test-events` checks independent golden encoding, hostile fields, state
distinctions, disabled behavior, identity exhaustion, saturation, concurrency and
shutdown accounting. Its native harness uses private namespaces/configuration and
the actual Lua dispatcher, containment and file worker protocol. It verifies read/write,
stale refusal, committed presentation failure, committed-but-truncated reply, questions,
callback side effects before a missing result, broker validation and actual disk state
despite disabled/full/closed queues. The harness reports nine scenarios separately
from Rust unit-test counts. Eight isolated server-trace scenarios check unwinding,
duplicate transitions and shared ownership. Twenty-three native server scenarios
exercise stdio/HTTP discovery, calls, reuse, replacement, failures and useful work
with disabled/late/full/closed queues. Stdio fixtures resolve their namespace PID
through the selected executable's host `/proc` identity and check actual reaping;
these fixture checks do not change disconnected into a universal reaping receipt.
Eight scenarios hold an actual server request in flight during replacement or
shutdown, across enabled/disabled/full/closed event queues. They check cancellation
before the native request deadline, exactly one execution of the held request,
successful recovery reuse and reaping of both owned server processes. Two isolated
loopback probes retain real native Session owners after pool closure/replacement:
disconnected remains absent until the final owner drops, and replacing the old
session leaves the new session open.
No installed configuration or provider is used.

Collector delivery, complete mutation attribution, cancellation/reaping telemetry,
cross-process deduplication and measured overhead remain separate acceptance.

## Native forwarding

`run --event-bootstrap` and `serve --event-bootstrap` consume one prequeued native
packet from stdin before other readers or workers start. Only these verbs accept
the flag, once. Tool arguments, project configuration and environment cannot
activate it. The receiver is independently implemented with Casper's existing
rustix1.1.4 dependency plus its empty stdio feature; there is no Magi crate/source
dependency or lockfile/version change.

The schema2 packet is at most512bytes and accepts only the Casper owner. It carries
exactly two descriptors: a bounded connected unnamed nonblocking CLOEXEC datagram
sender and readable original stdin. The bootstrap channel must be an unnamed Unix
SEQPACKET socket from the actual parent PID/effective UID. Truncation, unknown or
duplicate fields, wrong owner/kind/revision, malformed identities and descriptor
count/type errors refuse before useful work. Received rights close on errors.
Safe dup2_stdin restores the original descriptor; input bytes and file offsets
are not serialized or copied. The event descriptor stays CLOEXEC.

The packet binds opaque run/agent/turn/ask identities independently of tool input.
Run and agent must be supplied together; turn/ask require that root. Bound fields
override producer-local candidates, while tool-call/request identities remain
source-owned. The receiving collector checks bindings before deduplication and
accounting. Event schema1 and4096byte bounds are unchanged. Capability replies
advertise bootstrap2; legacy peers are not silently admitted under its contract.
`lineage-native-fixture.log` verifies the owner tests and fully bound selected
native interoperability. Request-scoped daemon and nested ancestry remain open.

An owned forwarding thread makes one attempt per dequeued event, with no second
queue or retry. Status attempts, including failures and final reports, are limited
to one per second. Shutdown joins, closes the receiver and counts abandoned queued
events instead of waiting for delivery. Active payloads remain unconfirmed; payload
panic counts encoding loss and releases the active guard. Missing/rate-limited
final reports stay unknown. If optional activation fails after stdin restoration,
useful work continues without blocking diagnostic output; the root retains unknown
loss. The native parent retains its lifetime token through actual child reaping.

Candidate receipt `casper-bootstrap-selected.log` (session76044, exit0) passes32
reported Casper Rust results, nine tool and23server scenarios, both owners' Clippy,
ten Casper/eleven Magi gates and configured offline audit/deny. The independent
Magi `oslo make test-events-native` test uses selected real Casper, checks its
`/proc/<pid>/exe`, stdin identity, normal/admitted call input, daemon key and
announcements, useful shell output, shell descriptor exclusion and actual reaping.
Invalid bootstrap refuses before executing a supplied call. Forced daemon death
is explicitly unknown loss. Isolated EOF sentinels check rejected descriptor cleanup.

Normal Magi run/serve negotiation is now covered by `supplier-cancel-repair.log`.
Disconnecting a socket caller interrupts its running subprocess group and reaps
the direct child; later run/binary/piped requests in that call scope are refused.
Nested peer scopes restore their previous watch on return or unwind. A half-closed
write side alone does not cancel a caller still reading its reply.

`supplier-headless-settlement.log` adds selected four-owner native session tests
with idle/active tools and collection disabled/enabled. It checks useful results,
shell descriptor exclusion, namespace-aware host-PID cleanup and owned daemon
socket removal on graceful Magi shutdown. This is not proof of every jail/MCP/
provider descendant path, inherited lineage, measured overhead or Phase11 completion.

## Parent-bound daemon requests

Root-only native bootstrap enables `request_context: 1` on the actual listening
announcement. Fully turn/ask-bound single-call bootstrap, disabled collection and
legacy launches do not enable that extension. Optional schema1 `event_context`
metadata stays outside tool arguments and is capped at256 serialized bytes.

After the ordinary daemon key check, Casper requires the original validated
bootstrap parent PID/effective UID and exact run/agent binding. Missing, malformed,
oversized or untrusted context leaves turn/ask attribution empty for that request;
it neither refuses nor replays useful work. Each synchronous request scope clears
and restores its predecessor, including unwind. Tool and MCP connection traces
capture immutable context before asynchronous/lifetime handoff. Event emission
does not read ambient request scope; detached tasks do not implicitly inherit it.

`daemon-lineage-typed.log` passes the owner gates,33 local event tests, native tool/
server scenarios and selected native interoperability. The stronger native fixture
in `daemon-lineage-reuse-fixture.log` reuses a single connection across valid,
missing, wrong-root and oversized contexts, restoring valid context between each
clearing check. Another same-UID process with the daemon key cannot claim the
parent's context. Concurrent calls retain distinct captured turn IDs, and all
admitted fixture calls still return their useful result. Phase11's remaining
ancestry/containment/overhead and full-family/editor requirements are not completed
by these focused receipts.
