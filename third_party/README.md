# third_party

C/C++ 依赖，按《工程规范》第 1、6 节组织。

| 目录 | 内容 | 状态 |
|---|---|---|
| `pdfium-prebuilt/` | pdfium-binaries 发布的原版 PDFium（`chromium/8138`），由 `scripts/fetch-pdfium-prebuilt.ps1` 拉取，不入库 | P0 往返基线（V02）用 |
| `pdfium/` | 自家 PDFium 分支 `magpdf-pdfium`，子模块 + 构建脚本 | P0 V01 建立 |
| `zint/` | 条码生成库，子模块，锁定发布标签 | P1 |
| `zxing-cpp/` | 条码识别库，子模块，锁定发布标签 | P1 |

`pdfium-sys` 默认链接 `pdfium-prebuilt/`；要改用本地构建的分支产物，设置环境变量
`PDFIUM_PREBUILT_DIR` 指向包含 `bin/pdfium.dll`、`lib/pdfium.dll.lib`、`include/` 与 `VERSION` 的目录。
