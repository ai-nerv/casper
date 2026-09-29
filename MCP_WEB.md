# MCP and web implementation

## Requirements

- A native Rust MCP server exposes Casper's configured tool registry to external clients.
- Preserve disabled/hidden tools, existing configuration, tool failures and jail grants.
- Standard stdio transport, protocol negotiation, tool schemas, calls and cancellation.
- Native Rust web search, readable page extraction, links and bounded/paged results.
- Real browser navigation and interaction through a Rust CDP backend, without Node/Python.
- Document configuration, client registration, network boundaries and runtime dependencies.
- Test actual executable calls, SDK interoperability, HTTP fixtures and browser interaction.

## Reference designs

- [Pi package catalog](https://pi.dev/packages): discoverable optional tool extensions.
- [Pi Web Access](https://github.com/nicobailon/pi-web-access): search/fetch separation,
  bounded raw results, source URLs, explicit provider routing and SSRF checks.
- [Pi browser agent](https://github.com/bigEvilBanana/pi-browser-agent): separate browser
  lifecycle, snapshots/actions and a dedicated browser profile.
- [Official Rust MCP SDK](https://github.com/modelcontextprotocol/rust-sdk): transport,
  protocol negotiation, validation and cancellation rather than a private JSON-RPC dialect.

## Implementation boundaries

MCP is a new Casper transport, not a replacement for the family's socket protocol. It uses
the official Rust SDK and invokes the same executable/configuration path as ordinary calls.
Each call runs in its own cancellable child process. Stdout carries only MCP messages.

Web operations run natively in Rust. HTTP(S) destinations are validated and DNS resolutions
pinned per redirect. Private destinations require trusted configuration, never a tool argument.
Network access respects the jail's reach grant. Responses have time, redirect, byte and output
limits. Extraction returns sources and followable links, not model-generated summaries.
The HTTP destination policy conservatively excludes special-purpose IPv6 protocol/tunnelling
and documentation ranges as well as private destinations; see the
[IANA IPv6 special-purpose registry](https://www.iana.org/assignments/iana-ipv6-special-registry/).

Browser control is Rust speaking CDP to Chromium. Chromium is a runtime dependency, not an
implementation language/runtime. Browser lifecycle and profile isolation must be verified
with real navigation, a DOM interaction and a screenshot before claiming completion.

## Acceptance evidence

- Implemented: MCP stdio transport, native web search/fetch and Chromium CDP control.
- `oslo make verify`: passed the complete Casper gate, including all-target/all-feature
  tests, Clippy, documentation, architectural checks, hermetic cleanup and family/role checks.
- `oslo make test-mcp`: executable-level tests using the official Rust SDK client,
  covering discovery, schemas, actual tool calls, failures, disabled tools, deadlines and cancellation.
- `oslo make test-web`: four executable-level HTTP-fixture tests covering extraction,
  Unicode paging, sources, redirects, download limits, private targets and jail grants.
- `oslo make test-browser` provisions the pinned Chromium runtime and runs real browser
  acceptance, including native tool calls and MCP image content. Browser tests are explicitly
  ignored by the ordinary suite; this separate gate must be run to claim browser acceptance.

## Using the implemented tools

Install the executable and its declarations with `oslo make install`. Register an MCP
client with command `casper` and arguments `mcp`, `--root`, `/path/to/project`.
Use `--tools=read,web` to limit exposure and `--timeout-ms=60000` to set the call deadline.
The server inherits the launch environment and configuration, including `CASPER_JAIL`;
it does not grant additional permissions or expose a remote HTTP listener.
`--root` sets the working directory, not filesystem containment. Use `CASPER_JAIL` for
containment and the MCP client's approval controls for permitted tool actions.

The `web` tool accepts `{"action":"search","query":"Rust memory systems"}` or
`{"action":"fetch","url":"https://example.com","max_chars":12000}`.
Fetch replies include source URLs, links and `next_offset` for another page of text.
Search uses DuckDuckGo by default, or SearXNG when its endpoint is configured in the
trusted `web.searxng` setting. Private addresses are refused unless trusted configuration
sets `web.allow_private`; a tool argument cannot grant that access. Search-provider
availability is external, and the fixture tests do not establish live-provider availability.
Live smoke checks on this host also fetched the Rust homepage over HTTPS (including its redirect)
and obtained three sourced DuckDuckGo search results without API keys or model calls. Those are
observations of provider availability during testing, not a guarantee of future availability.

## Browser configuration and actions

An MCP client can launch an owned browser with:

```json
{"command":"casper","args":["mcp","--root","/path/to/project","--tools=read,web,browse","--browser","/path/to/chromium"]}
```

Without `--browser`, supply a trusted loopback HTTP origin in `CASPER_BROWSER_ENDPOINT`, or
the `browser.endpoint` setting under `CASPER_CONFIGURE`. Use literal loopback IPs, not DNS
names. Request arguments cannot set endpoints, private-target permissions or sandbox flags.
Trusted `browser.allow_private` permits initial navigation to local/private sites; it defaults
to false. `browser.timeout_secs` bounds CDP operations, defaults to 20 and is capped at 60.

The `browse` action schema supports:

- `tabs`: list up to 100 page targets.
- `open` with `url`: create a tab, navigate and return its `target` and snapshot.
- `navigate` with `target` and `url`: navigate an existing tab.
- `snapshot` with `target`: visible text, title, URL and up to 100 interactive elements.
- `click` with `target` and `selector`: activate a DOM element and return a snapshot.
- `type` with `target`, `selector` and `value`: replace an input/textarea value and dispatch events.
- `press` with `target` and `key`: Enter, Tab, Escape, Backspace, Delete, arrows and page keys.
- `scroll` with `target`, optional `x` and `y`: move the viewport.
- `screenshot` with `target`: PNG of the current viewport, capped at 1.5 MB encoded.
- `close` with `target`: close that tab.

`max_chars` bounds snapshot text to 1..50,000 characters, default 12,000. Password input values
are excluded from DOM snapshots. No arbitrary JavaScript evaluation, cookie export, local file
navigation, file upload or storage-reading operation is exposed.

## Browser trust and lifecycle boundaries

Chromium executes untrusted page scripts and can follow redirects or load subresources beyond
the initial URL. Its network traffic is **not** DNS-pinned by the HTTP fetch backend. Browser
control therefore requires a full `reach` grant and a dedicated, unauthenticated profile unless
the operator deliberately authorizes an authenticated session. Do not attach a personal profile.
Download behavior is denied for pages opened/navigated by the tool. Browser actions may have
remote side effects; the MCP client remains responsible for approvals.

Managed launch refuses an active `CASPER_JAIL` rather than bypassing its filesystem or process
restrictions. Launch a dedicated browser separately when operating jailed Casper tools; grant
`reach` to control it. Browser endpoint discovery follows no redirects and accepts only loopback
origins. Page debugger URLs must remain on the same trusted host/port. Each CDP message is bounded.

Owned Chromium has a mode-0700 temporary profile; its cache and temporary files stay beneath
that profile. Profiles use the configured temporary directory when its path is at most 30 bytes,
otherwise `/tmp`, keeping Chromium's nested local socket paths short. A separate Casper
supervisor handles parent death, Chromium shutdown and profile
cleanup even if the MCP process receives SIGKILL. The default Chromium sandbox is never disabled
automatically. Hosts without a usable Chromium sandbox must explicitly opt into `--no-sandbox`;
use that only for trusted test content, not as a production default.

On this host, the pinned Chromium SUID helper is not configured. Real-browser fixture acceptance
uses the explicit test-only opt-out:

```sh
CASPER_TEST_NO_SANDBOX=1 oslo make test-browser
```

Without that environment variable, the test requires a working Chromium sandbox. The pinned
runtime is available with `oslo make browser-runtime`. The browser acceptance test also writes
a screenshot when `CASPER_BROWSER_SCREENSHOT` names a destination; this is test evidence, not a
browser tool permission to write arbitrary files. MCP runs at most eight workers concurrently
and serializes browser calls; deadlines and cancellation include time spent waiting for a slot.
