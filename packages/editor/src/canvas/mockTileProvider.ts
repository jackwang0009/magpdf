import { code128Bars } from '../document/code128';
import type { DocumentSnapshot, PageObject, PageSnapshot } from '../document/types';
import { inflateRect, rectsIntersect } from '../geometry';
import { AbortError } from './tileProvider';
import type { TileProvider, TileRequest, TileResult } from './tileProvider';

/**
 * 模拟瓦片来源：用 OffscreenCanvas 2D 按文档快照里的对象画出“像文档的页面”，
 * 并用可配置的异步延迟模拟引擎渲染耗时。V06 接入引擎后由共享内存实现替换，画布代码不改。
 */
export interface MockTileProviderOptions {
  /** 取当前文档快照（对象移动后重绘要读到新位置） */
  readonly getDocument: () => DocumentSnapshot | undefined;
  /** 每块瓦片的固定延迟（ms），默认 8 */
  readonly delayMs?: number;
  /** 在固定延迟上再加 0–jitterMs 的随机延迟，默认 6 */
  readonly jitterMs?: number;
  /** 并发上限，默认 2（模拟引擎线程有限） */
  readonly concurrency?: number;
}

const PAGE_FONT = 'Arial, Helvetica, "Microsoft YaHei", sans-serif';

export class MockTileProvider implements TileProvider {
  readonly concurrency: number;
  private readonly delayMs: number;
  private readonly jitterMs: number;
  private readonly getDocument: () => DocumentSnapshot | undefined;
  /** 统计：已渲染的瓦片数与累计绘制耗时（ms），写进报告用 */
  readonly stats = { rendered: 0, drawMs: 0 };

  constructor(options: MockTileProviderOptions) {
    this.getDocument = options.getDocument;
    this.delayMs = options.delayMs ?? 8;
    this.jitterMs = options.jitterMs ?? 6;
    this.concurrency = options.concurrency ?? 2;
  }

  async renderTile(request: TileRequest): Promise<TileResult> {
    if (request.signal.aborted) throw new AbortError();
    await sleep(this.delayMs + Math.random() * this.jitterMs, request.signal);
    const document = this.getDocument();
    const page = document?.pages[request.page];
    if (!page) throw new Error(`模拟文档没有第 ${request.page + 1} 页`);
    const { width, height } = request.pixelRect;
    const started = performance.now();
    const canvas = new OffscreenCanvas(width, height);
    const ctx = canvas.getContext('2d');
    if (!ctx) throw new Error('OffscreenCanvas 2D 不可用');
    drawTile(ctx, page, request);
    this.stats.rendered++;
    this.stats.drawMs += performance.now() - started;
    return { width, height, pixels: canvas.transferToImageBitmap() };
  }
}

function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      signal.removeEventListener('abort', onAbort);
      resolve();
    }, ms);
    const onAbort = () => {
      clearTimeout(timer);
      reject(new AbortError());
    };
    signal.addEventListener('abort', onAbort, { once: true });
  });
}

/** 把一块瓦片画进 2D 上下文：白底、页面描边、与瓦片相交的对象。 */
export function drawTile(
  ctx: OffscreenCanvasRenderingContext2D,
  page: PageSnapshot,
  request: TileRequest,
): void {
  const { pxPerPt, pixelRect } = request;
  ctx.fillStyle = '#ffffff';
  ctx.fillRect(0, 0, pixelRect.width, pixelRect.height);
  ctx.setTransform(pxPerPt, 0, 0, pxPerPt, -pixelRect.x, -pixelRect.y);
  // 旋转的文字章会略微超出包围盒，判断相交时把瓦片范围放大一点
  const probe = inflateRect(request.pageRect, 12);
  for (const object of page.objects) {
    if (!rectsIntersect(object.bbox, probe)) continue;
    drawObject(ctx, object, pxPerPt);
  }
  // 页面边缘 1 个设备像素的描边，便于看清页边
  ctx.strokeStyle = 'rgba(0, 0, 0, 0.25)';
  ctx.lineWidth = 1 / pxPerPt;
  ctx.strokeRect(0.5 / pxPerPt, 0.5 / pxPerPt, page.width - 1 / pxPerPt, page.height - 1 / pxPerPt);
}

function drawObject(
  ctx: OffscreenCanvasRenderingContext2D,
  object: PageObject,
  pxPerPt: number,
): void {
  const a = object.appearance;
  const b = object.bbox;
  switch (a.kind) {
    case 'textBars': {
      ctx.fillStyle = a.color;
      for (let i = 0; i < a.lines; i++) {
        const w = b.width * (a.widths?.[i] ?? 1);
        const y = b.y + i * a.lineHeight + (a.lineHeight - a.barHeight) / 2;
        ctx.fillRect(b.x, y, w, a.barHeight);
      }
      break;
    }
    case 'text': {
      ctx.fillStyle = a.color;
      ctx.font = `${a.bold ? 'bold ' : ''}${a.fontSize}px ${PAGE_FONT}`;
      ctx.textBaseline = 'top';
      const lineHeight = a.lineHeight ?? a.fontSize * 1.2;
      const lines = a.text.split('\n');
      const align = a.align ?? 'left';
      ctx.textAlign = align;
      const x = align === 'right' ? b.x + b.width : align === 'center' ? b.x + b.width / 2 : b.x;
      for (let i = 0; i < lines.length; i++) {
        ctx.fillText(lines[i]!, x, b.y + i * lineHeight + (lineHeight - a.fontSize) / 2);
      }
      break;
    }
    case 'rect': {
      if (a.fill) {
        ctx.fillStyle = a.fill;
        ctx.fillRect(b.x, b.y, b.width, b.height);
      }
      if (a.stroke) {
        const w = a.strokeWidth ?? 1;
        ctx.strokeStyle = a.stroke;
        ctx.lineWidth = w;
        ctx.strokeRect(b.x + w / 2, b.y + w / 2, b.width - w, b.height - w);
      }
      break;
    }
    case 'image': {
      ctx.fillStyle = a.color;
      ctx.fillRect(b.x, b.y, b.width, b.height);
      if (a.label) {
        ctx.fillStyle = '#ffffff';
        ctx.font = `bold ${b.height * 0.66}px ${PAGE_FONT}`;
        ctx.textAlign = 'center';
        ctx.textBaseline = 'middle';
        ctx.fillText(a.label, b.x + b.width / 2, b.y + b.height / 2 + b.height * 0.04);
      } else {
        // 占位图：浅色对角线
        ctx.strokeStyle = 'rgba(255, 255, 255, 0.55)';
        ctx.lineWidth = 1 / pxPerPt;
        ctx.beginPath();
        ctx.moveTo(b.x, b.y);
        ctx.lineTo(b.x + b.width, b.y + b.height);
        ctx.moveTo(b.x + b.width, b.y);
        ctx.lineTo(b.x, b.y + b.height);
        ctx.stroke();
      }
      break;
    }
    case 'barcode': {
      const bars = code128Bars(a.content);
      const unit = b.width / bars.modules;
      ctx.fillStyle = '#000000';
      for (const [start, w] of bars.bars) {
        ctx.fillRect(b.x + start * unit, b.y, w * unit, a.barHeight);
      }
      if (a.showText) {
        const fontSize = Math.min(8.2, Math.max(4, (b.height - a.barHeight) * 0.9));
        ctx.font = `${fontSize}px ${PAGE_FONT}`;
        ctx.textAlign = 'center';
        ctx.textBaseline = 'top';
        ctx.letterSpacing = `${fontSize * 0.16}px`;
        ctx.fillText(a.content, b.x + b.width / 2, b.y + a.barHeight + 1.5);
        ctx.letterSpacing = '0px';
      }
      break;
    }
    case 'stamp': {
      ctx.save();
      ctx.translate(b.x + b.width / 2, b.y + b.height / 2);
      ctx.rotate((a.angle * Math.PI) / 180);
      ctx.globalAlpha = 0.9;
      ctx.globalCompositeOperation = 'multiply';
      const w = b.width * 0.96;
      const h = b.height * 0.8;
      ctx.strokeStyle = a.color;
      ctx.lineWidth = 1.7;
      ctx.strokeRect(-w / 2, -h / 2, w, h);
      ctx.lineWidth = 0.8;
      ctx.strokeRect(-w / 2 + 2.6, -h / 2 + 2.6, w - 5.2, h - 5.2);
      ctx.fillStyle = a.color;
      ctx.font = `900 ${h * 0.5}px "Arial Black", ${PAGE_FONT}`;
      ctx.textAlign = 'center';
      ctx.textBaseline = 'middle';
      ctx.letterSpacing = `${h * 0.06}px`;
      ctx.fillText(a.text, 0, h * 0.02);
      ctx.restore();
      break;
    }
  }
}
