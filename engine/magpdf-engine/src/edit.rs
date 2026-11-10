//! 编辑服务（方案 4.2、4.3）：变换、改填充色、删除，以及撤销/重做和点选。
//!
//! 撤销按后进先出。删除把对象摘下交给撤销记录，撤销时插回原来的序号；
//! 因此只要按顺序撤销，序号在执行逆操作时和当初一致。
//! 历史步数默认 50。超出后丢掉最早的一步，那一步如果还握着已删除的对象，对象随之销毁。

use pdfium_sys::{Document, Library, Matrix, ObjectKind, Page, Rect, Rgba};

/// 点选模式。`Top` 跳过看不见的对象；`All` 返回光标下的全部对象，供右键菜单使用（方案 4.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitMode {
    Top,
    All,
}

/// 一次命中。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ObjectHit {
    pub page: usize,
    pub index: usize,
    pub kind: ObjectKind,
    pub bounds: Rect,
}

/// 图层面板用的对象摘要。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ObjectSummary {
    pub index: usize,
    pub kind: ObjectKind,
    pub bounds: Option<Rect>,
}

/// 编辑错误。
#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error(transparent)]
    Pdfium(#[from] pdfium_sys::Error),
    #[error("没有可撤销的操作")]
    NothingToUndo,
    #[error("没有可重做的操作")]
    NothingToRedo,
    #[error("对象不存在：第 {page} 页第 {index} 个")]
    NoObject { page: usize, index: usize },
    #[error("变换矩阵不可逆")]
    SingularMatrix,
    #[error("对象没有可恢复的填充色")]
    NoFill,
}

pub type Result<T> = std::result::Result<T, EditError>;

const DEFAULT_LIMIT: usize = 50;

enum Step {
    Transform {
        page: usize,
        index: usize,
        forward: Matrix,
        inverse: Matrix,
    },
    Fill {
        page: usize,
        index: usize,
        before: Option<Rgba>,
        after: Rgba,
    },
    Delete {
        page: usize,
        index: usize,
        /// `Some` 表示对象当前不在页面上。
        object: Option<pdfium_sys::DetachedObject>,
    },
}

impl Step {
    fn label(&self) -> &'static str {
        match self {
            Self::Transform { .. } => "变换",
            Self::Fill { .. } => "填充色",
            Self::Delete { .. } => "删除",
        }
    }
}

/// 一份打开的文档和它的撤销栈。文档是数据的唯一来源（方案 3.1）。
pub struct Session<'lib> {
    doc: Document<'lib>,
    undo: Vec<Step>,
    redo: Vec<Step>,
    limit: usize,
    version: u64,
}

impl std::fmt::Debug for Session<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("pages", &self.doc.page_count())
            .field("undo", &self.undo.len())
            .field("redo", &self.redo.len())
            .field("version", &self.version)
            .finish()
    }
}

impl<'lib> Session<'lib> {
    /// 打开磁盘上的文档。版本号从 0 开始，每次成功的编辑、撤销或重做加 1。
    pub fn open(
        lib: &'lib Library,
        path: &std::path::Path,
        password: Option<&str>,
    ) -> Result<Self> {
        Ok(Self::from_document(Document::load_file(
            lib, path, password,
        )?))
    }

    /// 空文档，用来建测试页或以后的“新建”。
    pub fn create(lib: &'lib Library) -> Result<Self> {
        Ok(Self::from_document(Document::create(lib)?))
    }

    fn from_document(doc: Document<'lib>) -> Self {
        Self {
            doc,
            undo: Vec::new(),
            redo: Vec::new(),
            limit: DEFAULT_LIMIT,
            version: 0,
        }
    }

    pub fn history_limit(&self) -> usize {
        self.limit
    }

    /// 调整撤销步数上限。已经超出的最早记录会被丢掉。
    pub fn set_history_limit(&mut self, limit: usize) {
        self.limit = limit;
        self.trim();
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// 从早到晚的撤销标签，供历史记录面板显示。
    pub fn history(&self) -> Vec<&'static str> {
        self.undo.iter().map(Step::label).collect()
    }

    pub fn page_count(&self) -> usize {
        self.doc.page_count()
    }

    pub fn add_blank_page(&mut self, width: f32, height: f32) -> Result<()> {
        let index = self.doc.page_count();
        let _page = self.doc.add_blank_page(index, width, height)?;
        Ok(())
    }

    pub fn objects(&self, page: usize) -> Result<Vec<ObjectSummary>> {
        let page = self.doc.page(page)?;
        Ok((0..page.object_count())
            .filter_map(|index| {
                let object = page.object(index)?;
                Some(ObjectSummary {
                    index,
                    kind: object.kind(),
                    bounds: object.bounds(),
                })
            })
            .collect())
    }

    /// 点选。`Top` 只返回最上面一个可见对象；`All` 自上而下返回全部相交对象。
    pub fn hit_test(&self, page: usize, x: f32, y: f32, mode: HitMode) -> Result<Vec<ObjectHit>> {
        let page_obj = self.doc.page(page)?;
        Ok(hit_test_page(page, &page_obj, x, y, mode))
    }

    /// 把矩阵左乘到对象上。路径点会跟着变，撤销时用逆矩阵再变一次。
    pub fn transform(&mut self, page: usize, index: usize, matrix: Matrix) -> Result<u64> {
        let inverse = matrix.inverse().ok_or(EditError::SingularMatrix)?;
        self.with_page(page, |page_obj| {
            let object = page_obj
                .object(index)
                .ok_or(EditError::NoObject { page, index })?;
            object.transform(matrix)?;
            Ok(())
        })?;
        self.push(Step::Transform {
            page,
            index,
            forward: matrix,
            inverse,
        });
        Ok(self.version)
    }

    /// 改填充色。撤销恢复改之前的颜色；对象读不出原颜色时撤销会失败。
    pub fn set_fill(&mut self, page: usize, index: usize, color: Rgba) -> Result<u64> {
        let before = self.with_page(page, |page_obj| {
            let object = page_obj
                .object(index)
                .ok_or(EditError::NoObject { page, index })?;
            let before = object.fill_color();
            object.set_fill_color(color)?;
            Ok(before)
        })?;
        self.push(Step::Fill {
            page,
            index,
            before,
            after: color,
        });
        Ok(self.version)
    }

    /// 删除对象。对象留在撤销记录里，直到这一步被丢掉或重做之后再次删除。
    pub fn delete(&mut self, page: usize, index: usize) -> Result<u64> {
        let object = self.with_page(page, |page_obj| {
            if index >= page_obj.object_count() {
                return Err(EditError::NoObject { page, index });
            }
            Ok(page_obj.detach_at(index)?)
        })?;
        self.push(Step::Delete {
            page,
            index,
            object: Some(object),
        });
        Ok(self.version)
    }

    pub fn undo(&mut self) -> Result<u64> {
        let mut step = self.undo.pop().ok_or(EditError::NothingToUndo)?;
        if let Err(err) = self.apply_undo(&mut step) {
            self.undo.push(step);
            return Err(err);
        }
        self.redo.push(step);
        self.version += 1;
        Ok(self.version)
    }

    pub fn redo(&mut self) -> Result<u64> {
        let mut step = self.redo.pop().ok_or(EditError::NothingToRedo)?;
        if let Err(err) = self.apply_redo(&mut step) {
            self.redo.push(step);
            return Err(err);
        }
        self.undo.push(step);
        self.trim();
        self.version += 1;
        Ok(self.version)
    }

    /// 改完必须 `generate_content`，否则页面句柄一关，内存里的对象改动就没了。
    fn with_page<T>(&self, page: usize, f: impl FnOnce(&Page<'_>) -> Result<T>) -> Result<T> {
        let page_obj = self.doc.page(page)?;
        let value = f(&page_obj)?;
        page_obj.generate_content()?;
        Ok(value)
    }

    fn push(&mut self, step: Step) {
        self.redo.clear();
        self.undo.push(step);
        self.trim();
        self.version += 1;
    }

    fn trim(&mut self) {
        while self.undo.len() > self.limit {
            self.undo.remove(0);
        }
    }

    fn apply_undo(&mut self, step: &mut Step) -> Result<()> {
        match step {
            Step::Transform {
                page,
                index,
                inverse,
                ..
            } => {
                let inverse = *inverse;
                let page_index = *page;
                let object_index = *index;
                self.with_page(page_index, |page_obj| {
                    let object = page_obj.object(object_index).ok_or(EditError::NoObject {
                        page: page_index,
                        index: object_index,
                    })?;
                    object.transform(inverse)?;
                    Ok(())
                })?;
            }
            Step::Fill {
                page,
                index,
                before,
                ..
            } => {
                let color = before.ok_or(EditError::NoFill)?;
                let page_index = *page;
                let object_index = *index;
                self.with_page(page_index, |page_obj| {
                    let object = page_obj.object(object_index).ok_or(EditError::NoObject {
                        page: page_index,
                        index: object_index,
                    })?;
                    object.set_fill_color(color)?;
                    Ok(())
                })?;
            }
            Step::Delete {
                page,
                index,
                object,
            } => {
                let page_index = *page;
                let object_index = *index;
                if object.is_none() {
                    return Err(EditError::NoObject {
                        page: page_index,
                        index: object_index,
                    });
                }
                let page_obj = self.doc.page(page_index)?;
                {
                    let detached = object.as_mut().ok_or(EditError::NoObject {
                        page: page_index,
                        index: object_index,
                    })?;
                    page_obj.attach(detached, object_index)?;
                }
                // 句柄已经归页面。生成内容流即使失败，撤销记录也不能再销毁它。
                *object = None;
                page_obj.generate_content()?;
            }
        }
        Ok(())
    }

    fn apply_redo(&mut self, step: &mut Step) -> Result<()> {
        match step {
            Step::Transform {
                page,
                index,
                forward,
                ..
            } => {
                let forward = *forward;
                let page_index = *page;
                let object_index = *index;
                self.with_page(page_index, |page_obj| {
                    let object = page_obj.object(object_index).ok_or(EditError::NoObject {
                        page: page_index,
                        index: object_index,
                    })?;
                    object.transform(forward)?;
                    Ok(())
                })?;
            }
            Step::Fill {
                page, index, after, ..
            } => {
                let color = *after;
                let page_index = *page;
                let object_index = *index;
                self.with_page(page_index, |page_obj| {
                    let object = page_obj.object(object_index).ok_or(EditError::NoObject {
                        page: page_index,
                        index: object_index,
                    })?;
                    object.set_fill_color(color)?;
                    Ok(())
                })?;
            }
            Step::Delete {
                page,
                index,
                object,
            } => {
                if object.is_some() {
                    return Err(EditError::NoObject {
                        page: *page,
                        index: *index,
                    });
                }
                let page_index = *page;
                let object_index = *index;
                *object = Some(
                    self.with_page(page_index, |page_obj| Ok(page_obj.detach_at(object_index)?))?,
                );
            }
        }
        Ok(())
    }
}

fn hit_test_page(
    page_index: usize,
    page: &Page<'_>,
    x: f32,
    y: f32,
    mode: HitMode,
) -> Vec<ObjectHit> {
    let mut hits = Vec::new();
    let count = page.object_count();
    for index in (0..count).rev() {
        let Some(object) = page.object(index) else {
            continue;
        };
        let Some(bounds) = object.bounds() else {
            continue;
        };
        if !bounds.contains(x, y) {
            continue;
        }
        if mode == HitMode::Top && skipped_when_picking(&object, page.width(), page.height()) {
            continue;
        }
        hits.push(ObjectHit {
            page: page_index,
            index,
            kind: object.kind(),
            bounds,
        });
        if mode == HitMode::Top {
            break;
        }
    }
    hits
}

/// `Top` 点选要跳过的对象：不可见文字、完全透明、铺满页面的白底（方案 4.3）。
fn skipped_when_picking(object: &pdfium_sys::PageObject<'_>, page_w: f32, page_h: f32) -> bool {
    if object
        .text_render_mode()
        .is_some_and(pdfium_sys::TextRenderMode::is_invisible)
    {
        return true;
    }
    let fill = object.fill_color();
    let stroke = object.stroke_color();
    let fill_clear = fill.is_none_or(|c| c.a == 0);
    let stroke_clear = stroke.is_none_or(|c| c.a == 0);
    if fill_clear && stroke_clear && (fill.is_some() || stroke.is_some()) {
        return true;
    }
    if let Some(bounds) = object.bounds()
        && let Some(color) = fill
    {
        let covers = bounds.left <= 1.0
            && bounds.bottom <= 1.0
            && bounds.right >= page_w - 1.0
            && bounds.top >= page_h - 1.0;
        if covers && color.r == 255 && color.g == 255 && color.b == 255 && color.a == 255 {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use pdfium_sys::{Library, Matrix, PageObject, Rgba};

    use super::{HitMode, Session};

    fn with_session(f: impl FnOnce(&mut Session<'_>)) {
        let _guard = crate::pdfium_lock::lock();
        let lib = Library::init().expect("PDFium");
        let mut session = Session::create(&lib).expect("空文档");
        session.add_blank_page(200.0, 100.0).expect("空白页");
        add_rect(&session, 10.0, 10.0, 40.0, 20.0);
        f(&mut session);
    }

    fn add_rect(session: &Session<'_>, x: f32, y: f32, w: f32, h: f32) {
        let page = session.doc.page(0).expect("页");
        let path = PageObject::new_path(x, y).expect("路径");
        path.line_to(x + w, y).unwrap();
        path.line_to(x + w, y + h).unwrap();
        path.line_to(x, y + h).unwrap();
        path.close_path().unwrap();
        path.set_fill_color(Rgba {
            r: 10,
            g: 20,
            b: 30,
            a: 255,
        })
        .unwrap();
        path.set_path_fill(true, false).unwrap();
        let index = page.object_count();
        page.insert_object_at(path, index).unwrap();
        page.generate_content().unwrap();
    }

    #[test]
    fn hit_transform_fill_delete_round_trip() {
        with_session(|session| {
            let miss = session.hit_test(0, 1.0, 1.0, HitMode::Top).unwrap();
            assert!(miss.is_empty());
            let hit = session.hit_test(0, 20.0, 15.0, HitMode::Top).unwrap();
            assert_eq!(hit.len(), 1);
            assert_eq!(hit[0].index, 0);

            let before = session.objects(0).unwrap()[0].bounds.unwrap();
            session
                .transform(0, 0, Matrix::translation(50.0, 0.0))
                .unwrap();
            let after = session.objects(0).unwrap()[0].bounds.unwrap();
            assert!(
                (after.left - before.left - 50.0).abs() < 0.6,
                "{before:?} -> {after:?}"
            );
            assert!(
                session
                    .hit_test(0, 20.0, 15.0, HitMode::Top)
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                session.hit_test(0, 70.0, 15.0, HitMode::Top).unwrap()[0].index,
                0
            );

            session.undo().unwrap();
            let restored = session.objects(0).unwrap()[0].bounds.unwrap();
            assert!((restored.left - before.left).abs() < 0.6);
            session.redo().unwrap();
            let again = session.objects(0).unwrap()[0].bounds.unwrap();
            assert!((again.left - before.left - 50.0).abs() < 0.6);
            session.undo().unwrap();

            session
                .set_fill(
                    0,
                    0,
                    Rgba {
                        r: 1,
                        g: 2,
                        b: 3,
                        a: 255,
                    },
                )
                .unwrap();
            let page = session.doc.page(0).unwrap();
            let color = page.object(0).unwrap().fill_color().unwrap();
            assert_eq!(color.r, 1);
            drop(page);
            assert_eq!(session.history(), vec!["填充色"]);
            session.undo().unwrap();
            let page = session.doc.page(0).unwrap();
            let color = page.object(0).unwrap().fill_color().unwrap();
            assert_eq!((color.r, color.g, color.b), (10, 20, 30));
            drop(page);

            assert_eq!(session.objects(0).unwrap().len(), 1);
            session.delete(0, 0).unwrap();
            assert!(session.objects(0).unwrap().is_empty());
            assert!(
                session
                    .hit_test(0, 20.0, 15.0, HitMode::Top)
                    .unwrap()
                    .is_empty()
            );
            session.undo().unwrap();
            assert_eq!(session.objects(0).unwrap().len(), 1);
            session.redo().unwrap();
            assert!(session.objects(0).unwrap().is_empty());
            session.undo().unwrap();
            assert!(session.history().is_empty());
        });
    }

    #[test]
    fn top_hit_skips_transparent_object() {
        with_session(|session| {
            add_rect(session, 10.0, 10.0, 40.0, 20.0);
            let page = session.doc.page(0).unwrap();
            let top = page.object(1).unwrap();
            top.set_fill_color(Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 0,
            })
            .unwrap();
            top.set_stroke_color(Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 0,
            })
            .unwrap();
            page.generate_content().unwrap();
            drop(page);
            let hit = session.hit_test(0, 20.0, 15.0, HitMode::Top).unwrap();
            assert_eq!(hit[0].index, 0);
            let all = session.hit_test(0, 20.0, 15.0, HitMode::All).unwrap();
            assert_eq!(all.len(), 2);
            assert_eq!(all[0].index, 1);
        });
    }

    #[test]
    fn history_limit_drops_the_oldest_step() {
        with_session(|session| {
            session.set_history_limit(1);
            session
                .transform(0, 0, Matrix::translation(5.0, 0.0))
                .unwrap();
            session
                .transform(0, 0, Matrix::translation(5.0, 0.0))
                .unwrap();
            assert_eq!(session.history(), vec!["变换"]);
            session.undo().unwrap();
            assert!(session.undo().is_err());
        });
    }
}
