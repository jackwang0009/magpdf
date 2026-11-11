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
    DetachedObject, Document, Library, Matrix, ObjectKind, Page, PageObject, Rect, RenderOptions,
    Rgba, SaveFlags,
};

use crate::compare::{Thresholds, compare};

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
    /// 在画布上点选、框选时算不算数。不可见文字、完全透明的对象、铺满页面的白底不算（方案 4.3），
    /// 它们仍然列在图层面板里，可以从那里选中。
    pub pickable: bool,
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
    /// 这一页重新生成内容流后对象会变，不能修改原有对象（方案 2.3 的降级）。
    #[error("第 {} 页不能修改原有对象：{reason}", .page + 1)]
    PageNotEditable { page: usize, reason: &'static str },
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
    lib: &'lib Library,
    doc: Document<'lib>,
    /// 已经列过对象的页：页序号 → 按层级排列的对象编号。
    ids: BTreeMap<usize, Vec<ObjectId>>,
    next_id: u64,
    /// 保真校验用的另一份文档，第一次校验时建：文件原来的样子，在它上面试着重写页面，不碰正在用的这份。
    /// `Err` 表示建不出来。
    ///
    /// 它和下面的 `fidelity` 都按页序号对应到正在用的文档。原型还没有页面命令（插入、删除、重排）；
    /// 加页面命令的时候，这两处要改成按不随重排变化的页面编号来对。
    scratch: Option<std::result::Result<Document<'lib>, ()>>,
    /// 校验过的页：`None` 表示可以修改，`Some` 是不能修改的原因。
    fidelity: BTreeMap<usize, Option<&'static str>>,
    /// 刚校验出来、还没有被 [`Session::take_new_verdicts`] 取走的结论，按得出的先后。
    new_verdicts: Vec<(usize, Option<&'static str>)>,
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
        Ok(Self::from_document(
            lib,
            Document::load_file(lib, path, password)?,
        ))
    }

    /// 空文档，用来建测试页或以后的“新建”。
    pub fn create(lib: &'lib Library) -> Result<Self> {
        Ok(Self::from_document(lib, Document::create(lib)?))
    }

    /// 接管一份已经打开的文档。
    pub fn from_document(lib: &'lib Library, doc: Document<'lib>) -> Self {
        Self {
            lib,
            doc,
            ids: BTreeMap::new(),
            next_id: 0,
            scratch: None,
            fidelity: BTreeMap::new(),
            new_verdicts: Vec::new(),
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
        let page_box = page_obj.bounding_box();
        let ids = page_ids(&mut self.ids, &mut self.next_id, page, count);
        Ok((0..count)
            .filter_map(|z| {
                let object = page_obj.object(z)?;
                Some(summary(*ids.get(z)?, page, z, &object, page_box))
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
        let page_box = page_obj.bounding_box();
        let ids = page_ids(&mut self.ids, &mut self.next_id, page, count);
        let mut hits = Vec::new();
        for z in (0..count).rev() {
            let (Some(object), Some(id)) = (page_obj.object(z), ids.get(z)) else {
                continue;
            };
            if !object.bounds().is_some_and(|b| b.contains(x, y)) {
                continue;
            }
            let hit = summary(*id, page, z, &object, page_box);
            if mode == HitMode::Top && !hit.pickable {
                continue;
            }
            hits.push(hit);
            if mode == HitMode::Top {
                break;
            }
        }
        Ok(hits)
    }

    /// 一页能不能修改原有对象（方案 2.3 的保真校验）。返回 `None` 表示可以，`Some` 是原因。
    ///
    /// 做法：在文档的副本上把这一页的对象全部标记为已修改、重新生成内容流，再重新加载，比较前后
    /// 对象的个数、种类和包围盒（对象比对），再比较前后渲染出来的画面（像素比对，判定规则同往返测试）。
    /// PDFium 的生成器写不出来的对象会消失、挪位置或者变色，这样的页一旦改了任何对象，整页的内容流
    /// 都要重写，别的对象也跟着坏，所以只允许在上面叠加内容。结果按页记住，只校验一次。
    ///
    /// 还看不出来的几类，这里都还没有做：
    /// - 颜色空间被换算成画面相同的 RGB（灰度写成 `rg`）：像素一样、对象也一样，要靠操作符级比对；
    /// - 默认隐藏的可选内容图层：像素比对时没有把图层全部打开（方案 2.3 要求打开）；
    /// - 和别的页共用内容流的页：重写这一页会连带改掉那一页（方案 2.1），而这里只比这一页。
    ///
    /// 修改命令执行前会自己调用它；也可以提前调用（页面显示之后、引擎空闲时），这样用户第一次修改
    /// 时不用等。
    pub fn check_fidelity(&mut self, page: usize) -> Result<Option<&'static str>> {
        if let Some(known) = self.fidelity.get(&page) {
            return Ok(*known);
        }
        if self.scratch.is_none() {
            // 从文件打开的文档：把文件的数据再解析一遍。要校验的页还没有被改过（改之前必须先过校验），
            // 所以文件原来的样子就是它现在的样子；两份文档共用同一块数据，不复制，文件再大也只花解析的
            // 时间。新建的文档没有文件数据，只能把当前的样子存一份出来再读回去。
            let copy = self.doc.reopen(self.lib, None).unwrap_or_else(|| {
                self.doc
                    .save_to_vec(SaveFlags::NO_INCREMENTAL)
                    .and_then(|bytes| Document::load_bytes(self.lib, bytes, None))
            });
            self.scratch = Some(copy.map_err(|_| ()));
        }
        let verdict = match &self.scratch {
            Some(Ok(scratch)) => roundtrip_verdict(scratch, page)?,
            _ => Some("校验用的文档副本建不出来"),
        };
        self.fidelity.insert(page, verdict);
        self.new_verdicts.push((page, verdict));
        Ok(verdict)
    }

    /// 这一页已经有的校验结论，不做校验。外层的 `None` 表示还没有校验过。
    pub fn known_fidelity(&self, page: usize) -> Option<Option<&'static str>> {
        self.fidelity.get(&page).copied()
    }

    /// 取走上次调用以来新得出的校验结论（页序号，结论），按得出的先后。
    /// 修改命令会顺带校验它碰到的页，调用方靠这个知道哪些页刚有了结论，好通知界面。
    pub fn take_new_verdicts(&mut self) -> Vec<(usize, Option<&'static str>)> {
        std::mem::take(&mut self.new_verdicts)
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
        // 先确认对象都在、所在的页都能改，再动文档。
        let mut pages: Vec<usize> = Vec::new();
        for id in &targets {
            let (page, _) = self.locate(*id)?;
            if !pages.contains(&page) {
                pages.push(page);
            }
        }
        for page in pages {
            if let Some(reason) = self.check_fidelity(page)? {
                return Err(EditError::PageNotEditable { page, reason });
            }
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
    /// 这些对象在页内的层级序号，顺序和 `ids` 一致。分层渲染（方案 3.2）按序号告诉 PDFium 留哪些对象。
    /// 有对象不在这一页上，或者这一页的对象还没有列过，报 [`EditError::NoObject`]。
    pub fn z_indices(&self, page: usize, ids: &[ObjectId]) -> Result<Vec<usize>> {
        let on_page = self.ids.get(&page).map_or(&[][..], Vec::as_slice);
        ids.iter()
            .map(|id| {
                on_page
                    .iter()
                    .position(|known| known == id)
                    .ok_or(EditError::NoObject(*id))
            })
            .collect()
    }

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

/// 包围盒各边允许的偏差（pt）。生成器把坐标写成十进制小数再读回，会有很小的舍入误差。
const BOUNDS_TOLERANCE: f32 = 0.5;

fn page_shape(page: &Page<'_>) -> Vec<(ObjectKind, Option<Rect>)> {
    page.objects().map(|o| (o.kind(), o.bounds())).collect()
}

/// 保真校验渲染一页最多用多少像素。超过的大幅面页面降低比对的分辨率，而不是直接判为不能改。
const FIDELITY_MAX_PIXELS: f32 = 16_000_000.0;

/// 保真校验的渲染参数：2 倍分辨率（144 dpi），和往返测试用的一样（方案 13.2）；页面太大时降低倍率。
fn fidelity_render(page: &Page<'_>) -> RenderOptions {
    let area = (page.width() * page.height()).max(1.0);
    RenderOptions {
        scale: (FIDELITY_MAX_PIXELS / area).sqrt().min(2.0),
        ..RenderOptions::default()
    }
}

/// 在副本上重写一页的内容流，比较前后的对象和画面。`None` 表示没有变化。
fn roundtrip_verdict(scratch: &Document<'_>, page: usize) -> Result<Option<&'static str>> {
    const UNRENDERABLE: &str = "页面渲染不出来，没法比较重写前后的画面";
    let (before, picture_before) = {
        let page_obj = scratch.page(page)?;
        let shape = page_shape(&page_obj);
        let Ok(picture) = page_obj.render(fidelity_render(&page_obj)) else {
            return Ok(Some(UNRENDERABLE));
        };
        // 内容流重新生成不出来的页，当然也不能改。
        let regenerated = page_obj
            .objects()
            .try_for_each(|object| object.mark_modified())
            .and_then(|()| page_obj.generate_content());
        if regenerated.is_err() {
            return Ok(Some("这一页的内容流重新生成不出来"));
        }
        (shape, picture)
    };
    let rewritten = scratch.page(page)?;
    let after = page_shape(&rewritten);
    if before.len() != after.len() {
        return Ok(Some("重新生成内容流后对象的个数变了"));
    }
    for ((kind_a, bounds_a), (kind_b, bounds_b)) in before.iter().zip(&after) {
        if kind_a != kind_b {
            return Ok(Some("重新生成内容流后对象的种类变了"));
        }
        let same = match (bounds_a, bounds_b) {
            (None, None) => true,
            (Some(a), Some(b)) => [
                a.left - b.left,
                a.bottom - b.bottom,
                a.right - b.right,
                a.top - b.top,
            ]
            .iter()
            .all(|d| d.abs() <= BOUNDS_TOLERANCE),
            _ => false,
        };
        if !same {
            return Ok(Some("重新生成内容流后对象的位置变了"));
        }
    }
    // 对象一个没少、位置也没动，画面仍然可能变：生成器把 CMYK、专色这些颜色写丢了，对象就变黑。
    // 判定规则和往返测试相同（方案 2.3、13.2）。
    let Ok(picture_after) = rewritten.render(fidelity_render(&rewritten)) else {
        return Ok(Some(UNRENDERABLE));
    };
    let thresholds = Thresholds::default();
    if !compare(&picture_before, &picture_after, &thresholds).passes(&thresholds) {
        return Ok(Some("重新生成内容流后页面的画面变了"));
    }
    Ok(None)
}

/// `page_box` 是页面的可见区域（PDF 用户空间），用来认出铺满页面的底块。
fn summary(
    id: ObjectId,
    page: usize,
    z: usize,
    object: &PageObject<'_>,
    page_box: Option<Rect>,
) -> ObjectSummary {
    let bounds = object.bounds();
    ObjectSummary {
        id,
        page,
        z,
        kind: object.kind(),
        bounds,
        pickable: !skipped_when_picking(object, bounds, page_box),
    }
}

/// 点选、框选要跳过的对象：不可见文字、完全透明、铺满页面的白底（方案 4.3）。
/// 对象的包围盒和页面的可见区域都在 PDF 用户空间里，所以页面旋转、可见区域不从原点开始都不影响判断。
fn skipped_when_picking(
    object: &PageObject<'_>,
    bounds: Option<Rect>,
    page_box: Option<Rect>,
) -> bool {
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
    if let (Some(bounds), Some(page_box), Some(color)) = (bounds, page_box, fill) {
        let covers = bounds.left <= page_box.left + 1.0
            && bounds.bottom <= page_box.bottom + 1.0
            && bounds.right >= page_box.right - 1.0
            && bounds.top >= page_box.top - 1.0;
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
            // 对象列表里带着“点选时算不算数”，界面在本地做命中测试时靠它。
            assert_eq!(
                objects.iter().map(|o| o.pickable).collect::<Vec<_>>(),
                vec![true, false]
            );
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

    /// 铺满整页的白色底块点不中，露出下面的对象；同样大的有色底块、比页面小的白块照常能点中。
    #[test]
    fn top_hit_skips_a_white_block_that_covers_the_page() {
        const WHITE: Rgba = Rgba {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        };
        with_session(|session| {
            add_rect(session, 0.0, 0.0, 200.0, 100.0, WHITE);
            add_rect(session, 100.0, 40.0, 60.0, 40.0, WHITE);
            let objects = session.objects(0).unwrap();
            assert_eq!(
                objects.iter().map(|o| o.pickable).collect::<Vec<_>>(),
                vec![true, false, true]
            );
            // 深色矩形上面盖着整页白底：点到的还是深色矩形。
            let hit = session.hit_test(0, 20.0, 15.0, HitMode::Top).unwrap();
            assert_eq!(hit[0].id, objects[0].id);
            // 空白处：什么也点不中。
            assert!(
                session
                    .hit_test(0, 180.0, 10.0, HitMode::Top)
                    .unwrap()
                    .is_empty()
            );
            // 小白块能点中。
            let hit = session.hit_test(0, 120.0, 60.0, HitMode::Top).unwrap();
            assert_eq!(hit[0].id, objects[2].id);

            // 整页底块换成别的颜色，就是一个普通对象了。
            session.set_fill(&[objects[1].id], DARK).unwrap();
            assert!(session.objects(0).unwrap()[1].pickable);
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

    /// 渐变对象是生成器写不出来的（方案 2.1），带渐变的页重写后对象会少。这样的页必须拒绝修改，
    /// 否则改一个对象就丢掉渐变，编号表也跟着错位。生成器以后修好了，这一页就应该能正常编辑。
    #[test]
    fn a_page_the_generator_cannot_rewrite_is_not_editable() {
        let _guard = crate::pdfium_lock::lock();
        let lib = Library::init().expect("PDFium");
        let sample = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/corpus/synthetic/shading-axial.pdf");
        let mut session = Session::open(&lib, &sample, None).expect("打开样本");
        let before = session.objects(0).unwrap();
        assert!(!before.is_empty());
        let target = before[0].id;

        let verdict = session.check_fidelity(0).unwrap();
        let outcome = session.transform(&[target], Matrix::translation(3.0, 0.0));
        match verdict {
            Some(reason) => {
                assert!(
                    matches!(
                        outcome,
                        Err(EditError::PageNotEditable { page: 0, reason: got }) if got == reason
                    ),
                    "{outcome:?}"
                );
                assert_eq!(session.version(), 0);
                assert!(!session.can_undo());
            }
            None => {
                outcome.expect("校验通过的页可以修改");
            }
        }
        // 不管哪种情况，对象的个数和编号都没有乱。
        let after = session.objects(0).unwrap();
        assert_eq!(
            after.iter().map(|o| o.id).collect::<Vec<_>>(),
            before.iter().map(|o| o.id).collect::<Vec<_>>()
        );
    }

    /// CMYK 的色块：原版 PDFium 的生成器写不出 `k`、`K`（方案 2.1），重写之后对象一个不少、位置也不动，
    /// 只是变成了黑色。光比对象看不出来，要比画面。这样的页也不能改：一改，整页的内容流重写，
    /// 没碰过的色块跟着变黑。生成器修好之后这一页就能编辑，所以两种结论都接受，但要和实际重写一遍
    /// 的结果对得上。
    #[test]
    fn a_page_whose_colors_change_when_rewritten_is_not_editable() {
        use pdfium_sys::{Document, RenderOptions};

        use crate::compare::{Thresholds, compare};

        let _guard = crate::pdfium_lock::lock();
        let lib = Library::init().expect("PDFium");
        let sample = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/corpus/synthetic/color-devicecmyk.pdf");

        // 对照：另开一份，把第 1 页重写一遍，直接看对象和画面变没变。
        let (objects_same, picture_same) = {
            let doc = Document::load_file(&lib, &sample, None).expect("打开样本");
            let render = RenderOptions {
                scale: 2.0,
                ..RenderOptions::default()
            };
            let (shape, before) = {
                let page = doc.page(0).unwrap();
                let shape: Vec<_> = page.objects().map(|o| o.kind()).collect();
                let before = page.render(render).unwrap();
                crate::roundtrip::mark_all_and_regenerate(&page).unwrap();
                (shape, before)
            };
            let page = doc.page(0).unwrap();
            let after = page.render(render).unwrap();
            let thresholds = Thresholds::default();
            (
                shape == page.objects().map(|o| o.kind()).collect::<Vec<_>>(),
                compare(&before, &after, &thresholds).passes(&thresholds),
            )
        };
        assert!(objects_same, "这份样本重写之后对象的个数和种类不该变");

        let mut session = Session::open(&lib, &sample, None).expect("打开样本");
        let target = session.objects(0).unwrap()[0].id;
        let verdict = session.check_fidelity(0).unwrap();
        let outcome = session.transform(&[target], Matrix::translation(3.0, 0.0));
        if picture_same {
            assert_eq!(verdict, None);
            outcome.expect("重写之后画面不变的页可以修改");
        } else {
            assert_eq!(verdict, Some("重新生成内容流后页面的画面变了"));
            assert!(
                matches!(outcome, Err(EditError::PageNotEditable { page: 0, .. })),
                "{outcome:?}"
            );
            assert_eq!(session.version(), 0);
        }
    }

    /// 分层渲染按页内序号指对象：编号换成序号，删除、撤销之后跟着变。
    #[test]
    fn object_ids_map_to_their_current_z_index() {
        with_session(|session| {
            add_rect(session, 100.0, 40.0, 60.0, 40.0, DARK);
            add_rect(session, 150.0, 10.0, 20.0, 20.0, DARK);
            let ids: Vec<ObjectId> = session.objects(0).unwrap().iter().map(|o| o.id).collect();
            assert_eq!(session.z_indices(0, &[ids[2], ids[0]]).unwrap(), vec![2, 0]);
            assert!(session.z_indices(0, &[]).unwrap().is_empty());

            // 删掉中间的一个，上面的对象往下挪一位；撤销之后回到原位
            session.delete(&[ids[1]]).unwrap();
            assert_eq!(session.z_indices(0, &[ids[2]]).unwrap(), vec![1]);
            assert!(matches!(
                session.z_indices(0, &[ids[0], ids[1]]),
                Err(EditError::NoObject(id)) if id == ids[1]
            ));
            session.undo().unwrap();
            assert_eq!(session.z_indices(0, &ids).unwrap(), vec![0, 1, 2]);

            // 不认识的编号、别的页、没有列过对象的页
            assert!(session.z_indices(0, &[ObjectId(9999)]).is_err());
            session.add_blank_page(200.0, 100.0).unwrap();
            assert!(session.z_indices(1, &[ids[0]]).is_err());
        });
    }

    /// 校验的结论只得出一次、只报告一次：修改命令顺带做的校验也算。
    #[test]
    fn each_fidelity_verdict_is_reported_once() {
        with_session(|session| {
            session.add_blank_page(200.0, 100.0).unwrap();
            assert_eq!(session.known_fidelity(0), None);
            assert!(session.take_new_verdicts().is_empty());

            // 修改命令先校验它碰到的页。
            let id = session.objects(0).unwrap()[0].id;
            session
                .transform(&[id], Matrix::translation(5.0, 0.0))
                .unwrap();
            assert_eq!(session.known_fidelity(0), Some(None));
            assert_eq!(session.known_fidelity(1), None, "没碰过的页不校验");
            assert_eq!(session.take_new_verdicts(), vec![(0, None)]);

            // 已经有结论的页，再问、再改都不会重新报告。
            assert_eq!(session.check_fidelity(0).unwrap(), None);
            session
                .transform(&[id], Matrix::translation(5.0, 0.0))
                .unwrap();
            assert!(session.take_new_verdicts().is_empty());

            // 提前校验另一页。
            assert_eq!(session.check_fidelity(1).unwrap(), None);
            assert_eq!(session.take_new_verdicts(), vec![(1, None)]);
            assert!(
                session.check_fidelity(7).is_err(),
                "页码不对是错误，不是结论"
            );
            assert_eq!(session.known_fidelity(7), None);
            assert!(session.take_new_verdicts().is_empty());
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
