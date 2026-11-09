//! 位图输入与内部图像操作：像素格式转换、双线性缩放、透视裁切、旋转，以及写入模型输入张量。
//!
//! 缩放采用与 OpenCV `INTER_LINEAR` 相同的像素中心对齐规则，使结果与 PaddleOCR/RapidOCR 的预处理可比。

use super::geometry::{Point, Quad};
use crate::ocr::OcrError;

/// 像素格式。PDFium 渲染结果是 [`PixelFormat::Bgra8`]（`pdfium_sys::Bitmap`），`image` crate 解码的是 RGB/灰度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Bgra8,
    Rgba8,
    Rgb8,
    Bgr8,
    Gray8,
}

impl PixelFormat {
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Bgra8 | Self::Rgba8 => 4,
            Self::Rgb8 | Self::Bgr8 => 3,
            Self::Gray8 => 1,
        }
    }
}

/// 借用调用方像素缓冲区的位图视图，不复制。
#[derive(Debug, Clone, Copy)]
pub struct ImageView<'a> {
    width: u32,
    height: u32,
    stride: usize,
    format: PixelFormat,
    data: &'a [u8],
}

impl<'a> ImageView<'a> {
    /// `stride` 为每行字节数（可含行尾填充），`data` 至少 `stride * height` 字节。
    pub fn new(
        width: u32,
        height: u32,
        stride: usize,
        format: PixelFormat,
        data: &'a [u8],
    ) -> Result<Self, OcrError> {
        let min_stride = width as usize * format.bytes_per_pixel();
        if width == 0 || height == 0 {
            return Err(OcrError::InvalidImage(format!("尺寸为 {width}x{height}")));
        }
        if stride < min_stride {
            return Err(OcrError::InvalidImage(format!(
                "行距 {stride} 小于 {width} 像素 × {} 字节",
                format.bytes_per_pixel()
            )));
        }
        // 最后一行可以不带行尾填充。
        let needed = stride * (height as usize - 1) + min_stride;
        if data.len() < needed {
            return Err(OcrError::InvalidImage(format!(
                "缓冲区 {} 字节，至少需要 {needed} 字节",
                data.len()
            )));
        }
        Ok(Self {
            width,
            height,
            stride,
            format,
            data,
        })
    }

    /// BGRA 位图（PDFium 渲染结果）。
    pub fn bgra(width: u32, height: u32, stride: usize, data: &'a [u8]) -> Result<Self, OcrError> {
        Self::new(width, height, stride, PixelFormat::Bgra8, data)
    }

    /// 紧凑排列的 RGB 位图。
    pub fn rgb(width: u32, height: u32, data: &'a [u8]) -> Result<Self, OcrError> {
        Self::new(width, height, width as usize * 3, PixelFormat::Rgb8, data)
    }

    /// 紧凑排列的 8 位灰度位图。
    pub fn gray(width: u32, height: u32, data: &'a [u8]) -> Result<Self, OcrError> {
        Self::new(width, height, width as usize, PixelFormat::Gray8, data)
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn format(&self) -> PixelFormat {
        self.format
    }
}

/// 内部使用的紧凑 RGB 图像（每像素 3 字节）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RgbImage {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) data: Vec<u8>,
}

impl RgbImage {
    pub(crate) fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            data: vec![0; width * height * 3],
        }
    }

    pub(crate) fn from_view(v: &ImageView<'_>) -> Self {
        let (w, h) = (v.width as usize, v.height as usize);
        let mut out = Self::new(w, h);
        let bpp = v.format.bytes_per_pixel();
        for y in 0..h {
            let row = &v.data[y * v.stride..y * v.stride + w * bpp];
            let dst = &mut out.data[y * w * 3..(y + 1) * w * 3];
            let dst = dst.as_chunks_mut::<3>().0;
            match v.format {
                PixelFormat::Bgra8 => {
                    for (d, s) in dst.iter_mut().zip(row.as_chunks::<4>().0) {
                        *d = [s[2], s[1], s[0]];
                    }
                }
                PixelFormat::Rgba8 => {
                    for (d, s) in dst.iter_mut().zip(row.as_chunks::<4>().0) {
                        *d = [s[0], s[1], s[2]];
                    }
                }
                PixelFormat::Rgb8 => {
                    for (d, s) in dst.iter_mut().zip(row.as_chunks::<3>().0) {
                        *d = *s;
                    }
                }
                PixelFormat::Bgr8 => {
                    for (d, s) in dst.iter_mut().zip(row.as_chunks::<3>().0) {
                        *d = [s[2], s[1], s[0]];
                    }
                }
                PixelFormat::Gray8 => {
                    for (d, &g) in dst.iter_mut().zip(row.iter()) {
                        *d = [g, g, g];
                    }
                }
            }
        }
        out
    }

    #[inline]
    pub(crate) fn pixel(&self, x: usize, y: usize) -> [u8; 3] {
        let i = (y * self.width + x) * 3;
        [self.data[i], self.data[i + 1], self.data[i + 2]]
    }

    /// 在 `(fx, fy)`（像素中心为整数坐标）处双线性采样，越界部分复制边缘（`BORDER_REPLICATE`）。
    #[inline]
    fn sample_bilinear(&self, fx: f32, fy: f32) -> [u8; 3] {
        let max_x = (self.width - 1) as f32;
        let max_y = (self.height - 1) as f32;
        let fx = fx.clamp(0.0, max_x);
        let fy = fy.clamp(0.0, max_y);
        let x0 = fx.floor() as usize;
        let y0 = fy.floor() as usize;
        let x1 = (x0 + 1).min(self.width - 1);
        let y1 = (y0 + 1).min(self.height - 1);
        let tx = fx - x0 as f32;
        let ty = fy - y0 as f32;
        let p00 = self.pixel(x0, y0);
        let p10 = self.pixel(x1, y0);
        let p01 = self.pixel(x0, y1);
        let p11 = self.pixel(x1, y1);
        let mut out = [0u8; 3];
        for c in 0..3 {
            let top = p00[c] as f32 + (p10[c] as f32 - p00[c] as f32) * tx;
            let bottom = p01[c] as f32 + (p11[c] as f32 - p01[c] as f32) * tx;
            out[c] = (top + (bottom - top) * ty + 0.5).clamp(0.0, 255.0) as u8;
        }
        out
    }

    /// 双线性缩放到 `(nw, nh)`，像素中心对齐（OpenCV `INTER_LINEAR`：`src = (dst + 0.5) * scale - 0.5`）。
    pub(crate) fn resize_bilinear(&self, nw: usize, nh: usize) -> Self {
        let nw = nw.max(1);
        let nh = nh.max(1);
        if nw == self.width && nh == self.height {
            return self.clone();
        }
        let sx = self.width as f32 / nw as f32;
        let sy = self.height as f32 / nh as f32;
        let mut out = Self::new(nw, nh);
        // 预先算好每列的源坐标与权重，避免内层循环重复计算。
        let cols: Vec<(usize, usize, f32)> = (0..nw)
            .map(|x| {
                let fx = ((x as f32 + 0.5) * sx - 0.5).clamp(0.0, (self.width - 1) as f32);
                let x0 = fx.floor() as usize;
                (x0, (x0 + 1).min(self.width - 1), fx - x0 as f32)
            })
            .collect();
        for y in 0..nh {
            let fy = ((y as f32 + 0.5) * sy - 0.5).clamp(0.0, (self.height - 1) as f32);
            let y0 = fy.floor() as usize;
            let y1 = (y0 + 1).min(self.height - 1);
            let ty = fy - y0 as f32;
            let row0 = &self.data[y0 * self.width * 3..(y0 + 1) * self.width * 3];
            let row1 = &self.data[y1 * self.width * 3..(y1 + 1) * self.width * 3];
            let dst = &mut out.data[y * nw * 3..(y + 1) * nw * 3];
            for (x, &(x0, x1, tx)) in cols.iter().enumerate() {
                for c in 0..3 {
                    let a = row0[x0 * 3 + c] as f32;
                    let b = row0[x1 * 3 + c] as f32;
                    let cc = row1[x0 * 3 + c] as f32;
                    let d = row1[x1 * 3 + c] as f32;
                    let top = a + (b - a) * tx;
                    let bottom = cc + (d - cc) * tx;
                    dst[x * 3 + c] = (top + (bottom - top) * ty + 0.5).clamp(0.0, 255.0) as u8;
                }
            }
        }
        out
    }

    /// 按四点框透视裁切为 `w × h` 的正立图像（PaddleOCR `get_rotate_crop_image` 的前半部分）。
    /// 四点顺序为左上、右上、右下、左下。
    pub(crate) fn warp_quad(&self, quad: &Quad, w: usize, h: usize) -> Self {
        let w = w.max(1);
        let h = h.max(1);
        let dst = [
            Point::new(0.0, 0.0),
            Point::new(w as f32, 0.0),
            Point::new(w as f32, h as f32),
            Point::new(0.0, h as f32),
        ];
        let mut out = Self::new(w, h);
        let Some(m) = homography(&dst, quad) else {
            return out;
        };
        for y in 0..h {
            let row = &mut out.data[y * w * 3..(y + 1) * w * 3];
            for x in 0..w {
                let (xf, yf) = (x as f32, y as f32);
                let den = m[6] * xf + m[7] * yf + m[8];
                let (sx, sy) = if den.abs() <= f32::EPSILON {
                    (0.0, 0.0)
                } else {
                    (
                        (m[0] * xf + m[1] * yf + m[2]) / den,
                        (m[3] * xf + m[4] * yf + m[5]) / den,
                    )
                };
                let p = self.sample_bilinear(sx, sy);
                row[x * 3..x * 3 + 3].copy_from_slice(&p);
            }
        }
        out
    }

    pub(crate) fn rotate_180(&self) -> Self {
        let mut out = Self::new(self.width, self.height);
        for (d, s) in out
            .data
            .as_chunks_mut::<3>()
            .0
            .iter_mut()
            .zip(self.data.as_chunks::<3>().0.iter().rev())
        {
            *d = *s;
        }
        out
    }

    /// 逆时针旋转 90°（`numpy.rot90`），竖排文字行转成横排。
    pub(crate) fn rotate_90_ccw(&self) -> Self {
        let (w, h) = (self.width, self.height);
        let mut out = Self::new(h, w);
        for y in 0..h {
            for x in 0..w {
                // 新图第 (w-1-x) 行、第 y 列取自旧图 (x, y)。
                let ny = w - 1 - x;
                let nx = y;
                let p = self.pixel(x, y);
                let i = (ny * h + nx) * 3;
                out.data[i..i + 3].copy_from_slice(&p);
            }
        }
        out
    }
}

/// 求把 `src` 四点映射到 `dst` 四点的单应矩阵（行优先 3×3，`m[8]` 归一为 1）。
/// 这里用于从目标图坐标反查源图坐标，所以调用方传入 `(目标四点, 源四点)`。
fn homography(src: &Quad, dst: &Quad) -> Option<[f32; 9]> {
    // 8 个未知数 h0..h7（h8 = 1），每对点给出两个方程。
    let mut a = [[0f64; 9]; 8];
    for i in 0..4 {
        let (x, y) = (src[i].x as f64, src[i].y as f64);
        let (u, v) = (dst[i].x as f64, dst[i].y as f64);
        a[2 * i] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
        a[2 * i + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
    }
    // 高斯消元（列主元）。
    for col in 0..8 {
        let pivot = (col..8).max_by(|&r1, &r2| {
            a[r1][col]
                .abs()
                .partial_cmp(&a[r2][col].abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;
        if a[pivot][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, pivot);
        for r in 0..8 {
            if r != col {
                let f = a[r][col] / a[col][col];
                if f != 0.0 {
                    let pivot_row = a[col];
                    for (dst, src) in a[r][col..].iter_mut().zip(&pivot_row[col..]) {
                        *dst -= f * src;
                    }
                }
            }
        }
    }
    let mut m = [0f32; 9];
    for i in 0..8 {
        m[i] = (a[i][8] / a[i][i]) as f32;
    }
    m[8] = 1.0;
    Some(m)
}

/// 归一化方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Normalize {
    /// `(x/255 - mean) / std`，ImageNet 均值方差（det、文字行方向模型）。
    ImageNet,
    /// `(x/255 - 0.5) / 0.5`（rec、旧版 cls）。
    Half,
}

/// 写入张量时的通道顺序。PaddleOCR 的 det/rec 用 OpenCV 读图，是 BGR；PaddleClas 系的分类模型是 RGB。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChannelOrder {
    Bgr,
    Rgb,
}

const IMAGENET_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const IMAGENET_STD: [f32; 3] = [0.229, 0.224, 0.225];

/// 把图像写进 CHW 浮点张量的一个平面组：`out` 长度为 `3 * plane_h * plane_w`，图像放在左上角，
/// 其余位置保持原值（调用方负责预先清零以实现右侧补零）。
pub(crate) fn fill_chw(
    img: &RgbImage,
    out: &mut [f32],
    plane_w: usize,
    plane_h: usize,
    order: ChannelOrder,
    norm: Normalize,
) {
    debug_assert!(out.len() >= 3 * plane_w * plane_h);
    debug_assert!(img.width <= plane_w && img.height <= plane_h);
    // 输出通道 c 取源 RGB 的哪个下标：Bgr 时通道 0 是 B，即源下标 2。
    let src_index: [usize; 3] = match order {
        ChannelOrder::Bgr => [2, 1, 0],
        ChannelOrder::Rgb => [0, 1, 2],
    };
    for c in 0..3 {
        let (mean, std) = match norm {
            // 均值方差按位置套用，与 PaddleOCR 的 NormalizeImage 一致：det/rec 的图是 BGR，
            // 第 0 个通道（B）配 0.485；PaddleClas 系模型的图是 RGB，第 0 个通道（R）配 0.485。
            Normalize::ImageNet => (IMAGENET_MEAN[c], IMAGENET_STD[c]),
            Normalize::Half => (0.5, 0.5),
        };
        let scale = 1.0 / (255.0 * std);
        let offset = -mean / std;
        let plane = &mut out[c * plane_w * plane_h..(c + 1) * plane_w * plane_h];
        for y in 0..img.height {
            let src_row = &img.data[y * img.width * 3..(y + 1) * img.width * 3];
            let dst_row = &mut plane[y * plane_w..y * plane_w + img.width];
            for (d, px) in dst_row.iter_mut().zip(src_row.as_chunks::<3>().0) {
                *d = px[src_index[c]] as f32 * scale + offset;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: usize, h: usize) -> RgbImage {
        let mut img = RgbImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 3;
                img.data[i] = (x * 255 / (w - 1).max(1)) as u8;
                img.data[i + 1] = (y * 255 / (h - 1).max(1)) as u8;
                img.data[i + 2] = 128;
            }
        }
        img
    }

    #[test]
    fn view_validates_buffer() {
        let buf = vec![0u8; 10 * 4 * 5];
        assert!(ImageView::bgra(10, 5, 40, &buf).is_ok());
        assert!(ImageView::bgra(10, 5, 44, &buf).is_err());
        assert!(ImageView::bgra(0, 5, 40, &buf).is_err());
        assert!(ImageView::rgb(10, 5, &buf[..100]).is_err());
    }

    #[test]
    fn bgra_to_rgb_swaps_channels() {
        let buf = [10u8, 20, 30, 255, 40, 50, 60, 255];
        let v = ImageView::bgra(2, 1, 8, &buf).unwrap();
        let img = RgbImage::from_view(&v);
        assert_eq!(img.pixel(0, 0), [30, 20, 10]);
        assert_eq!(img.pixel(1, 0), [60, 50, 40]);
    }

    #[test]
    fn gray_expands_to_three_channels() {
        let buf = [7u8, 200];
        let img = RgbImage::from_view(&ImageView::gray(2, 1, &buf).unwrap());
        assert_eq!(img.pixel(1, 0), [200, 200, 200]);
    }

    #[test]
    fn resize_identity_and_halving() {
        let img = gradient(8, 4);
        assert_eq!(img.resize_bilinear(8, 4), img);
        let half = img.resize_bilinear(4, 2);
        assert_eq!((half.width, half.height), (4, 2));
        // 均匀渐变缩小后仍单调。
        let r: Vec<u8> = (0..4).map(|x| half.pixel(x, 0)[0]).collect();
        assert!(r.windows(2).all(|w| w[0] <= w[1]), "{r:?}");
    }

    #[test]
    fn warp_axis_aligned_quad_equals_crop() {
        let img = gradient(20, 10);
        let quad = [
            Point::new(4.0, 2.0),
            Point::new(14.0, 2.0),
            Point::new(14.0, 8.0),
            Point::new(4.0, 8.0),
        ];
        let crop = img.warp_quad(&quad, 10, 6);
        assert_eq!(crop.pixel(0, 0), img.pixel(4, 2));
        assert_eq!(crop.pixel(9, 5), img.pixel(13, 7));
    }

    #[test]
    fn rotations() {
        let img = gradient(3, 2);
        let r = img.rotate_180();
        assert_eq!(r.pixel(0, 0), img.pixel(2, 1));
        let ccw = img.rotate_90_ccw();
        assert_eq!((ccw.width, ccw.height), (2, 3));
        // 旧图右上角 (2,0) 旋到新图左上角 (0,0)。
        assert_eq!(ccw.pixel(0, 0), img.pixel(2, 0));
        // 旧图左上角 (0,0) 旋到新图左下角 (0,2)。
        assert_eq!(ccw.pixel(0, 2), img.pixel(0, 0));
    }

    #[test]
    fn fill_chw_normalizes_and_pads() {
        let mut img = RgbImage::new(2, 1);
        img.data.copy_from_slice(&[255, 0, 0, 0, 0, 255]);
        // 3 通道 × 高 1 × 宽 4。
        let mut out = vec![0f32; 12];
        fill_chw(&img, &mut out, 4, 1, ChannelOrder::Bgr, Normalize::Half);
        // 通道 0（B）：像素 0 的 B=0 → -1，像素 1 的 B=255 → 1；右侧补零保持 0。
        assert_eq!(&out[0..4], &[-1.0, 1.0, 0.0, 0.0]);
        // 通道 2（R）：像素 0 的 R=255 → 1。
        assert_eq!(&out[8..12], &[1.0, -1.0, 0.0, 0.0]);

        let mut out = vec![0f32; 3 * 2];
        fill_chw(&img, &mut out, 2, 1, ChannelOrder::Rgb, Normalize::ImageNet);
        let expect_r = (1.0 - 0.485) / 0.229;
        assert!((out[0] - expect_r).abs() < 1e-5);
    }
}
