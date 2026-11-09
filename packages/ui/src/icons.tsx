import type { CSSProperties, ReactElement } from 'react';

import { metrics } from './tokens';

/**
 * 16px 线性图标（方案 8.3），路径取自界面图源文件 docs/images/ui-main.html，
 * 并补充了面板和菜单需要的几种。所有图标用 currentColor，跟随文字颜色。
 */
const glyphs = {
  select: <path d="M4 1.8v12l3.1-3.1 2.2 4.6 1.9-.9-2.2-4.5h4.3z" fill="currentColor" />,
  text: <path d="M3 3.2h10M8 3.2v9.8M6 13h4" stroke="currentColor" strokeWidth="1.5" fill="none" />,
  shape: (
    <rect
      x="2.5"
      y="3.5"
      width="11"
      height="9"
      rx="1"
      stroke="currentColor"
      strokeWidth="1.3"
      fill="none"
    />
  ),
  eyedropper: (
    <>
      <path d="M3.3 12.7l6.4-6.4" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" />
      <path d="M8.3 4.9l2.8 2.8" stroke="currentColor" strokeWidth="1.4" />
      <circle cx="11.9" cy="4.1" r="2.1" fill="currentColor" />
    </>
  ),
  crop: (
    <path d="M4.5 1.5v10h10M1.5 4.5h10v10" stroke="currentColor" strokeWidth="1.4" fill="none" />
  ),
  hand: (
    <path
      d="M5.2 8.6V3.8a1 1 0 0 1 2 0v3.7M7.2 7.2V2.9a1 1 0 0 1 2 0v4.3M9.2 7.2V3.6a1 1 0 0 1 2 0v4.2M11.2 8V5.4a1 1 0 0 1 2 0v4.4c0 2.7-1.9 4.7-4.5 4.7h-.6c-1.5 0-2.6-.7-3.4-1.9L2.9 9.8a1 1 0 0 1 1.6-1.1l.7.9"
      stroke="currentColor"
      strokeWidth="1.1"
      fill="none"
      strokeLinecap="round"
      strokeLinejoin="round"
    />
  ),
  zoom: (
    <>
      <circle cx="6.8" cy="6.8" r="4.3" stroke="currentColor" strokeWidth="1.4" fill="none" />
      <path d="M10 10l3.8 3.8" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />
    </>
  ),
  search: (
    <>
      <circle cx="7" cy="7" r="4" stroke="currentColor" strokeWidth="1.3" fill="none" />
      <path d="M10 10l3.5 3.5" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" />
    </>
  ),
  caret: <path d="M4.5 6.5L8 10l3.5-3.5" stroke="currentColor" strokeWidth="1.3" fill="none" />,
  caretRight: (
    <path d="M6.5 4.5L10 8l-3.5 3.5" stroke="currentColor" strokeWidth="1.3" fill="none" />
  ),
  check: (
    <path
      d="M3.5 8.2l2.8 2.8 6-6.2"
      stroke="currentColor"
      strokeWidth="1.8"
      fill="none"
      strokeLinecap="round"
      strokeLinejoin="round"
    />
  ),
  eye: (
    <>
      <path
        d="M1.5 8S3.9 4 8 4s6.5 4 6.5 4-2.4 4-6.5 4S1.5 8 1.5 8z"
        stroke="currentColor"
        strokeWidth="1.1"
        fill="none"
      />
      <circle cx="8" cy="8" r="1.8" fill="currentColor" />
    </>
  ),
  lock: (
    <>
      <rect x="4" y="7.2" width="8" height="6" rx="1" fill="currentColor" />
      <path
        d="M5.7 7.2V5.6a2.3 2.3 0 0 1 4.6 0v1.6"
        stroke="currentColor"
        strokeWidth="1.2"
        fill="none"
      />
    </>
  ),
  menu: <path d="M3 5h10M3 8h10M3 11h10" stroke="currentColor" strokeWidth="1.2" />,
  collapse: (
    <path
      d="M8.5 4.5L5 8l3.5 3.5M12.5 4.5L9 8l3.5 3.5"
      stroke="currentColor"
      strokeWidth="1.2"
      fill="none"
    />
  ),
  expand: (
    <path
      d="M7.5 4.5L11 8l-3.5 3.5M3.5 4.5L7 8l-3.5 3.5"
      stroke="currentColor"
      strokeWidth="1.2"
      fill="none"
    />
  ),
  gridView: <path d="M3 3h4v4H3zM9 3h4v4H9zM3 9h4v4H3zM9 9h4v4H9z" fill="currentColor" />,
  listView: <path d="M3 4h10M3 8h10M3 12h10" stroke="currentColor" strokeWidth="1.4" />,
  alignLeft: (
    <>
      <path d="M2.5 2v12" stroke="currentColor" strokeWidth="1.2" />
      <path d="M4.5 4h8v3h-8zM4.5 9h5v3h-5z" fill="currentColor" opacity=".75" />
    </>
  ),
  alignCenter: (
    <>
      <path d="M8 2v12" stroke="currentColor" strokeWidth="1.2" />
      <path d="M3.5 4h9v3h-9zM5 9h6v3H5z" fill="currentColor" opacity=".75" />
    </>
  ),
  alignRight: (
    <>
      <path d="M13.5 2v12" stroke="currentColor" strokeWidth="1.2" />
      <path d="M3.5 4h8v3h-8zM6.5 9h5v3h-5z" fill="currentColor" opacity=".75" />
    </>
  ),
  alignTop: (
    <>
      <path d="M2 2.5h12" stroke="currentColor" strokeWidth="1.2" />
      <path d="M4 4.5h3v8H4zM9 4.5h3v5H9z" fill="currentColor" opacity=".75" />
    </>
  ),
  alignMiddle: (
    <>
      <path d="M2 8h12" stroke="currentColor" strokeWidth="1.2" />
      <path d="M4 3.5h3v9H4zM9 5h3v6H9z" fill="currentColor" opacity=".75" />
    </>
  ),
  alignBottom: (
    <>
      <path d="M2 13.5h12" stroke="currentColor" strokeWidth="1.2" />
      <path d="M4 3.5h3v8H4zM9 6.5h3v5H9z" fill="currentColor" opacity=".75" />
    </>
  ),
  windowMinimize: <path d="M3 8h10" stroke="currentColor" strokeWidth="1" />,
  windowMaximize: (
    <rect x="3.5" y="3.5" width="9" height="9" stroke="currentColor" strokeWidth="1" fill="none" />
  ),
  windowClose: <path d="M3.5 3.5l9 9M12.5 3.5l-9 9" stroke="currentColor" strokeWidth="1" />,
  workspace: (
    <path d="M2.5 3.5h11v9h-11zM6.5 3.5v9" stroke="currentColor" strokeWidth="1.1" fill="none" />
  ),
  palette: (
    <>
      <circle cx="8" cy="8" r="5.5" stroke="currentColor" strokeWidth="1.2" fill="none" />
      <circle cx="6" cy="6" r="1" fill="currentColor" />
      <circle cx="10" cy="6" r="1" fill="currentColor" />
      <circle cx="5.6" cy="9.6" r="1" fill="currentColor" />
    </>
  ),
  swatches: (
    <path
      d="M2.5 2.5H7V7H2.5zM9 2.5h4.5V7H9zM2.5 9H7v4.5H2.5zM9 9h4.5v4.5H9z"
      stroke="currentColor"
      strokeWidth="1.1"
      fill="none"
    />
  ),
  navigator: (
    <>
      <rect
        x="1.5"
        y="3"
        width="13"
        height="10"
        rx="1"
        stroke="currentColor"
        strokeWidth="1.1"
        fill="none"
      />
      <rect
        x="5"
        y="5.5"
        width="5"
        height="4"
        stroke="currentColor"
        strokeWidth="1.3"
        fill="none"
      />
    </>
  ),
  info: (
    <>
      <circle cx="8" cy="8" r="6" stroke="currentColor" strokeWidth="1.2" fill="none" />
      <path d="M8 7v4.5" stroke="currentColor" strokeWidth="1.5" />
      <circle cx="8" cy="4.8" r=".9" fill="currentColor" />
    </>
  ),
  font: (
    <text
      x="8"
      y="12"
      textAnchor="middle"
      fontSize="10"
      fontFamily="Georgia, serif"
      fill="currentColor"
    >
      Aa
    </text>
  ),
  scan: (
    <>
      <path
        d="M2 5V2.5h2.5M11.5 2.5H14V5M14 11v2.5h-2.5M4.5 13.5H2V11"
        stroke="currentColor"
        strokeWidth="1.2"
        fill="none"
      />
      <path d="M5 6h6M5 8h6M5 10h4" stroke="currentColor" strokeWidth="1.1" />
    </>
  ),
  group: (
    <rect
      x="2.5"
      y="3.5"
      width="11"
      height="9"
      rx="1"
      stroke="currentColor"
      strokeWidth="1.1"
      fill="none"
      strokeDasharray="2 1.5"
    />
  ),
  trash: (
    <path
      d="M3.5 4.5h9M6.5 4.5V3h3v1.5M4.5 4.5l.7 9h5.6l.7-9"
      stroke="currentColor"
      strokeWidth="1.1"
      fill="none"
    />
  ),
  barcode: (
    <path
      d="M2 3v10M4 3v10M5.5 3v10M8 3v10M9.5 3v10M12 3v10M14 3v10"
      stroke="currentColor"
      strokeWidth="1"
    />
  ),
  properties: (
    <>
      <path d="M2.5 4.5h11M2.5 8h11M2.5 11.5h11" stroke="currentColor" strokeWidth="1.1" />
      <circle cx="6" cy="4.5" r="1.5" fill="currentColor" />
      <circle cx="10.5" cy="8" r="1.5" fill="currentColor" />
      <circle cx="5" cy="11.5" r="1.5" fill="currentColor" />
    </>
  ),
  layers: (
    <path
      d="M8 2.5l6 3-6 3-6-3zM2 9l6 3 6-3M2 12l6 3 6-3"
      stroke="currentColor"
      strokeWidth="1.1"
      fill="none"
      strokeLinejoin="round"
    />
  ),
  pages: (
    <>
      <path d="M3 2.5h6l3 3v8H3z" stroke="currentColor" strokeWidth="1.1" fill="none" />
      <path d="M9 2.5v3h3" stroke="currentColor" strokeWidth="1.1" fill="none" />
    </>
  ),
  history: (
    <>
      <circle cx="8" cy="8" r="5.5" stroke="currentColor" strokeWidth="1.2" fill="none" />
      <path d="M8 4.8V8l2.4 1.6" stroke="currentColor" strokeWidth="1.2" fill="none" />
    </>
  ),
  character: (
    <path
      d="M3 13l4-10h2l4 10M4.8 9.5h6.4"
      stroke="currentColor"
      strokeWidth="1.3"
      fill="none"
      strokeLinejoin="round"
    />
  ),
  paragraph: (
    <path
      d="M12.5 2.5H7a3 3 0 0 0 0 6h1.5v5M10.5 2.5v11"
      stroke="currentColor"
      strokeWidth="1.2"
      fill="none"
    />
  ),
  components: (
    <path
      d="M3 3h4v4H3zM9 3h4v4H9zM3 9h4v4H3zM9.5 11h3M11 9.5v3"
      stroke="currentColor"
      strokeWidth="1.2"
      fill="none"
    />
  ),
  close: <path d="M4 4l8 8M12 4l-8 8" stroke="currentColor" strokeWidth="1.2" />,
  more: (
    <path
      d="M3 8h.01M8 8h.01M13 8h.01"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
    />
  ),
  stats: <path d="M2.5 13.5l3.5-5 3 3 4.5-7" stroke="currentColor" strokeWidth="1.3" fill="none" />,
} satisfies Record<string, ReactElement>;

export type IconName = keyof typeof glyphs;

export const iconNames = Object.keys(glyphs) as readonly IconName[];

export interface IconProps {
  name: IconName;
  /** 像素尺寸，默认 16（方案 8.3） */
  size?: number;
  className?: string;
  style?: CSSProperties;
  /** 有语义时传入，否则对读屏软件隐藏 */
  title?: string;
}

export function Icon({ name, size = metrics.iconSize, className, style, title }: IconProps) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 16 16"
      className={className}
      style={{ display: 'block', flex: 'none', ...style }}
      aria-hidden={title ? undefined : true}
      role={title ? 'img' : undefined}
    >
      {title ? <title>{title}</title> : null}
      {glyphs[name]}
    </svg>
  );
}
