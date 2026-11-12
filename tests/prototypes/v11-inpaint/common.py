"""V11 背景修补原型：各脚本共用的路径、读写和小工具。

图像在这一套脚本里一律是 OpenCV 的习惯：`uint8`，三通道按 B、G、R 排；掩膜是单通道 `uint8`，
要处理的像素是 255，其余是 0。
"""
from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any

import cv2
import numpy as np
import numpy.typing as npt

HERE = Path(__file__).resolve().parent
# 工作树的根：tests/prototypes/v11-inpaint 往上三层
REPO = HERE.parents[2]

# 输出目录（不入库，/tests/out/ 在 .gitignore 里）
DEFAULT_OUT = REPO / "tests" / "out" / "v11"

# 下面三个是这一批任务约定的固定位置（见 README），可以用环境变量或命令行参数改。
DEFAULT_CLI = Path(os.environ.get("MAGPDF_CLI", r"D:\workspace\magpdf-wt\bin\magpdf-cli.exe"))
DEFAULT_MODELS_DIR = Path(os.environ.get("MAGPDF_MODELS_DIR", r"D:\workspace\magpdf\models"))
DEFAULT_LAMA_MODEL = Path(
    os.environ.get("MAGPDF_LAMA_MODEL", r"D:\workspace\magpdf-wt\models-extra\lama_fp32.onnx")
)

DPI = 300

Image = npt.NDArray[np.uint8]
Mask = npt.NDArray[np.uint8]
FloatImage = npt.NDArray[np.float32]


def imread(path: Path, *, gray: bool = False) -> Image:
    """读图。不用 cv2.imread：路径里有中文时它在 Windows 上读不出来。"""
    data = np.fromfile(str(path), dtype=np.uint8)
    img = cv2.imdecode(data, cv2.IMREAD_GRAYSCALE if gray else cv2.IMREAD_COLOR)
    if img is None:
        raise OSError(f"读不出图像：{path}")
    return img


def imwrite(path: Path, img: npt.NDArray[Any], *, jpeg_quality: int = 95) -> None:
    """写图，格式看扩展名：PNG 无损；JPEG 按 4:4:4 写（只用于拼好的对照图）。"""
    path.parent.mkdir(parents=True, exist_ok=True)
    ext = path.suffix.lower()
    if ext == ".png":
        params = [cv2.IMWRITE_PNG_COMPRESSION, 3]
    elif ext in (".jpg", ".jpeg"):
        params = [
            cv2.IMWRITE_JPEG_QUALITY,
            jpeg_quality,
            cv2.IMWRITE_JPEG_SAMPLING_FACTOR,
            cv2.IMWRITE_JPEG_SAMPLING_FACTOR_444,
        ]
    else:
        raise ValueError(f"不认识的图像扩展名：{path}")
    ok, buf = cv2.imencode(ext, img, params)
    if not ok:
        raise OSError(f"图像编码失败：{path}")
    buf.tofile(str(path))


def read_json(path: Path) -> Any:
    with path.open("r", encoding="utf-8") as f:
        return json.load(f)


def write_json(path: Path, data: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="\n") as f:
        json.dump(data, f, ensure_ascii=False, indent=1)
        f.write("\n")


def quad_to_array(quad: list[list[float]]) -> npt.NDArray[np.float32]:
    """四个角点（左上、右上、右下、左下）转成 4×2 的数组。"""
    arr = np.asarray(quad, dtype=np.float32)
    if arr.shape != (4, 2):
        raise ValueError(f"行框要有四个点：{quad}")
    return arr


def quad_height(quad: npt.NDArray[np.float32]) -> float:
    """行框的高：左右两条边长度的平均。"""
    left = float(np.linalg.norm(quad[3] - quad[0]))
    right = float(np.linalg.norm(quad[2] - quad[1]))
    return (left + right) / 2.0


def expand_quad(quad: npt.NDArray[np.float32], pad_x: float, pad_y: float) -> npt.NDArray[np.float32]:
    """沿行框自己的两个方向往外扩：行的方向扩 pad_x，垂直方向扩 pad_y。"""
    ux = (quad[1] - quad[0]) + (quad[2] - quad[3])
    uy = (quad[3] - quad[0]) + (quad[2] - quad[1])
    ux = ux / max(float(np.linalg.norm(ux)), 1e-6)
    uy = uy / max(float(np.linalg.norm(uy)), 1e-6)
    out = quad.copy()
    out[0] += -ux * pad_x - uy * pad_y
    out[1] += ux * pad_x - uy * pad_y
    out[2] += ux * pad_x + uy * pad_y
    out[3] += -ux * pad_x + uy * pad_y
    return out


def fill_quads(shape: tuple[int, int], quads: list[npt.NDArray[np.float32]]) -> Mask:
    """把若干个行框画成一张掩膜（框里是 255）。"""
    mask = np.zeros(shape, dtype=np.uint8)
    for q in quads:
        cv2.fillConvexPoly(mask, np.round(q).astype(np.int32), 255)
    return mask


def dilate(mask: Mask, radius: int) -> Mask:
    """用圆形结构元把掩膜往外扩 radius 个像素；radius 为 0 时原样返回。"""
    if radius <= 0:
        return mask.copy()
    k = cv2.getStructuringElement(cv2.MORPH_ELLIPSE, (2 * radius + 1, 2 * radius + 1))
    return cv2.dilate(mask, k)


def bounding_rect(mask: Mask, pad: int = 0) -> tuple[int, int, int, int]:
    """掩膜的外接矩形 (x0, y0, x1, y1)，右下不含；四周再让出 pad，不超出图像。掩膜是空的时返回全零。"""
    ys, xs = np.nonzero(mask)
    if xs.size == 0:
        return (0, 0, 0, 0)
    h, w = mask.shape[:2]
    return (
        max(int(xs.min()) - pad, 0),
        max(int(ys.min()) - pad, 0),
        min(int(xs.max()) + 1 + pad, w),
        min(int(ys.max()) + 1 + pad, h),
    )
