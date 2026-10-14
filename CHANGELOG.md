# Changelog

## [0.6.0] - 2026-10-14

### <!-- 1 -->🐛 Bug Fixes

- Hide a userdata's metatable
- Remove debug from the declaration VM
- Bound Lua-to-JSON by nodes and cycles
- Refuse __gc metatables
- Refuse precompiled chunks
- Hold every VM under a memory ceiling
- Hand the session as casper.session
- Runtime dir by the private-dir rule
- Refuse a call from a newer family
- Rustls 0.23.46, yoke-derive 0.8.4
- Remember the directory per session
- Private log file, arguments only counted
- Allow only network socket families
- Refuse a newer family; close on bad reply
- Reach hexe and oslo sockets by name
- Private runtime dir, lock, stale rule
- Cap frames, never build paths from UID
- A deadline, group kill and pipe grace
- Keyed calls, a thread each, deadlines
- One 16 MiB cap, on read and write
- Keep cwd memory per project, as data
- List only the family's socket directories
- Run tools in the jail floor by default
- Deny family config and credential stores
- Refuse a CASPER_JAIL that does not parse
- Reach opens the network, not local sockets
- Let the family's contract names through
- Drop string escapes and controls
- Bind project config files read-only
- Put an instruction budget on every VM
- Dial only the family's own sockets

### <!-- 2 -->🚜 Refactor

- No panics on input
- No panics on input
- No panics on input
- No panics on held output
- No panics on input
- No panics on input
- No panics on bind arguments
- No panics on frame bounds

### <!-- 3 -->📚 Documentation

- The three VMs and getmetatable
- The memory key and socket families
- Name the new sandbox refusals
- One contract in all four
- One text across the family
- Install from the ai-nerv Nix cache
- Only the clients ride in the binary
- Say what bounds the socket
- What bounds the socket door
- The serve key, reach, and mcp's floor
- The socket is keyed; unjailed, a shell
- Drop a link to a private item
- Frames, sockets, deadlines, callers

### <!-- 5 -->🎨 Styling

- Keep the cut note to one line

### <!-- 6 -->🧪 Testing

- Hold a one-slice chunk to the ceiling
- One gate-family in all four
- Sweep what a caller left by its dir
- Hold the VM's names to an allowlist
- Wire gate checks the cap and dir rule
- Probe a live serve for refusals
- Hold both backends to one verdict table
- Retry a busy refusal, not panic on it
- A bind that names no path is refused
- Run the family corpus against serve
- Sandbox gate checks dial, list, budget
- Fail, not skip, when containment is required

### <!-- 7 -->⚙️ Miscellaneous Tasks

- Pin actions by commit
- Publish tagged builds to ai-nerv

### Build

- Enable flakes for the dev shell
- Cargo deny and audit in verify and CI
- Read the version from Cargo.toml
- A static package with its config
- Nix-build, nix-check, download check
- Deny expect and indexing outside tests
- Audit in verify where installed
- Turn overflow checks on

## [0.5.6] - 2026-10-14

### <!-- 9 -->◀️ Revert

- Remove the Chromium browser tools

## [0.5.5] - 2026-09-29

### <!-- 0 -->⛰️  Features

- Add native Chromium tools

## [0.5.4] - 2026-09-29

### <!-- 0 -->⛰️  Features

- Add MCP server and native web tools

## [0.5.3] - 2026-09-29

## [0.5.2] - 2026-09-23

## [0.5.1] - 2026-09-23

## [0.5.0] - 2026-09-23

### <!-- 3 -->📚 Documentation

- Say an unlocked tool is called as a tool

## [0.4.0] - 2026-09-23

## [0.3.0] - 2026-09-21

### <!-- 0 -->⛰️  Features

- A tool says which branch it sits under
- Ls, tree and sese
- The side edited last wins
- Every step into the family's log
- Stubs, ways back and keep hints

### <!-- 1 -->🐛 Bug Fixes

- Ask a model that decides, a question a passage
- Ls, tree and sese are always available
- Read the toolchain wherever it lives

### <!-- 3 -->📚 Documentation

- Say it locates, and never answers
- Plan as the layout fallback
- Layout, jobs and notes verbs

## [0.2.0] - 2026-09-15

### <!-- 0 -->⛰️  Features

- Bat's grammars for toml, nix and ts
- Highlight read, write and edit
- Float place, doom and a games picker
- A prettier permission prompt
- Read/write/edit tools and a tools manual
- A shared /tmp for a project's commands
- A socket door for the tool surface
- Landlock walls when bwrap is absent
- A seccomp filter denies ptrace and io_uring
- A grant widens the profile
- Run a tool command in a bubblewrap sandbox
- What this program is for, as a contract
- Casper writes one file and removes none
- Install the declarations instead of embedding them
- Discover, acknowledge and version the surface
- Implement the family contract
- Casper takes configuration, and answers the family contract
- Answer in cbor when asked

### <!-- 1 -->🐛 Bug Fixes

- Keep the cwd in /tmp under the jail
- A failed read drops the connection
- A declaration's print is not the wire
- A program does not pick casper's memory
- A refusal names its kind, like its siblings
- A reply naming no revision predates it
- One door, and a fault on the reply
- A note beside SAFETY is still a note
- An empty install still ends the frames
- A write that failed is not a panic
- Failures that were swallowed are said out loud
- A reply that did not land is not a success
- A wrapped command dies with casper too
- A tool's own process dies with casper too
- A call does not outlive the magi that asked
- A declared setting must change something
- The gate skip list was not valid sh

### <!-- 2 -->🚜 Refactor

- Capture the jail context per call
- Build jail as data, arm it in tied

### <!-- 3 -->📚 Documentation

- Casper's socket door, and its jail
- How a tools program is configured
- Sessions is core to the memory role
- Both client shapes are correct
- The export corpus is not an answer
- The tool door, and the gate that knows it
- The reachable set, as it is
- The rule stated, not defended
- Terminal facts, not the hunt for them
- What is pinned, not how it broke
- The shape stated once, not argued
- The declaration surface, described once
- Comments describe, they do not argue
- A lua tool declares its transport
- Link the wiring walkthrough

### <!-- 6 -->🧪 Testing

- A test reads no directory of the person's
- The acknowledgement is held by a test
- A leaked process is a leak too
- The suite leaves nothing behind

### <!-- 7 -->⚙️ Miscellaneous Tasks

- The one door, said where it was not
- A gate for the comment budget

## [0.1.0] - 2026-09-06

### <!-- 7 -->⚙️ Miscellaneous Tasks

- Merge develop to main
- Probe after toggling actions
- Probe whether a push triggers a run
- A run can be started by hand

