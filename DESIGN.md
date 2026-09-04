# casper — the tooling interface

casper is where the tools live. magi keeps `read`, `write` and `edit` as a floor it can never be
without; everything else — `ls`, `shell`, `oslo`, `hexe`, `cat`, `patch`, and the things that ask
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
addition, for every tool, because it has nothing else to go on. That guess is wrong for a `shell`
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
a `cat` has a result and a view; a `shell` has a result and no view. One field could not hold all
three without meaning something different each time.

## A surface: rows a tool asks for and fills itself

An `ask` is still magi drawing. The picker's shape, its keys, its layout — all of that is magi's
idea of what a question looks like, and a tool that wanted to ask differently could not.

So the general form is not a question at all. It is **space**.

A tool says *"I need five rows"*. magi reserves five rows, forwards input to whoever holds them,
and blits back whatever spans come out. It does not know what is in them. A permission prompt, a
file picker, a diff the person can scroll, a form, a game — magi cannot tell those apart and does
not have to. That is the whole point: the list of things that can appear there stops being a list
somebody has to extend.

```
magi → surface   {"open": {"rows": 5, "cols": 92}}
surface → magi   {"draw": [[{"role":"title","text":"run rm -rf build?"}], …]}
magi → surface   {"key": "j"}
surface → magi   {"draw": [ … ]}
magi → surface   {"key": "enter"}
surface → magi   {"done": {"answered": "once"}}
```

Rows are the existing painted vocabulary — roles, not colours — so a surface and a `cat` agree on
screen without either knowing about the other's palette.

### A key arrives twice

Where the Kitty keyboard protocol is live, one keystroke is *two* frames — `down` and then `up` —
and nothing about the frame makes that obvious. A list that acts on both moves two rows for one
press of the arrow, which is precisely the bug that shipped in the permission prompt.

Nothing in the protocol says which kind of tenant you are, so the safe reading is the short one:

```lua
local key = casper.tapped(event)     -- a press or a repeat, lower-cased; nil for anything else
```

A repeat counts, because it says the key is still down — that is what makes holding an arrow
scroll rather than needing a tap a row. A release comes back as `nil`, and so does a tick, a
resize or the pointer, so one call answers "is this a keypress, and which".

Two tenants should *not* use it, and both are in `config/tools.lua` as the worked examples:

- one where holding a key means something — both games read `event.state` themselves, because the
  release is what ends a jump;
- one where the character matters as typed — `event.key` keeps its case, and `casper.tapped` does
  not.

**magi reserves, the tenant draws.** magi owns *how much* room there is, because only magi knows
what else is on the screen; it clips to the reservation and never grows it mid-frame. Everything
inside is the tenant's.

### The rows are a screen, not an echo of the keyboard

A surface that could only be typed at is a picker with extra steps. So the pointer crosses too:

```
magi → surface   {"to": "mouse", "kind": "press", "button": "left", "row": 2, "col": 11}
```

Two rules, and both are about what magi *does not* say:

- **The coordinates are the surface's own.** Row 0 is its first row. magi knows where the
  reservation landed on screen and never passes that on — those rows move whenever the prompt
  grows a line, and a tenant that had been told its own `y` is one magi could no longer place.
- **Nothing outside the reservation is forwarded.** A click on the transcript above is the
  transcript's; magi keeps its wheel, its fold handles and its drag-selection while a game is open
  below them. The tenant needs no bounds check, because out-of-bounds never arrives.

`press` / `drag` / `release` / `moved` / `scroll_up` / `scroll_down`, with a button on the three
that have one. That is enough for a list you hover and click, a diff you scroll, and a button you
*hold* — which on a terminal whose keyboard cannot report a release is the only hold there is,
since the mouse protocol has always said when a button came up.

And back the other way, the terminal's own caret:

```lua
return { lines = rows, cursor = { row = 0, col = 6 } }
```

Optional, and almost always absent — a game wants nothing blinking in its picture. A tenant that
draws a field somebody types into asks for it, because the block a surface paints for itself is a
*picture* of a cursor: an IME candidate window and a screen reader both follow the real one, and
while a surface holds the keyboard the prompt is not where anybody is typing.

### A screen: rows with a real terminal in them

Once the rows are a screen, the obvious tenant is a *program*. `shell` runs a command and reads
what it printed, which is right for `make` and useless for anything that draws: a pager waits on a
key that never comes, `htop` sees no terminal and refuses, an editor opens on nothing.

So a declaration may name a program instead of drawing:

```lua
casper.tool("screen", {
  needs  = "run",
  run    = function(args) return casper.surface{ rows = 16, about = "…", tick = 33 } end,
  screen = function(args, size) return { command = "sh", args = { "-c", args.command } } end,
})
```

casper opens a pty of exactly the granted size, spawns the command on it, types in what the person
types, and reads what comes back through a terminal emulator. **Nothing about the wire changes.**
What goes out is the same rows of spans a game sends, so the harness cannot tell `htop` from the
dinosaur and does not have to.

|  | `surface` | `screen` |
|---|---|---|
| returns | a function, called per frame | a table, read once |
| fills the rows | Lua, span by span | whatever is on the pty |
| state lives in | the closure's upvalues | the program |
| ends when | it answers | the program exits |

**Why casper and not the harness.** Running programs is casper's whole job, and the reason it has
a spawn link rather than a socket verb — *a socket that runs commands is remote code execution*. A
harness that opened its own pty would be back to spawning commands, which is the thing the split
exists to prevent.

Three consequences worth stating:

- **A `screen` always ticks, and casper is what makes sure of it.** A drawing redraws when a key
  arrives and needs nothing else; a program paints whenever it likes, and with no tick nothing
  goes looking for what it painted. So a tool that declares a `screen` and names no rate is given
  thirty a second — filled in, never overridden, because a declaration one line short of working
  reads as a hung tool rather than as a missing field.
- **One instruction, spelled two ways.** `ESC [ r ; c H` and `ESC [ r ; c f` both position the
  cursor; the emulator implements the first and drops the second. `btop` uses the second — 452
  times in two seconds — so it came out wrapping mid-word while `top` was perfect. The byte is
  rewritten on the way in, by a state machine rather than a search, because the pty splits
  sequences across reads and an `f` in a program's output is a letter.
- **Keys travel by name and are built back into bytes** — `enter` becomes `\r`, `f5` becomes
  `ESC [ 15 ~`, and the arrows follow whichever mode the program asked for. A name is the only
  form both a Lua table and a pty can read, which is why the harness sends one.
- **Escape twice closes a surface, once is forwarded.** It used to be once, which is how a person
  escapes a tenant that has stopped answering — and `esc` is a key `vim` very much wants. Every
  drawing tenant answers the first one anyway, so it never sees a second.

### The one thing that does not move

**The answer to a permission lands in magi's ledger, not in the surface's return value.**

A surface that could return "allowed" is a sibling granting itself permission, which is the
failure the ledger exists to prevent — see *casper describes, magi decides*. So the split is:

- magi decides **that** a question exists, and what is being decided about — it holds `Ops::allow`
  and the standing grants.
- the surface decides **how it looks** and collects the keystroke.
- the answer travels back as an *id*, and magi maps that id onto its own scopes.

A surface is a renderer with an input channel. It is never an authority.

### Why this needs a held connection

A call today is one exec: request on stdin, reply on stdout, exit. A surface redraws per
keystroke, so magi holds `casper surface --stdio` open for the life of the reservation and
exchanges frames over it — the same frames, over a spawn that lives longer than one call. One exec
per keypress would work for a picker and not for anything that animates.

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
A turn is a stream — a `shell` writes output for a minute — so `run` streams a line per event and
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
6. The rest — `ls`, `shell`, `oslo`, `hexe` — which by then is declaration, not plumbing.

Steps 1–3 are the interface. Step 4 is the thing that makes it look right. Step 5 is what makes
"anything can ask the person" true, and it is the one that removes code from magi rather than
adding it.
