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

Browser control is Rust speaking CDP to Chromium. Chromium is a runtime dependency, not an
implementation language/runtime. Browser lifecycle and profile isolation must be verified
with real navigation, a DOM interaction and a screenshot before claiming completion.

## Acceptance evidence

- Implemented: MCP stdio transport and native web search/fetch.
- `oslo make verify`: passed the complete Casper gate, including all-target/all-feature
  tests, Clippy, documentation, architectural checks, hermetic cleanup and family/role checks.
- `oslo make test-mcp`: two executable-level tests using the official Rust SDK client,
  covering discovery, schemas, actual tool calls, failures and disabled tools.
- `oslo make test-web`: four executable-level HTTP-fixture tests covering extraction,
  Unicode paging, sources, redirects, download limits, private targets and jail grants.
- Not implemented: CDP browser lifecycle, navigation, interaction and screenshots.
  The full MCP/web/browser objective remains incomplete.

## Using the implemented tools

Install the executable and its declarations with `oslo make install`. Register an MCP
client with command `casper` and arguments `mcp`, `--root`, `/path/to/project`.
Use `--tools=read,web` to limit exposure and `--timeout-ms=60000` to set the call deadline.
The server inherits the launch environment and configuration, including `CASPER_JAIL`;
it does not grant additional permissions or expose a remote HTTP listener.

The `web` tool accepts `{"action":"search","query":"Rust memory systems"}` or
`{"action":"fetch","url":"https://example.com","max_chars":12000}`.
Fetch replies include source URLs, links and `next_offset` for another page of text.
Search uses DuckDuckGo by default, or SearXNG when its endpoint is configured in the
trusted `web.searxng` setting. Private addresses are refused unless trusted configuration
sets `web.allow_private`; a tool argument cannot grant that access. Search-provider
availability is external, and the fixture tests do not establish live-provider availability.
