/**
 * 编辑器核心：瓦片画布（WebGL2）、工具状态机（方案 8.4）、选择与变换、快捷键、工作区布局。
 * P0 的 V12 在这里实现外壳、瓦片画布和选择框原型。
 */

export { PROTOCOL_VERSION } from '@magpdf/protocol';

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
  readonly label: string;
  readonly v1_0: boolean;
}

export const tools: readonly ToolSpec[] = [
  { id: 'select', key: 'V', label: '选择', v1_0: true },
  { id: 'direct-select', key: 'A', label: '直接选择', v1_0: false },
  { id: 'text', key: 'T', label: '文字', v1_0: true },
  { id: 'pen', key: 'P', label: '钢笔', v1_0: false },
  { id: 'shape', key: 'U', label: '形状', v1_0: true },
  { id: 'heal', key: 'J', label: '修复画笔', v1_0: false },
  { id: 'eyedropper', key: 'I', label: '吸管', v1_0: true },
  { id: 'crop', key: 'C', label: '裁剪', v1_0: true },
  { id: 'hand', key: 'H', label: '抓手', v1_0: true },
  { id: 'zoom', key: 'Z', label: '缩放', v1_0: true },
];

/** 按快捷键查工具（不区分大小写）。 */
export function toolForKey(key: string): ToolSpec | undefined {
  const upper = key.toUpperCase();
  return tools.find((t) => t.key === upper);
}
