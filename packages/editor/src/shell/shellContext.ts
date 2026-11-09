import { createContext, useContext } from 'react';
import type { CanvasController } from '../canvas/canvasController';
import type { CommandId } from '../tools/shortcuts';
import type { PanelId } from '../workspace/workspaceTypes';

/**
 * 外壳与面板共享的命令入口：菜单、快捷键、状态栏都通过它找当前文档的画布控制器。
 * 由 EditorShell 创建，Workbench 在控制器就绪时登记。
 */
export interface ShellCommands {
  /** 当前文档的画布；没有打开的文档时为 null */
  controller(): CanvasController | null;
  /** 执行快捷键命令，返回是否处理 */
  run(command: CommandId): boolean;
  openDocument(preset: 'blank' | 'contract' | 'billOfLading' | 'large'): void;
  closeActiveDocument(): void;
  /** 切换工作区（复位到内置布局） */
  applyWorkspace(id: 'basic' | 'freight'): void;
  /** 复位当前工作区到内置布局并清掉保存的版本 */
  resetWorkspace(): void;
  runBenchmark(): void;
  toggleFrameStats(): void;
  togglePanels(): void;
  setComponentsPanel(mode: 'expanded' | 'collapsed' | 'hidden'): void;
  /** 图标条里显示哪些面板（切换工作区时同步工作区记录的那组） */
  setCollapsedPanels(panels: readonly PanelId[]): void;
  /** 把图标条里的面板浮到右侧 */
  openPanel(id: PanelId): void;
  toggleRulers(): void;
}

export const ShellContext = createContext<ShellCommands | null>(null);

export function useShellCommands(): ShellCommands {
  const commands = useContext(ShellContext);
  if (!commands) throw new Error('ShellContext 未提供');
  return commands;
}
