# Third-party licenses

nnnyaz is MIT licensed (see [`LICENSE`](LICENSE)). It also contains material
derived from the projects below.

## Yazi

`src/translit.rs` is derived from Yazi's transliteration table
(`yazi-shared/src/translit/table.rs`, <https://github.com/sxyazi/yazi>).

MIT License

Copyright (c) 2023 - sxyazi

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

## nnn

nnnyaz emulates parts of nnn's command line and keymap, but contains no code
copied from [nnn](https://github.com/jarun/nnn). nnn is BSD-2-Clause licensed;
its license therefore does not apply to this project.

## Rust dependencies

The crates listed in `Cargo.lock` are distributed under their own licenses.
Run `cargo license` or `cargo deny check licenses` to list them.
