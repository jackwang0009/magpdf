//! 共享内存槽位的分配与复用（《接口协议》3.3、待定项 I2）。
//!
//! 每个槽位是一个 `(doc_id, page_id, scale, x, y)` 的瓦片缓存项。命中时直接返回已有的代数，界面可以用
//! 同一个 URL 取回；未命中时按 LRU 选一个槽位，渲染后写入，代数随之加 2。渲染内容不变时也可以强制重绘
//! （恢复后主进程会清空缓存，界面重新请求）。

use std::collections::HashMap;

/// 瓦片缓存键：文档、页面、缩放档与瓦片在整页网格上的位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub doc_id: u32,
    pub page_id: u32,
    /// 缩放倍数的二进制表示。界面同一个缩放档每次传来的是同一个数，按位比较就能当键用；
    /// 渲染用的也是这个数本身。取整（比如只留三位小数）会让整页的像素尺寸和界面算的不一样，
    /// 瓦片贴上去就差出零点几个像素。
    scale_bits: u32,
    pub x: u32,
    pub y: u32,
    /// 分层渲染（方案 3.2）时这块瓦片属于哪一层：0 是整页，其余是那一层的指纹。
    /// 同一个位置上，整页的瓦片和各层的瓦片是不同的缓存项。
    layer: u64,
}

impl TileKey {
    pub fn new(doc_id: u32, page_id: u32, scale: f32, x: u32, y: u32) -> Self {
        Self {
            doc_id,
            page_id,
            scale_bits: scale.to_bits(),
            x,
            y,
            layer: 0,
        }
    }

    /// 同一个位置上某一层的瓦片。`fingerprint` 由“留哪些对象”算出来，不同的层给不同的值。
    pub fn in_layer(mut self, fingerprint: u64) -> Self {
        // 0 留给整页
        self.layer = fingerprint.max(1);
        self
    }

    /// 请求里的缩放倍数，原样取回。
    pub fn scale(&self) -> f32 {
        f32::from_bits(self.scale_bits)
    }
}

#[derive(Debug)]
struct SlotEntry {
    key: TileKey,
    generation: u32,
    /// 最近一次使用或写入的序号，越大越新。
    used: u64,
}

/// 瓦片槽位分配器。不直接持有共享内存，只记账。
#[derive(Debug)]
pub struct SlotTable {
    entries: Vec<Option<SlotEntry>>,
    index: HashMap<TileKey, u32>,
    tick: u64,
}

impl SlotTable {
    pub fn new(slots: u32) -> Self {
        Self {
            entries: (0..slots).map(|_| None).collect(),
            index: HashMap::new(),
            tick: 0,
        }
    }

    pub fn slots(&self) -> u32 {
        u32::try_from(self.entries.len()).unwrap_or(0)
    }

    /// 已有的瓦片：返回槽位号与代数，并刷新使用时间。
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "V06 原型每次都重新渲染，缓存命中还没接进渲染路径")
    )]
    pub fn hit(&mut self, key: &TileKey) -> Option<(u32, u32)> {
        let slot = *self.index.get(key)?;
        self.tick += 1;
        let entry = self.entries.get_mut(slot as usize)?.as_mut()?;
        entry.used = self.tick;
        Some((slot, entry.generation))
    }

    /// 为 `key` 分配一个槽位：优先用空槽，否则淘汰最久未用的槽位。返回槽位号；写入后调用 [`Self::commit`]。
    pub fn acquire(&mut self, key: TileKey) -> u32 {
        self.tick += 1;
        let tick = self.tick;
        let slot = if let Some((i, _)) = self.entries.iter().enumerate().find(|(_, e)| e.is_none())
        {
            i
        } else {
            self.entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.as_ref().map_or(0, |s| s.used))
                .map(|(i, _)| i)
                .unwrap_or(0)
        };
        if let Some(Some(old)) = self.entries.get(slot) {
            self.index.remove(&old.key);
        }
        if let Some(place) = self.entries.get_mut(slot) {
            *place = Some(SlotEntry {
                key,
                generation: 0,
                used: tick,
            });
            self.index.insert(key, u32::try_from(slot).unwrap_or(0));
        }
        u32::try_from(slot).unwrap_or(0)
    }

    /// 写入完成：记录新的代数。
    pub fn commit(&mut self, slot: u32, generation: u32) {
        self.tick += 1;
        if let Some(Some(entry)) = self.entries.get_mut(slot as usize) {
            entry.generation = generation;
            entry.used = self.tick;
        }
    }

    /// 槽位对应的瓦片键。
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "V06 原型每次都重新渲染，缓存命中还没接进渲染路径")
    )]
    pub fn key_of(&self, slot: u32) -> Option<TileKey> {
        self.entries.get(slot as usize)?.as_ref().map(|e| e.key)
    }

    /// 清空记账（引擎重启后共享内存里的内容不再可信）。
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "V06 原型里引擎重启就是新进程，还没有需要清空的场合"
        )
    )]
    pub fn clear(&mut self) {
        for place in &mut self.entries {
            *place = None;
        }
        self.index.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(x: u32) -> TileKey {
        TileKey::new(1, 0, 1.0, x, 0)
    }

    #[test]
    fn hits_and_lru_eviction() {
        let mut t = SlotTable::new(2);
        let a = t.acquire(key(0));
        t.commit(a, 2);
        let b = t.acquire(key(512));
        t.commit(b, 2);
        assert_eq!(t.hit(&key(0)), Some((a, 2)));
        // 0 刚用过，淘汰 512 那个。
        let c = t.acquire(key(1024));
        assert_eq!(c, b);
        assert_eq!(t.hit(&key(512)), None);
        assert_eq!(t.key_of(c), Some(key(1024)));
        t.clear();
        assert_eq!(t.hit(&key(0)), None);
    }

    #[test]
    fn scale_is_kept_exactly() {
        // 界面的缩放档是 √2 的幂乘 96/72，三位小数留不住。
        let scale = 1.885_618_f32;
        assert_eq!(TileKey::new(1, 0, scale, 0, 0).scale(), scale);
        assert_eq!(
            TileKey::new(1, 0, scale, 0, 0),
            TileKey::new(1, 0, scale, 0, 0)
        );
        assert_ne!(
            TileKey::new(1, 0, scale, 0, 0),
            TileKey::new(1, 0, 1.886, 0, 0)
        );
        assert_ne!(TileKey::new(1, 0, 1.0, 0, 0), TileKey::new(1, 0, 2.0, 0, 0));
    }

    #[test]
    fn layers_of_the_same_tile_are_separate_entries() {
        let whole = key(0);
        assert_ne!(whole, whole.in_layer(7));
        assert_ne!(whole.in_layer(7), whole.in_layer(8));
        assert_eq!(whole.in_layer(7), whole.in_layer(7));
        // 指纹恰好是 0 也不会和整页的瓦片混在一起
        assert_ne!(whole, whole.in_layer(0));

        let mut t = SlotTable::new(4);
        let a = t.acquire(whole);
        t.commit(a, 2);
        let b = t.acquire(whole.in_layer(7));
        t.commit(b, 2);
        assert_ne!(a, b);
        assert_eq!(t.hit(&whole), Some((a, 2)));
        assert_eq!(t.hit(&whole.in_layer(7)), Some((b, 2)));
        assert_eq!(t.hit(&whole.in_layer(8)), None);
    }
}
