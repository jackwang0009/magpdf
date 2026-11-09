import { mmToPt } from '../geometry';
import type { Rect } from '../geometry';
import { format, t } from '../i18n';
import { code128Bars } from './code128';
import { A4 } from './types';
import type { DocumentSnapshot, PageObject, PageSnapshot } from './types';

/**
 * 模拟文档：引擎接入（V06）之前，界面用它充当只读快照。
 * 两种模板：`report` 是“像文档的页面”（标题、几段灰色文字条、图和表格），用于 300 页的性能测试；
 * `billOfLading` 按界面图 8.2 里的提单摆放对象，用于核对外观。
 */
export type MockTemplate = 'report' | 'billOfLading';

export interface MockDocumentOptions {
  readonly id: string;
  readonly name: string;
  readonly pageCount: number;
  readonly template: MockTemplate;
  readonly dirty?: boolean;
}

/** 可复现的伪随机数（mulberry32），同一页每次生成的内容一致。 */
export function seededRandom(seed: number): () => number {
  let s = seed >>> 0;
  return () => {
    s = (s + 0x6d2b79f5) >>> 0;
    let x = Math.imul(s ^ (s >>> 15), 1 | s);
    x = (x + Math.imul(x ^ (x >>> 7), 61 | x)) ^ x;
    return ((x ^ (x >>> 14)) >>> 0) / 4294967296;
  };
}

export function createMockDocument(options: MockDocumentOptions): DocumentSnapshot {
  const pages: PageSnapshot[] = [];
  for (let i = 0; i < options.pageCount; i++) {
    pages.push(
      options.template === 'billOfLading'
        ? createBillOfLadingPage(i, options.pageCount)
        : createReportPage(i, options.pageCount),
    );
  }
  return {
    id: options.id,
    name: options.name,
    dirty: options.dirty ?? false,
    version: 0,
    pages,
  };
}

const INK = '#2a2a2a';
const TEXT_GRAY = '#8a8a8a';
const TITLE_GRAY = '#4a4a4a';

function createReportPage(index: number, pageCount: number): PageSnapshot {
  const rnd = seededRandom(index * 7919 + 17);
  const objects: PageObject[] = [];
  const margin = mmToPt(20);
  const contentWidth = A4.width - margin * 2;
  const names = t.mock.objectNames;
  let n = 0;
  const id = () => `p${index}-o${n++}`;

  // 标题
  objects.push({
    id: id(),
    type: 'text',
    name: names.title,
    bbox: { x: margin, y: margin, width: contentWidth * (0.45 + rnd() * 0.3), height: 20 },
    appearance: { kind: 'textBars', lines: 1, lineHeight: 20, barHeight: 12, color: TITLE_GRAY },
  });
  // 页眉线
  objects.push({
    id: id(),
    type: 'path',
    name: format(names.line, { n: 1 }),
    bbox: { x: margin, y: margin + 28, width: contentWidth, height: 0.8 },
    appearance: { kind: 'rect', fill: INK },
  });

  let y = margin + 44;
  const bottom = A4.height - margin - 30;
  let paragraph = 1;
  let figure = 1;
  const withFigure = index % 3 === 1;
  const withTable = index % 4 === 2;
  while (y < bottom - 40) {
    const roll = rnd();
    if (withFigure && roll < 0.18 && y + 140 < bottom) {
      const width = contentWidth * (0.5 + rnd() * 0.4);
      objects.push({
        id: id(),
        type: 'image',
        name: format(names.figure, { n: figure++ }),
        bbox: { x: margin + (contentWidth - width) / 2, y, width, height: 120 },
        appearance: { kind: 'image', color: '#8fa7c2' },
      });
      y += 136;
      continue;
    }
    if (withTable && roll < 0.3 && y + 100 < bottom) {
      objects.push({
        id: id(),
        type: 'path',
        name: names.table,
        bbox: { x: margin, y, width: contentWidth, height: 84 },
        appearance: { kind: 'rect', stroke: INK, strokeWidth: 0.8 },
      });
      for (let r = 1; r < 4; r++) {
        objects.push({
          id: id(),
          type: 'path',
          name: format(names.line, { n: r + 1 }),
          bbox: { x: margin, y: y + r * 21, width: contentWidth, height: 0.6 },
          appearance: { kind: 'rect', fill: INK },
        });
      }
      y += 100;
      continue;
    }
    const lines = 2 + Math.floor(rnd() * 6);
    const lineHeight = 14;
    const height = lines * lineHeight;
    if (y + height > bottom) break;
    const widths: number[] = [];
    for (let k = 0; k < lines; k++) widths.push(k === lines - 1 ? 0.3 + rnd() * 0.6 : 1);
    objects.push({
      id: id(),
      type: 'text',
      name: format(names.paragraph, { n: paragraph++ }),
      bbox: { x: margin, y, width: contentWidth, height },
      appearance: { kind: 'textBars', lines, lineHeight, barHeight: 6.5, color: TEXT_GRAY, widths },
    });
    y += height + 12;
  }

  // 页脚：页码
  objects.push({
    id: id(),
    type: 'text',
    name: names.footer,
    bbox: { x: A4.width / 2 - 30, y: A4.height - margin, width: 60, height: 12 },
    appearance: {
      kind: 'text',
      text: `${index + 1} / ${pageCount}`,
      fontSize: 9,
      color: '#555555',
      align: 'center',
    },
  });

  return { index, width: A4.width, height: A4.height, objects };
}

const BL_MODULE_MM = 0.375;

/** 界面图 8.2 里的提单：坐标取自 docs/images/ui-main.html（mm）。 */
function createBillOfLadingPage(index: number, _pageCount: number): PageSnapshot {
  const names = t.mock.objectNames;
  const objects: PageObject[] = [];
  let n = 0;
  const id = () => `p${index}-o${n++}`;
  const mm = mmToPt;
  const box = (x: number, y: number, w: number, h: number): Rect => ({
    x: mm(x),
    y: mm(y),
    width: mm(w),
    height: mm(h),
  });
  const label = (name: string, x: number, y: number, w: number, text: string) =>
    objects.push({
      id: id(),
      type: 'text',
      name,
      bbox: box(x, y, w, 2.6),
      appearance: { kind: 'text', text, fontSize: mm(2.15), color: '#555555' },
    });
  const block = (name: string, x: number, y: number, w: number, lines: string[]) =>
    objects.push({
      id: id(),
      type: 'text',
      name,
      bbox: box(x, y, w, 4 * lines.length),
      appearance: {
        kind: 'text',
        text: lines.join('\n'),
        fontSize: mm(2.65),
        color: '#151515',
        lineHeight: mm(4),
      },
    });
  const line = (x: number, y: number, w: number, h: number) =>
    objects.push({
      id: id(),
      type: 'path',
      name: format(names.line, { n: n }),
      bbox: box(x, y, w, h),
      appearance: { kind: 'rect', fill: INK },
    });

  // 抬头
  objects.push({
    id: id(),
    type: 'image',
    name: names.logo,
    bbox: box(12, 13, 15, 15),
    locked: true,
    appearance: { kind: 'image', color: '#1d4f91', label: 'S' },
  });
  objects.push({
    id: id(),
    type: 'text',
    name: names.heading,
    bbox: box(30, 13.2, 78, 5),
    appearance: {
      kind: 'text',
      text: 'SAMPLE LOGISTICS CO., LTD.',
      fontSize: mm(4.2),
      color: '#151515',
      bold: true,
    },
  });
  objects.push({
    id: id(),
    type: 'text',
    name: names.heading,
    bbox: box(30, 19, 40, 4),
    appearance: { kind: 'text', text: '示例国际物流有限公司', fontSize: mm(3.3), color: '#222222' },
  });
  objects.push({
    id: id(),
    type: 'text',
    name: names.heading,
    bbox: box(30, 24.4, 80, 3),
    appearance: {
      kind: 'text',
      text: '18F, SAMPLE TOWER, 100 EXAMPLE ROAD, SHANGHAI, CHINA',
      fontSize: mm(2.3),
      color: '#555555',
    },
  });
  objects.push({
    id: id(),
    type: 'text',
    name: 'BILL OF LADING',
    bbox: box(128, 12.2, 70, 7.5),
    appearance: {
      kind: 'text',
      text: 'BILL OF LADING',
      fontSize: mm(6.2),
      color: '#151515',
      bold: true,
      align: 'right',
    },
  });
  objects.push({
    id: id(),
    type: 'text',
    name: names.heading,
    bbox: box(128, 20.6, 70, 3),
    appearance: {
      kind: 'text',
      text: 'FOR COMBINED TRANSPORT OR PORT TO PORT SHIPMENT',
      fontSize: mm(2.15),
      color: '#555555',
      align: 'right',
    },
  });
  objects.push({
    id: id(),
    type: 'text',
    name: names.heading,
    bbox: box(128, 24.6, 70, 3.5),
    appearance: {
      kind: 'text',
      text: 'NON-NEGOTIABLE COPY',
      fontSize: mm(2.6),
      color: '#333333',
      bold: true,
      align: 'right',
    },
  });

  // 表格框线
  objects.push({
    id: id(),
    type: 'path',
    name: names.frame,
    bbox: box(12, 34, 186, 202),
    locked: true,
    appearance: { kind: 'rect', stroke: INK, strokeWidth: mm(0.35) },
  });
  for (const y of [58, 82, 98, 110, 117, 180, 188, 206]) line(12, y, 186, 0.3);
  const verticals: [number, number, number][] = [
    [105, 34, 76],
    [58.5, 98, 12],
    [151.5, 98, 12],
    [50, 110, 70],
    [80, 110, 70],
    [102, 110, 70],
    [158, 110, 70],
    [179, 110, 70],
    [105, 188, 48],
    [151.5, 188, 18],
  ];
  for (const [x, y, h] of verticals) line(x, y, 0.3, h);

  // 栏目
  label('Shipper', 13.5, 35.3, 20, 'Shipper');
  block(names.shipper, 13.5, 39, 85, [
    'NINGBO SAMPLE TRADING CO., LTD.',
    'NO. 88 SAMPLE ROAD, YINZHOU DISTRICT,',
    'NINGBO, ZHEJIANG 315100, CHINA',
    'TEL: +86 574 0000 0000',
  ]);
  label('B/L No.', 106.5, 35.3, 20, 'B/L No.');
  label('Consignee', 13.5, 59.3, 20, 'Consignee');
  block(names.consignee, 13.5, 63, 85, [
    'SAMPLE IMPORTS LLC',
    '1200 EXAMPLE BLVD, SUITE 300',
    'ONTARIO, CA 91761, UNITED STATES',
    'TEL: +1 909 000 0000',
  ]);
  label('Delivery Agent', 106.5, 59.3, 30, 'Delivery Agent');
  block('Delivery Agent', 106.5, 63, 85, [
    'SAMPLE AGENCY INC.',
    '500 HARBOR DRIVE, LONG BEACH, CA 90802',
    'TEL: +1 562 000 0000',
  ]);
  label('Notify Party', 13.5, 83.3, 25, 'Notify Party');
  block(names.notify, 13.5, 87, 60, ['SAME AS CONSIGNEE']);
  label('Place of Receipt', 106.5, 83.3, 30, 'Place of Receipt');
  block('Place of Receipt', 106.5, 87, 60, ['NINGBO CY, CHINA']);
  label('Ocean Vessel / Voy. No.', 13.5, 99.2, 40, 'Ocean Vessel / Voy. No.');
  block('Ocean Vessel', 13.5, 103, 40, ['SAMPLE STAR V.2611E']);
  label('Port of Loading', 60, 99.2, 30, 'Port of Loading');
  block('Port of Loading', 60, 103, 40, ['NINGBO, CHINA']);
  label('Port of Discharge', 106.5, 99.2, 30, 'Port of Discharge');
  block('Port of Discharge', 106.5, 103, 40, ['LOS ANGELES, CA, USA']);
  label('Place of Delivery', 153, 99.2, 30, 'Place of Delivery');
  block('Place of Delivery', 153, 103, 40, ['ONTARIO, CA, USA']);

  const headers: [string, number, number][] = [
    ['Container No. / Seal No.', 12, 38],
    ['Marks & Nos.', 50, 30],
    ['No. of Pkgs.', 80, 22],
    ['Description of Goods', 102, 56],
    ['Gross Weight', 158, 21],
    ['Measurement', 179, 19],
  ];
  for (const [text, x, w] of headers) {
    objects.push({
      id: id(),
      type: 'text',
      name: text,
      bbox: box(x, 111.9, w, 3),
      appearance: {
        kind: 'text',
        text,
        fontSize: mm(2.15),
        color: '#333333',
        bold: true,
        align: 'center',
      },
    });
  }
  block(names.goods, 13.5, 119.5, 35, ['SMPU1234561', 'SEAL: SL260118', "1 X 40'HQ", 'CY-CY']);
  block(names.goods, 51.5, 119.5, 27, ['SAMPLE', 'ONTARIO', 'C/NO. 1-20', 'MADE IN CHINA']);
  block(names.goods, 81.5, 119.5, 20, ['20', 'CARTONS']);
  block(names.goods, 103.5, 119.5, 54, [
    "SHIPPER'S LOAD, COUNT AND SEAL",
    'SAID TO CONTAIN:',
    'LED DESK LAMPS',
    'HS CODE: 9405.21',
    '',
    'FREIGHT COLLECT',
  ]);
  block(names.goods, 159.5, 119.5, 19, ['260.00', 'KGS']);
  block(names.goods, 180.5, 119.5, 17, ['2.150', 'CBM']);

  label(
    'Total Number of Packages (in words):',
    13.5,
    182.6,
    48,
    'Total Number of Packages (in words):',
  );
  const amountText = 'SAY TOTAL TWENTY (20) CARTONS ONLY.';
  objects.push({
    id: id(),
    type: 'component',
    componentKind: 'amountWords',
    name: format(names.amount, { text: 'SAY TOTAL TWENTY…' }),
    bbox: box(62, 181.9, 80, 4),
    appearance: {
      kind: 'text',
      text: amountText,
      fontSize: mm(2.65),
      color: '#151515',
      bold: true,
    },
  });
  label('Freight & Charges', 13.5, 189.3, 30, 'Freight & Charges');
  block('Freight & Charges', 13.5, 193, 60, ['FREIGHT COLLECT AS ARRANGED']);
  label('Place and Date of Issue', 106.5, 189.3, 40, 'Place and Date of Issue');
  block('Place and Date of Issue', 106.5, 193, 44, ['NINGBO, NOV. 06, 2026']);
  label('No. of Original B(s)/L', 153, 189.3, 40, 'No. of Original B(s)/L');
  block('No. of Original B(s)/L', 153, 193, 40, ['ZERO (0)']);
  label('Laden on Board the Vessel', 13.5, 207.3, 40, 'Laden on Board the Vessel');
  block('Laden on Board', 13.5, 211, 60, ['DATE: NOV. 06, 2026', 'BY: SAMPLE STAR V.2611E']);
  label('Signed for the Carrier', 106.5, 207.3, 40, 'Signed for the Carrier');
  block('Signed for the Carrier', 106.5, 211, 60, ['SAMPLE LOGISTICS CO., LTD.', 'AS CARRIER']);

  // 文字章
  objects.push({
    id: id(),
    type: 'component',
    componentKind: 'textStamp',
    name: format(names.stamp, { text: 'TELEX RELEASED' }),
    bbox: box(108, 147, 74, 14),
    appearance: { kind: 'stamp', text: 'TELEX RELEASED', color: '#c62828', angle: -8 },
  });

  // 条码组件（界面图中选中的对象）
  const content = 'MGSHA26110015';
  const bars = code128Bars(content);
  objects.push({
    id: id(),
    type: 'component',
    componentKind: 'barcode',
    name: format(names.barcode, { content }),
    bbox: box(120, 39.5, bars.modules * BL_MODULE_MM, 16),
    appearance: {
      kind: 'barcode',
      content,
      moduleWidth: mm(BL_MODULE_MM),
      barHeight: mm(12),
      showText: true,
    },
  });

  return { index, width: A4.width, height: A4.height, objects };
}
