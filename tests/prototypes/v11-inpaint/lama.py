"""V11 背景修补原型：LaMa 修复模型（ONNX，CPU）的封装（方案 6.4 第三行、6.5“背景修复”）。

模型的输入固定是 512×512：`image` 是 RGB、0..1 的浮点数，`mask` 里 1 是要补的像素；输出是 RGB、0..255。
一行字在 300 dpi 下常常比 512 像素宽，所以沿着掩膜的外接矩形排窗口：每个窗口负责中间最多 384 像素宽的一段，
两边各留至少 64 像素当上下文；窗口按原图的分辨率取，不缩放。只把掩膜里的像素贴回去。
"""
from __future__ import annotations

import math
import time
from pathlib import Path

import cv2
import numpy as np

from common import Image, Mask, bounding_rect

SIZE = 512
MARGIN = 64


def _segments(lo: int, hi: int, limit: int) -> list[tuple[int, int, int]]:
    """把 [lo, hi) 分成几段，每段不超过 SIZE − 2×MARGIN。返回（段起，段止，负责这一段的窗口从哪里开始）。"""
    count = max(1, math.ceil((hi - lo) / (SIZE - 2 * MARGIN)))
    out = []
    for i in range(count):
        a = lo + (hi - lo) * i // count
        b = lo + (hi - lo) * (i + 1) // count
        start = min(max((a + b) // 2 - SIZE // 2, 0), limit - SIZE)
        out.append((a, b, start))
    return out


class Lama:
    def __init__(self, model_path: Path, threads: int = 0) -> None:
        """threads 是算子内的线程数，0 交给 ONNX Runtime 自己定（默认是物理核数）。"""
        import onnxruntime as ort

        if not model_path.exists():
            raise FileNotFoundError(f"没有 LaMa 模型文件：{model_path}（先运行 fetch_lama.py）")
        options = ort.SessionOptions()
        if threads > 0:
            options.intra_op_num_threads = threads
        start = time.perf_counter()
        self.session = ort.InferenceSession(str(model_path), options, providers=["CPUExecutionProvider"])
        self.init_seconds = time.perf_counter() - start
        self.last_windows = 0  # 最近一次 inpaint 用了几个窗口（几次推理）

    def infer(self, bgr: Image, mask: Mask) -> Image:
        """一次推理：512×512 的 BGR 图和掩膜进，512×512 的 BGR 图出。"""
        if bgr.shape[:2] != (SIZE, SIZE) or mask.shape != (SIZE, SIZE):
            raise ValueError("LaMa 的输入必须是 512×512")
        image = np.ascontiguousarray(bgr[..., ::-1].transpose(2, 0, 1)[None], dtype=np.float32) / 255.0
        hole = np.ascontiguousarray((mask > 0)[None, None], dtype=np.float32)
        out = self.session.run(None, {"image": image, "mask": hole})[0][0]
        rgb = np.clip(np.round(out.transpose(1, 2, 0)), 0, 255).astype(np.uint8)
        return np.ascontiguousarray(rgb[..., ::-1])

    def inpaint(self, img: Image, mask: Mask) -> Image:
        h, w = img.shape[:2]
        pad_b, pad_r = max(SIZE - h, 0), max(SIZE - w, 0)
        if pad_b or pad_r:  # 图比窗口小：用镜像补到 512
            img = cv2.copyMakeBorder(img, 0, pad_b, 0, pad_r, cv2.BORDER_REFLECT_101)
            mask = cv2.copyMakeBorder(mask, 0, pad_b, 0, pad_r, cv2.BORDER_CONSTANT, value=0)
        out = img.copy()
        x0, y0, x1, y1 = bounding_rect(mask)
        self.last_windows = 0
        if x1 > x0:
            for ya, yb, wy in _segments(y0, y1, img.shape[0]):
                for xa, xb, wx in _segments(x0, x1, img.shape[1]):
                    part = mask[ya:yb, xa:xb] > 0
                    if not part.any():
                        continue
                    result = self.infer(
                        np.ascontiguousarray(img[wy : wy + SIZE, wx : wx + SIZE]),
                        np.ascontiguousarray(mask[wy : wy + SIZE, wx : wx + SIZE]),
                    )
                    out[ya:yb, xa:xb][part] = result[ya - wy : yb - wy, xa - wx : xb - wx][part]
                    self.last_windows += 1
        return np.ascontiguousarray(out[:h, :w])
