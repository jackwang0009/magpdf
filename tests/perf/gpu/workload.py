"""把页面图和文字行框变成模型的输入张量（V10）。

预处理照 `engine/magpdf-scan/src/ocr/` 的 `image.rs`、`det.rs`、`cls.rs`、`rec.rs`、`mod.rs` 逐步移植，
尺寸、归一化、分批都一样；浮点运算用 float32、顺序和 Rust 一致：

- 检测：长边超过 1600 时缩到 1600，宽高取 32 的倍数；BGR、ImageNet 归一化；一页一个 `[1, 3, H, W]`。
- 文字行：按四点框透视裁切（宽取上边、高取左边，四舍五入），高宽比 ≥ 1.5 的逆时针转 90°。
- 方向分类：每行缩到 160×80，RGB、ImageNet 归一化，按原顺序每 6 行一批。
- 识别：高 48、宽按比例；按长宽比排序后每 6 行一批，批宽向上取 32 的倍数、不小于 320、不超过 3200，右边补零；
  BGR、`(x/255-0.5)/0.5`。

文字行的框来自固定的命令行工具（`magpdf-cli ocr <目录> --json <目录>`）的输出，不在这里做检测后处理。

用法（一般由 run_all.py 调用）：
    python workload.py --images <图片目录> --ocr-json <CLI 输出目录> --out <缓存目录>
每页写一个 `<页名>.npz`（不压缩），另写一份 `index.json`。
"""
from __future__ import annotations

import argparse
import json
import math
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Sequence

import numpy as np
from numpy.typing import NDArray

U8Image = NDArray[np.uint8]
F32 = NDArray[np.float32]

_F = np.float32

# 和 Rust 的默认参数一致（DetParams::default、RecParams::default、ClsParams::default）。
DET_LIMIT_SIDE_LEN = 1600
DET_MAX_SIDE_LIMIT = 4000
REC_HEIGHT = 48
REC_BASE_WIDTH = 320
REC_MAX_WIDTH = 3200
REC_BATCH = 6
CLS_BATCH = 6
CLS_W, CLS_H = 160, 80

IMAGENET_MEAN = (_F(0.485), _F(0.456), _F(0.406))
IMAGENET_STD = (_F(0.229), _F(0.224), _F(0.225))


# ---------------------------------------------------------------------------
# 图像操作（对应 image.rs）
# ---------------------------------------------------------------------------


def load_rgb(path: Path) -> U8Image:
    """读成紧凑排列的 RGB（H×W×3）。"""
    from PIL import Image

    with Image.open(path) as im:
        return np.asarray(im.convert("RGB"), dtype=np.uint8)


def _axis_weights(src: int, dst: int) -> tuple[NDArray[np.intp], NDArray[np.intp], F32]:
    """一条轴上每个目标像素的两个源下标和权重：`src = (dst + 0.5) * scale - 0.5`，越界取边。"""
    scale = _F(src) / _F(dst)
    f = (np.arange(dst, dtype=np.float32) + _F(0.5)) * scale - _F(0.5)
    f = np.clip(f, _F(0.0), _F(src - 1))
    i0 = np.floor(f).astype(np.intp)
    i1 = np.minimum(i0 + 1, src - 1)
    return i0, i1, (f - i0.astype(np.float32)).astype(np.float32)


def _lerp_round(p00: F32, p10: F32, p01: F32, p11: F32, tx: F32, ty: F32) -> U8Image:
    """双线性插值后加 0.5 截断成 u8，运算顺序和 Rust 一样。"""
    top = p00 + (p10 - p00) * tx
    bottom = p01 + (p11 - p01) * tx
    out = top + (bottom - top) * ty + _F(0.5)
    return np.clip(out, _F(0.0), _F(255.0)).astype(np.uint8)


def resize_bilinear(img: U8Image, nw: int, nh: int) -> U8Image:
    """双线性缩放到 `(nw, nh)`，像素中心对齐（`RgbImage::resize_bilinear`）。"""
    nw, nh = max(nw, 1), max(nh, 1)
    h, w = img.shape[:2]
    if nw == w and nh == h:
        return img.copy()
    x0, x1, tx = _axis_weights(w, nw)
    y0, y1, ty = _axis_weights(h, nh)
    row0 = img[y0]
    row1 = img[y1]
    txb = tx[None, :, None]
    tyb = ty[:, None, None]
    return _lerp_round(
        row0[:, x0].astype(np.float32),
        row0[:, x1].astype(np.float32),
        row1[:, x0].astype(np.float32),
        row1[:, x1].astype(np.float32),
        txb,
        tyb,
    )


def _homography(src: Sequence[tuple[float, float]], dst: Sequence[tuple[float, float]]) -> list[float] | None:
    """把 `src` 四点映到 `dst` 四点的单应矩阵；高斯消元的步骤和 `image.rs::homography` 一样（f64）。"""
    a = [[0.0] * 9 for _ in range(8)]
    for i in range(4):
        x, y = float(src[i][0]), float(src[i][1])
        u, v = float(dst[i][0]), float(dst[i][1])
        a[2 * i] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u]
        a[2 * i + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v]
    for col in range(8):
        pivot = col
        for r in range(col, 8):
            # Rust 的 max_by 在并列时取最后一个。
            if abs(a[r][col]) >= abs(a[pivot][col]):
                pivot = r
        if abs(a[pivot][col]) < 1e-12:
            return None
        a[col], a[pivot] = a[pivot], a[col]
        for r in range(8):
            if r != col:
                f = a[r][col] / a[col][col]
                if f != 0.0:
                    pivot_row = list(a[col])
                    for k in range(col, 9):
                        a[r][k] -= f * pivot_row[k]
    return [a[i][8] / a[i][i] for i in range(8)] + [1.0]


def warp_quad(img: U8Image, quad: Sequence[tuple[float, float]], w: int, h: int) -> U8Image:
    """按四点框（左上、右上、右下、左下）透视裁切成 `w × h`（`RgbImage::warp_quad`）。"""
    w, h = max(w, 1), max(h, 1)
    dst = [(0.0, 0.0), (float(w), 0.0), (float(w), float(h)), (0.0, float(h))]
    m64 = _homography(dst, quad)
    if m64 is None:
        return np.zeros((h, w, 3), dtype=np.uint8)
    m = [_F(v) for v in m64]
    src_h, src_w = img.shape[:2]
    xf = np.arange(w, dtype=np.float32)[None, :]
    yf = np.arange(h, dtype=np.float32)[:, None]
    den = m[6] * xf + m[7] * yf + m[8]
    ok = np.abs(den) > np.finfo(np.float32).eps
    safe = np.where(ok, den, _F(1.0))
    sx = np.where(ok, (m[0] * xf + m[1] * yf + m[2]) / safe, _F(0.0)).astype(np.float32)
    sy = np.where(ok, (m[3] * xf + m[4] * yf + m[5]) / safe, _F(0.0)).astype(np.float32)
    fx = np.clip(sx, _F(0.0), _F(src_w - 1))
    fy = np.clip(sy, _F(0.0), _F(src_h - 1))
    x0 = np.floor(fx).astype(np.intp)
    y0 = np.floor(fy).astype(np.intp)
    x1 = np.minimum(x0 + 1, src_w - 1)
    y1 = np.minimum(y0 + 1, src_h - 1)
    tx = (fx - x0.astype(np.float32))[..., None]
    ty = (fy - y0.astype(np.float32))[..., None]
    return _lerp_round(
        img[y0, x0].astype(np.float32),
        img[y0, x1].astype(np.float32),
        img[y1, x0].astype(np.float32),
        img[y1, x1].astype(np.float32),
        tx,
        ty,
    )


def _round_half_away(v: np.float32) -> int:
    """Rust `f32::round`：四舍五入，0.5 远离零（这里只有非负数）。"""
    return int(math.floor(float(v) + 0.5))


def crop_line(img: U8Image, quad: Sequence[tuple[float, float]]) -> tuple[U8Image, bool]:
    """`mod.rs::crop_line`：宽取上边、高取左边；高宽比 ≥ 1.5 视为竖排，逆时针转 90°。"""
    q = [(_F(x), _F(y)) for x, y in quad]

    def dist(a: tuple[np.float32, np.float32], b: tuple[np.float32, np.float32]) -> np.float32:
        dx, dy = a[0] - b[0], a[1] - b[1]
        return np.sqrt(dx * dx + dy * dy)

    w = max(_round_half_away(dist(q[0], q[1])), 1)
    h = max(_round_half_away(dist(q[0], q[3])), 1)
    crop = warp_quad(img, [(float(x), float(y)) for x, y in q], w, h)
    if _F(h) / _F(w) >= _F(1.5):
        return np.ascontiguousarray(np.rot90(crop)), True
    return crop, False


def rotate_180(img: U8Image) -> U8Image:
    return np.ascontiguousarray(img[::-1, ::-1])


def fill_chw(img: U8Image, plane_w: int, plane_h: int, *, bgr: bool, imagenet: bool) -> F32:
    """写成 `[3, plane_h, plane_w]` 的浮点张量，图放左上角，其余为 0（`image.rs::fill_chw`）。"""
    h, w = img.shape[:2]
    if w > plane_w or h > plane_h:
        raise ValueError(f"图像 {w}x{h} 放不进 {plane_w}x{plane_h}")
    out = np.zeros((3, plane_h, plane_w), dtype=np.float32)
    src_index = (2, 1, 0) if bgr else (0, 1, 2)
    for c in range(3):
        mean, std = (IMAGENET_MEAN[c], IMAGENET_STD[c]) if imagenet else (_F(0.5), _F(0.5))
        scale = _F(1.0) / (_F(255.0) * std)
        offset = -mean / std
        out[c, :h, :w] = img[:, :, src_index[c]].astype(np.float32) * scale + offset
    return out


# ---------------------------------------------------------------------------
# 三个模型的输入（对应 det.rs、cls.rs、rec.rs）
# ---------------------------------------------------------------------------


def det_input_size(width: int, height: int) -> tuple[int, int]:
    """`DetParams::input_size`（`limit_type = max`）：返回 `(宽, 高)`，都是 32 的倍数。"""
    w, h = _F(width), _F(height)
    limit = _F(DET_LIMIT_SIDE_LEN)
    long_side = max(w, h)
    ratio = limit / long_side if long_side > limit else _F(1.0)
    max_side = _F(max(DET_MAX_SIDE_LIMIT, 32))
    if long_side * ratio > max_side:
        ratio = max_side / long_side

    def round32(v: np.float32) -> int:
        return int(max(_round_half_away(v / _F(32.0)) * 32, 32))

    return round32(np.floor(w * ratio)), round32(np.floor(h * ratio))


def det_tensor(img: U8Image) -> F32:
    h, w = img.shape[:2]
    in_w, in_h = det_input_size(w, h)
    resized = resize_bilinear(img, in_w, in_h)
    return fill_chw(resized, in_w, in_h, bgr=True, imagenet=True)[None]


def cls_batches(crops: Sequence[U8Image], batch: int = CLS_BATCH) -> list[F32]:
    """方向分类的输入：每行缩到 160×80，按原顺序分批。"""
    batches: list[F32] = []
    for start in range(0, len(crops), batch):
        chunk = crops[start : start + batch]
        batches.append(
            np.stack([fill_chw(resize_bilinear(c, CLS_W, CLS_H), CLS_W, CLS_H, bgr=False, imagenet=True) for c in chunk])
        )
    return batches


def _ratio(crop: U8Image) -> np.float32:
    h, w = crop.shape[:2]
    return _F(w) / _F(max(h, 1))


@dataclass(frozen=True)
class WidthPolicy:
    """识别批次的宽度怎么定。

    - `step`：批宽向上取这个数的倍数（产品现在是 32）。
    - `buckets`：给了就改成向上取到这几档里最小的一档（超过最大一档时仍按 `step` 取整）。
    - `pad_batch`：最后不足一批时用全零的行补满，让批大小恒定。
    """

    name: str
    step: int = 32
    buckets: tuple[int, ...] = ()
    pad_batch: bool = False

    def width_for(self, want: int) -> int:
        w = -(-want // self.step) * self.step
        for b in self.buckets:
            if want <= b:
                w = b
                break
        return min(max(w, 32), max(REC_MAX_WIDTH, 32))


PRODUCT_POLICY = WidthPolicy("product-step32")


def rec_batches(
    crops: Sequence[U8Image],
    policy: WidthPolicy = PRODUCT_POLICY,
    batch: int = REC_BATCH,
) -> tuple[list[F32], list[list[int]]]:
    """识别的输入。返回 `(各批张量, 各批里每一行在 crops 里的序号)`。

    和 `Recognizer::recognize` 一样：按长宽比稳定排序、每批取最大长宽比定宽度。
    """
    h = REC_HEIGHT
    ratios = [_ratio(c) for c in crops]
    order = sorted(range(len(crops)), key=lambda i: float(ratios[i]))
    tensors: list[F32] = []
    members: list[list[int]] = []
    base_ratio = _F(REC_BASE_WIDTH) / _F(h)
    for start in range(0, len(order), batch):
        chunk = order[start : start + batch]
        max_ratio = base_ratio
        for i in chunk:
            max_ratio = max(max_ratio, ratios[i])
        want = int(math.ceil(float(_F(h) * max_ratio)))
        w = policy.width_for(want)
        n = batch if policy.pad_batch else len(chunk)
        tensor = np.zeros((n, 3, h, w), dtype=np.float32)
        for slot, i in enumerate(chunk):
            rw = min(max(int(math.ceil(float(_F(h) * ratios[i]))), 1), w)
            resized = resize_bilinear(crops[i], rw, h)
            tensor[slot] = fill_chw(resized, w, h, bgr=True, imagenet=False)
        tensors.append(tensor)
        members.append(chunk)
    return tensors, members


# ---------------------------------------------------------------------------
# 解码（对应 rec.rs、cls.rs）
# ---------------------------------------------------------------------------


def load_dict(path: Path, classes: int) -> list[str]:
    """解码表：blank + 字典 + 空格（`rec.rs::parse_dict`、`build_table`）。"""
    lines = path.read_text(encoding="utf-8").split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    chars = [line[:-1] if line.endswith("\r") else line for line in lines]
    if classes == len(chars) + 2:
        return [""] + chars + [" "]
    if classes == len(chars) + 1:
        return [""] + chars
    raise ValueError(f"字典 {len(chars)} 类，模型输出 {classes} 类，对不上")


def ctc_decode(logits: F32, table: Sequence[str]) -> tuple[str, float]:
    """CTC 贪心解码一行：`logits` 是 `[T, C]`。返回 `(文字, 行置信度)`。"""
    idx = logits.argmax(axis=1)
    val = logits.max(axis=1)
    chars: list[str] = []
    confs: list[float] = []
    prev = -1
    for i, v in zip(idx.tolist(), val.tolist()):
        if i != 0 and i != prev and i < len(table):
            chars.append(table[i])
            confs.append(min(max(v, 0.0), 1.0))
        prev = i
    return "".join(chars), (sum(confs) / len(confs) if confs else 0.0)


def cls_flipped(probs: F32, thresh: float = 0.5) -> NDArray[np.bool_]:
    """方向分类的判定（`Classifier::classify`）：180° 的概率不小于 0° 的、且达到阈值。"""
    p = probs.astype(np.float64)
    row_sum = p.sum(axis=1)
    needs_softmax = ~(((p >= 0) & (p <= 1)).all(axis=1) & (np.abs(row_sum - 1.0) < 1e-3))
    if needs_softmax.any():
        e = np.exp(p - p.max(axis=1, keepdims=True))
        p = np.where(needs_softmax[:, None], e / e.sum(axis=1, keepdims=True), p)
    return (p[:, 1] >= p[:, 0]) & (p[:, 1] >= thresh)


# ---------------------------------------------------------------------------
# 一页的工作量
# ---------------------------------------------------------------------------


@dataclass
class Page:
    """一页的全部输入。`crops` 已经按 CLI 的结果转正（判为 180° 的转过了），识别直接用。"""

    name: str
    det: F32
    cls: list[F32]
    rec: list[F32]
    rec_members: list[list[int]]
    crops: list[U8Image]
    texts: list[str]

    @property
    def lines(self) -> int:
        return len(self.texts)


def build_page(image_path: Path, ocr_json: Path) -> tuple[Page, dict[str, Any]]:
    """读一页图和 CLI 的结果，算出三个模型的输入。"""
    img = load_rgb(image_path)
    result = json.loads(ocr_json.read_text(encoding="utf-8"))
    if (result["width"], result["height"]) != (img.shape[1], img.shape[0]):
        raise ValueError(f"{image_path.name}：图的尺寸和 CLI 结果里的对不上")
    det = det_tensor(img)
    cli_input = result["timings"]["det"]["input_size"]
    if [det.shape[3], det.shape[2]] != list(cli_input):
        raise ValueError(f"{image_path.name}：检测输入 {det.shape[3]}x{det.shape[2]}，CLI 是 {cli_input}")
    raw_crops: list[U8Image] = []
    rec_crops: list[U8Image] = []
    texts: list[str] = []
    vertical = 0
    for line in result["lines"]:
        quad = [(float(x), float(y)) for x, y in line["points"]]
        crop, is_vertical = crop_line(img, quad)
        if is_vertical != bool(line["vertical"]):
            raise ValueError(f"{image_path.name}：竖排判定和 CLI 不一致")
        vertical += int(is_vertical)
        raw_crops.append(crop)
        rec_crops.append(rotate_180(crop) if line["rotated_180"] else crop)
        texts.append(line["text"])
    cls = cls_batches(raw_crops)
    rec, members = rec_batches(rec_crops)
    page = Page(image_path.stem, det, cls, rec, members, rec_crops, texts)
    meta = {
        "name": page.name,
        "width": int(img.shape[1]),
        "height": int(img.shape[0]),
        "det_input": [int(det.shape[3]), int(det.shape[2])],
        "lines": len(rec_crops),
        "dropped_by_cli": int(result.get("dropped", 0)),
        "vertical": vertical,
        "rotated_180": sum(1 for line in result["lines"] if line["rotated_180"]),
        "cls_batches": len(cls),
        "rec_shapes": [[int(t.shape[0]), int(t.shape[3])] for t in rec],
    }
    return page, meta


def save_page(page: Page, path: Path) -> None:
    arrays: dict[str, Any] = {"det": page.det}
    for i, t in enumerate(page.cls):
        arrays[f"cls_{i}"] = t
    for i, t in enumerate(page.rec):
        arrays[f"rec_{i}"] = t
        arrays[f"recm_{i}"] = np.asarray(page.rec_members[i], dtype=np.int32)
    for i, c in enumerate(page.crops):
        arrays[f"crop_{i}"] = c
    arrays["texts"] = np.asarray(json.dumps(page.texts, ensure_ascii=False))
    np.savez(path, **arrays)


def load_page(path: Path, *, with_crops: bool = False) -> Page:
    """读回一页。计时用的时候不读裁切图（`with_crops=False`），省内存。"""
    with np.load(path) as z:
        n_cls = sum(1 for k in z.files if k.startswith("cls_"))
        n_rec = sum(1 for k in z.files if k.startswith("rec_"))
        n_crop = sum(1 for k in z.files if k.startswith("crop_"))
        return Page(
            name=path.stem,
            det=z["det"],
            cls=[z[f"cls_{i}"] for i in range(n_cls)],
            rec=[z[f"rec_{i}"] for i in range(n_rec)],
            rec_members=[z[f"recm_{i}"].tolist() for i in range(n_rec)],
            crops=[z[f"crop_{i}"] for i in range(n_crop)] if with_crops else [],
            texts=json.loads(str(z["texts"])),
        )


def build_cache(images: Path, ocr_json: Path, out: Path) -> dict[str, Any]:
    """整个目录：每页一个 npz，外加 `index.json`（页名、行数、各批形状）。"""
    out.mkdir(parents=True, exist_ok=True)
    pages: list[dict[str, Any]] = []
    started = time.perf_counter()
    for image_path in sorted(images.glob("*.png")):
        json_path = ocr_json / f"{image_path.stem}.json"
        if not json_path.is_file():
            raise FileNotFoundError(f"没有 {json_path}，先用 magpdf-cli ocr 跑一遍")
        page, meta = build_page(image_path, json_path)
        save_page(page, out / f"{page.name}.npz")
        pages.append(meta)
    index = {
        "images": str(images),
        "ocr_json": str(ocr_json),
        "pages": pages,
        "total_lines": sum(p["lines"] for p in pages),
        "dropped_by_cli": sum(p["dropped_by_cli"] for p in pages),
        "build_seconds": round(time.perf_counter() - started, 1),
    }
    (out / "index.json").write_text(json.dumps(index, ensure_ascii=False, indent=1), encoding="utf-8")
    return index


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--images", type=Path, required=True)
    ap.add_argument("--ocr-json", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    args = ap.parse_args()
    index = build_cache(args.images, args.ocr_json, args.out)
    shapes = {tuple(s) for p in index["pages"] for s in p["rec_shapes"]}
    print(
        f"{len(index['pages'])} 页，{index['total_lines']} 行（CLI 丢弃 {index['dropped_by_cli']} 行），"
        f"识别批次的形状 {len(shapes)} 种，用时 {index['build_seconds']} 秒",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()
