-- casper's build, as recipes. This replaced the Makefile; there is no other.
--
--   make            the recipes, with what each of them says it does
--   make build      the binary
--   make test       the suite
--   make verify     the whole local gate
--
-- At an oslo prompt in this directory `make` is enough; everywhere else it is `oslo make`.
-- CI has no oslo, so it calls the language's own tool -- nothing here is on the release path.

local make = oslo.make

-- Name and version live in PROJECT, one per line, so every tool reads them from one place.
local function project()
  local found = {}
  for line in (oslo.fs.read("PROJECT") or ""):gmatch("[^\n]+") do
    local value = line:match("^%s*([^#%[%s]%S*)%s*$")
    if value then found[#found + 1] = value end
  end
  return found[1] or "casper", found[2] or "0.1.0"
end

local NAME, VERSION = project()
local PREFIX = os.getenv("PREFIX") or (os.getenv("HOME") .. "/.local")

------------------------------------------------------------------ what was built

local function dim(text)
  return oslo.ui.style(text, { dim = true })
end

local function line(label, value)
  print(dim(oslo.ui.pad(label, 8)) .. value)
end

-- `1524720` -> `1,524,720`. A number this long is read in groups or not at all.
local function grouped(n)
  local text = tostring(math.floor(n))
  local out = text:sub(-3)
  local at = #text - 3
  while at > 0 do
    out = text:sub(math.max(1, at - 2), at) .. "," .. out
    at = at - 3
  end
  return out
end

-- Asked of the ELF, not assumed. `ldd` is not enough on its own: it prints "statically linked" for
-- a binary that still carries an INTERP and will not start.
local function linkage(path)
  local segments = oslo.run{ "readelf", "-l", path, capture = true }
  if not segments.ok then return nil end
  local dynamic = oslo.run{ "readelf", "-d", path, capture = true }
  if (segments.out or ""):find("program interpreter") or (dynamic.out or ""):find("NEEDED") then
    return "dynamic"
  end
  return "static"
end

-- What was built, how big it is, and whether it needs anything on the target machine. Silent when
-- the artifact is not there, so a recipe that builds nothing does not pretend it did.
local function report(path)
  local stat = oslo.fs.stat(path)
  if not stat then return end
  local megabytes = ("%.2f MB"):format(stat.size / 1048576)

  print("")
  print(oslo.ui.title(("%s %s   %s"):format(NAME, VERSION, megabytes)))
  line("binary", path)
  -- Bytes beside megabytes: `1.45 MB` cannot be subtracted from last week's `1.42 MB` to get one.
  line("size", megabytes .. dim("   " .. grouped(stat.size) .. " bytes"))

  local kind = linkage(path)
  if kind == "static" then
    line("linking", oslo.ui.style("✓ static", { fg = "green" }) ..
                    dim("   no runtime dependencies"))
  elseif kind == "dynamic" then
    line("linking", oslo.ui.style("dynamic", { fg = "yellow" }) ..
                    dim("   needs a matching libc on the target machine"))
  end
  print("")
end


make.recipe{ name = "version", desc = "what this checkout calls itself",
             run = function() print(("%s v%s"):format(NAME, VERSION)) end }

local function need(tool, why)
  assert(oslo.run{ "sh", "-c", "command -v " .. tool, capture = true }.ok, why)
end

make.recipe{
  name = "release",
  desc = "cut a version: --type patch | minor | major | M.m.p",
  params = { { "--type", desc = "patch | minor | major | M.m.p" } },
  run = function(a)
    need("git-rel", "git-rel is not installed; install it first")
    assert(type(a.type) == "string",
           "which release? make release --type patch|minor|major|M.m.p")
    sh.git("rel", a.type)
  end,
}

make.recipe{
  name = "changelog",
  desc = "regenerate CHANGELOG.md",
  run = function()
    need("git-cliff", "git-cliff is not installed; install it first")
    sh.git("cliff", "-o", "CHANGELOG.md")
  end,
}

---------------------------------------------------------------------------- rust

-- Where `build` leaves it, and what `run` and `install` then reach for.
local function binary_path()
  return "target/release/" .. NAME
end

-- **The binary, not the library.** casper is a program: the library exists so the tests can
-- reach inside it, and nobody runs an rlib. This built `--lib` for a while, inherited from the
-- scaffold, and the cost was not an error -- it was `make build` succeeding and leaving the
-- previous binary in place, so a change to `config/tools.lua` (which rides in the binary through
-- `include_str!`) was absent from the thing you then ran.
--
-- Release, because this is the binary somebody runs. `make debug` is for iterating.
make.recipe{
  name = "build",
  desc = "the binary",
  run = function()
    sh.cargo("build", "--release", "--bin", NAME)
    report(binary_path())
  end,
}
make.alias("b", "build")

make.recipe{
  name = "debug",
  desc = "an unoptimized build, for iterating",
  run = function()
    sh.cargo("build", "--bin", NAME)
    report("target/debug/" .. NAME)
  end,
}

make.recipe{
  name = "run",
  desc = "run the binary: --args='tools'",
  params = { { "--args", desc = "what to pass it" } },
  run = function(a)
    sh.cargo("build", "--release", "--bin", NAME)
    sh.sh("-c", ("%s %s"):format(binary_path(), a.args or ""))
  end,
}
make.alias("r", "run")

make.recipe{
  name = "install",
  desc = ("install the binary to %s/bin"):format(PREFIX),
  -- The declarations ride in the binary rather than beside it: casper with no tools is not a
  -- casper, and a relative `config/` would load whichever checkout the working directory
  -- happened to be in -- which is how a sibling ends up running another project's tools.
  deps = { "build" },
  run = function()
    local bin = PREFIX .. "/bin"
    assert(oslo.run{ "mkdir", "-p", bin }.ok, "could not create " .. bin)
    assert(oslo.run{ "install", "-m", "755", binary_path(), bin .. "/" .. NAME }.ok,
           "could not install to " .. bin)
    print(("installed %s"):format(bin .. "/" .. NAME))
  end,
}

make.recipe{ name = "test", desc = "the suite",
             run = function() sh.cargo("test", "--all-targets") end }
make.alias("t", "test")

make.recipe{ name = "test-all", desc = "the suite, with every feature on",
             run = function() sh.cargo("test", "--all-targets", "--all-features") end }

make.recipe{ name = "check", desc = "type-check every target",
             run = function() sh.cargo("check", "--all-targets") end }

make.recipe{ name = "check-all", desc = "type-check every target, every feature",
             run = function() sh.cargo("check", "--all-targets", "--all-features") end }

make.recipe{ name = "clippy", desc = "clippy, with warnings denied",
             run = function()
               sh.cargo("clippy", "--all-targets", "--all-features", "--", "-Dwarnings")
             end }

make.recipe{
  name = "rustdoc",
  desc = "build the docs, with warnings denied",
  run = function()
    local built = oslo.run{ "env", "RUSTDOCFLAGS=-Dwarnings",
                            "cargo", "doc", "--all-features", "--no-deps" }
    assert(built.ok, "rustdoc failed")
  end,
}

make.recipe{ name = "fmt", desc = "format the workspace",
             run = function() sh.cargo("fmt", "--all") end }

make.recipe{ name = "fmt-check", desc = "fail if anything is unformatted",
             run = function() sh.cargo("fmt", "--all", "--", "--check") end }

make.recipe{ name = "clean", desc = "remove every build output",
             run = function() sh.cargo("clean") end }

make.recipe{ name = "compile", desc = "clean, then build", deps = { "clean", "build" } }
make.alias("c", "compile")

make.recipe{
  name = "gates",
  desc = "the architectural gates",
  run = function()
    local failed = {}
    for _, name in ipairs({ "gate-cycles", "gate-file-size", "gate-modules", "gate-wire" }) do
      -- Executed, not handed to `sh`: the shebang is the portability contract, and CI runs
      -- these on a machine whose /bin/sh is dash.
      local result = oslo.run{ "scripts/" .. name .. ".sh", capture = true }
      print((result.ok and "\u{2713}  %s" or "\u{2717}  %s"):format(name))
      if not result.ok then
        failed[#failed + 1] = name
        print(((result.out or "") .. (result.err or "")))
      end
    end
    assert(#failed == 0, ("%d gate(s) failed"):format(#failed))
  end,
}
make.alias("g", "gates")


-- Runs the whole suite a second time, under a `TMPDIR` of its own, and asserts the directory is
-- empty afterwards. Its own recipe rather than one of the `gates` above, because those are greps
-- that finish instantly and this one costs a full test run — and because a failure here is a
-- leaking test, not a violated rule about how the code is written.
make.recipe{
  name = "gate-hermetic",
  desc = "the suite leaves nothing behind in the temporary directory",
  run = function()
    local ran = oslo.run{ "scripts/gate-hermetic.sh" }
    assert(ran.ok, "gate-hermetic failed")
  end,
}

-- Every dependency a manifest declares is one the code actually uses.
--
-- Nine were not, across this family: an edge in `Cargo.toml`, in the lockfile and in every
-- diagram drawn from them, and nowhere in the source. See the note in `Cargo.toml` for why this
-- rather than the `unused_crate_dependencies` lint.
make.recipe{
  name = "machete",
  desc = "no dependency nothing uses",
  run = function()
    -- Through the dev shell when it is not already on the path. `make` is run from a plain
    -- terminal as often as from inside `nix develop`, and a check that quietly did not run
    -- because a tool was missing is worse than one that is slow: CI would then be the only
    -- place it happened, which is the arrangement this milestone exists to end.
    local direct = oslo.run{ "cargo", "machete", capture = true }
    if direct.ok then return end
    local said = (direct.out or "") .. (direct.err or "")
    if not said:find("no such command") then
      print(said)
      error("cargo machete failed")
    end
    local shelled = oslo.run{ "nix", "develop", "--command", "cargo", "machete" }
    assert(shelled.ok, "cargo machete failed")
  end,
}

make.recipe{
  name = "verify",
  desc = "the whole local gate",
  deps = { "fmt-check", "check", "test", "check-all", "test-all", "clippy", "rustdoc", "gates", "gate-hermetic", "machete" },
}
make.alias("v", "verify")
