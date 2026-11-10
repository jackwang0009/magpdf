//! 编辑服务（方案 4.2、4.3）：点选，变换、改填充色、删除，以及撤销和重做。
//!
//! 对象用会话内稳定的 [`ObjectId`] 指代（《接口协议》5.1）：编辑后不变，删除后不复用，撤销删除时
//! 回到原来的编号。PDFium 的页面对象没有稳定编号，页面每次重新加载都会重建，所以这里按页维护一张
//! “层级序号 → 编号”的表，第一次列出某页的对象时分配。
//!
//! 一条命令可以作用于多个对象，记成一步撤销。命令是原子的：中途失败时把已经做了的部分撤回去，
//! 文档和版本号都不变。撤销按后进先出；删除把对象摘下交给撤销记录，撤销时插回原来的层级。
//! 历史步数默认 50，超出后丢掉最早的一步，那一步如果还握着已删除的对象，对象随之销毁。

use std::collections::BTreeMap;

use pdfium_sys::{
    DetachedObject, Document, Library, Matrix, ObjectKind, Page, PageObject, Rect, Rgba,
};

/// 对象在一次会话内的稳定编号（《接口协议》5.1）。只在会话内有效，不写进文件。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId(pub u64);

/// 点选模式。`Top` 跳过看不见的对象；`All` 返回光标下的全部对象，供右键菜单使用（方案 4.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitMode {
    Top,
    All,
}

/// 对象摘要，图层面板和命中测试都用它。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ObjectSummary {
    pub id: ObjectId,
    pub page: usize,
    /// 页内层级：0 在最下面。
    pub z: usize,
    pub kind: ObjectKind,
    pub bounds: Option<Rect>,
}

/// 一页上的变化。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PageChange {
    pub page: usize,
    pub added: Vec<ObjectId>,
    pub removed: Vec<ObjectId>,
    pub modified: Vec<ObjectId>,
}

/// 一条命令（或一次撤销、重做）之后文档的变化，对应 `doc.changed` 事件（《接口协议》第 7 节）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// 变化之后的文档版本号。
    pub version: u64,
    /// 按页序号从小到大。
    pub pages: Vec<PageChange>,
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
    #[error("对象 {} 不存在", .0.0)]
    NoObject(ObjectId),
    #[error("命令没有指定对象")]
    NoTarget,
    #[error("变换矩阵不可逆")]
    SingularMatrix,
    #[error("对象没有可恢复的填充色")]
    NoFill,
}

pub type Result<T> = std::result::Result<T, EditError>;

const DEFAULT_LIMIT: usize = 50;

/// 作用在一个对象上的一步。
enum Atom {
    Transform {
        id: ObjectId,
        forward: Matrix,
        inverse: Matrix,
    },
    Fill {
        id: ObjectId,
        /// 改之前的颜色，第一次执行时记下。
        before: Option<Rgba>,
        after: Rgba,
        /// 为真时沿用对象原来的不透明度；第一次执行后 `after` 里就是最终的颜色。
        keep_alpha: bool,
    },
    Delete {
        id: ObjectId,
        /// 删除那一刻对象所在的页和层级，撤销时插回这里。
        slot: Option<(usize, usize)>,
        /// `Some` 表示对象当前不在页面上。
        object: Option<DetachedObject>,
    },
}

/// 一条命令：若干步，撤销时整体撤回。
struct Command {
    label: &'static str,
    atoms: Vec<Atom>,
}

/// 边执行边攒变化，最后按页整理成 [`Change`]。
#[derive(Default)]
struct ChangeLog {
    pages: BTreeMap<usize, PageChange>,
}

impl ChangeLog {
    fn page(&mut self, page: usize) -> &mut PageChange {
        self.pages.entry(page).or_insert_with(|| PageChange {
            page,
            ..PageChange::default()
        })
    }

    fn finish(self, version: u64) -> Change {
        Change {
            version,
            pages: self.pages.into_values().collect(),
        }
    }
}

/// 一份打开的文档和它的撤销栈。文档是数据的唯一来源（方案 3.1）。
pub struct Session<'lib> {
    doc: Document<'lib>,
    /// 已经列过对象的页：页序号 → 按层级排列的对象编号。
    ids: BTreeMap<usize, Vec<ObjectId>>,
    next_id: u64,
    undo: Vec<Command>,
    redo: Vec<Command>,
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

/// 某页的编号表；第一次用到时按当前的对象个数分配。
fn page_ids<'a>(
    ids: &'a mut BTreeMap<usize, Vec<ObjectId>>,
    next_id: &mut u64,
    page: usize,
    count: usize,
) -> &'a [ObjectId] {
    ids.entry(page).or_insert_with(|| {
        (0..count)
            .map(|_| {
                *next_id += 1;
                ObjectId(*next_id)
            })
            .collect()
    })
}

impl<'lib> Session<'lib> {
    /// 打开磁盘上的文档。版本号从 0 开始，每条成功的命令、每次撤销或重做加 1。
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

    /// 接管一份已经打开的文档。
    pub fn from_document(doc: Document<'lib>) -> Self {
        Self {
            doc,
            ids: BTreeMap::new(),
            next_id: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            limit: DEFAULT_LIMIT,
            version: 0,
        }
    }

    /// 底下的文档，渲染和查询页面尺寸时用。修改要走本类型的命令，否则撤销记录和编号表会对不上。
    pub fn document(&self) -> &Document<'lib> {
        &self.doc
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
        self.undo.iter().map(|c| c.label).collect()
    }

    pub fn page_count(&self) -> usize {
        self.doc.page_count()
    }

    pub fn add_blank_page(&mut self, width: f32, height: f32) -> Result<()> {
        let index = self.doc.page_count();
        let _page = self.doc.add_blank_page(index, width, height)?;
        Ok(())
    }

    /// 一页上的全部对象，按层级从下到上。
    pub fn objects(&mut self, page: usize) -> Result<Vec<ObjectSummary>> {
        let page_obj = self.doc.page(page)?;
        let count = page_obj.object_count();
        let ids = page_ids(&mut self.ids, &mut self.next_id, page, count);
        Ok((0..count)
            .filter_map(|z| {
                let object = page_obj.object(z)?;
                Some(summary(*ids.get(z)?, page, z, &object))
            })
            .collect())
    }

    /// 点选。`Top` 只返回最上面一个可见对象；`All` 自上而下返回光标下的全部对象。
    pub fn hit_test(
        &mut self,
        page: usize,
        x: f32,
        y: f32,
        mode: HitMode,
    ) -> Result<Vec<ObjectSummary>> {
        let page_obj = self.doc.page(page)?;
        let count = page_obj.object_count();
        let ids = page_ids(&mut self.ids, &mut self.next_id, page, count);
        let mut hits = Vec::new();
        for z in (0..count).rev() {
            let (Some(object), Some(id)) = (page_obj.object(z), ids.get(z)) else {
                continue;
            };
            if !object.bounds().is_some_and(|b| b.contains(x, y)) {
                continue;
            }
            if mode == HitMode::Top
                && skipped_when_picking(&object, page_obj.width(), page_obj.height())
            {
                continue;
            }
            hits.push(summary(*id, page, z, &object));
            if mode == HitMode::Top {
                break;
            }
        }
        Ok(hits)
    }

    /// 把矩阵左乘到每个对象上。撤销时用逆矩阵再变一次。
    pub fn transform(&mut self, ids: &[ObjectId], matrix: Matrix) -> Result<Change> {
        let inverse = matrix.inverse().ok_or(EditError::SingularMatrix)?;
        self.execute("变换", ids, |id| Atom::Transform {
            id,
            forward: matrix,
            inverse,
        })
    }

    /// 改填充色，颜色和不透明度都按 `color` 设。撤销恢复改之前的颜色。
    pub fn set_fill(&mut self, ids: &[ObjectId], color: Rgba) -> Result<Change> {
        self.execute("填充色", ids, |id| Atom::Fill {
            id,
            before: None,
            after: color,
            keep_alpha: false,
        })
    }

    /// 改填充色，只换颜色，各个对象的不透明度不动。
    pub fn set_fill_rgb(&mut self, ids: &[ObjectId], r: u8, g: u8, b: u8) -> Result<Change> {
        self.execute("填充色", ids, |id| Atom::Fill {
            id,
            before: None,
            after: Rgba { r, g, b, a: 255 },
            keep_alpha: true,
        })
    }

    /// 删除对象。对象留在撤销记录里，直到这一步被丢掉。
    pub fn delete(&mut self, ids: &[ObjectId]) -> Result<Change> {
        self.execute("删除", ids, |id| Atom::Delete {
            id,
            slot: None,
            object: None,
        })
    }

    pub fn undo(&mut self) -> Result<Change> {
        let mut command = self.undo.pop().ok_or(EditError::NothingToUndo)?;
        let mut log = ChangeLog::default();
        for i in (0..command.atoms.len()).rev() {
            if let Err(err) = self.apply(&mut command.atoms[i], false, &mut log) {
                // 撤到一半失败：把已经撤回的几步重新做一遍，这条记录仍然留在撤销栈里。
                let mut scratch = ChangeLog::default();
                for atom in &mut command.atoms[i + 1..] {
                    let _ = self.apply(atom, true, &mut scratch);
                }
                self.undo.push(command);
                return Err(err);
            }
        }
        self.redo.push(command);
        self.version += 1;
        Ok(log.finish(self.version))
    }

    pub fn redo(&mut self) -> Result<Change> {
        let mut command = self.redo.pop().ok_or(EditError::NothingToRedo)?;
        let mut log = ChangeLog::default();
        for i in 0..command.atoms.len() {
            if let Err(err) = self.apply(&mut command.atoms[i], true, &mut log) {
                let mut scratch = ChangeLog::default();
                for atom in command.atoms[..i].iter_mut().rev() {
                    let _ = self.apply(atom, false, &mut scratch);
                }
                self.redo.push(command);
                return Err(err);
            }
        }
        self.undo.push(command);
        self.trim();
        self.version += 1;
        Ok(log.finish(self.version))
    }

    /// 执行一条命令：对 `ids` 里的每个对象各做一步，全部成功才记进撤销栈、版本号加 1。
    fn execute(
        &mut self,
        label: &'static str,
        ids: &[ObjectId],
        make: impl Fn(ObjectId) -> Atom,
    ) -> Result<Change> {
        let mut targets: Vec<ObjectId> = Vec::with_capacity(ids.len());
        for id in ids {
            if !targets.contains(id) {
                targets.push(*id);
            }
        }
        if targets.is_empty() {
            return Err(EditError::NoTarget);
        }
        // 先确认对象都在，再动文档。
        for id in &targets {
            self.locate(*id)?;
        }
        let mut atoms: Vec<Atom> = targets.into_iter().map(make).collect();
        let mut log = ChangeLog::default();
        for i in 0..atoms.len() {
            if let Err(err) = self.apply(&mut atoms[i], true, &mut log) {
                // 命令是原子的：把已经做了的几步撤回去。
                let mut scratch = ChangeLog::default();
                for atom in atoms[..i].iter_mut().rev() {
                    let _ = self.apply(atom, false, &mut scratch);
                }
                return Err(err);
            }
        }
        self.redo.clear();
        self.undo.push(Command { label, atoms });
        self.trim();
        self.version += 1;
        Ok(log.finish(self.version))
    }

    /// 正向（`forward` 为真）或反向执行一步，并把变化记进 `log`。
    fn apply(&mut self, atom: &mut Atom, forward: bool, log: &mut ChangeLog) -> Result<()> {
        match atom {
            Atom::Transform {
                id,
                forward: matrix,
                inverse,
            } => {
                let (page, z) = self.locate(*id)?;
                let matrix = if forward { *matrix } else { *inverse };
                self.with_object(page, z, *id, |object| Ok(object.transform(matrix)?))?;
                log.page(page).modified.push(*id);
            }
            Atom::Fill {
                id,
                before,
                after,
                keep_alpha,
            } => {
                let (page, z) = self.locate(*id)?;
                if forward {
                    let (was, now) = self.with_object(page, z, *id, |object| {
                        let was = object.fill_color();
                        let mut now = *after;
                        if *keep_alpha && let Some(was) = was {
                            now.a = was.a;
                        }
                        object.set_fill_color(now)?;
                        Ok((was, now))
                    })?;
                    if before.is_none() {
                        *before = was;
                    }
                    *after = now;
                    *keep_alpha = false;
                } else {
                    let color = before.ok_or(EditError::NoFill)?;
                    self.with_object(page, z, *id, |object| Ok(object.set_fill_color(color)?))?;
                }
                log.page(page).modified.push(*id);
            }
            Atom::Delete { id, slot, object } => {
                if forward {
                    let (page, z) = self.locate(*id)?;
                    let detached = self.with_page(page, |page_obj| Ok(page_obj.detach_at(z)?))?;
                    if let Some(ids) = self.ids.get_mut(&page) {
                        ids.remove(z);
                    }
                    *slot = Some((page, z));
                    *object = Some(detached);
                    log.page(page).removed.push(*id);
                } else {
                    let (page, z) = slot.ok_or(EditError::NoObject(*id))?;
                    let detached = object.as_mut().ok_or(EditError::NoObject(*id))?;
                    let page_obj = self.doc.page(page)?;
                    page_obj.attach(detached, z)?;
                    // 句柄已经归页面。生成内容流即使失败，撤销记录也不能再销毁它。
                    *object = None;
                    if let Some(ids) = self.ids.get_mut(&page) {
                        ids.insert(z.min(ids.len()), *id);
                    }
                    page_obj.generate_content()?;
                    log.page(page).added.push(*id);
                }
            }
        }
        Ok(())
    }

    /// 对象现在在哪一页、第几层。没列过对象的页上的对象还没有编号，自然也找不到。
    fn locate(&self, id: ObjectId) -> Result<(usize, usize)> {
        self.ids
            .iter()
            .find_map(|(page, ids)| ids.iter().position(|x| *x == id).map(|z| (*page, z)))
            .ok_or(EditError::NoObject(id))
    }

    /// 改完必须 `generate_content`，否则页面句柄一关，内存里的对象改动就没了。
    fn with_page<T>(&self, page: usize, f: impl FnOnce(&Page<'_>) -> Result<T>) -> Result<T> {
        let page_obj = self.doc.page(page)?;
        let value = f(&page_obj)?;
        page_obj.generate_content()?;
        Ok(value)
    }

    fn with_object<T>(
        &self,
        page: usize,
        z: usize,
        id: ObjectId,
        f: impl FnOnce(&PageObject<'_>) -> Result<T>,
    ) -> Result<T> {
        self.with_page(page, |page_obj| {
            let object = page_obj.object(z).ok_or(EditError::NoObject(id))?;
            f(&object)
        })
    }

    fn trim(&mut self) {
        while self.undo.len() > self.limit {
            self.undo.remove(0);
        }
    }
}

fn summary(id: ObjectId, page: usize, z: usize, object: &PageObject<'_>) -> ObjectSummary {
    ObjectSummary {
        id,
        page,
        z,
        kind: object.kind(),
        bounds: object.bounds(),
    }
}

/// `Top` 点选要跳过的对象：不可见文字、完全透明、铺满页面的白底（方案 4.3）。
fn skipped_when_picking(object: &PageObject<'_>, page_w: f32, page_h: f32) -> bool {
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

    use super::{EditError, HitMode, ObjectId, Session};

    const DARK: Rgba = Rgba {
        r: 10,
        g: 20,
        b: 30,
        a: 255,
    };

    /// 一页 200×100，上面一个 40×20 的深色矩形，左下角在 (10, 10)。
    fn with_session(f: impl FnOnce(&mut Session<'_>)) {
        let _guard = crate::pdfium_lock::lock();
        let lib = Library::init().expect("PDFium");
        let mut session = Session::create(&lib).expect("空文档");
        session.add_blank_page(200.0, 100.0).expect("空白页");
        add_rect(&session, 10.0, 10.0, 40.0, 20.0, DARK);
        f(&mut session);
    }

    fn add_rect(session: &Session<'_>, x: f32, y: f32, w: f32, h: f32, color: Rgba) {
        let page = session.doc.page(0).expect("页");
        let path = PageObject::new_path(x, y).expect("路径");
        path.line_to(x + w, y).unwrap();
        path.line_to(x + w, y + h).unwrap();
        path.line_to(x, y + h).unwrap();
        path.close_path().unwrap();
        path.set_fill_color(color).unwrap();
        path.set_path_fill(true, false).unwrap();
        let index = page.object_count();
        page.insert_object_at(path, index).unwrap();
        page.generate_content().unwrap();
    }

    fn left_of(session: &mut Session<'_>, id: ObjectId) -> f32 {
        let object = session
            .objects(0)
            .unwrap()
            .into_iter()
            .find(|o| o.id == id)
            .expect("对象还在");
        object.bounds.unwrap().left
    }

    fn fill_of(session: &Session<'_>, z: usize) -> Rgba {
        let page = session.doc.page(0).unwrap();
        page.object(z).unwrap().fill_color().unwrap()
    }

    #[test]
    fn hit_transform_fill_delete_round_trip() {
        with_session(|session| {
            assert!(
                session
                    .hit_test(0, 1.0, 1.0, HitMode::Top)
                    .unwrap()
                    .is_empty()
            );
            let hit = session.hit_test(0, 20.0, 15.0, HitMode::Top).unwrap();
            assert_eq!(hit.len(), 1);
            let id = hit[0].id;
            assert_eq!(session.objects(0).unwrap()[0].id, id);

            let before = left_of(session, id);
            let change = session
                .transform(&[id], Matrix::translation(50.0, 0.0))
                .unwrap();
            assert_eq!(change.version, 1);
            assert_eq!(change.pages.len(), 1);
            assert_eq!(change.pages[0].modified, vec![id]);
            assert!((left_of(session, id) - before - 50.0).abs() < 0.6);
            assert!(
                session
                    .hit_test(0, 20.0, 15.0, HitMode::Top)
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                session.hit_test(0, 70.0, 15.0, HitMode::Top).unwrap()[0].id,
                id
            );

            assert_eq!(session.undo().unwrap().pages[0].modified, vec![id]);
            assert!((left_of(session, id) - before).abs() < 0.6);
            session.redo().unwrap();
            assert!((left_of(session, id) - before - 50.0).abs() < 0.6);
            session.undo().unwrap();
            assert_eq!(session.version(), 4);

            session
                .set_fill(
                    &[id],
                    Rgba {
                        r: 1,
                        g: 2,
                        b: 3,
                        a: 255,
                    },
                )
                .unwrap();
            assert_eq!(fill_of(session, 0).r, 1);
            assert_eq!(session.history(), vec!["填充色"]);
            session.undo().unwrap();
            assert_eq!(fill_of(session, 0), DARK);

            let change = session.delete(&[id]).unwrap();
            assert_eq!(change.pages[0].removed, vec![id]);
            assert!(session.objects(0).unwrap().is_empty());
            assert!(
                session
                    .hit_test(0, 20.0, 15.0, HitMode::Top)
                    .unwrap()
                    .is_empty()
            );
            // 撤销删除：对象回来了，编号还是原来那个。
            assert_eq!(session.undo().unwrap().pages[0].added, vec![id]);
            assert_eq!(session.objects(0).unwrap()[0].id, id);
            assert_eq!(session.redo().unwrap().pages[0].removed, vec![id]);
            assert!(session.objects(0).unwrap().is_empty());
            session.undo().unwrap();
            assert!(session.history().is_empty());
        });
    }

    #[test]
    fn top_hit_skips_transparent_object() {
        with_session(|session| {
            // 在原来的矩形上面再盖一个，填充和描边都设成完全透明。
            add_rect(session, 10.0, 10.0, 40.0, 20.0, DARK);
            let clear = Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 0,
            };
            {
                let page = session.doc.page(0).unwrap();
                let top = page.object(1).unwrap();
                top.set_fill_color(clear).unwrap();
                top.set_stroke_color(clear).unwrap();
                page.generate_content().unwrap();
            }
            let objects = session.objects(0).unwrap();
            assert_eq!(objects.len(), 2);
            let hit = session.hit_test(0, 20.0, 15.0, HitMode::Top).unwrap();
            assert_eq!(hit[0].id, objects[0].id);
            let all = session.hit_test(0, 20.0, 15.0, HitMode::All).unwrap();
            assert_eq!(
                all.iter().map(|o| o.id).collect::<Vec<_>>(),
                vec![objects[1].id, objects[0].id],
                "All 自上而下"
            );
        });
    }

    #[test]
    fn one_command_on_several_objects_is_one_undo_step() {
        with_session(|session| {
            add_rect(session, 60.0, 10.0, 40.0, 20.0, DARK);
            add_rect(session, 110.0, 10.0, 40.0, 20.0, DARK);
            let ids: Vec<ObjectId> = session.objects(0).unwrap().iter().map(|o| o.id).collect();
            let (a, b, c) = (ids[0], ids[1], ids[2]);

            // 删掉最下面和最上面两个，剩下中间那个。
            let change = session.delete(&[a, c, a]).unwrap();
            assert_eq!(change.pages[0].removed, vec![a, c]);
            assert_eq!(session.history(), vec!["删除"]);
            assert_eq!(
                session
                    .objects(0)
                    .unwrap()
                    .iter()
                    .map(|o| o.id)
                    .collect::<Vec<_>>(),
                vec![b]
            );
            assert!((left_of(session, b) - 60.0).abs() < 0.6);

            // 一次撤销全部回来，层级和编号都和原来一样。
            session.undo().unwrap();
            let restored = session.objects(0).unwrap();
            assert_eq!(restored.iter().map(|o| o.id).collect::<Vec<_>>(), ids);
            let lefts: Vec<f32> = restored.iter().map(|o| o.bounds.unwrap().left).collect();
            assert!(
                (lefts[0] - 10.0).abs() < 0.6
                    && (lefts[1] - 60.0).abs() < 0.6
                    && (lefts[2] - 110.0).abs() < 0.6,
                "{lefts:?}"
            );

            // 只换颜色时，不透明度不动。
            session.set_fill_rgb(&[a, b], 200, 0, 0).unwrap();
            assert_eq!(
                fill_of(session, 1),
                Rgba {
                    r: 200,
                    g: 0,
                    b: 0,
                    a: 255
                }
            );
            assert_eq!(fill_of(session, 2), DARK);
        });
    }

    #[test]
    fn a_command_with_a_missing_object_changes_nothing() {
        with_session(|session| {
            let id = session.objects(0).unwrap()[0].id;
            let ghost = ObjectId(9999);
            assert!(matches!(
                session.transform(&[id, ghost], Matrix::translation(5.0, 0.0)),
                Err(EditError::NoObject(missing)) if missing == ghost
            ));
            assert!(matches!(session.delete(&[]), Err(EditError::NoTarget)));
            assert_eq!(session.version(), 0);
            assert!(!session.can_undo());
            assert!((left_of(session, id) - 10.0).abs() < 0.6);

            // 删掉之后编号不复用，也不能再引用。
            session.delete(&[id]).unwrap();
            assert!(matches!(
                session.set_fill(&[id], DARK),
                Err(EditError::NoObject(_))
            ));
        });
    }

    #[test]
    fn history_limit_drops_the_oldest_step() {
        with_session(|session| {
            let id = session.objects(0).unwrap()[0].id;
            session.set_history_limit(1);
            session
                .transform(&[id], Matrix::translation(5.0, 0.0))
                .unwrap();
            session
                .transform(&[id], Matrix::translation(5.0, 0.0))
                .unwrap();
            assert_eq!(session.history(), vec!["变换"]);
            session.undo().unwrap();
            assert!(session.undo().is_err());
        });
    }
}
