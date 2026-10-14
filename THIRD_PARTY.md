# Third-party notices

## maki

`execute` is a port of maki's `code_execution` tool: its interpreter runner, the allocator that
holds a script to its memory limit, the conversion between JSON and Python values, the Python
preamble and `gather` helper, the tool description, the output limits, and the tests that came
with them. The ported code lives in `src/scripting.rs` and `src/scripting/`.

Source: <https://github.com/tontinton/maki>, commit `54a1529`.

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

## monty

The Python interpreter itself is [monty](https://github.com/pydantic/monty), tag `v0.0.23`, used
as a dependency under its own MIT license.
