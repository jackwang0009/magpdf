import { useState } from 'react';
import type { KeyboardEvent } from 'react';
import { mmToPt, ptToMm } from '../geometry';

export interface MmFieldProps {
  /** 现在的值（pt），显示成 mm、两位小数 */
  readonly value: number;
  readonly label: string;
  readonly disabled?: boolean;
  /** 值必须大于 0（宽、高） */
  readonly positive?: boolean;
  /** 改好的值（pt）。回车或离开输入框时调；没变、写错了不调 */
  readonly onCommit: (pt: number) => void;
}

/**
 * 以 mm 为单位的数值输入框（属性面板的 X、Y、宽、高）。回车或离开输入框时生效，Esc 恢复原值；
 * 写的不是数（或者要求大于 0 而不是）就恢复原值。外面的值变了（引擎改完、撤销）输入框跟着换：
 * 调用方用 `key` 带上值。
 */
export function MmField({
  value,
  label,
  disabled = false,
  positive = false,
  onCommit,
}: MmFieldProps) {
  const shown = ptToMm(value).toFixed(2);
  const [draft, setDraft] = useState(shown);
  const commit = () => {
    const mm = Number(draft);
    if (draft.trim() === '' || !Number.isFinite(mm) || (positive && mm <= 0)) {
      setDraft(shown);
      return;
    }
    // 显示的是两位小数：差不到半个最小单位算没改
    if (Math.abs(mm - Number(shown)) < 0.005) return;
    onCommit(mmToPt(mm));
  };
  const onKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === 'Enter') commit();
    if (event.key === 'Escape') setDraft(shown);
  };
  return (
    <span className="mg-mm-field">
      <input
        className="mg-input"
        aria-label={label}
        value={draft}
        disabled={disabled}
        inputMode="decimal"
        onChange={(event) => setDraft(event.target.value)}
        onBlur={commit}
        onKeyDown={onKeyDown}
      />
      <span className="mg-field-unit">mm</span>
    </span>
  );
}
