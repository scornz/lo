# Third-party notices

Lo includes code from the projects below, under their licenses.

## difftastic

[difftastic](https://github.com/Wilfred/difftastic), at commit
`87aad1aea4aacdf3cab68d2006e614d4994ece69`.

`crates/diff_extras/src/syntax/` ports difftastic's structural diff, rewritten for Lo. Each file
taken from difftastic starts with the notice below and names the difftastic files it ports.

```text
MIT License

Copyright (c) 2021-2025 Wilfred Hughes

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

## tree-sitter grammars' tags queries

`crates/symbol_index/queries/<language>/tags.scm` are copied unchanged from each grammar's
`queries/tags.scm`, at the version Zed uses:

| File | Source | Copyright |
|---|---|---|
| `rust/tags.scm` | [tree-sitter-rust](https://github.com/tree-sitter/tree-sitter-rust) `v0.24.2` (`77a3747266f4d621d0757825e6b11edcbf991ca5`) | Copyright (c) 2017 Maxim Sokolov |
| `typescript/tags.scm` | [zed-industries/tree-sitter-typescript](https://github.com/zed-industries/tree-sitter-typescript) at `e2c53597d6a5d9cf7bbe8dccde576fe1e46c5899` | Copyright (c) 2017 Max Brunsfeld |
| `javascript/tags.scm` | [tree-sitter-javascript](https://github.com/tree-sitter/tree-sitter-javascript) `v0.23.1` (`3a837b6f3658ca3618f2022f8707e29739c91364`) | Copyright (c) 2014 Max Brunsfeld |
| `python/tags.scm` | [tree-sitter-python](https://github.com/tree-sitter/tree-sitter-python) `v0.25.0` (`293fdc02038ee2bf0e2e206711b69c90ac0d413f`) | Copyright (c) 2016 Max Brunsfeld |
| `go/tags.scm` | [zed-industries/tree-sitter-go](https://github.com/zed-industries/tree-sitter-go) at `6862a49e3c5a7567a6c576420e9a8f944e198a85` | Copyright (c) 2014 Max Brunsfeld |

Each is under the MIT License, with the copyright line above and this notice:

```text
MIT License

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
