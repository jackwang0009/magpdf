import type { IconName } from '@magpdf/ui';

import { format, t } from '../i18n';

/**
 * 工具标识与快捷键，照抄方案 8.4。
 * v1.0 工具栏只含 `v1_0` 为 true 的项；直接选择（A）、钢笔（P）在 P4，修复画笔（J）在 v1.1。
 */
export type ToolId =
  | 'select'
  | 'direct-select'
  | 'text'
  | 'pen'
  | 'shape'
  | 'heal'
  | 'eyedropper'
  | 'crop'
  | 'hand'
  | 'zoom';

export interface ToolSpec {
  readonly id: ToolId;
  readonly key: string;
  readonly v1_0: boolean;
  readonly icon: IconName;
  /** 工具栏里排在该工具之后是否画分隔线（界面图 8.2 的分组） */
  readonly separatorAfter?: boolean;
  /** 右下角的小三角：按住可切换同组工具（形状工具 Shift+U） */
  readonly hasVariants?: boolean;
}

export const tools: readonly ToolSpec[] = [
  { id: 'select', key: 'V', v1_0: true, icon: 'select', separatorAfter: true },
  { id: 'direct-select', key: 'A', v1_0: false, icon: 'select' },
  { id: 'text', key: 'T', v1_0: true, icon: 'text' },
  { id: 'pen', key: 'P', v1_0: false, icon: 'shape' },
  { id: 'shape', key: 'U', v1_0: true, icon: 'shape', separatorAfter: true, hasVariants: true },
  { id: 'heal', key: 'J', v1_0: false, icon: 'eyedropper' },
  { id: 'eyedropper', key: 'I', v1_0: true, icon: 'eyedropper' },
  { id: 'crop', key: 'C', v1_0: true, icon: 'crop', separatorAfter: true },
  { id: 'hand', key: 'H', v1_0: true, icon: 'hand' },
  { id: 'zoom', key: 'Z', v1_0: true, icon: 'zoom' },
];

/** v1.0 工具栏显示的工具 */
export const v1Tools: readonly ToolSpec[] = tools.filter((tool) => tool.v1_0);

/** 按快捷键查工具（不区分大小写）。 */
export function toolForKey(key: string): ToolSpec | undefined {
  const upper = key.toUpperCase();
  return tools.find((tool) => tool.key === upper);
}

export function toolSpec(id: ToolId): ToolSpec {
  const spec = tools.find((tool) => tool.id === id);
  if (!spec) throw new Error(`未知工具 ${id}`);
  return spec;
}

/** 工具名称（界面文字） */
export function toolLabel(id: ToolId): string {
  return t.tools[id];
}

/** 工具提示：`选择（V）` */
export function toolTooltip(id: ToolId): string {
  return format(t.toolTip, { label: toolLabel(id), key: toolSpec(id).key });
}
