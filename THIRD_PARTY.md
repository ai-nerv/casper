# Third-party code

## maki

The `code` tool is a port of the `index` tool from
[maki](https://github.com/tontinton/maki), at commit `54a1529`.

These files derive from maki's `plugins/index/`:

| casper | maki |
|---|---|
| `src/indexing/lua/indexer.lua` | `plugins/index/indexer.lua` |
| `src/indexing/lua/lang/*.lua`, `lang/nix/*.lua`, `lang/bazel/*.lua` | `plugins/index/lang/` |
| `src/indexing/lua/code.lua` | the handler in `plugins/index/init.lua` |
| `tests/code/helpers.lua`, `tests/code/indexer_core.lua`, `tests/code/lang/*.lua` | `plugins/index/tests/` |
| `tests/code/test_helpers.lua` | `plugins/lib/maki/test_helpers.lua` |
| `src/indexing/treesitter.rs`, `src/indexing/grammars.rs` | `maki-lua/src/api/treesitter/`, `maki-lua/src/language.rs` |
| the directory listing in `src/indexing.rs` | `plugins/lib/maki/dir_listing.lua` |

What changed in the Lua: `maki.treesitter` is the global `treesitter`, and `maki.split` and
`table.find` are local functions in `indexer.lua`. The specs still `require("maki.test_helpers")`,
which casper's test harness answers with `tests/code/test_helpers.lua`.

```
MIT License

Copyright (c) 2026 Tony Solomonik

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
