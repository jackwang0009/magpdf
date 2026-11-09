# 模型

OCR 与背景修补模型的清单和下载脚本（方案 6.5、12.1、13.3）。模型文件和 ONNX Runtime 不入库，
由 `fetch-models.ps1` 按 `manifest.json` 下载并校验 SHA-256。

```powershell
pwsh models/fetch-models.ps1              # 默认内置模型 + ONNX Runtime（约 32 MB + 17 MB）
pwsh models/fetch-models.ps1 -Set server  # 服务器版 det/rec（约 165 MB，对照用）
pwsh models/fetch-models.ps1 -Set all     # 清单里全部
pwsh models/fetch-models.ps1 -Verify      # 只校验
```

GitHub Release（ONNX Runtime）国内直连常超时，需要时设置 `$env:HTTPS_PROXY`；ModelScope 一般直连可达。

## 清单

| 模型（`id`） | 用途 | 文件 | 大小 | 许可 | 集合 |
|---|---|---|---|---|---|
| `PP-OCRv5_mobile_det` | 文字检测 | `ocr/PP-OCRv5_mobile_det.onnx` | 4.6 MB | Apache-2.0 | default |
| `PP-OCRv5_mobile_rec` + `ppocrv5_dict` | 中英日文识别（18383 字） | `ocr/PP-OCRv5_mobile_rec.onnx`、`ocr/ppocrv5_dict.txt` | 15.9 MB | Apache-2.0 | default |
| `en_PP-OCRv5_mobile_rec` + `ppocrv5_en_dict` | 英文专用识别（436 字） | `ocr/en_PP-OCRv5_mobile_rec.onnx`、`ocr/ppocrv5_en_dict.txt` | 7.5 MB | Apache-2.0 | default（P0 评估后定是否内置） |
| `PP-LCNet_x0_25_textline_ori` | 文字行方向 0°/180° | `ocr/PP-LCNet_x0_25_textline_ori.onnx` | 1.0 MB | Apache-2.0 | default |
| `ch_ppocr_mobile_v2.0_cls` | 旧版文字行方向模型 | `ocr/ch_ppocr_mobile_v2.0_cls.onnx` | 0.6 MB | Apache-2.0 | legacy（对照） |
| `PP-OCRv5_server_det` / `PP-OCRv5_server_rec` | 高精度可选组件 | `ocr/PP-OCRv5_server_*.onnx` | 84 + 81 MB | Apache-2.0 | server |
| ONNX Runtime 1.31.1（CPU） | 推理运行时 | `onnxruntime/onnxruntime.dll` 等 | 17.3 MB | MIT（内含 Eigen MPL-2.0，见 `ThirdPartyNotices.txt`） | 运行时 |
| LaMa（ONNX） | 背景修补可选组件 | — | 208 MB | Apache-2.0 | P4，尚未加入清单 |

## 来源与许可

- PaddleOCR 官方只发布 Paddle 推理模型（百度 BOS：`paddle-model-ecology.bj.bcebos.com/paddlex/official_inference_model/paddle3.0.0/<模型名>_infer.tar`）。
  这里的 ONNX 文件取自 [RapidAI/RapidOCR 在 ModelScope 的模型托管仓库](https://www.modelscope.cn/models/RapidAI/RapidOCR)
  （许可 Apache-2.0，与原模型一致），由 `paddleocr_convert`（内部调用 paddle2onnx）转换，识别模型的字典同时写进了 ONNX metadata 的 `character` 键。
- 字典文件（`ppocrv5_dict.txt`、`ppocrv5_en_dict.txt`）与官方 `inference.yml` 中的 `character_dict` 逐项核对一致（V09 报告 2.1）。
- 预处理参数以官方 `inference.yml` 为准：det 归一化 ImageNet 均值方差、BGR；rec 高 48、`(x/255-0.5)/0.5`、BGR；
  `PP-LCNet_x0_25_textline_ori` 输入 160×80、ImageNet 归一化、RGB。
- 文档方向分类模型 `PP-LCNet_x1_0_doc_ori`（方案 6.2 第 1 步）目前只有 Paddle 格式，需自行用 paddle2onnx 转换，V09 报告列为遗留项。
- 随产品分发时按《开源许可与合规清单》2.1、2.3 附许可全文并注明来源。

## 与代码的对应

`magpdf-scan` 的 `models::Manifest` 读取本清单，按 `id` 找到文件；`magpdf-cli ocr --models-dir models` 默认用 `default` 集合。
更换模型时同时更新清单的 `version` 字段——OCR 缓存按模型名称和版本判断是否作废（《文件私有数据格式规范》9.5）。
