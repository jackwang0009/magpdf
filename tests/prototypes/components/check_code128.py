"""用 ZXing 识别引擎画出来的 Code 128 条码，和应有的内容逐条对比。

先让引擎把样本画出来（PPM 和 expected.txt，一行一个内容）：

    $env:MAGPDF_COMPONENT_DUMP = "<目录>"
    cargo test -p magpdf-engine -- --ignored dump_code128

再识别：

    python tests/prototypes/components/check_code128.py <目录>

全部识别对了退出码 0，否则 1。要装 zxing-cpp 和 Pillow。
"""

from __future__ import annotations

import sys
from pathlib import Path

import zxingcpp
from PIL import Image


def main(directory: Path) -> int:
    expected = (directory / "expected.txt").read_text(encoding="utf-8").split("\n")[:-1]
    failures = 0
    for index, content in enumerate(expected):
        image = Image.open(directory / f"code128-{index}.ppm")
        results = zxingcpp.read_barcodes(image, formats=zxingcpp.BarcodeFormat.Code128)
        decoded = [r.text for r in results]
        ok = decoded == [content]
        failures += not ok
        print(f"{'ok ' if ok else 'BAD'} #{index} {len(content)} 个字符 -> {decoded!r}")
    print(f"{len(expected) - failures}/{len(expected)} 条识别正确")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(Path(sys.argv[1])))
