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
  Run a command in a shell. Asks before it does.

  The working directory is kept between calls, so `cd build` and then `make` works.]],
    parameters = {
      type = "object",
      properties = {
        command = { type = "string", description = "The command to run." },
      },
      required = { "command" },
    },
    needs = "run",

    run = function(args)
      if not args.answered then
        -- The command goes in the detail rather than the question: a question has to fit on one
        -- line and a command does not, and what somebody is deciding about is the text.
        return casper.ask(
          "run a shell command?",
          {
            { id = "once", label = "Allow once" },
            { id = "no",   label = "Deny", about = "the model is told, and carries on" },
          },
          casper.paint.plain(args.command).lines
        )
      end

      if args.answered ~= "once" then
        -- A refusal is a result the model reads, not an error: it should try something else
        -- rather than the same thing again.
        return { said = "the person did not permit that command", failed = true }
      end

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
