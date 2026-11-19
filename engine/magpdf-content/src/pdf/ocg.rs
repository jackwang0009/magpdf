//! 把默认在屏幕上隐藏的可选内容组（OCG）打开，供保真校验的像素比对使用（方案 2.3）。
//!
//! PDFium 没有“渲染时忽略图层”的公开接口。屏幕渲染走 `CPDF_OCContext` 的 View 用途
//! （`core/fpdfapi/page/cpdf_occontext.cpp` 的 `GetConfig`、`LoadOCGState`）：组自己的
//! `/Usage /View /ViewState` 优先，否则看默认配置（`/OCProperties /D`，或 `/Configs` 里第一个
//! Intent 为 View 的配置）的 `/BaseState`、`/ON`、`/OFF`，再看 `/AS` 里 Event 为 View、且带
//! `/View /ViewState` 字典的条目。这里按同一套规则找出 View 下不可见的组，只追加一节增量更新
//! 把它们改成可见，原文件字节不动。
//!
//! 打不开、返回 [`Unwritable`] 的两种：加密文件（追加的明文对象 PDFium 不一定认）；文件里有
//! “组关闭才显示”的可选内容成员字典（OCMD 的 `/P /AnyOff`、`/P /AllOff`，或 `/VE` 里带 `Not`）——
//! 把组打开反而会把这部分内容藏起来。调用方应当成图层没能打开，而不是当作已经比过。

use std::collections::{BTreeMap, BTreeSet};

use super::object::{PdfDict, PdfObject};
use super::reader::PdfDocument;
use super::writer::IncrementalWriter;

/// 有隐藏图层，但打不开：文件加了密，有“组关闭才显示”的成员字典，或者要改的对象放在交叉引用表
/// 文件的对象流里。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("隐藏的可选内容图层写不回文件")]
pub struct Unwritable;

/// 屏幕渲染（View 用途）下把隐藏的可选内容组打开。
///
/// - `Ok(None)`：没有可选内容，或者 View 下已经全部可见。字节不用改。
/// - `Ok(Some(bytes))`：增量更新后的文件，隐藏的组在 View 下可见。
/// - `Err(Unwritable)`：有隐藏的组，但写不回去。
pub fn reveal_view_layers(bytes: &[u8]) -> Result<Option<Vec<u8>>, Unwritable> {
    let doc = match PdfDocument::load(bytes.to_vec()) {
        Ok(doc) => doc,
        Err(_) => return Ok(None),
    };
    let config_loc = find_view_config(&doc);
    let config = config_loc
        .as_ref()
        .and_then(|loc| read_dict(&doc, loc.owner, &loc.path));
    let mut usage_off = Vec::new();
    let mut config_off = Vec::new();
    for number in collect_ocgs(&doc) {
        let Some(dict) = doc.object(number).and_then(|obj| obj.as_dict().cloned()) else {
            continue;
        };
        if !has_intent(&doc, &dict, b"View", b"View") {
            continue;
        }
        if let Some(state) = usage_view_state(&doc, &dict) {
            if is_off(&state) {
                usage_off.push(number);
            }
            continue;
        }
        if config_loc.is_some()
            && listed_in_ocgs(&doc, number)
            && !config_visible(&doc, config.as_ref(), number)
        {
            config_off.push(number);
        }
    }
    if usage_off.is_empty() && config_off.is_empty() {
        return Ok(None);
    }
    if doc.encrypt.is_some() || has_inverting_ocmd(&doc) {
        return Err(Unwritable);
    }
    let mut edits = Edits::new(&doc);
    for number in usage_off {
        force_view_on(&mut edits, number);
    }
    if !config_off.is_empty()
        && let Some(loc) = &config_loc
    {
        let ocgs = config_off;
        edits.mutate(loc.owner, |obj| {
            map_dict(obj, &loc.path, |dict| rewrite_config(&doc, dict, &ocgs));
        });
    }
    if edits.replaced.is_empty() {
        // 算出来有隐藏组，但一个对象都没改成：结构对不上，不能假装已经打开。
        return Err(Unwritable);
    }
    let mut writer = IncrementalWriter::new(&doc);
    for (number, object) in edits.replaced {
        writer.replace(number, object).map_err(|_| Unwritable)?;
    }
    writer.finish().map(Some).map_err(|_| Unwritable)
}

struct ConfigLoc {
    owner: u32,
    path: Vec<Step>,
}

struct Edits<'a> {
    doc: &'a PdfDocument,
    replaced: BTreeMap<u32, PdfObject>,
}

impl<'a> Edits<'a> {
    fn new(doc: &'a PdfDocument) -> Self {
        Self {
            doc,
            replaced: BTreeMap::new(),
        }
    }

    fn get(&self, number: u32) -> Option<PdfObject> {
        self.replaced
            .get(&number)
            .cloned()
            .or_else(|| self.doc.object(number))
    }

    fn mutate(&mut self, number: u32, f: impl FnOnce(&mut PdfObject)) {
        let Some(mut obj) = self.get(number) else {
            return;
        };
        let before = obj.clone();
        f(&mut obj);
        if obj != before {
            self.replaced.insert(number, obj);
        }
    }
}

#[derive(Clone)]
enum Step {
    Key(&'static [u8]),
    Index(usize),
}

fn collect_ocgs(doc: &PdfDocument) -> Vec<u32> {
    let mut nums = BTreeSet::new();
    if let Some(props) = oc_properties(doc)
        && let Some(items) = resolve_array(doc, props.get(b"OCGs"))
    {
        for item in items {
            if let Some((number, _)) = item.as_ref()
                && doc
                    .object(number)
                    .as_ref()
                    .and_then(PdfObject::as_dict)
                    .is_some()
            {
                nums.insert(number);
            }
        }
    }
    for (number, _, _) in doc.live_entries() {
        let Some(obj) = doc.object(number) else {
            continue;
        };
        if obj
            .as_dict()
            .and_then(|dict| dict.get(b"Type"))
            .and_then(PdfObject::as_name)
            == Some(b"OCG".as_slice())
        {
            nums.insert(number);
        }
    }
    nums.into_iter().collect()
}

/// 文件里有没有“组关闭才显示”的可选内容成员字典（`CPDF_OCContext::LoadOCMDState`）：
/// `/P /AnyOff`、`/P /AllOff`，或者 `/VE` 里带 `Not`。成员字典可以直接写在资源的 `/Properties`、
/// 外部对象的 `/OC` 里，所以把每个对象的字典、数组都翻一遍，不只看顶层。
fn has_inverting_ocmd(doc: &PdfDocument) -> bool {
    doc.live_entries()
        .into_iter()
        .filter_map(|(number, _, _)| doc.object(number))
        .any(|obj| contains_inverting_ocmd(doc, &obj, 0))
}

fn contains_inverting_ocmd(doc: &PdfDocument, obj: &PdfObject, depth: usize) -> bool {
    if depth > 32 {
        return false;
    }
    if let PdfObject::Array(items) = obj {
        return items
            .iter()
            .any(|item| contains_inverting_ocmd(doc, item, depth + 1));
    }
    let Some(dict) = obj.as_dict() else {
        return false;
    };
    if dict.get(b"Type").and_then(PdfObject::as_name) == Some(b"OCMD".as_slice())
        && ocmd_inverts(doc, dict)
    {
        return true;
    }
    dict.iter()
        .any(|(_, value)| contains_inverting_ocmd(doc, value, depth + 1))
}

fn ocmd_inverts(doc: &PdfDocument, ocmd: &PdfDict) -> bool {
    if let Some(ve @ PdfObject::Array(_)) = doc.resolve(ocmd.get(b"VE")) {
        return expression_has_not(doc, &ve, 0);
    }
    // `/OCGs` 只写一个组（不是数组）时 PDFium 直接取这个组的可见性，`/P` 不起作用。
    matches!(doc.resolve(ocmd.get(b"OCGs")), Some(PdfObject::Array(_)))
        && matches!(
            dict_name(doc, ocmd, b"P", b"AnyOn").as_slice(),
            b"AnyOff" | b"AllOff"
        )
}

fn expression_has_not(doc: &PdfDocument, expr: &PdfObject, depth: usize) -> bool {
    if depth > 32 {
        return false;
    }
    let PdfObject::Array(items) = expr else {
        return false;
    };
    if items.first().and_then(PdfObject::as_name) == Some(b"Not".as_slice()) {
        return true;
    }
    items.iter().skip(1).any(|item| {
        doc.resolve(Some(item))
            .is_some_and(|item| expression_has_not(doc, &item, depth + 1))
    })
}

fn oc_properties(doc: &PdfDocument) -> Option<PdfDict> {
    let (number, _) = doc.trailer_dict().get(b"Root")?.as_ref()?;
    let catalog = doc.object(number)?;
    let entry = catalog.as_dict()?.get(b"OCProperties")?;
    doc.resolve(Some(entry))?.as_dict().cloned()
}

fn listed_in_ocgs(doc: &PdfDocument, ocg: u32) -> bool {
    let Some(props) = oc_properties(doc) else {
        return false;
    };
    array_contains_ref(doc, props.get(b"OCGs"), ocg)
}

/// `GetConfig`：`/Configs` 里第一个 Intent 含 View 的配置，否则 `/D`。
fn find_view_config(doc: &PdfDocument) -> Option<ConfigLoc> {
    let catalog_num = doc.trailer_dict().get(b"Root")?.as_ref()?.0;
    let catalog = doc.object(catalog_num)?;
    let oc_entry = catalog.as_dict()?.get(b"OCProperties")?;
    let (oc_owner, oc_path, oc_dict) = match oc_entry {
        PdfObject::Ref(number, _) => {
            let dict = doc.object(*number)?.as_dict()?.clone();
            (*number, Vec::new(), dict)
        }
        PdfObject::Dict(dict) => (catalog_num, vec![Step::Key(b"OCProperties")], dict.clone()),
        _ => return None,
    };
    if let Some(items) = resolve_array(doc, oc_dict.get(b"Configs")) {
        for (index, item) in items.iter().enumerate() {
            let Some(dict) = resolve_dict(doc, item) else {
                continue;
            };
            if !has_intent(doc, &dict, b"View", b"") {
                continue;
            }
            return Some(config_item_loc(&oc_dict, oc_owner, &oc_path, index, item));
        }
    }
    match oc_dict.get(b"D") {
        Some(PdfObject::Ref(number, _)) => Some(ConfigLoc {
            owner: *number,
            path: Vec::new(),
        }),
        Some(PdfObject::Dict(_)) => {
            let mut path = oc_path;
            path.push(Step::Key(b"D"));
            Some(ConfigLoc {
                owner: oc_owner,
                path,
            })
        }
        _ => None,
    }
}

fn config_item_loc(
    oc_dict: &PdfDict,
    oc_owner: u32,
    oc_path: &[Step],
    index: usize,
    item: &PdfObject,
) -> ConfigLoc {
    if let Some((number, _)) = item.as_ref() {
        return ConfigLoc {
            owner: number,
            path: Vec::new(),
        };
    }
    // `/Configs` 本身是间接对象时，数组在那个对象里，不能先解引用再找。
    match oc_dict.get(b"Configs") {
        Some(PdfObject::Ref(number, _)) => ConfigLoc {
            owner: *number,
            path: vec![Step::Index(index)],
        },
        _ => {
            let mut path = oc_path.to_vec();
            path.push(Step::Key(b"Configs"));
            path.push(Step::Index(index));
            ConfigLoc {
                owner: oc_owner,
                path,
            }
        }
    }
}

fn read_dict(doc: &PdfDocument, owner: u32, path: &[Step]) -> Option<PdfDict> {
    let mut obj = doc.object(owner)?;
    for step in path {
        obj = match step {
            Step::Key(key) => {
                let next = obj.as_dict()?.get(key)?.clone();
                doc.resolve(Some(&next))?
            }
            Step::Index(index) => {
                let PdfObject::Array(items) = &obj else {
                    return None;
                };
                let next = items.get(*index)?.clone();
                doc.resolve(Some(&next))?
            }
        };
    }
    obj.as_dict().cloned()
}

fn map_dict(obj: &mut PdfObject, path: &[Step], f: impl FnOnce(&mut PdfDict)) {
    if path.is_empty() {
        if let PdfObject::Dict(dict) = obj {
            f(dict);
        }
        return;
    }
    match &path[0] {
        Step::Key(key) => {
            let PdfObject::Dict(dict) = obj else {
                return;
            };
            let mut child = dict.get(key).cloned().unwrap_or(PdfObject::Null);
            map_dict(&mut child, &path[1..], f);
            dict.set(key, child);
        }
        Step::Index(index) => {
            let PdfObject::Array(items) = obj else {
                return;
            };
            if let Some(child) = items.get_mut(*index) {
                map_dict(child, &path[1..], f);
            }
        }
    }
}

fn force_view_on(edits: &mut Edits<'_>, ocg: u32) {
    let (owner, path) = descend(edits, ocg, &[b"Usage", b"View"]);
    edits.mutate(owner, |obj| {
        set_nested_name(obj, &path, b"ViewState", b"ON")
    });
}

/// 沿间接引用往下走，停在还要按内嵌字典改的那个对象上。
fn descend(
    edits: &Edits<'_>,
    mut owner: u32,
    mut path: &[&'static [u8]],
) -> (u32, Vec<&'static [u8]>) {
    while let Some((key, rest)) = path.split_first() {
        let Some(obj) = edits.get(owner) else {
            break;
        };
        let Some(dict) = obj.as_dict() else {
            break;
        };
        match dict.get(key) {
            Some(PdfObject::Ref(number, _)) => {
                owner = *number;
                path = rest;
            }
            _ => break,
        }
    }
    (owner, path.to_vec())
}

fn set_nested_name(obj: &mut PdfObject, path: &[&[u8]], key: &[u8], name: &[u8]) {
    if path.is_empty() {
        if let PdfObject::Dict(dict) = obj {
            dict.set(key, PdfObject::Name(name.to_vec()));
        }
        return;
    }
    let PdfObject::Dict(dict) = obj else {
        return;
    };
    let mut child = match dict.get(path[0]) {
        Some(PdfObject::Dict(inner)) => PdfObject::Dict(inner.clone()),
        Some(other) => other.clone(),
        None => PdfObject::Dict(PdfDict::new()),
    };
    set_nested_name(&mut child, &path[1..], key, name);
    dict.set(path[0], child);
}

fn usage_view_state(doc: &PdfDocument, ocg: &PdfDict) -> Option<PdfObject> {
    let usage = doc.deref(ocg.get(b"Usage")?)?;
    let view = doc.deref(usage.as_dict()?.get(b"View")?)?;
    let state = view.as_dict()?.get(b"ViewState")?;
    Some(doc.resolve(Some(state)).unwrap_or(PdfObject::Null))
}

fn config_visible(doc: &PdfDocument, config: Option<&PdfDict>, ocg: u32) -> bool {
    let Some(config) = config else {
        return true;
    };
    let mut state = match config.get(b"BaseState") {
        Some(value) => !resolved_off(doc, value),
        None => true,
    };
    if array_contains_ref(doc, config.get(b"ON"), ocg) {
        state = true;
    }
    if array_contains_ref(doc, config.get(b"OFF"), ocg) {
        state = false;
    }
    if let Some(entries) = resolve_array(doc, config.get(b"AS")) {
        for entry in &entries {
            state = apply_as(doc, entry, ocg, state);
        }
    }
    state
}

fn apply_as(doc: &PdfDocument, entry: &PdfObject, ocg: u32, state: bool) -> bool {
    let Some(dict) = resolve_dict(doc, entry) else {
        return state;
    };
    if dict_name(doc, &dict, b"Event", b"View") != b"View" {
        return state;
    }
    if !array_contains_ref(doc, dict.get(b"OCGs"), ocg) {
        return state;
    }
    let Some(view) = dict.get(b"View").and_then(|value| doc.deref(value)) else {
        return state;
    };
    let Some(view) = view.as_dict() else {
        return state;
    };
    // PDFium 取不到键时得空串，`!= "OFF"` 为真：有 /View 字典但没写 ViewState 也算可见。
    !view
        .get(b"ViewState")
        .is_some_and(|value| resolved_off(doc, value))
}

fn rewrite_config(doc: &PdfDocument, dict: &mut PdfDict, ocgs: &[u32]) {
    if dict
        .get(b"BaseState")
        .is_some_and(|value| resolved_off(doc, value))
    {
        dict.set(b"BaseState", PdfObject::Name(b"ON".to_vec()));
    }
    if let Some(items) = resolve_array(doc, dict.get(b"OFF")) {
        let kept: Vec<PdfObject> = items
            .into_iter()
            .filter(|item| {
                !item
                    .as_ref()
                    .is_some_and(|(number, _)| ocgs.contains(&number))
            })
            .collect();
        dict.set(b"OFF", PdfObject::Array(kept));
    }
    if let Some(entries) = resolve_array(doc, dict.get(b"AS")) {
        let entries = entries
            .into_iter()
            .map(|entry| rewrite_as_entry(doc, entry))
            .collect();
        dict.set(b"AS", PdfObject::Array(entries));
    }
}

fn rewrite_as_entry(doc: &PdfDocument, entry: PdfObject) -> PdfObject {
    let direct = match &entry {
        PdfObject::Ref(number, _) => doc.object(*number).unwrap_or(entry.clone()),
        other => other.clone(),
    };
    let PdfObject::Dict(mut dict) = direct else {
        return entry;
    };
    if dict_name(doc, &dict, b"Event", b"View") != b"View" {
        return entry;
    }
    let Some(view_obj) = dict.get(b"View").cloned() else {
        return entry;
    };
    let view_direct = doc.resolve(Some(&view_obj)).unwrap_or(view_obj);
    let PdfObject::Dict(mut view) = view_direct else {
        return entry;
    };
    if !view
        .get(b"ViewState")
        .is_some_and(|value| resolved_off(doc, value))
    {
        return entry;
    }
    view.set(b"ViewState", PdfObject::Name(b"ON".to_vec()));
    dict.set(b"View", PdfObject::Dict(view));
    PdfObject::Dict(dict)
}

fn has_intent(doc: &PdfDocument, dict: &PdfDict, element: &[u8], default: &[u8]) -> bool {
    let Some(intent) = dict.get(b"Intent") else {
        return element == default;
    };
    match doc.resolve(Some(intent)) {
        Some(PdfObject::Array(items)) => items.iter().any(|item| {
            doc.resolve(Some(item))
                .as_ref()
                .and_then(name_bytes)
                .is_some_and(|name| name == b"All" || name == element)
        }),
        Some(other) => name_bytes(&other).is_some_and(|name| name == b"All" || name == element),
        None => false,
    }
}

fn dict_name(doc: &PdfDocument, dict: &PdfDict, key: &[u8], default: &'static [u8]) -> Vec<u8> {
    dict.get(key)
        .and_then(|value| doc.resolve(Some(value)))
        .as_ref()
        .and_then(name_bytes)
        .unwrap_or(default)
        .to_vec()
}

fn array_contains_ref(doc: &PdfDocument, value: Option<&PdfObject>, ocg: u32) -> bool {
    resolve_array(doc, value).is_some_and(|items| {
        items
            .iter()
            .any(|item| item.as_ref().is_some_and(|(number, _)| number == ocg))
    })
}

fn resolve_array(doc: &PdfDocument, value: Option<&PdfObject>) -> Option<Vec<PdfObject>> {
    match doc.resolve(value)? {
        PdfObject::Array(items) => Some(items),
        _ => None,
    }
}

fn resolve_dict(doc: &PdfDocument, value: &PdfObject) -> Option<PdfDict> {
    doc.resolve(Some(value))?.as_dict().cloned()
}

fn resolved_off(doc: &PdfDocument, value: &PdfObject) -> bool {
    doc.resolve(Some(value)).as_ref().is_some_and(is_off)
}

fn is_off(obj: &PdfObject) -> bool {
    name_bytes(obj) == Some(b"OFF".as_slice())
}

fn name_bytes(obj: &PdfObject) -> Option<&[u8]> {
    match obj {
        PdfObject::Name(name) | PdfObject::Str(name) => Some(name),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::reader::tests::{CATALOG, PAGE, PAGES, build};

    const PAGE_WITH_LAYER: &str = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] \
        /Resources << /Properties << /H 5 0 R >> >> /Contents 4 0 R >>";
    const STREAM: &str = "<< /Length 3 >>\nstream\nq Q\nendstream";
    const OCG_OFF: &str =
        "<< /Type /OCG /Usage << /View << /ViewState /OFF >> /Print << /PrintState /ON >> >> >>";
    const OCG_ON: &str = "<< /Type /OCG /Usage << /View << /ViewState /ON >> >> >>";
    const OCG_BARE: &str = "<< /Type /OCG >>";

    fn view_state_of(bytes: &[u8], ocg: u32) -> Option<Vec<u8>> {
        let doc = PdfDocument::load(bytes.to_vec()).unwrap();
        let dict = doc.object(ocg).unwrap();
        usage_view_state(&doc, dict.as_dict().unwrap())
            .and_then(|state| name_bytes(&state).map(<[u8]>::to_vec))
    }

    #[test]
    fn no_optional_content_is_left_alone() {
        let raw = build(&[(1, CATALOG), (2, PAGES), (3, PAGE), (4, STREAM)], "");
        assert_eq!(reveal_view_layers(&raw).unwrap(), None);
    }

    #[test]
    fn view_state_off_is_turned_on_and_the_original_bytes_stay() {
        let raw = build(
            &[
                (
                    1,
                    "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [5 0 R] /D << >> >> >>",
                ),
                (2, PAGES),
                (3, PAGE_WITH_LAYER),
                (4, STREAM),
                (5, OCG_OFF),
            ],
            "",
        );
        let revealed = reveal_view_layers(&raw).unwrap().expect("应该写出增量更新");
        assert!(revealed.starts_with(&raw), "原文件字节原样保留");
        assert_eq!(
            view_state_of(&revealed, 5).as_deref(),
            Some(b"ON".as_slice())
        );
        assert_eq!(view_state_of(&raw, 5).as_deref(), Some(b"OFF".as_slice()));
    }

    #[test]
    fn already_visible_group_is_not_rewritten() {
        let raw = build(
            &[
                (
                    1,
                    "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [5 0 R] /D << >> >> >>",
                ),
                (2, PAGES),
                (3, PAGE_WITH_LAYER),
                (4, STREAM),
                (5, OCG_ON),
            ],
            "",
        );
        assert_eq!(reveal_view_layers(&raw).unwrap(), None);
    }

    #[test]
    fn off_array_and_base_state_are_cleared_for_view() {
        let raw = build(
            &[
                (
                    1,
                    "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [5 0 R 6 0 R] /D << \
                    /BaseState /OFF /OFF [5 0 R] /ON [6 0 R] >> >> >>",
                ),
                (2, PAGES),
                (3, PAGE),
                (4, STREAM),
                (5, OCG_BARE),
                (6, OCG_BARE),
            ],
            "",
        );
        let revealed = reveal_view_layers(&raw).unwrap().unwrap();
        let doc = PdfDocument::load(revealed).unwrap();
        let catalog = doc.object(1).unwrap();
        let props = doc
            .deref(catalog.as_dict().unwrap().get(b"OCProperties").unwrap())
            .unwrap();
        let config = props.as_dict().unwrap().get(b"D").unwrap();
        let config = config.as_dict().unwrap();
        assert_eq!(
            config.get(b"BaseState").and_then(PdfObject::as_name),
            Some(b"ON".as_slice())
        );
        let off = config.get(b"OFF").and_then(PdfObject::as_array).unwrap();
        assert!(
            off.iter().all(|item| item.as_ref() != Some((5, 0))),
            "5 号组还留在 /OFF 里"
        );
        assert!(config_visible(&doc, Some(config), 5));
        assert!(config_visible(&doc, Some(config), 6));
    }

    #[test]
    fn view_intent_config_wins_over_the_default_and_design_intent_does_not() {
        let hidden_by_view_config = build(
            &[
                (
                    1,
                    "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [5 0 R] \
                    /D << /ON [5 0 R] >> /Configs [6 0 R] >> >>",
                ),
                (2, PAGES),
                (3, PAGE),
                (4, STREAM),
                (5, OCG_BARE),
                (6, "<< /Intent /View /OFF [5 0 R] >>"),
            ],
            "",
        );
        let revealed = reveal_view_layers(&hidden_by_view_config).unwrap().unwrap();
        let doc = PdfDocument::load(revealed).unwrap();
        let config = doc.object(6).unwrap();
        let config = config.as_dict().unwrap();
        assert!(
            !array_contains_ref(&doc, config.get(b"OFF"), 5),
            "View 配置的 /OFF 还含着 5 号组"
        );

        let design_only = build(
            &[
                (
                    1,
                    "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [5 0 R] \
                    /D << /ON [5 0 R] >> /Configs [6 0 R] >> >>",
                ),
                (2, PAGES),
                (3, PAGE),
                (4, STREAM),
                (5, OCG_BARE),
                (6, "<< /Intent /Design /OFF [5 0 R] >>"),
            ],
            "",
        );
        assert_eq!(
            reveal_view_layers(&design_only).unwrap(),
            None,
            "Intent 不是 View 的配置 PDFium 不用"
        );
    }

    #[test]
    fn as_view_state_off_is_turned_on_without_touching_print_entries() {
        let raw = build(
            &[
                (
                    1,
                    "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [5 0 R] /D << /AS [ \
                    << /Event /Print /OCGs [5 0 R] /Print << /PrintState /OFF >> >> \
                    << /Event /View /OCGs [5 0 R] /View << /ViewState /OFF >> >> ] >> >> >>",
                ),
                (2, PAGES),
                (3, PAGE),
                (4, STREAM),
                (5, OCG_BARE),
            ],
            "",
        );
        let revealed = reveal_view_layers(&raw).unwrap().unwrap();
        let doc = PdfDocument::load(revealed).unwrap();
        let catalog = doc.object(1).unwrap();
        let props = doc
            .deref(catalog.as_dict().unwrap().get(b"OCProperties").unwrap())
            .unwrap();
        let config = props
            .as_dict()
            .unwrap()
            .get(b"D")
            .unwrap()
            .as_dict()
            .unwrap();
        assert!(config_visible(&doc, Some(config), 5));
        let entries = config.get(b"AS").unwrap().as_array().unwrap();
        let print = entries[0].as_dict().unwrap();
        let print_state = print
            .get(b"Print")
            .unwrap()
            .as_dict()
            .unwrap()
            .get(b"PrintState")
            .unwrap();
        assert!(is_off(print_state), "打印用途的状态不该被改掉");
    }

    fn with_membership(properties: &str, extra: &[(u32, &str)]) -> Vec<u8> {
        let page = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] \
            /Resources << /Properties << /M {properties} >> >> /Contents 4 0 R >>"
        );
        let mut objects = vec![
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [5 0 R] /D << >> >> >>",
            ),
            (2, PAGES),
            (3, page.as_str()),
            (4, STREAM),
            (5, OCG_OFF),
        ];
        objects.extend_from_slice(extra);
        build(&objects, "")
    }

    #[test]
    fn membership_shown_when_the_group_is_off_is_refused() {
        for properties in [
            "<< /Type /OCMD /OCGs [5 0 R] /P /AnyOff >>",
            "<< /Type /OCMD /OCGs [5 0 R] /P /AllOff >>",
            "<< /Type /OCMD /VE [/Or [/Not 5 0 R] 5 0 R] >>",
        ] {
            assert_eq!(
                reveal_view_layers(&with_membership(properties, &[])),
                Err(Unwritable),
                "{properties}"
            );
        }
        let indirect = with_membership(
            "6 0 R",
            &[(6, "<< /Type /OCMD /OCGs [5 0 R] /P /AnyOff >>")],
        );
        assert_eq!(reveal_view_layers(&indirect), Err(Unwritable));
    }

    #[test]
    fn membership_shown_when_the_group_is_on_is_opened() {
        for properties in [
            "<< /Type /OCMD /OCGs 5 0 R >>",
            // 只有一个组时 PDFium 不看 /P。
            "<< /Type /OCMD /OCGs 5 0 R /P /AnyOff >>",
            "<< /Type /OCMD /OCGs [5 0 R] /P /AllOn >>",
            "<< /Type /OCMD /VE [/And 5 0 R [/Or 5 0 R]] >>",
        ] {
            let revealed = reveal_view_layers(&with_membership(properties, &[]))
                .unwrap()
                .unwrap_or_else(|| panic!("{properties} 应该写出增量更新"));
            assert_eq!(
                view_state_of(&revealed, 5).as_deref(),
                Some(b"ON".as_slice())
            );
        }
    }

    #[test]
    fn indirect_view_state_and_as_entry_without_view_state_follow_pdfium() {
        let indirect = build(
            &[
                (
                    1,
                    "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [5 0 R] /D << >> >> >>",
                ),
                (2, PAGES),
                (3, PAGE),
                (4, STREAM),
                (
                    5,
                    "<< /Type /OCG /Usage << /View << /ViewState 6 0 R >> >> >>",
                ),
                (6, "/OFF"),
            ],
            "",
        );
        let revealed = reveal_view_layers(&indirect).unwrap().unwrap();
        assert_eq!(
            view_state_of(&revealed, 5).as_deref(),
            Some(b"ON".as_slice())
        );

        // /AS 条目里有 /View 字典但没写 ViewState：PDFium 判为可见，不用改。
        let as_without_state = build(
            &[
                (
                    1,
                    "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [5 0 R] /D << \
                    /OFF [5 0 R] /AS [ << /Event /View /OCGs [5 0 R] /View << >> >> ] >> >> >>",
                ),
                (2, PAGES),
                (3, PAGE),
                (4, STREAM),
                (5, OCG_BARE),
            ],
            "",
        );
        assert_eq!(reveal_view_layers(&as_without_state).unwrap(), None);
    }

    #[test]
    fn encrypted_file_with_a_hidden_group_is_refused() {
        let raw = build(
            &[
                (
                    1,
                    "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [5 0 R] >> >>",
                ),
                (2, PAGES),
                (3, PAGE),
                (4, STREAM),
                (5, OCG_OFF),
                (9, "<< /Filter /Standard >>"),
            ],
            "/Encrypt 9 0 R",
        );
        assert_eq!(reveal_view_layers(&raw), Err(Unwritable));
    }

    #[test]
    fn sample_print_only_layer_is_revealed_without_touching_the_content_stream() {
        let raw = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/corpus/synthetic/struct-ocg-layers.pdf"),
        )
        .expect("样本");
        let revealed = reveal_view_layers(&raw)
            .unwrap()
            .expect("样本里有 ViewState /OFF");
        assert!(revealed.starts_with(&raw));
        let marker = b"PRINT ONLY \\(View";
        assert!(
            revealed
                .windows(marker.len())
                .any(|window| window == marker),
            "内容流里的文字不该被改写"
        );
        let doc = PdfDocument::load(revealed).unwrap();
        let mut off = 0;
        let mut on = 0;
        for number in collect_ocgs(&doc) {
            let dict = doc.object(number).unwrap();
            match usage_view_state(&doc, dict.as_dict().unwrap()).as_ref() {
                Some(state) if is_off(state) => off += 1,
                Some(_) => on += 1,
                None => {}
            }
        }
        assert_eq!(off, 0, "View 下不应再有 /ViewState /OFF 的组");
        assert!(on >= 2, "两个组的 View 状态都还在");
    }
}
