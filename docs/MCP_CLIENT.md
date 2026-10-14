# External MCP servers

Casper implements both directions of MCP:

- **Client:** Casper connects to external servers, imports their tools, and executes calls for Magi.
- **Server:** `casper mcp` exposes Casper's registry to other MCP clients.

Magi remains the coordinator. No external-server transport or executor is embedded in Magi.
The client uses the official Rust MCP SDK. The standard stdio and Streamable HTTP transports
support initialization, protocol negotiation, capability discovery, paginated listings and calls.

## Configure MCP servers

Casper automatically reads **only** `$XDG_DATA_HOME/casper/mcp/*.json`, defaulting to
`~/.local/share/casper/mcp/`. Magi uses Casper's discovered tools; it does not scan server
definitions itself. No other application's configuration or project registry is imported.
There is one regular JSON file per server, with its alias as the filename. Aliases contain
1–20 ASCII letters and digits, words joined by single underscores: no `__` and no leading or
trailing `_`, so one alias's tool names never read as another's. Nested directories and
non-JSON files are ignored; symlinked definitions and invalid files are reported without
preventing healthy files from loading.

For example, `~/.local/share/casper/mcp/filesystem.json`:

```json
{
  "command": "/absolute/path/to/installed/mcp-server",
  "args": ["--root", "/your/project"],
  "needs": "run",
  "timeout_secs": 60
}
```

An HTTP server file uses `url` and optional `headers` instead of a stdio `command`.
These files are trusted user configuration: their launch commands can start at discovery.
Discovery does not grant tool permissions. Server environment variables and credentials are
explicit strings in `env` or `headers`; other applications' credential stores are not read.

`tools({ path = "mcp" })` reports configured servers and tool descriptions.
Within Magi it reads the session's startup discovery snapshot, handed over as the
`mcp_catalog` and `mcp_discovery_errors` settings, without launching servers or requiring
network grants; actual availability is checked when a tool is called. Direct Casper
lookups discover under the caller's current launch policy and report connection failures.
`tools({ path = "mcp/<alias>/<namespaced_tool>" })` shows the argument schema.
An unavailable server does not remove native tools or other servers' tools.
Server credentials and launch commands are not included in these pages.
Discovery reruns when a new Magi session starts; existing sessions retain their startup settings.

### Optional coordinator overrides

Explicit `magi.casper.mcp_servers` entries override same-named directory definitions, including
entries with `off = true`. No Magi MCP settings are needed for normal folder discovery.
If coordinator overrides are needed, merge them with any existing `magi.casper` settings:

```lua
magi.casper = {
  mcp_servers = {
    filesystem = {
      command = "/absolute/path/to/an/installed/mcp-server",
      args = { "--root", "/your/project" },
      needs = "run",
      timeout_secs = 60,
    },
    remote = {
      url = "https://example.org/mcp",
      headers = { Authorization = "Bearer your-token" },
      needs = "reach",
    },
  },
}
```

Use exactly one of `command` or `url`. Server aliases contain 1–20 ASCII letters and digits,
words joined by single underscores. Unknown settings and invalid combinations are errors, not
ignored configuration.
Commands, arguments, credentials and endpoints come from this trusted configuration, never
from model tool arguments. Project `.magi.lua` files cannot add or change external servers.

Settings per server:

| Field | Meaning |
|---|---|
| `command`, `args`, `env` | Stdio executable, argument array, explicit environment map |
| `url`, `headers` | Streamable HTTP endpoint and authentication/custom headers |
| `tools` | Optional allowlist of **original** server tool names; an empty list exposes nothing |
| `excluded_tools` | Denylist of original server tool names; takes precedence over the allowlist |
| `needs` | Magi permission verb: `read`, `write`, `run` (default), or `reach` |
| `network` | Stdio server requires network access; default false |
| `timeout_secs` | Initialization, discovery and call deadline; 1–300, default 60 |
| `off` | Do not connect or expose this server |

The server's tool descriptions and JSON Schemas are imported, not recreated. Names are
`mcp__filesystem__original_tool`; incompatible or long names receive a stable hash suffix.
Aliases prevent collisions between servers. Collisions with native tools are refused.
Casper's existing `tools.<namespaced_name>.off` and `.hidden` settings apply to imported tools.
The MCP namespace is reserved for imported operations.

Resources and prompts are exposed, when advertised by a server without a tool allowlist:

- `mcp_<alias>_resources`: `action = "list"`, `"templates"`, or `"read"` with `uri`.
- `mcp_<alias>_prompts`: `action = "list"`, or `"get"` with `name` and optional string `arguments`.

## Sessions and failures

`casper serve` retains one initialized connection per configured server and launch policy.
Magi automatically prefers this door for MCP tools when `XDG_RUNTIME_DIR` is available.
`MAGI_TOOLS_DOOR=command` chooses a new session per call. Without a usable socket door, a
not-yet-sent call may use the command door. A call whose socket response was lost is **never
automatically replayed**: a remote operation may already have happened.

MCP tool errors remain failed results. Protocol, transport and configuration errors are reported
as failures. One message from a server holds at most 4 MiB, as one stdio line, one SSE event or
one plain JSON reply; a larger one closes the session with a failure naming the cap, and nothing
is retried. An SSE stream reaches the SDK whole: notifications and requests sent on it are
delivered, and a reply stream that closes before its response is resumed with `Last-Event-ID`.
A stream with an event past the cap is neither resumed nor retried. Call deadlines send MCP
cancellation; cancellation is cooperative and does not undo remote effects. HTTP session expiration does not automatically replay requests.
Normal shutdown closes sessions and reaps owned stdio children; children also carry Casper's
parent-death tie. Server output remains on stderr; stdout stays on the family/MCP wire.

The original MCP result is serialized into `said`, including `content`, `structuredContent`
and `isError`. Images become attachments, as described below; other non-text blocks are
retained as JSON, not silently discarded. Existing `output_bytes` bounds apply, with explicit
truncation.

## Permissions and containment

Server settings are privileged executable configuration. Only configure servers you trust.
Imported descriptions, prompts and content remain untrusted data. A remote `readOnlyHint`
does not confer permissions; `needs` is chosen by the trusted configuration.

Stdio commands use Casper's existing jail preparation, credential exclusions and process ties.
A server's `env` reaches the server and never the sandbox launcher: under bubblewrap each
variable is handed over with `--setenv`. Loader variables (`LD_*`) are dropped on every path.
HTTP connections and networked stdio servers require effective network grants. Magi judges
MCP permission subjects by their imported tool names, not a model-supplied `command` or `host`.
The existing kernel profile supports unrestricted network grants, not per-endpoint network
containment. A host-specific or one-off UI approval alone does not widen that profile.

Install stdio server dependencies before starting the session. Under the conservative jail,
executables and libraries must be in the working tree or readable system paths. An `uvx` or
`npx` command pointing at an inaccessible home cache will fail; do not disable the jail to
hide that. Place a preinstalled server environment under the project or a system path instead.

Code executed by a remote tool (including code inside an already-running Blender) is **not
contained by Casper's local process jail**. Use allowlists and the remote application's own
controls. HTTP redirects and ambient proxies are disabled; credentials are explicit headers.

## Blender example: existing addon

The Blender addon listener on port 9876 is raw TCP, **not an MCP HTTP endpoint**. The external
`mcp-for-blender` stdio server bridges MCP to that listener. No addon reinstall is necessary.
[Upstream setup](https://github.com/ahujasid/mcp-for-blender#quickstart).

Install only the server environment in the project you will run Magi from:

```sh
uv venv --python /usr/bin/python3 .mcp/blender
uv pip install --python .mcp/blender/bin/python mcp-for-blender==2.1.3
```

Save `~/.local/share/casper/mcp/blender.json` (or the corresponding XDG directory):

```json
{
  "command": "/absolute/path/to/project/.mcp/blender/bin/mcp-for-blender",
  "env": {
    "DISABLE_TELEMETRY": "true",
    "BLENDER_MCP_SAFE_MODE": "1",
    "BLENDER_HOST": "127.0.0.1",
    "BLENDER_PORT": "9876"
  },
  "tools": ["get_scene_info", "get_object_info", "get_addon_status"],
  "needs": "reach",
  "network": true,
  "timeout_secs": 30
}
```

Start with inspection. This allowlist deliberately excludes arbitrary Blender code execution
and scene mutation. Expand it only when you intend to allow those actions. Do not pregrant
all network access unless that is your deliberate policy.

Magi can discover `mcp__blender__get_scene_info` and call it with the server's discovered schema:

```json
{"user_prompt":"Inspect the current Blender scene without changing anything."}
```

For direct Casper diagnostics, pass the same settings as JSON via `CASPER_CONFIGURE`:

```sh
CASPER_CONFIGURE="$(cat /path/to/casper-settings.json)" casper tools
printf '%s' '{"tool":"mcp__blender__get_scene_info","args":{"user_prompt":"Inspect only"}}' |
  CASPER_CONFIGURE="$(cat /path/to/casper-settings.json)" casper run
```

## Verification and supported scope

`oslo make test-mcp` tests both directions with actual SDK transports. External-client tests
cover generic stdio tools, Streamable HTTP tools/resources/prompts, schemas, structured results,
tool errors, allowlists, disabled tools, configuration errors, network denial, deadlines,
remote cancellation, SSE notifications and `Last-Event-ID` resumption, and the 4 MiB message
cap. A persistent socket test checks session reuse and child shutdown.
`oslo make verify` adds the complete Casper native gate; the family gate checks integration.

The live Blender acceptance used a deterministic local model provider: Magi discovered the
external schema, called the actual MCP server, and delivered the open scene's Cube, Light and
Camera back in a model tool message. Only the model's choice of tool was scripted; Casper,
the external server and the running Blender addon were real. No paid model or scene mutation.

From the NERV root, `oslo make test-mcp-blender` rebuilds the family and repeats this read-only
acceptance against an already-running addon on localhost:9876. It installs only a disposable
MCP server environment in `target/mcp-live-*`, never the addon, and keeps request/debug evidence.
The isolated test grants network access solely in its disposable configuration; it does not
change your own Magi settings or permissions.

This is standard **stdio and Streamable HTTP**, not an adapter for one application. It does
not claim support for every custom transport or optional MCP extension. Deprecated HTTP+SSE
endpoints, interactive OAuth enrollment, sampling, elicitation, subscriptions and task extensions
are not enabled; client capabilities do not advertise them. Bearer/API credentials can be
configured as HTTP headers. There is no website automation or Chromium runtime.

MCP image blocks and image resource blobs are validated, resized and returned as typed
`Ran.images` attachments. Their base64 is removed from `said`; ordinary text, structured data
and error flags remain in the model-readable result. Malformed, unsupported or excess images
produce an explicit omission notice and a failed result. The limit is four images per result,
20 MiB source bytes per image, and 128 KiB inline image bytes after resizing. Casper's own MCP
server emits standard MCP image content blocks for image-producing native tools.
