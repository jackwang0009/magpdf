//! 保存（方案 9.1）。P0 原型只有完整重写，没有增量保存：PDFium 的增量保存写出的文件，qpdf 对约三分之一报
//! 交叉引用损坏（V17 报告 9.7），这个问题解决之前不用它。
//!
//! 做法：
//! 1. 已签名的文档不存：完整重写会让签名失效（方案 9.2 要求这样的文档只能增量保存）；
//! 2. PDFium 完整存一份；
//! 3. 只留从 trailer 顺着引用找得到的对象重写一遍（[`magpdf_content::rewrite::save_reachable`]）。PDFium 把
//!    没有引用的对象也写出去：组件改参数之前那一版、撤销了的置入、生成器从资源字典里摘掉的字体和图像，
//!    不去掉的话这些旧内容都留在文件里；
//! 4. 用 PDFium 重新打开写出来的字节，页数对得上才算数；第 3 步的结果通不过时退回第 2 步的结果，
//!    那个也通不过就不存；
//! 5. 写到同一个目录下的临时文件，写完、刷到磁盘之后再换掉原文件（[`write_replacing`]），保存中途出错
//!    原文件不受影响。

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use magpdf_content::PdfDocument;
use magpdf_content::rewrite::save_reachable;
use pdfium_sys::{Document, Library, SaveFlags};

/// 存出来的文件。
#[derive(Debug)]
pub struct Saved {
    pub bytes: Arc<Vec<u8>>,
    /// 去掉了没有引用的对象。为假表示那一步的结果没通过自检，用的是 PDFium 原样的输出。
    pub compacted: bool,
    /// 去掉的对象个数。
    pub dropped: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    /// 文档已签名。
    #[error("文档已签名，完整重写会让签名失效")]
    Signed,
    #[error(transparent)]
    Pdfium(#[from] pdfium_sys::Error),
    /// 写出来的字节 PDFium 打不开，或者页数不对。
    #[error("写出来的文件重新打开不对")]
    SelfCheck,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// 把文档完整重写成一份新文件的字节（见本模块开头）。
pub fn full_save(
    lib: &Library,
    doc: &Document<'_>,
    password: Option<&str>,
) -> Result<Saved, SaveError> {
    if doc.signature_count() > 0 {
        return Err(SaveError::Signed);
    }
    let pages = doc.page_count();
    let written = Arc::new(doc.save_to_vec(SaveFlags::NO_INCREMENTAL)?);
    let opens = |bytes: &Arc<Vec<u8>>| {
        Document::load_shared(lib, Arc::clone(bytes), password)
            .is_ok_and(|d| d.page_count() == pages)
    };
    if let Ok(parsed) = PdfDocument::load_shared(Arc::clone(&written)) {
        let compacted = save_reachable(&parsed);
        let bytes = Arc::new(compacted.bytes);
        if opens(&bytes) {
            return Ok(Saved {
                bytes,
                compacted: true,
                dropped: compacted.dropped,
            });
        }
    }
    if opens(&written) {
        return Ok(Saved {
            bytes: written,
            compacted: false,
            dropped: 0,
        });
    }
    Err(SaveError::SelfCheck)
}

/// 先写同目录下的临时文件（刷到磁盘），再换掉 `path`。哪一步失败都删掉临时文件，`path` 原来的内容不动。
pub fn write_replacing(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temp = temp_path(path)?;
    let result = (|| {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        // Windows 上 rename 用 MoveFileExW(MOVEFILE_REPLACE_EXISTING)：目标在就换掉
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// 和 `path` 同一个目录、名字以点开头的临时文件：`.提单.pdf.1234.magpdf-tmp`（1234 是进程号）。
fn temp_path(path: &Path) -> std::io::Result<PathBuf> {
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "保存的路径没有文件名")
    })?;
    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(".{}.magpdf-tmp", std::process::id()));
    Ok(path.with_file_name(temp_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{ComponentKind, ComponentSpec};
    use crate::edit::Session;

    fn barcode(content: &str) -> ComponentSpec {
        ComponentSpec {
            kind: ComponentKind::Code128,
            content: content.to_owned(),
            size: 30.0,
        }
    }

    /// 文件里全部流解码之后拼在一起，找组件的参数用（PDFium 存盘时内容流是压缩的）。
    fn decoded_streams(bytes: &[u8]) -> Vec<u8> {
        let doc = PdfDocument::load(bytes.to_vec()).expect("能解析");
        let mut all = Vec::new();
        for (number, _, _) in doc.live_entries() {
            if let Some(magpdf_content::pdf::object::PdfObject::Stream(stream)) = doc.object(number)
            {
                all.extend(doc.decode_stream(&stream));
            }
        }
        all
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    fn session(lib: &Library) -> Session<'_> {
        let mut session = Session::create(lib).unwrap();
        session.add_blank_page(300.0, 200.0).unwrap();
        session
    }

    /// 改过参数的组件，旧的那一版不能留在存出来的文件里；PDFium 原样的输出里是有的。
    #[test]
    fn old_component_versions_do_not_survive_saving() {
        let _guard = crate::pdfium_lock::lock();
        let lib = Library::init().expect("PDFium");
        let mut session = session(&lib);
        let (id, _) = session
            .place_component(0, &barcode("OLDVALUE1"), (20.0, 180.0))
            .unwrap();
        session.update_component(id, &barcode("NEWVALUE2")).unwrap();
        // 再放一个然后撤销：撤销了的置入也不能留下
        session
            .place_component(0, &barcode("UNDONE3"), (20.0, 100.0))
            .unwrap();
        session.undo().unwrap();

        let raw = session
            .document()
            .save_to_vec(SaveFlags::NO_INCREMENTAL)
            .unwrap();
        let raw_streams = decoded_streams(&raw);
        assert!(
            contains(&raw_streams, b"OLDVALUE1"),
            "前提：PDFium 原样的输出里旧内容还在"
        );
        assert!(contains(&raw_streams, b"UNDONE3"));

        let saved = session.save_full().unwrap();
        assert!(saved.compacted);
        assert!(saved.dropped >= 2, "去掉了 {} 个对象", saved.dropped);
        let streams = decoded_streams(&saved.bytes);
        assert!(contains(&streams, b"NEWVALUE2"));
        assert!(!contains(&streams, b"OLDVALUE1"));
        assert!(!contains(&streams, b"UNDONE3"));

        // 存出来的文件重新打开，组件还认得出来，画面和内存里的一样
        let reopened = Document::load_bytes(&lib, saved.bytes.to_vec(), None).unwrap();
        let mut again = Session::from_document(&lib, reopened);
        let objects = again.objects(0).unwrap();
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0].component, Some(barcode("NEWVALUE2")));
        let options = pdfium_sys::RenderOptions::default();
        let before = session.page(0).unwrap().render(options).unwrap();
        let after = again.page(0).unwrap().render(options).unwrap();
        let diff = crate::compare::compare(&before, &after, &Default::default());
        assert_eq!(diff.diff_pixels, 0);
    }

    /// 带签名的文档不做完整重写（方案 9.2）。
    #[test]
    fn signed_documents_are_not_rewritten() {
        let _guard = crate::pdfium_lock::lock();
        let lib = Library::init().expect("PDFium");
        let objects: [&[u8]; 5] = [
            b"<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R] /SigFlags 3 >> >>",
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Annots [4 0 R] >>",
            b"<< /FT /Sig /T (Signature1) /Type /Annot /Subtype /Widget /Rect [0 0 0 0] /P 3 0 R /V 5 0 R >>",
            b"<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange [0 0 0 0] /Contents <00> >>",
        ];
        let mut pdf = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            pdf.extend_from_slice(body);
            pdf.extend_from_slice(b"\nendobj\n");
        }
        let xref = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        let doc = Document::load_bytes(&lib, pdf, None).unwrap();
        assert_eq!(doc.signature_count(), 1);
        let session = Session::from_document(&lib, doc);
        assert!(matches!(session.save_full(), Err(SaveError::Signed)));
    }

    /// 在一整批文件上核对：不改动直接保存，每一页的画面和原件一样，页数一样。平时不跑：
    /// `MAGPDF_SAVE_CORPUS=<目录> [MAGPDF_SAVE_OUT=<目录>] cargo test -p magpdf-engine -- --ignored save_corpus --nocapture`。
    /// 给了 `MAGPDF_SAVE_OUT` 就把存出来的文件按原来的相对路径写进它下面的 `saved/`，PDFium 原样的输出写进
    /// `pdfium/`，交给结构校验（`tests/corpus/tools/struct_check.py`）比。
    #[test]
    #[ignore = "要指定样本目录"]
    fn save_corpus_keeps_every_page_identical() {
        let Some(root) = std::env::var_os("MAGPDF_SAVE_CORPUS").map(PathBuf::from) else {
            return;
        };
        let out = std::env::var_os("MAGPDF_SAVE_OUT").map(PathBuf::from);
        // 已知会让 PDFium 崩溃的文件（V14 报告），和保存无关
        const SKIP: &[&str] = &["bad_page_type.pdf"];
        let mut files = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
                    && !SKIP
                        .iter()
                        .any(|s| path.file_name().is_some_and(|n| n == *s))
                {
                    files.push(path);
                }
            }
        }
        files.sort();
        let _guard = crate::pdfium_lock::lock();
        let lib = Library::init().expect("PDFium");
        let (mut opened, mut signed, mut compacted, mut fallback, mut pages) = (0, 0, 0, 0, 0);
        let mut encrypted = 0;
        let mut dropped = 0;
        let mut problems = Vec::new();
        for path in &files {
            let name = path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            // 加了密的样本用 PDFium 自己的测试里用的口令试一下，看加密的文件存完还解不解得开
            let attempt = [None, Some("foo"), Some("bar"), Some("test"), Some("1234")]
                .into_iter()
                .find_map(|password| Some((Session::open(&lib, path, password).ok()?, password)));
            let Some((session, password)) = attempt else {
                continue;
            };
            opened += 1;
            if password.is_some() {
                encrypted += 1;
            }
            let saved = match session.save_full() {
                Ok(saved) => saved,
                Err(SaveError::Signed) => {
                    signed += 1;
                    continue;
                }
                Err(error) => {
                    problems.push(format!("{name}：{error}"));
                    continue;
                }
            };
            if saved.compacted {
                compacted += 1;
                dropped += saved.dropped;
            } else {
                fallback += 1;
            }
            if let Some(out) = &out {
                let relative = path.strip_prefix(&root).unwrap();
                let target = out.join("saved").join(relative);
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                std::fs::write(&target, saved.bytes.as_slice()).unwrap();
                // PDFium 原样的输出也留一份，看去掉无用对象这一步有没有把结构弄坏
                let raw = out.join("pdfium").join(relative);
                std::fs::create_dir_all(raw.parent().unwrap()).unwrap();
                let bytes = session
                    .document()
                    .save_to_vec(SaveFlags::NO_INCREMENTAL)
                    .unwrap();
                std::fs::write(&raw, bytes).unwrap();
            }
            let original = session.document();
            let Ok(reopened) = Document::load_shared(&lib, Arc::clone(&saved.bytes), password)
            else {
                problems.push(format!("{name}：存出来的打不开"));
                continue;
            };
            for index in 0..original.page_count().min(30) {
                let render = |doc: &Document<'_>| {
                    doc.load_page(index)
                        .and_then(|p| p.render(pdfium_sys::RenderOptions::default()))
                };
                let (Ok(a), Ok(b)) = (render(original), render(&reopened)) else {
                    continue;
                };
                pages += 1;
                let diff = crate::compare::compare(&a, &b, &Default::default());
                if diff.diff_pixels > 0 {
                    problems.push(format!(
                        "{name} 第 {} 页：{} 个像素不同",
                        index + 1,
                        diff.diff_pixels
                    ));
                }
            }
        }
        println!(
            "文件 {} 个，打开 {opened}（其中要口令的 {encrypted}），已签名不存 {signed}，\
             去掉无用对象 {compacted}（共 {dropped} 个对象），退回 PDFium 原样输出 {fallback}，比了 {pages} 页",
            files.len()
        );
        for problem in &problems {
            println!("  {problem}");
        }
        assert!(problems.is_empty(), "{} 处不对", problems.len());
    }

    #[test]
    fn writing_replaces_the_file_and_leaves_no_temp_file_behind() {
        let dir = std::env::temp_dir().join(format!("magpdf-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("提单.pdf");
        std::fs::write(&path, b"old").unwrap();
        write_replacing(&path, b"new contents").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new contents");
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("提单.pdf")]);

        // 目录不存在：报错，什么也不留下
        let missing = dir.join("没有这个目录").join("a.pdf");
        assert!(write_replacing(&missing, b"x").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
