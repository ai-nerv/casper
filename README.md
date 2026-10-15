<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="misc/casper-dark.svg">
    <source media="(prefers-color-scheme: light)" srcset="misc/casper.svg">
    <img src="misc/casper.svg" alt="casper" width="180">
  </picture>
</p>

<p align="center"><em>The tools a coding agent runs, and the screen they draw on.</em></p>

<p align="center">
  <a href="https://claude.ai/code/artifact/cf3ff7f0-1c1d-472a-b01e-d08a854178b1"><strong>How the four fit together</strong></a> —
  a turn end to end, writing a tool, memory both directions, what may run
</p>

A separate binary a harness runs to do things: read a file, search a tree, run a command, and —
when a tool needs more than a line of output — hold rows on the harness's own screen and draw
into them. It knows nothing about models, turns or transcripts. Those belong to whatever harness
is using it.

## What it offers

```sh
casper tools                 # every tool it offers, as declarations a harness can register
casper run <tool>            # run one; the call arrives as JSON on stdin
casper surface <tool>        # hold rows on the harness's screen and draw into them
casper image <path>          # one PNG, JPEG, GIF or WebP through the jail, resized as an attachment
casper code <path>           # a source file's skeleton, or a directory's entries
casper grammar fetch|build|list [lang…]   # the parsers code reads with, built here
```

| | |
|---|---|
| `read` `write` `edit` | read a file, write one, change one — a highlighted file or a coloured diff on screen (Rust, C, C++, Go, Python, JavaScript, TypeScript, Lua, Zig, Nix, shell, TOML, YAML, JSON and Markdown; anything else plain); a PNG, JPEG, GIF or WebP image is read as a resized attachment, a BMP is refused |
| `code` | a source file's skeleton — imports, types, signatures, each with its line range — parsed with tree-sitter by casper run inside the jail, from grammars you build (below); a directory lists its entries |
| `ls` `tree` `sese` | look around a directory, draw its shape, and search a repository by meaning |
| `execute` | a Python script in which `read`, `code`, `ls` and `tree` are async functions: several looks, and filtering what they return, in one call |
| `shell` | run a command, and remember where it ran for the session that called: the process that started casper, never a session a call names, and nothing remembered without one |
| `web` | native Rust web search and bounded readable HTTP(S) page extraction, with source URLs |
| `tools` | the manual for everything below: a tree the model walks, and a page unlocks its tool |
| `screen` | an interactive program — a pager, an editor, `htop`, `git add -p` — in rows on the screen |
| `hexe` `oslo` `session` | ask the multiplexer, the shell, or the harness about themselves |
| `dino` `birdy` | two games, because a surface that can draw a game can draw anything |

The files, finding and `shell` tools are always in front of the model. The rest are *deferred*:
listed in the manual, and sent to the model only once a `tools` lookup reaches them, because every
card sent costs tokens on every request.

## Scripts

`execute` runs Python in [monty](https://github.com/pydantic/monty), a sandboxed interpreter with
no filesystem, network or imports of its own, inside casper's process. A tool declared
`script = true` is an async function there, called through the same path as a direct call: the
same jail, deadlines, output cap and log line. Only a tool with `needs = "read"` can be called that
way, whatever its declaration says, because the harness gates `execute` once, as a call with no
path, and never sees what the script calls. Writing, commands and the network stay with their own
tools. A script gets 30 seconds of its own time (time spent waiting on tools does not count) and
50 MiB of memory, and what it prints comes back cut at 2000 lines or 50 KiB.

It is a port of [maki](https://github.com/tontinton/maki)'s `code_execution`, with thanks to Tony
Solomonik; see `THIRD_PARTY.md`. `code` is a port of maki's `index`, likewise.

Every one of them is declared in `config/tools.lua`, in Lua, and nothing about them is compiled
in as a declaration. Network operations are implemented in Rust behind those declarations. A
tool of your own goes in the same file.

`code` is a port of [maki](https://github.com/tontinton/maki)'s `index` tool, under its MIT
license: see [THIRD_PARTY.md](THIRD_PARTY.md).

## Grammars for `code`

No tree-sitter grammar is compiled into casper. Like Helix's `hx --grammar fetch` and
`hx --grammar build`, casper fetches each grammar's source and builds it on your machine, from
the grammar list Helix pins: `grammars.lock` records Helix's release, each grammar's repository
and revision, and the tree-sitter runtime they are built against. Nothing is hosted by this
project, and the network is reached only when you run `fetch`.

```sh
casper grammar fetch           # clone every pinned revision, and the runtime, with git
casper grammar build           # compile each with $CC or cc into one program per language
casper grammar list            # each language: built, stale, fetched or missing
casper grammar build rust go   # or only the languages named
```

They land in `$XDG_DATA_HOME/casper/grammars`: sources under `src/`, and `bin/<language>`, a
program linked static where the machine can. It reads one file on stdin and writes its syntax
tree on stdout, capped at 2 MiB in, 2^20 nodes and 48 MiB out. `--dir DIR` fetches and builds
somewhere else. `CASPER_GRAMMARS` names more directories, separated as `PATH` is, searched before
that one: how the Nix set below is found. Inside the jail, `casper code` may read and run the
`bin` directories and nothing else of them.

A language whose grammar is not built answers with what to run; one built from another revision
than the lock pins says it is stale. `make grammars-lock` regenerates the lock from Helix's
`languages.toml`; it is never edited by hand. `make grammars` fetches and builds every grammar
into `target/grammars`, where the tests look when `CASPER_GRAMMARS` names nothing.

## MCP and web reading

Casper is also an **MCP client**: trusted `mcp_servers` settings connect to external servers
over stdio or Streamable HTTP. Their discovered tools appear in `casper tools` and Magi as
`mcp__<server>__<tool>`. Resources and prompts have namespaced bridge tools. This is generic;
there is no Blender-specific executor. See [MCP_CLIENT.md](docs/MCP_CLIENT.md) for setup and tests.

`casper mcp --root /path/to/project --tools=read,web` exposes configured tools using the
official Rust MCP SDK over stdio. Disabled and hidden tools are not exposed. Tool failures are
MCP error results, and cancelled or timed-out calls stop their worker processes.

The `web` tool searches for sources or fetches an HTTP(S) page and extracts readable text and
links. It uses a native Rust HTTP client, does not execute page scripts and needs no additional
runtime. Fetching is reading, not interactive website automation.

See [MCP_WEB.md](MCP_WEB.md) for configuration, trust boundaries, examples and HTTP tests.

## Surfaces

Most tools print and exit. Some need the screen: an editor, a pager, a picker, a permission
prompt, a game. A **surface** is a tool that asks the harness for a number of rows and then owns
them — it is handed keys and clicks, it draws each frame, and it ends when it says so or when
the person presses escape twice.

The harness decides how many rows; casper decides what goes in them. That split is the whole
protocol: a harness that knows nothing about pagers can host one, and a tool that knows nothing
about terminals can be drawn by any harness that can lend it rows.

A surface may also ask the harness questions it cannot answer itself:

```lua
local who   = casper.knows("session")                       -- which session, and where
local found = casper.knows("memories", { query = "deploy" }) -- what it remembers
```

`casper.knows` exists only inside a surface, and structurally so: a `run` is one exec whose
stdout is its reply, and a question written there would reach the harness as the tool's own
result.

## How this family talks

Three transports, two shapes, one encoding — written out because it was written out nowhere, and
five wires had grown five ways to say the same thing.

| Transport | When | Framing |
|---|---|---|
| **argv** | a question with an answer and nothing to hold open | one JSON object on stdout |
| **pipe** | a parent and the child it started | newline-delimited JSON, both directions |
| **socket** | anything may knock | four bytes of big-endian length, then JSON |

JSON is on all three. It is the *encoding*, not a transport.

**casper supports all three.** `serve` binds a local Unix socket under its runtime directory,
accepts same-user peers, and uses the jail profile supplied when the server was spawned. Calls
cannot replace that profile, and a call is answered only when it carries the key written on
`serve`'s standard input at spawn. With no jail profile, `run` on the socket runs commands as the
person, as far as their own shell would. The socket is not a remotely accessible transport.

A **call** is answered; an **event** is not:

```
->  {"call":"status","args":[]}
<-  {"ok":true,"family":1,"n":1,"result":[{"busy":false}]}

    {"event":"listening","at":"…"}
```

`result` is a **list** and `n` says how long it is: a sibling that unpacked a bare value would
read an answer as nothing at all. `family` says which revision the reply is written in — a reader
refuses a number newer than its own, by name, and accepts an older one or none. A refused call is
a *reply*, not a dropped connection.

**The tag key is `event`, everywhere, in both directions**, and `gate-wire` refuses any other.
The failure it prevents is silent: two of these wires exist as byte-identical copies in two
repositories, so when two spellings drift nothing fails and no test goes red — the surface simply
stops being answered.

## Command containment

`CASPER_JAIL=1` requests the conservative jail; JSON can add `write` directories, a shared
`tmp` directory, and unrestricted network permission through `reach: true`. An absent or empty
selector disables isolation, except under `casper mcp`, where an absent one is the conservative
jail and only an empty one disables it. A selector that does not parse refuses every command.
Magi supplies this selector and the working directory to ordinary tools, socket servers, and
surfaces; tool arguments do not grant permissions.

Ordinary commands and PTYs use the same preparation. Bubblewrap provides mount/process
namespaces and loads the syscall filter after setup. Without bubblewrap, Casper requires fully
enforced Landlock ABI 6 protections and seccomp; unavailable protection refuses the spawn.
Without reach, socket syscalls are denied. With it, the network opens and nothing local does:
`socket` takes IPv4, IPv6 and routing netlink (`NETLINK_ROUTE`), plus Unix sockets under
bubblewrap, whose mount namespace hides the runtime directory. Any other family, and a
`socketpair` of anything but Unix sockets, fails with `EAFNOSUPPORT`. There is no host Unix socket
by path, and no controlling terminal. Abstract Unix sockets stay closed wherever
the kernel can scope them (Landlock ABI 6, and a bubblewrap that is not setuid); where it cannot,
bubblewrap runs without that scope unless `CASPER_REQUIRE_CONTAINMENT=1` makes it a refusal.
Host-specific network allowances are not supported by this profile.

Isolated children receive a small environment allowlist rather than inherited provider keys or
shell startup variables. HOME, configured XDG credential locations, common credential stores,
and their resolved symlink targets are protected. This includes nested file and directory
symlinks inside the stores. Grants overlapping those stores or targets are refused. Regular
credential files with multiple hardlinks refuse the operation: pathname exclusions cannot
identify all their aliases. Nothing is unlinked, repaired or rewritten automatically.
Both backends expose only system/toolchain directories, the workspace, explicit grants and
temporary storage. Bubblewrap does not bind the host root. A credential store created outside
those grants after launch stays unreadable, just like an existing store. Filesystem grants that
contain a protected location are refused even when that location does not exist yet.
Mount sources are opened and their descriptor paths checked before spawn. Bubblewrap binds
those descriptors; Landlock grants those opened objects. Replacing a grant's symlink or directory
after capture does not substitute a new source. Setup descriptors are not inherited by the tool.
The bubblewrap executable is resolved independently of its PATH alias. A launcher with hardlink
aliases or inside any captured writable workspace, explicit grant or shared temporary directory
is refused. Changing a PATH symlink after preparation cannot substitute another executable.
The host-selected runtime and its dependencies outside those grants remain trusted; this is not
executable signature verification, and a failed launcher never triggers an unjailed retry.
Credential inspection reads metadata only, tracks visited directory identities and fails closed
on unreadable/unstable entries, dangling links, more than 16,384 visited entries or 128 nested
levels. A large or uninspectable store produces a refusal, never silently reduced protection.
The fallback gives each command private temporary storage unless trusted shared storage was
specified; it does not grant the whole host `/tmp`.

The two backends answer the same probes the same way, with two named differences. On the Landlock
path no Unix socket opens at all, since no mount namespace hides the runtime directory there. And
Landlock cannot carve a read-only file out of a writable tree, so where bubblewrap binds
`.magi.lua`, `.balthasar.lua`, `.git/config` and `.git/hooks` read-only beneath the workspace and
each write grant, casper reads the files that exist when the command starts and afterwards puts
back whatever it changed or removed, a new hook included, failing the command and naming each one;
one over 1 MiB, or reached through a link, refuses the command instead. On both paths one that did
not exist may be created. Until the command ends a reader can see its change, a process it leaves
running is not watched after it, and the person's own edit to one of them in the meantime is put
back too. What Landlock does keep read-only is everything outside the writable roots, such as the
home directory's dotfiles and the person's configuration.

`oslo make test-containment` uses synthetic credentials and local listeners to exercise both
backends, ordinary/PTY execution, terminal input/resize, and pinned Rust toolchain discovery.
Set `CASPER_REQUIRE_CONTAINMENT=1` to reject missing backend coverage. Real `casper surface`
processes also drive an interactive Bash prompt through input, resize, Ctrl+C and clean exit;
their captured draw grids are written to `target/surface/{bubblewrap,landlock}.jsonl`. These
are live surface-grid captures, not desktop screenshots or full TUI theme comparisons.
Parent-death, socket-server, late-created-store,
retargeted-grant, descriptor-lifetime and credential-alias cases run locally. The trusted host must
not inject credential contents or new aliases into a granted workspace while a child runs;
filesystem grants cannot distinguish those bytes from other authorized workspace data. Credentials
rotated or created at their protected locations outside the grants remain inaccessible. These
checks are not an exhaustive sandbox proof.

## Commands

The build is `.make.lua`, read by [oslo](https://github.com/termworks/oslo). At an oslo prompt in
this directory `make` is enough; anywhere else it is `oslo make`.

```sh
make                      # the recipes, with what each of them says it does
make build
make run --args='--help'
make test
make verify
make release --type patch
```

The directory environment is `.env.lua`, loaded when you `cd` here and unloaded when you leave. It
brings up the flake's dev shell and defines `_b`, `_r`, `_t`, `_v` and `_i` for the commands above.

## Requirements

`.make.lua` and `.env.lua` are read by [oslo](https://github.com/termworks/oslo), which provides
both the `make` task runner and the directory environment. Without it, `make` is whatever is on
your `$PATH` and `.env.lua` is never loaded.

```sh
# at an oslo prompt in this directory
make build

# anywhere else
oslo make build
```

## Nix

Each release tag publishes a static build for x86_64 and aarch64 Linux to the `ai-nerv` cache,
with `config/` under `share/casper/`. casper reads its declarations from `$XDG_CONFIG_HOME/casper`,
so copy `share/casper/config/` there, as `make install` does from a checkout.

```sh
cachix use ai-nerv
nix run github:ai-nerv/casper -- verbs
nix build github:ai-nerv/casper
```

The flake names the cache itself, so `--accept-flake-config` does the same as `cachix use`. In
another flake, take `inputs.casper.packages.${system}.default`, and configure the cache on the
consuming machine: an input's `nixConfig` is not inherited.

`casper-grammars` is every pinned grammar, prebuilt: each fetched by Nix with the lock's hash and
built static with casper's driver, under `share/casper/grammars/bin`. Point `CASPER_GRAMMARS` at
`share/casper/grammars` and `code` needs no `casper grammar` at all; the dev shell does this.

```sh
nix build github:ai-nerv/casper#casper-grammars
export CASPER_GRAMMARS=$PWD/result/share/casper/grammars
```

Here, `make nix-build` builds it into `result/`, and `make nix-check` checks the flake.
