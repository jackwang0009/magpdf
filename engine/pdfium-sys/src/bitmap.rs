/// 渲染结果：BGRA 字节，每像素 4 字节，自顶向下逐行排列。
///
/// 数据从 PDFium 的位图复制而来，不再借用任何 PDFium 句柄，因此可以跨线程传给像素比对（方案 3.3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitmap {
    width: u32,
    height: u32,
    stride: usize,
    data: Vec<u8>,
}

impl Bitmap {
    pub(crate) fn new(width: u32, height: u32, stride: usize, data: Vec<u8>) -> Self {
        Self {
            width,
            height,
            stride,
            data,
        }
    }

    /// 从已有的 BGRA 缓冲区构造（测试和比对工具用）。`stride` 至少为 `width * 4`，
    /// `data` 至少 `stride * height` 字节，否则返回 `None`。
    pub fn from_bgra(width: u32, height: u32, stride: usize, data: Vec<u8>) -> Option<Self> {
        if stride < width as usize * 4 || data.len() < stride * height as usize {
            return None;
        }
        Some(Self::new(width, height, stride, data))
    }

    /// 生成单色位图，颜色为 `[b, g, r, a]`。
    pub fn filled(width: u32, height: u32, bgra: [u8; 4]) -> Self {
        let stride = width as usize * 4;
        let data = bgra
            .iter()
            .copied()
            .cycle()
            .take(stride * height as usize)
            .collect();
        Self::new(width, height, stride, data)
    }

    /// 写入一个像素，颜色为 `[b, g, r, a]`。越界时忽略。
    pub fn put_pixel(&mut self, x: u32, y: u32, bgra: [u8; 4]) {
        if x >= self.width || y >= self.height {
            return;
        }
        let i = y as usize * self.stride + x as usize * 4;
        self.data[i..i + 4].copy_from_slice(&bgra);
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// 每行字节数，可能大于 `width * 4`。
    pub fn stride(&self) -> usize {
        self.stride
    }

    /// 原始 BGRA 缓冲区（含行尾填充）。
    pub fn bgra(&self) -> &[u8] {
        &self.data
    }

    /// 第 `y` 行的像素数据，长度为 `width * 4`。
    pub fn row(&self, y: u32) -> &[u8] {
        let start = y as usize * self.stride;
        &self.data[start..start + self.width as usize * 4]
    }

    /// 读取像素，返回 `[b, g, r, a]`。
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let row = self.row(y);
        let i = x as usize * 4;
        [row[i], row[i + 1], row[i + 2], row[i + 3]]
    }

    /// 转成紧凑的 RGBA 字节（无行尾填充），便于写 PNG。
    pub fn to_rgba(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.width as usize * self.height as usize * 4);
        for y in 0..self.height {
            for px in self.row(y).as_chunks::<4>().0 {
                out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
            }
        }
        out
    }
}
