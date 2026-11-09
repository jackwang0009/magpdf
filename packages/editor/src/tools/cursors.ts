/**
 * 浏览器没有内置的旋转光标，用内联 SVG 画一个（白底黑边的弧形箭头），热点在中心。
 */
const ROTATE_SVG =
  '<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" viewBox="0 0 20 20">' +
  '<path d="M10 3.5a6.5 6.5 0 1 1-6.1 4.3" fill="none" stroke="#fff" stroke-width="3.5" stroke-linecap="round"/>' +
  '<path d="M10 3.5a6.5 6.5 0 1 1-6.1 4.3" fill="none" stroke="#000" stroke-width="1.5" stroke-linecap="round"/>' +
  '<path d="M3 3.5v4.8h4.8" fill="none" stroke="#fff" stroke-width="3.5" stroke-linecap="round" stroke-linejoin="round"/>' +
  '<path d="M3 3.5v4.8h4.8" fill="none" stroke="#000" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>' +
  '</svg>';

export const ROTATE_CURSOR = `url("data:image/svg+xml;utf8,${encodeURIComponent(ROTATE_SVG)}") 10 10, auto`;

/** 缩放工具光标：放大 / 缩小 */
const ZOOM_SVG = (minus: boolean) =>
  '<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" viewBox="0 0 20 20">' +
  '<circle cx="8" cy="8" r="5.5" fill="rgba(255,255,255,0.85)" stroke="#000" stroke-width="1.5"/>' +
  '<path d="M12.2 12.2 17 17" stroke="#000" stroke-width="2" stroke-linecap="round"/>' +
  '<path d="M5.5 8h5" stroke="#000" stroke-width="1.5"/>' +
  (minus ? '' : '<path d="M8 5.5v5" stroke="#000" stroke-width="1.5"/>') +
  '</svg>';

export const ZOOM_IN_CURSOR = `url("data:image/svg+xml;utf8,${encodeURIComponent(ZOOM_SVG(false))}") 8 8, zoom-in`;
export const ZOOM_OUT_CURSOR = `url("data:image/svg+xml;utf8,${encodeURIComponent(ZOOM_SVG(true))}") 8 8, zoom-out`;
