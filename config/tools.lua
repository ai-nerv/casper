-- The tools casper offers.
--
-- A declaration is a description and a function. The description says what the tool is, what it
-- takes, and which permission verb it acts under; the function runs it and says what it produced.
--
-- **A tool never chooses a colour.** It says what its output *means* -- `added`, `keyword`, `path`
-- -- and the harness resolves that against its own palette. That is what makes a `patch` and a
-- highlighted `cat` agree on screen instead of being two programs' idea of green.

-- Which 256-colour index means what, for programs that speak ANSI.
--
-- The join between a pager with a theme and a vocabulary with meaning, and the reason it lives
-- here: when your `bat` theme changes, this is a table you edit rather than a match in Rust you
-- cannot reach. A colour missing from it is drawn as ordinary text -- legible, never wrong.
casper.theme = {
  -- Truecolour, which is what `bat` and `delta` send by default. Read off this machine's own
  -- output rather than guessed: a theme that names colours nothing emits matches nothing, and
  -- the symptom is a file that comes out perfectly legible and completely unhighlighted.
  --
  -- Mapped by what the highlighter *uses* a colour for, not by what it looks like. This theme's
  -- red is its variables and parameters, and read as `error` it drew `self`, `ops` and every
  -- argument name in the colour a failed call is drawn in.
  ["#abb2bf"] = "text",    ["#5c6370"] = "comment", ["#98c379"] = "string",
  ["#c678dd"] = "keyword", ["#e06c75"] = "text",    ["#e5c07b"] = "type",
  ["#61afef"] = "func",    ["#d19a66"] = "number",  ["#56b6c2"] = "keyword",

  -- The sixteen, for programs that still speak them.
  [1] = "error", [2] = "added",   [3] = "warn",  [4] = "path",
  [5] = "keyword", [6] = "type",  [9] = "removed", [10] = "added",
}

do -- cat
  -- `bat` when it is installed, `cat` when it is not. The fallback is not a lesser tool: it is
  -- the same file, unhighlighted, and a model reading it cannot tell the difference. Only the
  -- person can, which is exactly the half `shown` is for.
  casper.tool("cat", {
    description = "Read a file. Shown with syntax highlighting where the machine has it.",
    parameters = {
      type = "object",
      properties = {
        path = { type = "string", description = "The file to read." },
      },
      required = { "path" },
    },
    needs = "read",

    run = function(args)
      local pretty = casper.exec("bat", {
        "--color=always", "--style=plain", "--paging=never", args.path,
      })
      if pretty.code == 0 then
        -- What the model reads is the file, not the escapes: `said` is the plain text and
        -- `shown` is the painted one. Handing the model ANSI would spend its context on
        -- terminal control codes.
        local plain = casper.exec("cat", { args.path })
        return {
          said = plain.code == 0 and plain.out or pretty.out,
          shown = casper.paint.ansi(pretty.out, casper.theme),
        }
      end

      local plain = casper.exec("cat", { args.path })
      if plain.code ~= 0 then
        return { said = plain.err, failed = true }
      end
      return { said = plain.out }
    end,
  })
end

do -- patch
  -- The tool the palette argument is really about. A diff has structure a reader knows by heart,
  -- so it is read from the line's first character rather than from whatever colour some `diff`
  -- implementation chose -- and it comes out in the same green and red an `edit` already draws.
  casper.tool("patch", {
    description = "Show the difference between two files, as a unified diff.",
    parameters = {
      type = "object",
      properties = {
        old = { type = "string", description = "The file as it stands." },
        new = { type = "string", description = "The file to compare it with." },
      },
      required = { "old", "new" },
    },
    needs = "read",

    run = function(args)
      local done = casper.exec("diff", { "-u", args.old, args.new })
      -- `diff` exits 1 when the files differ, which is the ordinary case and not a failure.
      if done.code > 1 then
        return { said = done.err, failed = true }
      end
      if done.out == "" then
        return { said = "the two files are identical" }
      end
      return { said = done.out, shown = casper.paint.diff(done.out) }
    end,
  })
end

do -- ls
  -- The flags magi's own declaration used, kept exactly: one entry a line, a trailing `/` on
  -- directories, dotfiles included, no colour. A tool that moved here and quietly changed what
  -- it returns would be a model reading different output for the same call.
  casper.tool("ls", {
    description = [[
  List a directory. Returns one entry a line, with a trailing `/` on directories.]],
    parameters = {
      type = "object",
      properties = {
        path = { type = "string", description = "The directory. Defaults to the current one." },
      },
    },
    needs = "read",

    run = function(args)
      local done = casper.exec("ls", { "-1", "-p", "-A", "--color=never", args.path or "." })
      if done.code ~= 0 then
        return { said = done.err, failed = true }
      end
      return { said = done.out }
    end,
  })
end

do -- find
  -- `fd` when it is there, `find` when it is not. The same shape `cat` uses for `bat`: the
  -- better tool where the machine has it, and something that always works where it does not.
  casper.tool("find", {
    description = [[
  Find files and directories by name. Returns one path a line.

  Honours .gitignore where `fd` is installed. Results are capped; narrow the glob or the path
  rather than raising the limit.]],
    parameters = {
      type = "object",
      properties = {
        glob = { type = "string", description = "Name pattern, e.g. `*.rs` or `Cargo.toml`." },
        path = { type = "string", description = "Where to look. Defaults to the current directory." },
        limit = {
          type = "integer", minimum = 1, maximum = 5000, default = 1000,
          description = "Most paths to return.",
        },
      },
    },
    needs = "read",

    run = function(args)
      local where = args.path or "."
      local limit = args.limit or 1000
      local glob  = args.glob or "*"

      local done = casper.exec("fd", {
        "--color=never", "--glob", "--max-results=" .. tostring(limit),
        glob, "--search-path=" .. where,
      })
      if done.code == 0 then return { said = done.out } end

      -- `find` has no result cap of its own, so the limit is applied after it rather than not
      -- at all: an uncapped listing of a large tree is a turn that cannot be sent.
      local fell = casper.exec("find", { where, "-name", glob })
      if fell.code ~= 0 then
        return { said = fell.err ~= "" and fell.err or done.err, failed = true }
      end
      local kept, n = {}, 0
      for line in fell.out:gmatch("[^\n]+") do
        n = n + 1
        if n > limit then break end
        kept[#kept + 1] = line
      end
      return { said = table.concat(kept, "\n") }
    end,
  })
end

do -- grep
  -- `rg` honours ignore files and is faster; `grep` is everywhere. Both go through the same
  -- gate, because the gate is magi's and it is asked before either of them runs.
  casper.tool("grep", {
    description = [[
  Search file contents, preferring ripgrep and falling back to grep.

  Honours .gitignore when ripgrep is available. Returns `path:line:text`, one match a line.]],
    parameters = {
      type = "object",
      properties = {
        pattern = { type = "string", description = "The pattern to search for." },
        path = { type = "string", description = "Where to search. Defaults to the current directory." },
        limit = {
          type = "integer", minimum = 1, maximum = 1000, default = 200,
          description = "Most matches to return per file.",
        },
      },
      required = { "pattern" },
    },
    needs = "read",

    run = function(args)
      local where = args.path or "."
      local limit = tostring(args.limit or 200)

      local done = casper.exec("rg", {
        "--line-number", "--no-heading", "--color=never",
        "--max-count=" .. limit, "--regexp=" .. args.pattern, where,
      })
      -- `rg` exits 1 when it matched nothing, which is an answer and not a failure.
      if done.code == 0 then return { said = done.out } end
      if done.code == 1 and done.err == "" then return { said = "no matches" } end

      local fell = casper.exec("grep", {
        "-rnI", "--exclude-dir=.git", "--max-count=" .. limit, "-e", args.pattern, where,
      })
      if fell.code == 0 then return { said = fell.out } end
      if fell.code == 1 then return { said = "no matches" } end
      return { said = fell.err ~= "" and fell.err or done.err, failed = true }
    end,
  })
end

do -- shell
  -- **The tool that asks, and the one that remembers.**
  --
  -- Running a command is the thing a person most wants a say over, so this is where the asking
  -- mechanism earns itself: the first call returns a *question* and no result, the harness draws
  -- it, and the same declaration runs again with the answer.
  --
  -- It also has to keep a working directory. `cd build` and then `make` has to work, and that is
  -- what magi's own `shell` bought with a long-lived process. casper cannot hold one — a call is
  -- one exec of `casper run` and the process is gone when it answers — so the *directory* is
  -- remembered on disk instead of the shell that was in it. That is less than a live shell keeps
  -- (an exported variable does not survive, nor a shell function) and it is the part that
  -- actually gets used.
  local function remembered()
    local runtime = os.getenv("XDG_RUNTIME_DIR") or "/tmp"
    return runtime .. "/casper/cwd"
  end

  local function where()
    local done = casper.exec("cat", { remembered() })
    if done.code ~= 0 then return nil end
    local path = done.out:gsub("%s+$", "")
    if path == "" then return nil end
    return path
  end

  casper.tool("shell", {
    description = [[
  Run a command in a shell.

  The working directory is kept between calls, so `cd build` and then `make` works.]],
    parameters = {
      type = "object",
      properties = {
        command = { type = "string", description = "The command to run." },
      },
      required = { "command" },
    },
    needs = "run",

    -- **It does not ask.** It used to, with `casper.ask`, and `needs = "run"` above already sends
    -- the same command through the harness's ledger -- so one command raised two questions, and
    -- answering the first only bought you the second. casper says what a tool would do; the
    -- harness decides whether it may, and it is the one holding the standing grants a person has
    -- already given.
    run = function(args)
      -- The remembered directory, then the command, then wherever it ended up. Written every
      -- time rather than only after a `cd`, because a command may change directory in ways
      -- nothing here can see -- a script, a `pushd`, a `cd` inside an `if`.
      local held = where() or args.cwd or "."
      local kept = remembered()
      local done = casper.exec("sh", {
        "-c",
        ("mkdir -p \"$(dirname %q)\"; cd %q 2>/dev/null || cd .; { %s; }; code=$?; pwd > %q; exit $code")
          :format(kept, held, args.command, kept),
      })

      local out = done.out
      if done.err ~= "" then
        out = out == "" and done.err or (out .. "\n" .. done.err)
      end
      if done.code ~= 0 then
        return { said = out .. "\n(exit " .. tostring(done.code) .. ")", failed = true }
      end
      return { said = out }
    end,
  })
end

do -- pwd
  -- What `shell` is remembering, so a model can ask rather than run a command to find out.
  casper.tool("pwd", {
    description = "The directory `shell` will run its next command in.",
    parameters = { type = "object" },

    run = function()
      local runtime = os.getenv("XDG_RUNTIME_DIR") or "/tmp"
      local done = casper.exec("cat", { runtime .. "/casper/cwd" })
      if done.code ~= 0 then
        return { said = "nothing has run yet, so it is wherever the session is rooted" }
      end
      return { said = done.out:gsub("%s+$", "") }
    end,
  })
end

do -- screen
  -- **The other kind of tenant: rows with a real terminal in them.**
  --
  -- `shell` runs a command and reads what it printed, which is right for `make` and useless for
  -- anything that draws. A pager waits on a key that never comes; `htop` sees no terminal and
  -- refuses; an editor opens on nothing. So this declares a `screen` rather than a `run`: casper
  -- puts the command on a pty exactly the size of the rows the harness granted, types into it
  -- what the person types, and hands back what it painted. Nothing here draws, and neither does
  -- the harness -- the program does.
  --
  -- One rule a `screen` declaration has to keep: `needs = "run"`. It is a command, and it goes
  -- through the same ledger `shell` does. The tick it also needs -- a program paints whenever it
  -- likes, and something has to go and look -- is filled in by casper, because a declaration one
  -- line short of working reads as a hung tool rather than as a missing field.
  local MOST_ROWS = 30

  casper.tool("screen", {
    description = [[
  Run an interactive terminal program in rows on the screen: a pager, an editor, `htop`, `git
  add -p`. The person can type at it and click in it, and it ends when the program does or when
  they press escape twice.

  Use this where `shell` cannot: anything that draws, waits for a keypress, or needs a terminal.
  Use `shell` for anything that just prints and exits — a program run here holds the screen until
  somebody closes it.

  The result is what the program left on screen when it ended.]],
    parameters = {
      type = "object",
      properties = {
        command = { type = "string", description = "The command to run, as a shell would." },
        rows = {
          type = "integer", minimum = 3, maximum = MOST_ROWS,
          description = "How tall it should be. Defaults to 16.",
        },
      },
      required = { "command" },
    },
    needs = "run",

    run = function(args)
      local rows = math.min(MOST_ROWS, math.max(3, math.floor(tonumber(args.rows) or 16)))
      return casper.surface{
        rows = rows,
        -- What a harness with no screen says instead. `magi -p` cannot draw rows and cannot ask
        -- anybody, so it declines with this rather than waiting on a program nobody can see.
        about = "running " .. tostring(args.command),
        -- No `tick`: a tool declaring a `screen` is given one. Name a slower one here if a
        -- program is worth watching less often than thirty times a second.
      }
    end,

    -- Called once, before the program starts, and never again. It returns *data* -- there is no
    -- per-frame Lua here, because a pty is driven by casper and asking a declaration thirty times
    -- a second what to run would be thirty answers that never change.
    screen = function(args)
      return { command = "sh", args = { "-c", tostring(args.command) } }
    end,
  })
end

do -- hexe
  -- The client arrives as source in `casper.clients`: a declaration cannot open files. It is
  -- hexe's own stub, copied rather than reimplemented — two implementations of one protocol is
  -- where the two drift, and the family's guidance is explicit that a stub is plain Lua so that
  -- siblings can copy it.
  local function client()
    local source = casper.clients and casper.clients.hexe
    if not source then return nil, "hexe's client library is not installed" end
    local chunk, why = load(source, "hexe.lua")
    if not chunk then return nil, why end
    return chunk(casper.stream)
  end

  casper.tool("hexe", {
    description = [[
  Inspect the terminal multiplexer this session is running under: which panes and tabs exist,
  what is running in each, and where they are rooted.

  Use it to find out what the user is looking at. It reads; it does not rearrange anything.]],
    parameters = {
      type = "object",
      properties = {
        what = {
          type = "string",
          enum = { "panes", "tabs", "session", "verbs" },
          description = "Which question to ask. Defaults to panes.",
        },
      },
    },
    -- It reaches another process over a socket, which is the verb a person would want a say
    -- over even though nothing is executed.
    needs = "reach",

    run = function(args)
      local hexe, why = client()
      if not hexe then return { said = tostring(why), failed = true } end

      local mux, refused = hexe.connect()
      if not mux then
        -- Not an error the model should work around: there is simply no mux here.
        return { said = "no hexe session is running (" .. tostring(refused) .. ")" }
      end

      local what = args.what or "panes"
      local ok, answer = pcall(function() return mux[what]() end)
      mux:close()
      if not ok then
        return { said = "hexe refused " .. what .. ": " .. tostring(answer), failed = true }
      end
      return { said = casper.json.encode(answer) }
    end,
  })
end

do -- oslo
  local function client()
    local source = casper.clients and casper.clients.oslo
    if not source then return nil, "oslo's client library is not installed" end
    local chunk, why = load(source, "oslo.lua")
    if not chunk then return nil, why end
    return chunk(casper.stream)
  end

  casper.tool("oslo", {
    description = [[
  Ask the oslo shell about its own state: environment, working directory, and what it can do.

  Reads only. To run a command, use `shell`.]],
    parameters = {
      type = "object",
      properties = {
        what = {
          type = "string",
          description = "Which verb to ask. `verbs` lists what this shell offers.",
        },
      },
    },
    needs = "reach",

    run = function(args)
      local oslo, why = client()
      if not oslo then return { said = tostring(why), failed = true } end

      local shell, refused = oslo.connect()
      if not shell then
        return { said = "no oslo session is running (" .. tostring(refused) .. ")" }
      end

      local what = args.what or "verbs"
      local ok, answer = pcall(function() return shell[what]() end)
      shell:close()
      if not ok then
        return { said = "oslo refused " .. what .. ": " .. tostring(answer), failed = true }
      end
      return { said = casper.json.encode(answer) }
    end,
  })
end

do -- permission
  -- **The prompt the harness used to draw itself.** magi decides *that* a permission is needed and
  -- what it is about -- it holds the ledger and the standing grants -- and this draws the question
  -- and collects the keystroke. What goes back is the id of the row that was chosen, never a
  -- decision: a sibling that could answer "allowed" would make the ledger a suggestion.
  --
  -- Hidden, because the model must not be able to call it. A permission question is something the
  -- harness raises, not something a turn asks for.
  casper.tool("permission", {
    hidden = true,
    description = "The permission prompt. Opened by the harness, never by a model.",
    parameters = { type = "object", properties = {} },

    surface = function(args, size)
      local offers = args.offers or {}
      local at = 1

      -- Wrapped by hand rather than clipped: what is being decided about is the one thing on the
      -- screen that must be read in full, and a command cut in the middle is a command somebody
      -- allows without having seen the end of.
      local function wrapped(text, width)
        local out = {}
        for line in tostring(text or ""):gmatch("[^\n]+") do
          while #line > width do
            local cut = line:sub(1, width):match(".*()%s") or width
            out[#out + 1] = line:sub(1, cut - 1)
            line = line:sub(cut + 1)
          end
          out[#out + 1] = line
        end
        return out
      end

      -- The question, and nothing that can be chosen. Split out because the pointer has to know
      -- how many rows stand between the top and the first offer, and counting them in two places
      -- is how a click lands one row off the thing it was aimed at.
      local function head()
        local width = math.max(20, (size.cols or 80) - 4)
        local rows = {
          { { role = "warn", text = "  " .. (args.tool or "a tool") },
            { role = "muted", text = " wants to " },
            { role = "title", text = args.verb or "act" } },
        }
        for _, line in ipairs(wrapped(args.subject, width - 4)) do
          rows[#rows + 1] = { { role = "path", text = "    " .. line } }
        end
        rows[#rows + 1] = { { role = "text", text = "" } }
        return rows
      end

      -- Which offer a row of the surface is, or nil for a row that is not one.
      --
      -- Rows arrive counted from zero, the way a screen counts them; offers are a Lua array and
      -- start at one.
      local function offered(row)
        local n = row - #head() + 1
        return offers[n] and n or nil
      end

      local function draw()
        local rows = head()
        for n, offer in ipairs(offers) do
          local here = n == at
          rows[#rows + 1] = {
            { role = here and "ok" or "dim", text = here and "  > " or "    " },
            { role = here and "title" or "text", text = offer.label or offer.id },
            { role = "dim", text = offer.about and offer.about ~= "" and ("  " .. offer.about) or "" },
          }
        end
        rows[#rows + 1] = { { role = "dim", text = "  ↑↓ or the pointer · enter to answer · esc denies" } }
        return { lines = rows }
      end

      return function(event)
        -- **A key coming back up is not a second press.** Where the Kitty protocol is live every
        -- keystroke arrives twice, and a list that acted on both moved two rows for one press of
        -- the arrow. `casper.tapped` is that reading: a press or a repeat and nothing else, folded
        -- to lower case so `Q` quits too. A game reads the raw event instead -- it wants the
        -- release, because that is what ends a jump.
        local key = casper.tapped(event)
        if key then
          if key == "up" or key == "k" then
            at = at > 1 and at - 1 or #offers
          elseif key == "down" or key == "j" then
            at = at < #offers and at + 1 or 1
          elseif key == "enter" or key == "space" then
            -- The id of the row, and nothing else. What it *means* is the harness's to work out.
            return { answered = (offers[at] or {}).id or "no" }
          elseif key == "esc" or key == "q" then
            return { answered = "no" }
          end
        elseif event.kind == "mouse" then
          -- Hovering moves the selection and clicking takes it, which is what every list on a
          -- screen does. The harness forwards only what landed on these rows, so there is nothing
          -- to bounds-check beyond which of them was hit.
          local n = offered(event.row)
          if n then
            at = n
            -- The release, not the press: a person may put the pointer down on the wrong row and
            -- slide off it, and a list that answered on the way down gives them no way back.
            if event.what == "release" then
              return { answered = (offers[at] or {}).id or "no" }
            end
          end
        end
        return draw()
      end
    end,
  })
end

do -- session
  -- **The worked example for `casper.knows`.** Every other surface here is told everything it
  -- knows at open: its rows, its width, the arguments of the call. This one is told none of what
  -- it draws -- it asks, from inside the frame, and puts the answers on screen.
  --
  -- Which also makes it the thing to run when a session is not behaving: it says which session
  -- this is, which model is answering, and what the memory layer has to say about a word -- three
  -- questions that otherwise need three different places to look.
  --
  -- Asked once, at open, rather than every frame. The answers do not change while somebody is
  -- reading them, and `memories` is a search: a question per tick would be a search per tick.
  casper.tool("session", {
    description = [[
  What magi knows about this session, on screen: which session it is, which model is
  answering, and what it remembers about `query` if a memory layer is running.]],
    parameters = {
      type = "object",
      properties = {
        query = { type = "string", description = "What to recall about. Omitted is whatever is nearest." },
      },
    },

    run = function()
      return casper.surface{ rows = 12, about = "what this session knows about itself" }
    end,

    surface = function(args, size)
      local query = args.query or ""
      local rows = {}

      local function say(role, text)
        rows[#rows + 1] = { { role = role, text = "  " .. text } }
      end

      -- Two values back, Lua's own idiom: the answer, or nil and why not. A refusal is an
      -- ordinary answer -- there is no balthasar on this machine, no model is configured -- and
      -- drawing it is more use than drawing an empty list that reads as "nothing was found".
      local function about(verb, ask)
        local said, why = casper.knows(verb, ask)
        if not said then
          say("dim", verb .. " — " .. tostring(why))
          return nil
        end
        return said
      end

      local who = about("session")
      if who then
        say("title", "session")
        say("text", "  " .. tostring(who.id))
        say("path", "  " .. tostring(who.cwd))
      end

      local model = about("model")
      if model then
        say("title", "model")
        say("text", "  " .. tostring(model.name))
        say("dim", "  " .. tostring(model.context_window) .. " tokens of room")
      end

      local found = about("memories", { query = query, limit = math.max(1, (size.rows or 12) - 9) })
      if found then
        say("title", query == "" and "memories" or ("memories about " .. query))
        if #found == 0 then
          say("dim", "  nothing yet")
        end
        for _, held in ipairs(found) do
          say("text", "  " .. tostring(held.text or held.summary or held.id or "?"))
        end
      end

      rows[#rows + 1] = { { role = "dim", text = "  any key closes this" } }

      return function(event)
        if casper.tapped(event) then
          return { answered = "read" }
        end
        return { lines = rows }
      end
    end,
  })
end

-- ── the drawing kit ────────────────────────────────────────────────────────────────
--
-- Shared by every surface that draws a picture rather than text. Lifted out of the first
-- game that needed it the moment a second one did: two copies of a bit table is two places
-- for a dot to end up in the wrong corner.


-- Braille packs 2x4 dots into one cell, so eight rows of text hold thirty-two rows of pixels.
-- The bit order is the standard's and not a sane one: dots 1-3 and 7 go down the left column,
-- 4-6 and 8 down the right.
local DOT = {
  [0] = { 0x01, 0x02, 0x04, 0x40 },
  [1] = { 0x08, 0x10, 0x20, 0x80 },
  }

-- **Colours are asked for outright here, not named as roles.**
--
-- Everywhere else in this file a tool says what its output *means* -- `added`, `keyword` -- and
-- the harness paints it from the palette the rest of the screen uses. That is right for output.
-- It is wrong for a picture: a dinosaur is brown and grass is green whatever theme is loaded,
-- and asking for `ok` to get green would be a role lying about itself. So these are RGB, and
-- they stay put when pywal or anything else repaints the terminal underneath.
local BROWN  = { 205, 133,  63 }   -- the dinosaur
local GREEN  = {  76, 175,  80 }   -- cacti
local GRASS  = { 107, 142,  35 }   -- the ground
local SKY    = {  74, 163, 223 }   -- what flies
local BONE   = { 232, 224, 208 }   -- the score
local BLOOD  = { 214,  82,  70 }   -- and what it says when you stop

-- A canvas `cells` wide and `h` pixels tall, holding a colour per cell.
--
-- Per cell rather than per frame, because a frame has a brown dinosaur and a green cactus in it
-- and one colour for the lot would be a picture of one thing. Last writer wins, which is what
-- makes a sprite drawn over the ground look like it is standing on it.
--
-- The width arrives in cells and everything drawn into it is in pixels, which is two units for
-- one axis. Bounds-checking a pixel against the cell count clipped everything past the halfway
-- mark, and the ground stopped in the middle of the screen.
local function canvas(cells, h)
  local bits, ink, w = {}, {}, cells * 2
  return {
    w = w, h = h,
    set = function(x, y, rgb)
    x, y = math.floor(x), math.floor(y)
    if x < 0 or y < 0 or x >= w or y >= h then return end
    local cell = math.floor(y / 4) * cells + math.floor(x / 2)
    bits[cell] = (bits[cell] or 0) | DOT[x % 2][y % 4 + 1]
    ink[cell] = rgb
  end,
    -- One row per row of cells, broken into a span wherever the colour changes. Runs are joined
    -- rather than emitted a cell at a time: eighty spans a row, sixty times a second, is a lot
    -- of allocation to say the same thing.
    rows = function()
    local out = {}
    for cy = 0, math.floor((h - 1) / 4) do
    local line, run, hue = {}, {}, nil
    local function flush()
      if #run > 0 then
        line[#line + 1] = { role = "text", rgb = hue, text = table.concat(run) }
        run = {}
      end
      end
      for cx = 0, cells - 1 do
      local at = cy * cells + cx
      local rgb = ink[at]
      if rgb ~= hue then flush(); hue = rgb end
      run[#run + 1] = utf8.char(0x2800 + (bits[at] or 0))
      end
      flush()
      out[#out + 1] = line
    end
    return out
  end,
  }
  end

local function stamp(c, art, ox, oy, rgb)
  for row = 1, #art do
  local line = art[row]
    for col = 1, #line do
    if line:sub(col, col) == "#" then c.set(ox + col - 1, oy + row - 1, rgb) end
    end
end
end

local function overlaps(ax, aw, bx, bw) return ax < bx + bw and bx < ax + aw end

-- The pointer, as the one key a game has.
--
-- **A press is a press whatever pressed it.** Both games already know how to read a key going
-- down and coming back up; giving them a second way to say the same thing would be two paths to
-- keep in agreement. So a click on the rows arrives as `space`, and holding the button is holding
-- the key -- which on a terminal whose keyboard cannot report a release is the only way to hold
-- anything at all, since the mouse protocol has always said when a button came up.
--
-- Motion becomes a kind nothing matches, so the frame redraws and the world does not move. Read
-- as a tick it would run the game at the speed somebody waves the mouse.
local function clicked(event)
  if event.kind ~= "mouse" then return event end
  if event.what == "press" then
    return { kind = "key", key = "space", state = "down" }
  elseif event.what == "release" then
    return { kind = "key", key = "space", state = "up" }
  end
  return { kind = "hover" }
end


do -- dino
  -- **The showcase.** Everything a surface is for, in one tool the harness knows nothing about:
  -- it asks for rows, draws into them on a clock, reads the keyboard, and ends when the person
  -- says so. The harness reserves the space and blits what comes back -- it cannot tell this from
  -- the permission prompt above, which is the whole point of the mechanism.
  --
  -- Pure Lua. No Rust knows this game exists; deleting this block removes it.
  --
  -- The numbers are the real game's, from the Chromium source (`Runner.config` and `Trex.config`
  -- in offline.js): gravity 0.6, jump velocity 10, speed 6 rising by 0.001 a frame to 13, score
  -- at 0.025 of distance, and a pterodactyl only past speed 8.5. Guessing them produced something
  -- that looked like the game and did not feel like it -- the jump was the giveaway, because
  -- everything else is tuned around how long a dinosaur is in the air.

  -- Scaled from the sprite sheet: the real dinosaur is 44x47 and a small cactus 17x35, which is
  -- about four to one against a braille pixel here.
  local DINO = {
    "   #####", "   #.###", "   #####", "   ###..",
    "#  ###  ", "#####   ", "######  ", " ### ###", "  #   # ",
  }
  local DUCK = { "        ", "     ###", "  ######", "########", "#####.##", " ## ### ", " #   #  " }
  local SMALL = { " # ", "###", "###", " # ", " # ", " # " }
  local LARGE = { "  #  ", "# # #", "#####", "#####", "  #  ", "  #  ", "  #  ", "  #  " }
  local BIRD  = { "  #   ", "  ##  ", "######", " ###  ", "  #   " }

  casper.tool("dino", {
    description = [[
  Play the Chromium no-internet dinosaur game in the terminal, drawn in braille.

  A showcase for surfaces: this tool asks the harness for rows and fills them itself. Space or up
  jumps and so does a click on the rows — held either way, it jumps higher. Down ducks, `q` or
  escape quits. Call it when somebody asks to play, or to see whether surfaces work.]],
    parameters = { type = "object", properties = {} },

    run = function(args)
      if args.answered then
        return { said = "the dinosaur game ended: " .. tostring(args.answered) }
      end
      -- Eight rows, and a tick: it moves whether or not anybody is pressing anything, which is
      -- what separates a game from a picker.
      return casper.surface{
        rows = 8,
        about = "the dinosaur game — space jumps, down ducks, q quits",
        tick = 16,
      }
    end,

    surface = function(args, size)
      -- **The real game's shape, at this world's scale and this world's frame rate.**
      --
      -- Taking Chromium's numbers unchanged was wrong twice over. They are pixels on a 600x150
      -- canvas where the dinosaur is 47 tall; here it is 9 tall in a field of 32. And they are
      -- *per frame at sixty a second* -- applied at twenty, every jump took three times as long
      -- in wall-clock, which is exactly the floating the game does not do.
      --
      -- So the tick is 60Hz and these are solved for the feel rather than copied: a full jump
      -- rises about 12 pixels and lasts a little over half a second, which is what the original
      -- does. From `airtime = 2v/g` and `apex = v^2/2g` with airtime 33 frames and apex 12.
      local GRAVITY, JUMP = 0.16, 1.55
      -- **How much longer a held key keeps you up.** `THRUST` frames of `EASE`-reduced gravity
      -- while the key is down, which is what turns hold-length into air-time over a range wide
      -- enough to matter: a tap is about a third of a second up, a full hold a little over one.
      --
      -- Cutting the rise on release was the first attempt and it was not enough -- the rise lasts
      -- sixteen frames, so anything held past a quarter second was the same jump as anything held
      -- for a second, which is exactly the "holding does nothing" this replaces.
      local THRUST, EASE = 16, 0.3
      local DROP = 0.6
      local SPEED, MAX_SPEED, ACCEL = 0.85, 2.0, 0.0004
      local GAP = 30
      -- Scored so it climbs at about ten a second, which is the rate the original does. The
      -- coefficient is not Chromium's 0.025 because neither the distance nor the frame rate is.
      local SCORE = 0.2

      local cells, H = size.cols or 80, (size.rows or 8) * 4
      local floor = H - 4
      -- **Whether this terminal can say a key came back up.** With the Kitty keyboard protocol it
      -- can, and letting go early cuts a jump short -- so a tap is a hop and a hold is a full
      -- jump. Without it every key is a bare press and every jump is a full one, and the hint below
      -- says so rather than promising a control the terminal cannot deliver.
      local holds = size.holds == true
      -- `down` is whether the jump key is still held, `lift` how many thrust frames are left.
      local dino, fall, lift, down = 0, 0, 0, false
      local ducking, speed, dist, over, best = false, SPEED, 0, false, 0
      -- **The diagnostic, drawn on purpose.** Whether the terminal reports a key coming back up
      -- is the one thing that decides if holding can mean anything, and neither a player nor
      -- anybody reading this file can tell by watching the dinosaur. So: `saw` is the last key
      -- event verbatim, and `heldfor` counts the frames the jump key has been down. Inverted
      -- while it is down, so it is unmistakable.
      --
      -- If the counter climbs while you hold and stops when you let go, the protocol is live and
      -- anything still wrong is in this file. If it never climbs past one, the terminal is not
      -- reporting holds and no amount of work here changes that.
      local saw, heldfor = "—", 0
      local things = {}

      local function reset()
        dino, fall, lift = 0, 0, 0
        ducking, speed, dist, over, things = false, SPEED, 0, false, {}
      end

      local function one(w)
        -- A pterodactyl only past the speed the real game gates it behind, so the first minute is
        -- cacti and the bird is something you play long enough to meet.
        local pick = (speed > 1.4 and math.random() < 0.22) and "bird"
          or (math.random() < 0.35 and "large" or "small")
        local art = pick == "bird" and BIRD or (pick == "large" and LARGE or SMALL)
        -- The bird flies at one of three heights, and the lowest is duckable rather than
        -- jumpable, which is the only reason ducking is worth having.
        local lift = pick == "bird" and ({ 0, 6, 11 })[math.random(3)] or 0
        things[#things + 1] = { x = w, art = art, wide = #art[1], tall = #art, lift = lift }
        return art, pick
      end

      -- **Not every gap is the same gap.** A run of evenly spaced cacti is one jump learnt once
      -- and repeated; what makes the jump worth having a *length* is a pair close enough that
      -- clearing both means staying up, against singles you can hop. So a third of the time a
      -- second one lands a few pixels behind the first -- close enough to need the long jump,
      -- never so close that no jump clears it.
      --
      -- Birds are never doubled: one at head height with something under it is not a jump anybody
      -- can make, and an obstacle that cannot be passed is not difficulty.
      local function spawn(w)
        local _, pick = one(w)
        if pick ~= "bird" and math.random() < 0.33 then
          one(w + 9 + math.random(0, 5))
        end
      end

      return function(event)
        event = clicked(event)
        if event.kind == "resize" then
          cells, H = event.cols, event.rows * 4
          floor = H - 4
          -- The harness learns whether the keyboard reports holds the first time one arrives,
          -- which may be after this opened. Taken here so the controls stop understating
          -- themselves the moment it is known, rather than for the life of the game.
          holds = event.holds == true or holds
        elseif event.kind == "key" then
          local key, state = event.key:lower(), event.state or "down"
          -- **Shown on screen, on purpose.** Whether a terminal reports a key coming back up is
          -- the one thing that decides if holding can mean anything, and it is not something
          -- either of us can tell by looking at the dinosaur. So the last event is printed: see
          -- `up` after you let go and the protocol is live; see only `down` and it is not, and
          -- no amount of work on this side will change that.
          saw = key .. " " .. state
          -- **Quitting is a tap; jumping is a hold.** Both readings of one keyboard, side by
          -- side. `casper.tapped` drops the release, so `q` ends the game once rather than on the
          -- way down and again on the way up; the raw `state` below is what makes a long jump
          -- different from a hop, and no helper can give that back.
          local tap = casper.tapped(event)
          if tap == "q" or tap == "esc" then
            return { answered = "scored " .. tostring(math.floor(dist * SCORE)) }
          end
          -- **A held key is not a repeated tap.** `down` starts a jump; `up` while still rising
          -- cuts it short, which is what makes a short hop and a full jump different presses of
          -- the same key. Only a terminal speaking the Kitty protocol says `up` at all -- without
          -- it every jump is a full one, which is the old behaviour and still plays.
          if over then
            if state == "down" then reset() end
          elseif key == "space" or key == "up" or key == "enter" then
            -- **The key is held until it is let go, and thrust runs the whole time.**
            --
            -- Cutting the rise on release was not enough: the rise is over sixteen frames, so
            -- anything held past a quarter of a second was the same jump as anything held for a
            -- second. Holding has to *keep doing something*, which is what `lift` below is --
            -- reduced gravity for as long as the key is down, up to `THRUST` frames.
            --
            -- `repeat` counts as still-down, because it is: the terminal only sends one while the
            -- key has not come up.
            if state == "up" then
              down = false
            else
              -- **A jump needs the key to have come up first.** Without this a `repeat` arriving
              -- after the dinosaur has landed started a second jump while the key was never
              -- released -- which is holding space and watching it jump, land, jump, land.
              --
              -- Only checkable where releases are reported. Where they are not, `down` would
              -- never clear and the first jump would be the only one ever, so there every press
              -- is taken as fresh: the old behaviour, on the terminals that cannot do better.
              local fresh = not down or not holds
              down = true
              if fresh and dino == 0 and not over then
                fall, lift, ducking = JUMP, THRUST, false
              end
            end
          elseif key == "down" or key == "j" then
            if dino > 0 and state ~= "up" then
              -- Down while airborne drops you faster, exactly as the real one does.
              fall = -DROP
            else
              ducking = state ~= "up"
            end
          end
        elseif event.kind == "tick" and not over then
          -- Integrated a frame at a time, with no fudge factor: one tick is one frame, which is
          -- what makes the constants above mean what they say.
          -- **Gravity depends on whether the key is still down.** Held and rising and inside the
          -- thrust window, the dinosaur is barely pulled back at all; the moment it is let go --
          -- or the window runs out -- full weight returns. That is what makes half a second of
          -- holding a visibly different jump from a tap, rather than the same arc reached sooner.
          heldfor = down and heldfor + 1 or 0
          local pull = GRAVITY
          if down and lift > 0 and fall > 0 then
            lift = lift - 1
            pull = GRAVITY * EASE
          end
          dino = math.max(0, dino + fall)
          fall = dino > 0 and fall - pull or 0
          if dino == 0 then fall, lift = 0, 0 end
          speed = math.min(MAX_SPEED, speed + ACCEL)
          dist = dist + speed

          local kept = {}
          for _, it in ipairs(things) do
            it.x = it.x - speed
            if it.x + it.wide > 0 then kept[#kept + 1] = it end
          end
          things = kept
          -- Far enough apart to be cleared at the speed they are arriving at. A jump covers
          -- `speed * 33` pixels of ground, so the gap has to grow as the world slows down under
          -- you -- which is why it is divided by speed rather than fixed.
          local last = things[#things]
          -- Uneven on purpose, and never closer than a jump can cover. The floor keeps a pair
          -- from arriving on top of the last one; the random half is what stops the rhythm.
          if not last or last.x < cells * 2 - (34 + GAP / speed + math.random(0, 26)) then
            spawn(cells * 2)
          end

          -- The dinosaur stands at x=4. Ducking makes it wider and shorter, which is what lets a
          -- low bird pass over it.
          local w, tall = ducking and #DUCK[1] or #DINO[1], ducking and #DUCK or #DINO
          for _, it in ipairs(things) do
            if overlaps(4, w, it.x, it.wide) then
              local top = floor - it.tall - it.lift
              if floor - math.floor(dino) - tall < top + it.tall and floor - math.floor(dino) > top then
                over = true
                best = math.max(best, math.floor(dist * SCORE))
              end
            end
          end
        end

        local c = canvas(cells, H)
        for x = 0, cells * 2 - 1, 3 do c.set(x, floor, GRASS) end
        local art = ducking and DUCK or DINO
        -- Red only while it is over, so the moment of losing is the one thing that changes colour.
        stamp(c, art, 4, floor - #art - math.floor(dino), over and BLOOD or BROWN)
        for _, it in ipairs(things) do
          -- What flies is sky-coloured, what grows is green.
          stamp(c, it.art, it.x, floor - it.tall - it.lift, it.lift > 0 and SKY or GREEN)
        end
        local rows = c.rows()
        rows[1] = over
          and {
            { role = "text", rgb = BLOOD, text = "  game over " },
            { role = "text", rgb = GRASS, text = "· any key restarts · q quits    " },
            { role = "text", rgb = BONE, text = string.format("%05d", math.floor(dist * SCORE)) },
          }
          or {
            { role = "text", text = "  " },
            { role = "text", rgb = BONE, text = string.format("%05d", math.floor(dist * SCORE)) },
            {
              role = "text",
              rgb = GRASS,
              text = best > 0 and ("   hi " .. string.format("%05d", best)) or "",
            },
            -- Said once, in the space the score leaves: what this terminal can actually do. A hint
            -- promising "hold to jump higher" where no release is ever reported would be a control
            -- that silently is not there.
            {
              role = "text",
              rgb = SKY,
              text = holds and "   tap to hop · hold to jump" or "   space jumps · ↓ ducks",
            },
            { role = "text", rgb = BROWN, text = "   " .. saw .. " " },
            -- Inverted while the key is down: dark on bright, so it reads as a lit lamp rather
            -- than as another word. The number is frames, at sixty a second.
            down
              and { role = "text", rgb = { 20, 20, 20 }, bg = BONE,
                    text = string.format(" HELD %02d ", heldfor) }
              or { role = "text", rgb = GRASS, text = "  ·  " },
          }
        return { lines = rows }
      end
    end,
  })
end

do -- birdy
  -- **The second tenant, which is the point.** Nothing in Rust changed to add this: the drawing
  -- kit above was lifted out of the dinosaur the moment something else wanted it, and the rest is
  -- one `casper.tool` call. Sixteen rows instead of eight, because a bird needs somewhere to fall.
  --
  -- **It taps where the dinosaur holds.** A jump is one press whose *length* decides the arc; a
  -- flap is a fixed impulse and holding must not repeat it. Both need the same thing from the
  -- terminal and use it oppositely: dino reads how long a key was down, birdy reads that it came
  -- back up at all. Without the release, holding space here would be a bird that never falls.
  local WING = { 245, 200, 70 }   -- the bird
  local PIPE = { 60, 170, 90 }    -- what it flies between
  local LIP  = { 92, 208, 120 }   -- the mouth of each pipe, a shade up so the gap reads
  local DIRT = { 107, 142, 35 }   -- the ground
  local BONE = { 232, 224, 208 }  -- the score
  local GONE = { 214, 82, 70 }    -- and the end of it

  -- Five across, four down. The dot is an eye: `.` is off in the mask, so it reads as a gap in a
  -- solid head rather than as a pixel somebody forgot.
  local BIRDY = { " ### ", "#####", "##.##", " ### " }

  casper.tool("birdy", {
    description = [[
  Play flappy bird in the terminal, drawn in braille.

  Space, up or a click flaps once — each press is one flap, so holding does nothing. `q` quits.
  Call it when somebody asks to play, or to see a second surface running the same machinery as
  `dino` with none of its code.]],
    parameters = { type = "object", properties = {} },

    run = function(args)
      if args.answered then
        return { said = "birdy ended: " .. tostring(args.answered) }
      end
      return casper.surface{
        rows = 16,
        about = "birdy — space flaps, q quits",
        tick = 16,
      }
    end,

    surface = function(args, size)
      -- Solved for the field the way dino's were: a flap should rise about a fifth of the height
      -- and take a third of a second to top out, from `apex = v^2/2g` and `t = v/g`.
      local GRAV, FLAP, DIVE = 0.055, 1.15, 1.7
      local SPEED, MAX_SPEED, ACCEL = 0.62, 1.15, 0.00025
      local WIDE, AT = 4, 9        -- pipe width, and where the bird sits across
      local SCORE = 1              -- a point a pipe, which is what the original counts

      local cells, H = size.cols or 80, (size.rows or 16) * 4
      local floor = H - 3
      local holds = size.holds == true
      local y, vy, speed, score, over, best = H * 0.4, 0, SPEED, 0, false, 0
      local down, pipes = false, {}

      local function reset()
        y, vy, speed, score, over, pipes = H * 0.4, 0, SPEED, 0, false, {}
      end

      -- The gap narrows as you go, which is the whole difficulty curve. Floored well above the
      -- bird's own height: a gap it cannot fit through is not hard, it is broken.
      local function spawn(x)
        local gap = math.max(15, 24 - math.floor(score / 4))
        local top = math.random(4, math.max(5, floor - gap - 4))
        pipes[#pipes + 1] = { x = x, top = top, gap = gap, passed = false }
      end

      return function(event)
        event = clicked(event)
        if event.kind == "resize" then
          cells, H = event.cols, event.rows * 4
          floor = H - 3
          holds = event.holds == true or holds
        elseif event.kind == "key" then
          local key, state = event.key:lower(), event.state or "down"
          -- Quitting is a tap; flapping is a press with a release behind it. See `dino` for why
          -- the two are read differently.
          local tap = casper.tapped(event)
          if tap == "q" or tap == "esc" then
            return { answered = "scored " .. tostring(score) }
          end
          if state == "up" then
            down = false
          elseif key == "space" or key == "up" or key == "enter" then
            -- **One flap a press.** A repeat says the key is still down, not that it was pressed
            -- again, so it must not lift. Where releases are never reported `down` would stick on
            -- and the bird would flap once and never again -- so there, every press counts.
            local fresh = not down or not holds
            down = true
            if over then
              if fresh then reset() end
            elseif fresh then
              vy = -FLAP
            end
          elseif key == "down" or key == "j" then
            if state ~= "up" and not over then vy = DIVE end
          end
        elseif event.kind == "tick" and not over then
          vy = vy + GRAV
          y = y + vy
          speed = math.min(MAX_SPEED, speed + ACCEL)

          local kept = {}
          for _, p in ipairs(pipes) do
            p.x = p.x - speed
            if p.x + WIDE > 0 then kept[#kept + 1] = p end
          end
          pipes = kept
          local last = pipes[#pipes]
          if not last or last.x < cells * 2 - (46 + 14 / speed) then spawn(cells * 2) end

          -- The bird is five wide and four tall at `AT`. A pipe is solid above its gap and below
          -- it, so overlapping horizontally and *not* being inside the gap is a crash.
          for _, p in ipairs(pipes) do
            if p.x < AT + 5 and p.x + WIDE > AT then
              if y < p.top or y + 4 > p.top + p.gap then over = true end
            end
            -- Scored on the way past rather than on contact, so a pipe counts once and only once.
            if not p.passed and p.x + WIDE < AT then
              p.passed = true
              score = score + SCORE
              best = math.max(best, score)
            end
          end
          -- The ceiling is as fatal as the floor. Without that a bird could sit at the top and
          -- ride out every pipe, which is a game with one strategy.
          --
          -- Clamped as well as fatal: a bird that died at `y = -2` was drawn off the top of its own
          -- rows, so the one frame that says you lost was the one frame with no bird in it.
          if y < 0 then
            y, vy, over = 0, 0, true
          elseif y + 4 > floor then
            y, vy, over = floor - 4, 0, true
          end
        end

        local c = canvas(cells, H)
        for x = 0, cells * 2 - 1, 3 do c.set(x, floor + 1, DIRT) end
        for _, p in ipairs(pipes) do
          for x = math.floor(p.x), math.floor(p.x) + WIDE - 1 do
            for py = 0, p.top - 1 do c.set(x, py, PIPE) end
            for py = p.top + p.gap, floor do c.set(x, py, PIPE) end
          end
          -- A lip at each mouth, one shade brighter, so the opening reads at a glance instead of
          -- being a hole you find by flying into its edge.
          for x = math.floor(p.x) - 1, math.floor(p.x) + WIDE do
            c.set(x, p.top - 1, LIP)
            c.set(x, p.top + p.gap, LIP)
          end
        end
        stamp(c, BIRDY, AT, math.floor(y), over and GONE or WING)

        local rows = c.rows()
        rows[1] = over
          and {
            { role = "text", rgb = GONE, text = "  down " },
            { role = "text", rgb = DIRT, text = "· any key restarts · q quits    " },
            { role = "text", rgb = BONE, text = string.format("%03d", score) },
          }
          or {
            { role = "text", text = "  " },
            { role = "text", rgb = BONE, text = string.format("%03d", score) },
            { role = "text", rgb = DIRT, text = best > 0 and ("   best " .. string.format("%03d", best)) or "" },
            { role = "text", rgb = WING, text = "   space flaps · ↓ dives" },
          }
        return { lines = rows }
      end
    end,
  })
end
