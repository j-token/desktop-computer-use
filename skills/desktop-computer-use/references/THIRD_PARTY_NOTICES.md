# Third-party sources

The input sequencing, accessibility observation conventions and recovery design draw on
stablyai/orca commit fe4237cd41e4d48793fd135640939ef8aa5418e0. Original source paths include
native/computer-use-windows/runtime.ps1, native/computer-use-linux/runtime.py,
src/main/computer/, and skill-guides/computer-use.md. This project implements independent
platform backends; it does not incorporate proprietary Codex native code.

## Orca — MIT License

Copyright (c) 2026 Lovecast Inc.

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

The npm runtime dependency license texts used by the bundled CLI are generated in
`desktop-computer-use/references/THIRD_PARTY_LICENSES.md` during `npm run bundle`.

## Native daemon crates

The native daemon is built with Cargo from the Rust crates pinned in `Cargo.lock`. Their license
texts are generated into `native/THIRD_PARTY_LICENSES.md` by `node scripts/native-licenses.mjs` and
shipped as `desktop-computer-use/references/NATIVE_THIRD_PARTY_LICENSES.md`.
