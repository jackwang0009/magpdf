//! 引擎进程的服务层（方案 3.1"编辑服务"）：命令与撤销、页面保真校验、命中测试索引、
//! 分层渲染、保存与恢复。P0 阶段先承载往返测试需要的像素比对与保真判定。
#![forbid(unsafe_code)]

pub mod roundtrip;

/// 往返测试与保真校验共用的像素比对（方案 2.3、13.2；P0 计划 V02 的判定规则）。
pub mod compare {
    use pdfium_sys::Bitmap;
    use serde::{Deserialize, Serialize};

    /// 比对参数。默认值即 P0 计划 V02 的判定规则，W1 校准后如有调整在这里改，之后不再动。
    #[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
    pub struct Thresholds {
        /// 任一颜色通道差值超过此值的像素算差异像素（0–255）。
        pub channel_tolerance: u8,
        /// 差异像素占页面面积的比例上限。
        pub max_diff_ratio: f64,
        /// 最大连通差异区域的像素数上限。
        pub max_region_pixels: usize,
    }

    impl Default for Thresholds {
        fn default() -> Self {
            Self {
                channel_tolerance: 24,
                max_diff_ratio: 0.0001,
                max_region_pixels: 16,
            }
        }
    }

    /// 比对结果。
    #[derive(Debug, Clone, PartialEq)]
    pub struct Comparison {
        pub diff_pixels: usize,
        pub total_pixels: usize,
        pub largest_region: usize,
        /// 差异像素的包围盒 `(x0, y0, x1, y1)`，无差异时为 `None`。
        pub diff_bbox: Option<(u32, u32, u32, u32)>,
    }

    impl Comparison {
        pub fn diff_ratio(&self) -> f64 {
            if self.total_pixels == 0 {
                0.0
            } else {
                self.diff_pixels as f64 / self.total_pixels as f64
            }
        }

        /// 是否判为"无差异"。
        pub fn passes(&self, t: &Thresholds) -> bool {
            self.diff_ratio() < t.max_diff_ratio && self.largest_region <= t.max_region_pixels
        }
    }

    /// 比对两张同尺寸位图。尺寸不同视为整页差异。
    pub fn compare(a: &Bitmap, b: &Bitmap, t: &Thresholds) -> Comparison {
        let (w, h) = (a.width(), a.height());
        let total_pixels = w as usize * h as usize;
        if (w, h) != (b.width(), b.height()) {
            return Comparison {
                diff_pixels: total_pixels,
                total_pixels,
                largest_region: total_pixels,
                diff_bbox: Some((0, 0, w, h)),
            };
        }

        let mut mask = vec![false; total_pixels];
        let mut diff_pixels = 0;
        let mut bbox: Option<(u32, u32, u32, u32)> = None;
        for y in 0..h {
            let (ra, rb) = (a.row(y), b.row(y));
            for x in 0..w {
                let i = x as usize * 4;
                let differs = (0..3).any(|c| ra[i + c].abs_diff(rb[i + c]) > t.channel_tolerance);
                if differs {
                    mask[y as usize * w as usize + x as usize] = true;
                    diff_pixels += 1;
                    bbox = Some(match bbox {
                        None => (x, y, x + 1, y + 1),
                        Some((x0, y0, x1, y1)) => {
                            (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1))
                        }
                    });
                }
            }
        }
        let largest_region = if diff_pixels == 0 {
            0
        } else {
            largest_connected_region(&mask, w as usize, h as usize)
        };
        Comparison {
            diff_pixels,
            total_pixels,
            largest_region,
            diff_bbox: bbox,
        }
    }

    /// 4 连通的最大差异区域大小（迭代式洪泛，避免深递归）。
    fn largest_connected_region(mask: &[bool], w: usize, h: usize) -> usize {
        let mut visited = vec![false; mask.len()];
        let mut stack = Vec::new();
        let mut largest = 0;
        for start in 0..mask.len() {
            if !mask[start] || visited[start] {
                continue;
            }
            visited[start] = true;
            stack.push(start);
            let mut size = 0;
            while let Some(p) = stack.pop() {
                size += 1;
                let (x, y) = (p % w, p / w);
                let neighbours = [
                    (x > 0).then(|| p - 1),
                    (x + 1 < w).then(|| p + 1),
                    (y > 0).then(|| p - w),
                    (y + 1 < h).then(|| p + w),
                ];
                for n in neighbours.into_iter().flatten() {
                    if mask[n] && !visited[n] {
                        visited[n] = true;
                        stack.push(n);
                    }
                }
            }
            largest = largest.max(size);
        }
        largest
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const WHITE: [u8; 4] = [255, 255, 255, 255];
        const BLACK: [u8; 4] = [0, 0, 0, 255];

        #[test]
        fn identical_bitmaps_pass() {
            let a = Bitmap::filled(100, 100, WHITE);
            let c = compare(&a, &a.clone(), &Thresholds::default());
            assert_eq!(c.diff_pixels, 0);
            assert_eq!(c.largest_region, 0);
            assert!(c.passes(&Thresholds::default()));
        }

        #[test]
        fn small_change_within_tolerance_is_ignored() {
            let a = Bitmap::filled(10, 10, WHITE);
            let mut b = a.clone();
            b.put_pixel(3, 3, [240, 240, 240, 255]);
            let c = compare(&a, &b, &Thresholds::default());
            assert_eq!(c.diff_pixels, 0);
        }

        #[test]
        fn region_size_is_connected_component() {
            // 1000×1000 = 1e6 像素；阈值 0.01% 即 100 像素。
            let a = Bitmap::filled(1000, 1000, WHITE);
            let mut b = a.clone();
            // 两个互不相连的 3×3 黑块：共 18 个差异像素，最大连通区 9。
            for (ox, oy) in [(10, 10), (500, 500)] {
                for dx in 0..3 {
                    for dy in 0..3 {
                        b.put_pixel(ox + dx, oy + dy, BLACK);
                    }
                }
            }
            let c = compare(&a, &b, &Thresholds::default());
            assert_eq!(c.diff_pixels, 18);
            assert_eq!(c.largest_region, 9);
            assert_eq!(c.diff_bbox, Some((10, 10, 503, 503)));
            assert!(c.passes(&Thresholds::default()));

            // 一个 5×5 黑块：25 像素连通，超过 16 的上限。
            let mut d = a.clone();
            for dx in 0..5 {
                for dy in 0..5 {
                    d.put_pixel(100 + dx, 100 + dy, BLACK);
                }
            }
            let c = compare(&a, &d, &Thresholds::default());
            assert_eq!(c.largest_region, 25);
            assert!(!c.passes(&Thresholds::default()));
        }

        #[test]
        fn size_mismatch_is_total_difference() {
            let a = Bitmap::filled(10, 10, WHITE);
            let b = Bitmap::filled(11, 10, WHITE);
            let c = compare(&a, &b, &Thresholds::default());
            assert_eq!(c.diff_pixels, 100);
            assert!(!c.passes(&Thresholds::default()));
        }
    }
}
