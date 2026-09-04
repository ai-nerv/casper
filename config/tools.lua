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

      local function draw()
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
        for n, offer in ipairs(offers) do
          local here = n == at
          rows[#rows + 1] = {
            { role = here and "ok" or "dim", text = here and "  > " or "    " },
            { role = here and "title" or "text", text = offer.label or offer.id },
            { role = "dim", text = offer.about and offer.about ~= "" and ("  " .. offer.about) or "" },
          }
        end
        rows[#rows + 1] = { { role = "dim", text = "  ↑↓ to choose · enter to answer · esc denies" } }
        return { lines = rows }
      end

      return function(event)
        if event.kind == "key" then
          local key = event.key
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
        end
        return draw()
      end
    end,
  })
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

  -- Braille packs 2x4 dots into one cell, so eight rows of text hold thirty-two rows of pixels.
  -- The bit order is the standard's and not a sane one: dots 1-3 and 7 go down the left column,
  -- 4-6 and 8 down the right.
  local DOT = {
    [0] = { 0x01, 0x02, 0x04, 0x40 },
    [1] = { 0x08, 0x10, 0x20, 0x80 },
  }

  -- A canvas `cells` wide and `h` pixels tall.
  --
  -- The width arrives in cells and everything drawn into it is in pixels, which is two units for
  -- one axis. Bounds-checking a pixel against the cell count clipped everything past the halfway
  -- mark, and the ground stopped in the middle of the screen.
  local function canvas(cells, h)
    local bits, w = {}, cells * 2
    return {
      w = w, h = h,
      set = function(x, y)
        x, y = math.floor(x), math.floor(y)
        if x < 0 or y < 0 or x >= w or y >= h then return end
        local cell = math.floor(y / 4) * cells + math.floor(x / 2)
        bits[cell] = (bits[cell] or 0) | DOT[x % 2][y % 4 + 1]
      end,
      rows = function(role)
        local out = {}
        for cy = 0, math.floor((h - 1) / 4) do
          local line = {}
          for cx = 0, cells - 1 do
            line[#line + 1] = utf8.char(0x2800 + (bits[cy * cells + cx] or 0))
          end
          out[#out + 1] = { { role = role, text = table.concat(line) } }
        end
        return out
      end,
    }
  end

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

  local function stamp(c, art, ox, oy)
    for row = 1, #art do
      local line = art[row]
      for col = 1, #line do
        if line:sub(col, col) == "#" then c.set(ox + col - 1, oy + row - 1) end
      end
    end
  end

  local function overlaps(ax, aw, bx, bw) return ax < bx + bw and bx < ax + aw end

  casper.tool("dino", {
    description = [[
  Play the Chromium no-internet dinosaur game in the terminal, drawn in braille.

  A showcase for surfaces: this tool asks the harness for rows and fills them itself. Space or up
  jumps, down ducks, `q` or escape quits. Call it when somebody asks to play, or to see whether
  surfaces work.]],
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
        tick = 50,
      }
    end,

    surface = function(args, size)
      -- The real game's constants, at the scale a braille cell gives.
      local GRAVITY, JUMP, DROP = 0.6, 10.0, 5.0
      local SPEED, MAX_SPEED, ACCEL = 1.4, 3.0, 0.0004
      local GAP = 0.6
      -- The real game scores at 0.025 of distance at sixty frames a second. This ticks at twenty,
      -- so the coefficient is scaled to climb at the same rate a player remembers rather than to
      -- match a number nobody sees.
      local SCORE = 0.35

      local cells, H = size.cols or 80, (size.rows or 8) * 4
      local floor = H - 4
      local dino, fall, ducking, speed, dist, over, best = 0, 0, false, SPEED, 0, false, 0
      local things = {}

      local function reset()
        dino, fall, ducking, speed, dist, over, things = 0, 0, false, SPEED, 0, false, {}
      end

      local function spawn(w)
        -- A pterodactyl only past the speed the real game gates it behind, so the first minute is
        -- cacti and the bird is something you play long enough to meet.
        local pick = (speed > 2.0 and math.random() < 0.25) and "bird"
          or (math.random() < 0.35 and "large" or "small")
        local art = pick == "bird" and BIRD or (pick == "large" and LARGE or SMALL)
        -- The bird flies at one of three heights, and the lowest is duckable rather than
        -- jumpable, which is the only reason ducking is worth having.
        local lift = pick == "bird" and ({ 0, 6, 11 })[math.random(3)] or 0
        things[#things + 1] = { x = w, art = art, wide = #art[1], tall = #art, lift = lift }
      end

      return function(event)
        if event.kind == "resize" then
          cells, H = event.cols, event.rows * 4
          floor = H - 4
        elseif event.kind == "key" then
          local key, state = event.key, event.state or "down"
          if key == "q" or key == "esc" then
            return { answered = "scored " .. tostring(math.floor(dist * SCORE)) }
          end
          -- **A held key is not a repeated tap.** `down` starts a jump; `up` while still rising
          -- cuts it short, which is what makes a short hop and a full jump different presses of
          -- the same key. Only a terminal speaking the Kitty protocol says `up` at all -- without
          -- it every jump is a full one, which is the old behaviour and still plays.
          if over then
            if state == "down" then reset() end
          elseif key == "space" or key == "up" or key == "enter" then
            if state == "down" and dino == 0 then
              fall, ducking = JUMP, false
            elseif state == "up" and fall > 0 then
              fall = fall * 0.4
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
          dino = math.max(0, dino + fall * 0.35)
          fall = dino > 0 and fall - GRAVITY or 0
          if dino == 0 then fall = 0 end
          speed = math.min(MAX_SPEED, speed + ACCEL)
          dist = dist + speed

          local kept = {}
          for _, it in ipairs(things) do
            it.x = it.x - speed * 2
            if it.x + it.wide > 0 then kept[#kept + 1] = it end
          end
          things = kept
          local last = things[#things]
          if not last or last.x < cells * 2 - (40 + GAP * 60 / speed) then spawn(cells * 2) end

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
        for x = 0, cells * 2 - 1, 3 do c.set(x, floor) end
        local art = ducking and DUCK or DINO
        stamp(c, art, 4, floor - #art - math.floor(dino))
        for _, it in ipairs(things) do
          stamp(c, it.art, it.x, floor - it.tall - it.lift)
        end
        local rows = c.rows(over and "error" or "ok")
        rows[1] = over
          and {
            { role = "warn", text = "  game over " },
            { role = "dim", text = "· any key restarts · q quits    " },
            { role = "title", text = string.format("%05d", math.floor(dist * SCORE)) },
          }
          or {
            { role = "dim", text = "  " },
            { role = "title", text = string.format("%05d", math.floor(dist * SCORE)) },
            { role = "dim", text = best > 0 and ("   hi " .. string.format("%05d", best)) or "" },
          }
        return { lines = rows }
      end
    end,
  })
end
