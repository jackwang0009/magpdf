import { metrics } from '@magpdf/ui';
import { describe, expect, it } from 'vitest';
import { drawOverlay } from './overlay';
import type { OverlayScene } from './overlay';

/** 只记下画了什么的 2D 上下文。 */
function recordingContext() {
  const calls: string[] = [];
  const ctx = new Proxy(
    {},
    {
      get:
        (_target, name: string) =>
        (..._args: unknown[]) => {
          calls.push(name);
          return undefined;
        },
      set: () => true,
    },
  ) as unknown as CanvasRenderingContext2D;
  const count = (name: string) => calls.filter((call) => call === name).length;
  return { ctx, count };
}

function scene(overrides: Partial<OverlayScene>): OverlayScene {
  return {
    viewport: { width: 800, height: 600 },
    devicePixelRatio: 1,
    rulers: false,
    rulerOrigin: { x: 0, y: 0 },
    pxPerMm: 4,
    pointer: null,
    pages: [],
    hover: null,
    selected: [],
    selectionBox: null,
    handles: true,
    marquee: null,
    zoomRect: null,
    preview: [],
    rotation: null,
    scrollbars: { vertical: null, horizontal: null },
    statsText: null,
    ...overrides,
  };
}

describe('叠加层：选框', () => {
  const box = { x: 100, y: 100, width: 200, height: 80 };

  it('选区能修改时，选框上有 8 个白底控制点', () => {
    const { ctx, count } = recordingContext();
    drawOverlay(ctx, scene({ selectionBox: box }));
    expect(count('fillRect')).toBe(metrics.handleCount);
    // 选框自己一笔，每个控制点描一圈边
    expect(count('strokeRect')).toBe(1 + metrics.handleCount);
  });

  it('选区不能修改时（只能叠加内容的页），只画选框，不画控制点', () => {
    const { ctx, count } = recordingContext();
    drawOverlay(ctx, scene({ selectionBox: box, handles: false }));
    expect(count('fillRect')).toBe(0);
    expect(count('strokeRect')).toBe(1);
  });

  it('没有选区时什么都不画', () => {
    const { ctx, count } = recordingContext();
    drawOverlay(ctx, scene({}));
    expect(count('fillRect')).toBe(0);
    expect(count('strokeRect')).toBe(0);
  });
});
