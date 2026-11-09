//! 字体来源（方案 5.4）：系统字体与内置开源字体。
//!
//! 1. 系统字体：读 `C:\Windows\Fonts` 下的字体文件，按 OS/2 fsType 判断能否嵌入；
//! 2. 内置开源字体：`assets/fonts/` 里的子集（OFL 1.1，按 OFL 改名），随软件分发；
//! 3. 网络：环境变量 `MAGPDF_FONT_DIR` 指向额外字体目录时一并扫描（CI 与评测用）。
//!
//! 内置子集缺失时只用系统字体，报告里会写明（网络下载的字体不入库，避免无谓的体积）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::fontfile::{FontFace, fs_type_allows_embedding};

/// PDF 字体描述符 Flags 的位（PDF 32000-1 表 123）。
pub const FLAG_FIXED_PITCH: u32 = 1;
pub const FLAG_SERIF: u32 = 1 << 1;
pub const FLAG_SYMBOLIC: u32 = 1 << 2;
pub const FLAG_NONSYMBOLIC: u32 = 1 << 5;
pub const FLAG_ITALIC: u32 = 1 << 6;
pub const FLAG_FORCE_BOLD: u32 = 1 << 18;

/// 字体族分类（方案 5.4 列出的中文字体，加上常见的西文类别）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Family {
    Song,
    Hei,
    Kai,
    FangSong,
    YaHei,
    Serif,
    Sans,
    Mono,
    Unknown,
}

impl Family {
    /// 从 PDF 的 BaseFont 名（已去子集前缀）推断族。
    pub fn from_name(name: &str) -> Self {
        let n = name.to_ascii_lowercase();
        let has = |keys: &[&str]| keys.iter().any(|k| n.contains(k));
        if has(&[
            "fangsong",
            "fang song",
            "fangsong_gb2312",
            "stfangsong",
            "simfang",
            "仿宋",
        ]) {
            return Self::FangSong;
        }
        if has(&["kai", "zhuque", "stkaiti", "楷"]) {
            return Self::Kai;
        }
        if has(&["yahei", "雅黑"]) {
            return Self::YaHei;
        }
        if has(&[
            "heiti",
            "simhei",
            "stheiti",
            "黑体",
            "sourcehan-",
            "sourcehansans",
        ]) {
            return Self::Hei;
        }
        if has(&[
            "song",
            "simsun",
            "stsong",
            "nsimsun",
            "ming",
            "宋",
            "sourcehanserif",
            "sourcehan-regular",
        ]) {
            return Self::Song;
        }
        if has(&["courier", "mono", "consol", "cascadia"]) {
            return Self::Mono;
        }
        if has(&["times", "serif", "georgia", "book", "cambria", "antiqua"]) {
            return Self::Serif;
        }
        if has(&[
            "arial",
            "helvetica",
            "sans",
            "calibri",
            "segoe",
            "verdana",
            "tahoma",
        ]) {
            return Self::Sans;
        }
        Self::Unknown
    }
}

/// 一个候选字体。
#[derive(Debug, Clone)]
pub struct Candidate {
    pub path: PathBuf,
    pub index: u32,
    pub family: Family,
    /// 字重（400、700 等）。
    pub weight: i32,
    pub italic: bool,
    /// 字体文件里的 PS 名。
    pub name: String,
    /// 是否内置（`assets/fonts` 或 `MAGPDF_FONT_DIR`）。
    pub builtin: bool,
    /// OS/2 fsType；`None` 表示字体没有 OS/2 表。
    pub fs_type: Option<u16>,
}

impl Candidate {
    pub fn embeddable(&self) -> bool {
        self.fs_type.map(fs_type_allows_embedding).unwrap_or(true)
    }

    /// 与目标族的匹配分数：族相同 0，同类（中文/西文）1，不同 2。
    fn family_score(&self, want: Family) -> u8 {
        if self.family == want {
            return 0;
        }
        let cjk = |f: Family| {
            matches!(
                f,
                Family::Song | Family::Hei | Family::Kai | Family::FangSong | Family::YaHei
            )
        };
        if cjk(self.family) == cjk(want) || want == Family::Unknown {
            1
        } else {
            2
        }
    }
}

/// 字体库：候选列表加一个按需加载的缓存。
#[derive(Debug, Default)]
pub struct FontLibrary {
    pub candidates: Vec<Candidate>,
    /// 加载过的字体面（路径, 面索引）→ 字体。
    cache: BTreeMap<(PathBuf, u32), std::sync::Arc<FontFace>>,
}

impl FontLibrary {
    /// 扫描系统字体目录（Windows 用 `%WINDIR%\Fonts`，其他平台此时返回空表）与内置字体目录。
    pub fn scan_default() -> Self {
        let mut lib = Self::default();
        if let Some(dir) = system_fonts_dir() {
            lib.add_dir(&dir, false);
        }
        for dir in builtin_dirs() {
            lib.add_dir(&dir, true);
        }
        lib
    }

    /// 进程级的默认字体库（第一次调用时扫描）。
    pub fn shared() -> &'static FontLibrary {
        static LIB: OnceLock<FontLibrary> = OnceLock::new();
        LIB.get_or_init(Self::scan_default)
    }

    /// 加入一个目录下的全部字体（`.ttf`/`.otf`/`.ttc`，TTC 只取第 0 面，Windows 的字体集合
    /// 第 0 面就是常规体）。
    pub fn add_dir(&mut self, dir: &Path, builtin: bool) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut names: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                matches!(
                    p.extension()
                        .and_then(|e| e.to_str())
                        .map(|e| e.to_ascii_lowercase())
                        .as_deref(),
                    Some("ttf" | "otf" | "ttc")
                )
            })
            .collect();
        names.sort();
        for path in names {
            for index in 0..ttc_faces(&path) {
                if let Ok(face) = FontFace::from_path(&path, index) {
                    let stem = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let (family, weight, italic) = classify(&stem, &face);
                    self.candidates.push(Candidate {
                        path: path.clone(),
                        index,
                        family,
                        weight,
                        italic,
                        name: face.postscript_name().to_owned(),
                        builtin,
                        fs_type: face.fs_type(),
                    });
                }
            }
        }
    }

    /// 按族、字重、斜体挑最合适的**可嵌入**字体。
    pub fn best_match(&self, family: Family, weight: i32, italic: bool) -> Option<&Candidate> {
        self.candidates
            .iter()
            .filter(|c| c.embeddable())
            .min_by_key(|c| {
                (
                    c.family_score(family),
                    (c.weight - weight).abs(),
                    u8::from(c.italic != italic),
                    u8::from(!c.builtin),
                    family_name_rank(c),
                    c.name.clone(),
                )
            })
    }

    /// 名字完全一致的字体（第 2 级的"系统里装有同名字体"）。
    pub fn by_name(&self, name: &str) -> Option<&Candidate> {
        let want = name.to_ascii_lowercase();
        self.candidates
            .iter()
            .find(|c| c.name.to_ascii_lowercase() == want)
    }

    /// 加载字体面（带缓存）。
    pub fn face(&mut self, cand: &Candidate) -> crate::error::Result<std::sync::Arc<FontFace>> {
        let key = (cand.path.clone(), cand.index);
        if let Some(f) = self.cache.get(&key) {
            return Ok(f.clone());
        }
        let face = std::sync::Arc::new(FontFace::from_path(&cand.path, cand.index)?);
        self.cache.insert(key, face.clone());
        Ok(face)
    }
}

/// 系统自带的常见字体优先级（同族同字重时先选这些）。
fn family_name_rank(c: &Candidate) -> u8 {
    let ps = c.name.to_ascii_lowercase();
    const PREFERRED: [&str; 6] = [
        "simsun",
        "simhei",
        "simfang",
        "simkai",
        "microsoftyahei",
        "msyh",
    ];
    if PREFERRED.iter().any(|p| ps.starts_with(p)) {
        return 0;
    }
    if c.family == Family::Unknown { 2 } else { 1 }
}

/// 系统字体目录。
pub fn system_fonts_dir() -> Option<PathBuf> {
    let windir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
    let dir = PathBuf::from(windir).join("Fonts");
    dir.is_dir().then_some(dir)
}

/// 配置（字体来源、排版开关）。
#[derive(Debug, Clone, Default)]
pub struct FontConfig {
    /// 额外的字体目录（`MAGPDF_FONT_DIR`），CI 与评测用。
    pub extra_dirs: Vec<PathBuf>,
}

impl FontConfig {
    pub fn from_env() -> Self {
        let mut extra_dirs = Vec::new();
        if let Some(v) = std::env::var_os("MAGPDF_FONT_DIR") {
            extra_dirs.extend(std::env::split_paths(&v).filter(|p| p.is_dir()));
        }
        Self { extra_dirs }
    }
}

fn builtin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dirs.push(manifest.join("..").join("..").join("assets").join("fonts"));
    dirs.extend(FontConfig::from_env().extra_dirs);
    dirs
}

/// TTC 的面数；普通字体为 1。只看文件头，不解析整个字体。
fn ttc_faces(path: &Path) -> u32 {
    use std::io::Read;
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return 1,
    };
    let mut head = [0u8; 12];
    if f.read_exact(&mut head).is_err() {
        return 1;
    }
    if &head[0..4] == b"ttcf" {
        u32::from_be_bytes([head[8], head[9], head[10], head[11]]).clamp(1, 64)
    } else {
        1
    }
}

/// 从文件名与字体名判断族、字重与斜体。
fn classify(stem: &str, face: &FontFace) -> (Family, i32, bool) {
    let n = format!(
        "{} {}",
        stem.to_ascii_lowercase(),
        face.postscript_name().to_ascii_lowercase()
    );
    let family = Family::from_name(&n);
    let bold = n.contains("bold") || n.ends_with("b") || n.contains("bd");
    let weight = if n.contains("light") || n.contains("l.ttf") {
        300
    } else if bold {
        700
    } else {
        400
    };
    let italic =
        n.contains("italic") || n.contains("oblique") || n.contains("i.ttf") || n.contains("_i");
    (family, weight, italic)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_from_base_font_names() {
        assert_eq!(Family::from_name("ABCDEF+SimSun"), Family::Song);
        assert_eq!(Family::from_name("STSong-Light"), Family::Song);
        assert_eq!(Family::from_name("SimHei"), Family::Hei);
        assert_eq!(Family::from_name("SimFang"), Family::FangSong);
        assert_eq!(Family::from_name("STFangsong"), Family::FangSong);
        assert_eq!(Family::from_name("KaiTi"), Family::Kai);
        assert_eq!(Family::from_name("MicrosoftYaHei"), Family::YaHei);
        assert_eq!(Family::from_name("Arial-BoldMT"), Family::Sans);
        assert_eq!(Family::from_name("TimesNewRomanPS-BoldMT"), Family::Serif);
        assert_eq!(Family::from_name("CourierNewPSMT"), Family::Mono);
        assert_eq!(Family::from_name("Weird"), Family::Unknown);
    }

    #[test]
    fn system_library_matches_chinese_families() {
        let lib = FontLibrary::scan_default();
        if lib.candidates.is_empty() {
            return;
        }
        for (family, must_have) in [
            (Family::Song, "simsun"),
            (Family::Hei, "simhei"),
            (Family::FangSong, "simfang"),
            (Family::Kai, "simkai"),
            (Family::YaHei, "msyh"),
        ] {
            let m = lib.best_match(family, 400, false).expect("应匹配到字体");
            assert_eq!(m.family, family, "{family:?} 匹配到了 {:?}", m.path);
            assert!(
                m.path
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .contains(must_have),
                "{family:?} → {}",
                m.path.display()
            );
            assert!(m.embeddable());
        }
        // 本机没有黑体粗体文件（simhei 只有常规体），改用西文 Arial 验证字重匹配。
        let bold = lib
            .best_match(Family::Sans, 700, false)
            .expect("应匹配到粗体");
        assert_eq!(bold.weight, 700);
        assert!(
            bold.name.to_ascii_lowercase().contains("bold"),
            "{}",
            bold.name
        );
        assert!(
            lib.best_match(Family::Sans, 400, true)
                .is_some_and(|c| c.italic)
        );
    }

    #[test]
    fn ttc_faces_counts_faces() {
        let path = Path::new(r"C:\Windows\Fonts\simsun.ttc");
        if path.is_file() {
            assert!(ttc_faces(path) >= 1);
        }
        let single = Path::new(r"C:\Windows\Fonts\simhei.ttf");
        if single.is_file() {
            assert_eq!(ttc_faces(single), 1);
        }
    }
}
