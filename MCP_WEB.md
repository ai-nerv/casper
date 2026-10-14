# MCP and web reading

## Scope

- A native Rust MCP stdio server exposes Casper's configured tool registry.
- The native Rust `web` tool searches for sources or fetches and reads HTTP(S) pages.
- Returned pages contain readable text, source URLs and followable links.
- No website interaction, script execution, screenshots or additional runtime.

## Reference designs

- [Pi package catalog](https://pi.dev/packages): optional tool extensions.
- [Pi Web Access](https://github.com/nicobailon/pi-web-access): separate search/fetch actions,
  bounded raw results, source URLs, provider routing and SSRF checks.
- [Official Rust MCP SDK](https://github.com/modelcontextprotocol/rust-sdk): stdio transport,
  protocol negotiation, tool schemas, calls and cancellation.

## MCP

Register an MCP client with:

```json
{"command":"casper","args":["mcp","--root","/path/to/project","--tools=read,web"]}
```

Install the executable and declarations with `oslo make install`. The server uses the same
configured tools as ordinary Casper calls; disabled and hidden tools cannot be exposed.
`--tools` restricts the exposed tools. `--timeout-ms` bounds calls, defaults to 60,000 and
is capped at 600,000. At most eight workers run concurrently. Deadlines and cancellation
include time spent waiting for a worker slot and stop the owned worker.

Stdout carries only MCP messages. Tool failures become MCP error results. Terminal tools
requiring interaction are refused rather than presented as completed operations.

The server inherits its launch configuration and `CASPER_JAIL` grants; it adds no permissions.
With `CASPER_JAIL` unset, each tool runs inside the conservative jail, and only an empty value
turns isolation off. `--root` sets the working directory, not filesystem containment. Use jail grants for containment
and the MCP client's approval controls for tool actions.

## Search

```json
{"action":"search","query":"Rust memory systems","limit":5}
```

Returns titles, source URLs and snippets without making a model call. DuckDuckGo is the default.
A trusted `web.searxng` endpoint enables SearXNG; the `provider` argument selects between the
supported providers. Search results are untrusted data, not instructions.

## Fetch and read

```json
{"action":"fetch","url":"https://example.com","max_chars":12000}
```

Returns the final URL, HTTP status, title, readable text, links, offset, total character count
and `next_offset`. To read more, fetch the returned URL with `offset` set to `next_offset`.
`max_chars` defaults to 12,000 and is capped at 50,000; offsets use Unicode characters.

The HTTP client downloads page content and extracts text. It does not execute scripts.
Pages whose content only appears after script execution may therefore return little text.
Follow returned links with another fetch call; there is no click, input or submission action.

## Configuration and network boundaries

The trusted `web` configuration supports:

- `allow_private`: default false; permits private destinations only when explicitly configured.
- `timeout_secs`: default 20, bounded to 1..60.
- `max_bytes`: default 2,000,000, bounded to 1,024..8,000,000.
- `searxng`: the configured SearXNG endpoint.

Network access requires the jail's `reach` grant. Tool arguments cannot change trusted settings.
Only HTTP(S) URLs without embedded credentials are allowed. DNS resolutions are checked and
pinned for each request and redirect. HTTPS downgrade redirects are refused. Downloads, redirect
count, time and returned text are bounded; HTTP failures are reported rather than hidden.

The destination policy conservatively rejects private and special-purpose IP ranges, including
IPv6 protocol/tunnelling and documentation ranges; see the
[IANA IPv6 special-purpose registry](https://www.iana.org/assignments/iana-ipv6-special-registry/).
This is a conservative access policy, not an exact classifier of globally reachable addresses.

## Verification

- `oslo make test-mcp`: real executable calls with official SDK and legacy JSON-RPC clients,
  covering discovery, schemas, reads, errors, disabled/hidden tools, deadlines and cancellation.
- `oslo make test-web`: real HTTP fixtures covering readable extraction, Unicode paging,
  source links, redirects, response limits, HTTP failures, private targets and jail grants.
- `oslo make verify`: all-target/all-feature suites, Clippy, docs and native architectural,
  hermetic and family/role gates.

Live smoke checks also fetched the Rust homepage over HTTPS, followed its redirect and obtained
three sourced DuckDuckGo results without API keys or model calls. These were observations during
testing, not guarantees of future provider availability.
