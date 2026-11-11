import { fontFamily, metrics, palette } from '@magpdf/ui';
import type { Rect, Size } from '../geometry';
import { handleCenters } from '../tools/handles';
import { rulerTicks } from './rulers';

/**
 * 叠加层（Canvas 2D）：选择框、控制点、框选矩形、拖动预览、标尺。
 * 全部按屏幕像素绘制，不随文档缩放改变线宽和控制点大小。
 * 这里只画，不算：场景由 CanvasController 组装。
 */
export interface OverlayScene {
  readonly viewport: Size;
  readonly devicePixelRatio: number;
  readonly rulers: boolean;
  /** 标尺原点（当前页左上角）在视口中的位置与 1 mm 的像素数 */
  readonly rulerOrigin: { readonly x: number; readonly y: number };
  readonly pxPerMm: number;
  /** 光标位置（标尺上画指示线） */
  readonly pointer: { readonly x: number; readonly y: number } | null;
  /** 可见页面的屏幕矩形（画描边） */
  readonly pages: readonly Rect[];
  /** 悬停对象 */
  readonly hover: Rect | null;
  /** 选中对象各自的矩形（多选时画细线） */
  readonly selected: readonly Rect[];
  /** 选区整体包围盒 */
  readonly selectionBox: Rect | null;
  /** 选框上画不画 8 个控制点。选区不能修改时不画（方案 2.3：只能叠加内容的页上的原有对象） */
  readonly handles: boolean;
  readonly marquee: Rect | null;
  readonly zoomRect: Rect | null;
  /** 拖动/缩放预览 */
  readonly preview: readonly Rect[];
  readonly rotation: number | null;
  /** 滚动条滑块（右侧、底部） */
  readonly scrollbars: { readonly vertical: Rect | null; readonly horizontal: Rect | null };
  /** 右下角的帧时间文字（可选） */
  readonly statsText: string | null;
}

const RULER = metrics.rulerSize;

export function drawOverlay(ctx: CanvasRenderingContext2D, scene: OverlayScene): void {
  const { viewport, devicePixelRatio: dpr } = scene;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, viewport.width, viewport.height);
  const offset = scene.rulers ? RULER : 0;

  ctx.save();
  ctx.translate(offset, offset);
  ctx.beginPath();
  ctx.rect(0, 0, viewport.width - offset, viewport.height - offset);
  ctx.clip();

  // 页面描边
  ctx.strokeStyle = palette.pageOutline;
  ctx.lineWidth = 1;
  for (const page of scene.pages) {
    ctx.strokeRect(
      crisp(page.x) - 0.5,
      crisp(page.y) - 0.5,
      Math.round(page.width) + 1,
      Math.round(page.height) + 1,
    );
  }

  // 悬停
  if (scene.hover) {
    ctx.strokeStyle = palette.accent;
    ctx.globalAlpha = 0.6;
    strokeCrisp(ctx, scene.hover);
    ctx.globalAlpha = 1;
  }

  // 多选时每个对象的细框
  if (scene.selected.length > 1) {
    ctx.strokeStyle = palette.accent;
    ctx.globalAlpha = 0.5;
    for (const r of scene.selected) strokeCrisp(ctx, r);
    ctx.globalAlpha = 1;
  }

  // 拖动/缩放预览：虚线
  if (scene.preview.length > 0) {
    ctx.strokeStyle = palette.accent;
    ctx.setLineDash([4, 3]);
    for (const r of scene.preview) strokeCrisp(ctx, r);
    ctx.setLineDash([]);
  }

  // 选择框 + 8 个控制点（《方案》8.3：1px 强调色、白底方形）
  if (scene.selectionBox) {
    const box = scene.selectionBox;
    ctx.save();
    if (scene.rotation !== null) {
      ctx.translate(box.x + box.width / 2, box.y + box.height / 2);
      ctx.rotate((scene.rotation * Math.PI) / 180);
      ctx.translate(-(box.x + box.width / 2), -(box.y + box.height / 2));
    }
    ctx.strokeStyle = palette.accent;
    ctx.lineWidth = metrics.selectionStroke;
    strokeCrisp(ctx, box);
    const size = metrics.handleSize;
    for (const handle of scene.handles ? handleCenters(box) : []) {
      const hx = Math.round(handle.center.x) - Math.floor(size / 2) - 0.5;
      const hy = Math.round(handle.center.y) - Math.floor(size / 2) - 0.5;
      ctx.fillStyle = palette.handleFill;
      ctx.fillRect(hx, hy, size, size);
      ctx.strokeRect(hx, hy, size, size);
    }
    ctx.restore();
  }

  // 框选矩形
  if (scene.marquee) {
    ctx.fillStyle = palette.accent;
    ctx.globalAlpha = 0.12;
    ctx.fillRect(scene.marquee.x, scene.marquee.y, scene.marquee.width, scene.marquee.height);
    ctx.globalAlpha = 1;
    ctx.strokeStyle = palette.accent;
    ctx.setLineDash([3, 2]);
    strokeCrisp(ctx, scene.marquee);
    ctx.setLineDash([]);
  }

  // 缩放工具矩形
  if (scene.zoomRect) {
    ctx.strokeStyle = palette.text;
    ctx.setLineDash([3, 2]);
    strokeCrisp(ctx, scene.zoomRect);
    ctx.setLineDash([]);
  }

  // 滚动条
  ctx.fillStyle = palette.textMuted;
  ctx.globalAlpha = 0.45;
  if (scene.scrollbars.vertical) roundRect(ctx, scene.scrollbars.vertical, 3);
  if (scene.scrollbars.horizontal) roundRect(ctx, scene.scrollbars.horizontal, 3);
  ctx.globalAlpha = 1;

  // 帧时间统计
  if (scene.statsText) {
    ctx.font = `${metrics.fontSizeSmall}px ${fontFamily}`;
    ctx.textBaseline = 'bottom';
    ctx.textAlign = 'right';
    const width = ctx.measureText(scene.statsText).width + 12;
    const x = viewport.width - offset - 8;
    const y = viewport.height - offset - 8;
    ctx.fillStyle = palette.shell;
    ctx.globalAlpha = 0.85;
    ctx.fillRect(x - width, y - 18, width, 18);
    ctx.globalAlpha = 1;
    ctx.fillStyle = palette.text;
    ctx.fillText(scene.statsText, x - 6, y - 3);
  }
  ctx.restore();

  if (scene.rulers) drawRulers(ctx, scene);
}

function drawRulers(ctx: CanvasRenderingContext2D, scene: OverlayScene): void {
  const { viewport } = scene;
  ctx.fillStyle = palette.panel;
  ctx.fillRect(0, 0, viewport.width, RULER);
  ctx.fillRect(0, 0, RULER, viewport.height);
  ctx.fillStyle = palette.separator;
  ctx.fillRect(0, RULER - 1, viewport.width, 1);
  ctx.fillRect(RULER - 1, 0, 1, viewport.height);

  ctx.font = `${metrics.fontSizeSmall - 2}px ${fontFamily}`;
  ctx.fillStyle = palette.textMuted;
  ctx.strokeStyle = palette.rulerTick;
  ctx.lineWidth = 1;

  // 水平标尺
  const h = rulerTicks(scene.rulerOrigin.x + RULER, viewport.width, scene.pxPerMm);
  ctx.save();
  ctx.beginPath();
  ctx.rect(RULER, 0, viewport.width - RULER, RULER);
  ctx.clip();
  ctx.beginPath();
  for (const pos of h.minor) {
    const x = crisp(pos) - 0.5;
    ctx.moveTo(x, RULER - 5);
    ctx.lineTo(x, RULER - 1);
  }
  for (const tick of h.major) {
    const x = crisp(tick.pos) - 0.5;
    ctx.moveTo(x, RULER - 10);
    ctx.lineTo(x, RULER - 1);
  }
  ctx.stroke();
  ctx.textAlign = 'left';
  ctx.textBaseline = 'top';
  for (const tick of h.major) ctx.fillText(tick.label, tick.pos + 3, 2);
  if (scene.pointer) {
    ctx.strokeStyle = palette.accent;
    const x = crisp(scene.pointer.x + RULER) - 0.5;
    ctx.beginPath();
    ctx.moveTo(x, 0);
    ctx.lineTo(x, RULER);
    ctx.stroke();
    ctx.strokeStyle = palette.rulerTick;
  }
  ctx.restore();

  // 垂直标尺
  const v = rulerTicks(scene.rulerOrigin.y + RULER, viewport.height, scene.pxPerMm);
  ctx.save();
  ctx.beginPath();
  ctx.rect(0, RULER, RULER, viewport.height - RULER);
  ctx.clip();
  ctx.beginPath();
  for (const pos of v.minor) {
    const y = crisp(pos) - 0.5;
    ctx.moveTo(RULER - 5, y);
    ctx.lineTo(RULER - 1, y);
  }
  for (const tick of v.major) {
    const y = crisp(tick.pos) - 0.5;
    ctx.moveTo(RULER - 10, y);
    ctx.lineTo(RULER - 1, y);
  }
  ctx.stroke();
  for (const tick of v.major) {
    ctx.save();
    ctx.translate(2, tick.pos + 3);
    ctx.rotate(-Math.PI / 2);
    ctx.textAlign = 'right';
    ctx.textBaseline = 'top';
    ctx.fillText(tick.label, 0, 0);
    ctx.restore();
  }
  if (scene.pointer) {
    ctx.strokeStyle = palette.accent;
    const y = crisp(scene.pointer.y + RULER) - 0.5;
    ctx.beginPath();
    ctx.moveTo(0, y);
    ctx.lineTo(RULER, y);
    ctx.stroke();
  }
  ctx.restore();

  // 左上角
  ctx.fillStyle = palette.panel;
  ctx.fillRect(0, 0, RULER, RULER);
}

function crisp(v: number): number {
  return Math.round(v) + 0.5;
}

function strokeCrisp(ctx: CanvasRenderingContext2D, r: Rect): void {
  const x = Math.round(r.x) + 0.5;
  const y = Math.round(r.y) + 0.5;
  ctx.strokeRect(x, y, Math.max(0, Math.round(r.width) - 1), Math.max(0, Math.round(r.height) - 1));
}

function roundRect(ctx: CanvasRenderingContext2D, r: Rect, radius: number): void {
  ctx.beginPath();
  ctx.roundRect(r.x, r.y, r.width, r.height, radius);
  ctx.fill();
}
