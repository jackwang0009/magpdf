import type { CSSProperties, MouseEventHandler, ReactNode } from 'react';

import { Icon } from './icons';
import type { IconName } from './icons';
import './ui.css';

/*
 * 基础控件。P0 阶段只做展示和点击，数值输入的拖动改值、表达式、单位切换（方案 8.3）在 P1 做。
 * 样式在 ui.css 里，只引用 theme.ts 生成的 CSS 变量。
 */

export interface IconButtonProps {
  icon: IconName;
  /** 工具提示，同时作为无障碍名称 */
  label: string;
  active?: boolean;
  disabled?: boolean;
  /** 22px 的小号按钮，用于面板标题栏 */
  small?: boolean;
  onClick?: MouseEventHandler<HTMLButtonElement>;
  className?: string;
  style?: CSSProperties;
}

export function IconButton({
  icon,
  label,
  active = false,
  disabled = false,
  small = false,
  onClick,
  className,
  style,
}: IconButtonProps) {
  const classes = ['mg-ibtn'];
  if (small) classes.push('mg-ibtn-sm');
  if (active) classes.push('mg-on');
  if (className) classes.push(className);
  return (
    <button
      type="button"
      className={classes.join(' ')}
      title={label}
      aria-label={label}
      aria-pressed={active}
      disabled={disabled}
      onClick={onClick}
      style={style}
    >
      <Icon name={icon} />
    </button>
  );
}

export interface CheckboxProps {
  checked: boolean;
  label: ReactNode;
  onChange?: (checked: boolean) => void;
  disabled?: boolean;
}

export function Checkbox({ checked, label, onChange, disabled = false }: CheckboxProps) {
  return (
    <label className={`mg-ck${disabled ? ' mg-disabled' : ''}`}>
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange?.(e.target.checked)}
      />
      <span className={`mg-box${checked ? ' mg-on' : ''}`} aria-hidden>
        {checked ? <Icon name="check" size={11} /> : null}
      </span>
      {label}
    </label>
  );
}

export interface DropdownProps {
  value: ReactNode;
  /** 左侧图标，例如工作区菜单 */
  icon?: IconName;
  /** 占满所在行 */
  grow?: boolean;
  label?: string;
  onClick?: MouseEventHandler<HTMLButtonElement>;
  disabled?: boolean;
}

/** 下拉框外观；P0 只做按钮，点击由调用方决定弹出什么。 */
export function Dropdown({
  value,
  icon,
  grow = false,
  label,
  onClick,
  disabled = false,
}: DropdownProps) {
  return (
    <button
      type="button"
      className={`mg-dd${grow ? ' mg-grow' : ''}`}
      onClick={onClick}
      disabled={disabled}
      title={label}
      aria-label={label}
      aria-haspopup="menu"
    >
      {icon ? <Icon name={icon} /> : null}
      <span className="mg-dd-value">{value}</span>
      <Icon name="caret" size={12} className="mg-muted" />
    </button>
  );
}

export interface FieldProps {
  value: ReactNode;
  unit?: string;
  /** 等宽字体，用于条码内容等 */
  mono?: boolean;
  grow?: boolean;
  label?: string;
  minWidth?: number;
}

/** 数值/文本显示框。P0 只读。 */
export function Field({ value, unit, mono = false, grow = false, label, minWidth }: FieldProps) {
  const classes = ['mg-field'];
  if (mono) classes.push('mg-mono');
  if (grow) classes.push('mg-grow');
  return (
    <span
      className={classes.join(' ')}
      title={label}
      aria-label={label}
      style={minWidth !== undefined ? { minWidth } : undefined}
    >
      <span className="mg-field-value">{value}</span>
      {unit ? <span className="mg-field-unit">{unit}</span> : null}
    </span>
  );
}

export interface SectionHeaderProps {
  title: ReactNode;
  collapsed?: boolean;
  onToggle?: () => void;
}

/** 面板内的分组标题（如“变换”“组件参数”）。 */
export function SectionHeader({ title, collapsed = false, onToggle }: SectionHeaderProps) {
  return (
    <button type="button" className="mg-sect" onClick={onToggle} aria-expanded={!collapsed}>
      <Icon name={collapsed ? 'caretRight' : 'caret'} size={12} className="mg-muted" />
      {title}
    </button>
  );
}

export interface PanelTabsProps {
  tabs: readonly { id: string; label: ReactNode }[];
  activeId: string;
  onSelect?: (id: string) => void;
  tail?: ReactNode;
}

/** 面板标题栏的标签页（不在 dockview 里时使用，例如浮动的占位面板）。 */
export function PanelTabs({ tabs, activeId, onSelect, tail }: PanelTabsProps) {
  return (
    <div className="mg-ptabs" role="tablist">
      {tabs.map((tab) => (
        <button
          type="button"
          key={tab.id}
          role="tab"
          aria-selected={tab.id === activeId}
          className={`mg-ptab${tab.id === activeId ? ' mg-on' : ''}`}
          onClick={() => onSelect?.(tab.id)}
        >
          {tab.label}
        </button>
      ))}
      {tail ? <div className="mg-ptabs-tail">{tail}</div> : null}
    </div>
  );
}

export function VerticalSeparator() {
  return <span className="mg-vsep" aria-hidden />;
}
