# casper — the tooling interface

casper is where the tools live. magi keeps `read`, `write` and `edit` as a floor it can never be
without; everything else — `ls`, `bash`, `oslo`, `hexe`, `cat`, `patch`, and the things that ask
the *person* something — is casper's.

## The one idea

**A tool has two faces, and they are not the same content.**

| face | who reads it | what it is |
|---|---|---|
| **said** | the model | text, in the model's terms |
| **shown** | the person | a painted view, in magi's palette |

A `cat` of a Rust file *says* the file's text and *shows* it highlighted. A `patch` says a unified
diff and shows it in the same green and red magi already paints an `edit` with. A permission
question says nothing at all to the model — it is not a tool result yet — and shows a picker.

Everything below follows from keeping those two apart. Today they are one `String` and the
renderer *guesses*: `magi-tui/src/transcript/tool.rs` colours any line starting with `+` as an
addition, for every tool, because it has nothing else to go on. That guess is wrong for a `bash`
running `git log --oneline`, and it is unavailable to `bat`, whose output is already ANSI by the
time anybody sees it.

## Paint carries meaning, not colour

The thing that makes "the `cat` output and the `patch` output look right together" true is that
**neither of them chooses a colour**. A tool emits *roles*; magi resolves roles against its own
`magi.ui` palette, the same one the prompt box and the footer use.

```json
{"lines": [[{"role": "removed", "text": "-    was"}],
           [{"role": "added",   "text": "+    now"}]]}
```

A closed vocabulary, because an open one is a second palette:

- **prose** — `text` `muted` `dim` `title` `path`
- **outcome** — `ok` `warn` `error`
- **change** — `added` `removed` `marker` `context`
- **code** — `keyword` `string` `number` `comment` `type` `func`

`patch` emits `added`/`removed`. `bat` emits `keyword`/`string`/`comment`. Both land in one
palette, so a diff of a Rust file and a `cat` of it agree — which is the whole ask.

**A tool that has nothing to say about structure emits nothing.** `shown` is optional; absent
means "draw `said` as plain text", which is what every tool does today. Nothing has to be ported
for casper to be useful, and a tool grows a view when somebody writes one.

### Where ANSI goes

`bat` does not speak roles; it speaks ANSI. So the *adapter* translates, and the adapter is Lua.
This is the point of casper having a VM: `bat --color=always | <lua that maps SGR codes to roles>`
is a table somebody can edit when their theme changes, not a Rust match nobody can reach. A tool
whose adapter is missing falls back to stripping ANSI and emitting `text` — legible, unstyled,
never wrong.

## Asking the person is a tool

Permission is the first one. Today magi owns the prompt, the scopes, the ledger and the picker.
Under casper it becomes a tool that **returns a question instead of a result**:

```json
{"shown": {"ask": "run `rm -rf build`?",
           "options": [{"id": "once", "label": "Allow once"},
                       {"id": "always", "label": "Allow any rm"},
                       {"id": "no", "label": "Deny"}]}}
```

magi draws it with the picker it already has, sends the answer back, and the tool resumes. That
is the same shape a *selection* tool needs — "which of these files?" — and a confirmation, and a
form. One mechanism, and the list of things that can ask the person stops being a list.

**This is why `said` and `shown` had to be separated first.** A question has a view and no result;
a `cat` has a result and a view; a `bash` has a result and no view. One field could not hold all
three without meaning something different each time.

## What crosses, and how

The family contract, unchanged: 4-byte big-endian length, JSON body, replies
`{"ok":true,"n":N,"result":[…]}`, a refusal is `ok:false` with a fault and never a dropped
connection, the connection stays open, `verbs()` ships from v1, peer identity from `SO_PEERCRED`.

**But not `run`.** The skill is explicit — *a socket that runs commands is remote code execution* —
and running commands is casper's entire job. So the surface splits by trust:

| link | verbs | why |
|---|---|---|
| **socket** | `verbs` `tools` `needs` | read-only. Anyone the walls allow may ask what exists. |
| **spawn** (argv + stdin) | `run` `configure` | the parent could have run the command itself. |

magi spawns `casper run --json` and writes the call on stdin, exactly as it spawns `melchior ask`.
A turn is a stream — a `bash` writes output for a minute — so `run` streams a line per event and
exits, which is the shape the broker already reads.

## Declaring a tool

Lua, following the family's config style: settings assigned, descriptions handed to a registrar,
the file returns nothing.

```lua
casper.tool("cat", {
  description = "Read a file, with syntax highlighting.",
  parameters  = { type = "object", properties = { path = { type = "string" } },
                  required = { "path" } },
  needs       = { verb = "read" },        -- what magi must permit before this runs

  run = function(args)
    return { said = casper.run("bat", { "--color=always", args.path }) }
  end,

  -- Optional. Absent means "plain text", which is what everything does today.
  paint = casper.paint.ansi,              -- or a function returning painted lines
})
```

`needs` is how a tool says it wants permission, so the ledger stays magi's — casper describes
what a tool would do and never decides whether it may.

## Order of work

1. **The contract**, in `magi-proto::tooling`, with round-trip tests. Nothing works until both
   sides agree, and a shape settled late is two encoders that already drifted.
2. **casper answering `verbs`/`tools`** over the socket, with a Lua VM and one declared tool.
3. **`run` over the spawn link**, streaming, with `said` only.
4. **`shown`**: the paint vocabulary, the ANSI adapter, `cat` and `patch`.
5. **The ask**: permission moves out of magi.
6. The rest — `ls`, `bash`, `oslo`, `hexe` — which by then is declaration, not plumbing.

Steps 1–3 are the interface. Step 4 is the thing that makes it look right. Step 5 is what makes
"anything can ask the person" true, and it is the one that removes code from magi rather than
adding it.
