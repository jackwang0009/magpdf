import { describe, expect, it } from 'vitest';
import { A4 } from '../document/types';
import {
  TILE_LEVELS,
  TILE_SIZE,
  pickLevel,
  pageTileGrid,
  tileBytes,
  tilePageRect,
  tileTouches,
  tilesInPageRect,
} from './tiles';

describe('缩放档', () => {
  it('13 档，从 1/8 到 8，按 √2 递进', () => {
    expect(TILE_LEVELS).toHaveLength(13);
    expect(TILE_LEVELS[0]).toBeCloseTo(0.125, 6);
    expect(TILE_LEVELS[12]).toBeCloseTo(8, 6);
    for (let i = 1; i < TILE_LEVELS.length; i++) {
      expect(TILE_LEVELS[i]! / TILE_LEVELS[i - 1]!).toBeCloseTo(Math.SQRT2, 4);
    }
  });

  it('取不小于显示比例的最小档（宁可多渲染也不放大模糊）', () => {
    expect(TILE_LEVELS[pickLevel(1)]).toBe(1);
    expect(TILE_LEVELS[pickLevel(0.6)]).toBeGreaterThanOrEqual(0.6);
    // 0.8 落在 0.707 与 1 之间，应选 1
    expect(pickLevel(0.8)).toBe(pickLevel(1));
    // 超过最大档时夹到最后一档
    expect(pickLevel(20)).toBe(TILE_LEVELS.length - 1);
    // 极小比例落到第一档
    expect(pickLevel(0.01)).toBe(0);
  });

  it('0.5% 容差内不跳档', () => {
    expect(pickLevel(1.0000001)).toBe(pickLevel(1));
  });
});

describe('页面瓦片网格', () => {
  it('A4 在 100% 档上切成 4×5 个 256px 瓦片', () => {
    const level = pickLevel(1);
    const grid = pageTileGrid(A4, level);
    expect(grid.pxPerPt).toBeCloseTo((96 / 72) * TILE_LEVELS[level]!, 6);
    expect(grid.cols).toBe(Math.ceil(grid.pxWidth / TILE_SIZE));
    // 794×1123 设备像素 → 4 列 × 5 行
    expect(grid.cols).toBe(4);
    expect(grid.rows).toBe(5);
  });

  it('边缘瓦片裁到页面边界，覆盖面积不超过整页（误差在裁边一个像素内）', () => {
    const grid = pageTileGrid(A4, pickLevel(1));
    let area = 0;
    for (let ty = 0; ty < grid.rows; ty++) {
      for (let tx = 0; tx < grid.cols; tx++) {
        const px = tilePageRect(grid, tx, ty);
        area += px.width * px.height;
      }
    }
    // 页宽/高不是 256 的整数倍，覆盖区域到像素取整，最多多出半圈像素
    const slack = (grid.pxWidth + grid.pxHeight) / grid.pxPerPt / grid.pxPerPt;
    expect(area).toBeGreaterThanOrEqual(A4.width * A4.height);
    expect(area - A4.width * A4.height).toBeLessThan(slack * 2);
  });

  it('只返回与矩形相交的瓦片', () => {
    const grid = pageTileGrid(A4, pickLevel(1));
    const all = tilesInPageRect(grid, { x: 0, y: 0, width: A4.width, height: A4.height }, 3, 0);
    expect(all).toHaveLength(grid.cols * grid.rows);
    for (const key of all) expect(key.page).toBe(3);
    const topLeft = tilesInPageRect(grid, { x: 0, y: 0, width: 10, height: 10 }, 0, 0);
    expect(topLeft).toHaveLength(1);
    expect(topLeft[0]).toMatchObject({ tx: 0, ty: 0 });
  });

  it('瓦片字节数按 RGBA 计', () => {
    expect(tileBytes(TILE_SIZE, TILE_SIZE)).toBe(TILE_SIZE * TILE_SIZE * 4);
  });

  it('瓦片碰不碰得到页面上的一块地方：往外放宽两个设备像素，擦边也算', () => {
    // 100% 档（每 pt 4/3 像素），第 2 列：x 192–384 pt，y 0–192 pt
    const key = { page: 0, level: 6, tx: 1, ty: 0 };
    expect(tileTouches(key, { x: 200, y: 10, width: 10, height: 10 })).toBe(true);
    // 右边到 150 pt，也就是 200 像素，放宽之后到 202，碰不到从 256 像素开始的这一块
    expect(tileTouches(key, { x: 100, y: 10, width: 50, height: 10 })).toBe(false);
    // 右边到 191 pt，离这一块不到两个像素
    expect(tileTouches(key, { x: 150, y: 10, width: 41, height: 10 })).toBe(true);
    expect(tileTouches(key, { x: 200, y: 200, width: 10, height: 10 })).toBe(false);
  });
});
