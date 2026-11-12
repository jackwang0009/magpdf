# PDFium 版权声明

`magpdf-content` 里有一部分代码是照着 PDFium 的源码移植的（用 Rust 重写，行为逐条对齐，不是逐行翻译）。
对齐的版本是 PDFium 提交 `8477545cb9`（和仓库里修补版 PDFium 的基线一致）。

## 移植了什么

| 本 crate 的文件 | 对应的 PDFium 源文件 |
| --- | --- |
| `src/pdf/lexer.rs` | `core/fpdfapi/parser/cpdf_syntax_parser.cpp` |
| `src/pdf/reader.rs` | `core/fpdfapi/parser/cpdf_parser.cpp`、`cpdf_cross_ref_table.cpp`、`cpdf_object_stream.cpp` |
| `src/pdf/filter.rs` | `core/fpdfapi/parser/fpdf_parser_decode.cpp`、`cpdf_stream_acc.cpp`、`core/fxcodec/flate/flatemodule.cpp` |
| `src/pdf/pages.rs`、`src/pdf/pairs.rs` | `core/fpdfapi/parser/cpdf_document.cpp`、`core/fpdfapi/page/cpdf_page.cpp`、`cpdf_pageobjectholder.cpp` |
| `src/content/mod.rs` | `core/fpdfapi/page/cpdf_contentparser.cpp` |
| `src/content/parser.rs`、`src/content/inline.rs` | `core/fpdfapi/page/cpdf_streamparser.cpp` |
| `src/map/interp.rs` | `core/fpdfapi/page/cpdf_streamcontentparser.cpp`、`cpdf_contentparser.cpp`、`cpdf_clippath.cpp` |
| `src/map/resources.rs` | `core/fpdfapi/page/cpdf_docpagedata.cpp`、`cpdf_colorspace.cpp`、`cpdf_shadingpattern.cpp`、`cpdf_function.cpp`、`core/fpdfapi/font/cpdf_font.cpp` |

写回的部分（`src/rewrite/`、`src/pdf/writer.rs`）是自己写的，没有照搬 PDFium。

## 许可

PDFium 的源文件头写的是 “Use of this source code is governed by a BSD-style license that can be found in the
LICENSE file.”。下面是它的 `LICENSE` 文件里那份 BSD 许可的原文；同一个文件后面还附了 Apache License 2.0 的
全文（PDFium 自带的部分第三方代码用它），完整的文件随 PDFium 二进制一起分发（`third_party/pdfium-prebuilt/LICENSE`）。

```text
Copyright 2014 The PDFium Authors

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are
met:

   * Redistributions of source code must retain the above copyright
notice, this list of conditions and the following disclaimer.
   * Redistributions in binary form must reproduce the above
copyright notice, this list of conditions and the following disclaimer
in the documentation and/or other materials provided with the
distribution.
   * Neither the name of Google Inc. nor the names of its
contributors may be used to endorse or promote products derived from
this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
"AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```
