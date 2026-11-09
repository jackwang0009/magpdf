import { createMockDocument } from '../document/mockDocument';
import type { DocumentSnapshot } from '../document/types';
import { t } from '../i18n';

/**
 * 菜单「文件 → 新建」的示例文档：P0 没有真实文件读写，用模拟文档代替，
 * 文档名后加序号以免与前一个重名。
 */
export type DocumentPreset = 'blank' | 'contract' | 'billOfLading' | 'large';

export const builtinCounts = {
  blank: 1,
  contract: 6,
  billOfLading: 1,
  large: 300,
} as const;

export function documentPreset(preset: DocumentPreset, sequence: number): DocumentSnapshot {
  const suffix = sequence > 1 ? ` (${sequence})` : '';
  switch (preset) {
    case 'blank':
      return createMockDocument({
        id: `blank-${sequence}`,
        name: `untitled-${sequence}.pdf`,
        pageCount: builtinCounts.blank,
        template: 'report',
      });
    case 'large':
      return createMockDocument({
        id: `large-${sequence}`,
        name: `large-${sequence}.pdf`,
        pageCount: builtinCounts.large,
        template: 'report',
      });
    case 'billOfLading':
      return createMockDocument({
        id: `bl-${sequence}`,
        name: `${t.mock.billOfLadingName.replace('.pdf', '')}${suffix}.pdf`,
        pageCount: builtinCounts.billOfLading,
        template: 'billOfLading',
      });
    case 'contract':
    default:
      return createMockDocument({
        id: `contract-${sequence}`,
        name: `${t.mock.contractName.replace('.pdf', '')}${suffix}.pdf`,
        pageCount: builtinCounts.contract,
        template: 'report',
      });
  }
}
