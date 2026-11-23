import type { Point } from '../geometry';
import { format, t } from '../i18n';
import { code128Bars } from './code128';
import type { PageObject } from './types';

/**
 * 能置入的组件（方案第 11 节）。P0 原型只有文本和 Code 128 两种，参数也只有内容和一个尺寸：
 * 文本是字号，条码是条高（pt）。引擎那边是 `magpdf-engine` 的 `component.rs`，两边的检查要一致。
 */
export type PlaceableComponent = 'text' | 'code128';

export const PLACEABLE_COMPONENTS: readonly PlaceableComponent[] = ['text', 'code128'];

export function isPlaceable(id: string): id is PlaceableComponent {
  return (PLACEABLE_COMPONENTS as readonly string[]).includes(id);
}

export interface ComponentValues {
  readonly content: string;
  /** 文本的字号，或条码的条高（pt） */
  readonly size: number;
}

/** 新置入时的参数，和引擎的默认值相同。 */
export const COMPONENT_DEFAULTS: Readonly<Record<PlaceableComponent, ComponentValues>> = {
  text: { content: 'Text', size: 12 },
  code128: { content: 'SAMPLE-0001', size: 36 },
};

const SIZE_RANGE: Readonly<Record<PlaceableComponent, readonly [number, number]>> = {
  text: [4, 200],
  code128: [10, 300],
};

const CODE128_MAX_LENGTH = 48;
const TEXT_MAX_LENGTH = 100;

/** 从组件面板拖到画布上时，`dataTransfer` 里放组件种类用的类型。 */
export const COMPONENT_DRAG_TYPE = 'application/x-magpdf-component';

export interface ComponentProblem {
  readonly key: keyof ComponentValues;
  readonly message: string;
}

/** 参数能不能生成；不能时给出出错的参数和可以直接显示的说明。 */
export function checkComponent(
  kind: PlaceableComponent,
  values: ComponentValues,
): ComponentProblem | null {
  const strings = t.componentChecks;
  if (values.content.length === 0) return { key: 'content', message: strings.empty };
  const chars = [...values.content];
  if (kind === 'text') {
    // 中文可以写：引擎会嵌一份系统字体的子集；只有一行，不能有控制字符
    if (chars.some((c) => /\p{Cc}/u.test(c))) {
      return { key: 'content', message: strings.textControl };
    }
    if (chars.length > TEXT_MAX_LENGTH) {
      return { key: 'content', message: format(strings.textTooLong, { n: TEXT_MAX_LENGTH }) };
    }
  } else {
    if (!/^[\x20-\x7e]*$/.test(values.content)) {
      return { key: 'content', message: strings.code128AsciiOnly };
    }
    if (chars.length > CODE128_MAX_LENGTH) {
      return { key: 'content', message: format(strings.tooLong, { n: CODE128_MAX_LENGTH }) };
    }
  }
  const [min, max] = SIZE_RANGE[kind];
  if (!Number.isFinite(values.size) || values.size < min || values.size > max) {
    return {
      key: 'size',
      message: format(strings.sizeRange, { what: sizeLabel(kind), min, max }),
    };
  }
  return null;
}

/** 组件在图层面板、属性面板里的名字。 */
export function componentName(kind: PlaceableComponent): string {
  return kind === 'text' ? t.properties.componentText : t.components.items.code128;
}

/**
 * 模拟文档里的组件对象：外观交给模拟渲染画，大小按参数估算。左上角在 `at`（页面坐标）。
 * 条码的模块宽 1 pt，两边各留 10 个模块的静区（模拟渲染就是这么画的）。
 */
export function mockComponentObject(
  id: string,
  kind: PlaceableComponent,
  values: ComponentValues,
  at: Point,
): PageObject {
  const base = {
    id,
    type: 'component' as const,
    name: `${componentName(kind)} · ${values.content}`,
    component: { kind, values },
    added: true,
  };
  if (kind === 'code128') {
    const width = code128Bars(values.content).modules;
    return {
      ...base,
      bbox: { x: at.x, y: at.y, width, height: values.size },
      appearance: {
        kind: 'barcode',
        content: values.content,
        moduleWidth: 1,
        barHeight: values.size,
        showText: false,
      },
    };
  }
  return {
    ...base,
    bbox: {
      x: at.x,
      y: at.y,
      width: Math.max(values.size, values.content.length * values.size * 0.55),
      height: values.size * 1.2,
    },
    appearance: { kind: 'text', text: values.content, fontSize: values.size, color: '#000000' },
  };
}

/** 尺寸这一项叫什么：文本是字号，条码是条高。 */
export function sizeLabel(kind: PlaceableComponent): string {
  return kind === 'text' ? t.properties.fontSize : t.properties.barHeight;
}
