# 模型

OCR 与背景修补模型的清单和下载脚本（方案 6.5、12.1）。模型文件不入库。

| 模型 | 用途 | 许可 | 阶段 |
|---|---|---|---|
| PP-OCRv5 mobile det / rec / cls | 中英文 OCR，默认内置 | Apache-2.0 | P0 V09 |
| `en_PP-OCRv5_mobile_rec` | 英文单证专用识别，P0 评估后决定是否内置 | Apache-2.0 | P0 V09 |
| PP-OCRv5 server det / rec | 高精度可选组件 | Apache-2.0 | P0 V09 对照 |
| LaMa（ONNX） | 背景修补可选组件 | Apache-2.0 | P4 |

下载脚本统一从国内可达的官方源取文件，并校验 SHA-256；清单格式与脚本由 V09 建立。
