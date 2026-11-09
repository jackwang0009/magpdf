import { format, t } from '../i18n';
import { activeDocument, useEditorStore } from '../store/editorStore';
import { ptToMm } from '../geometry';
import { formatMs } from '../canvas/frameStats';
import { useShellCommands } from './shellContext';

/** 状态栏 22px：页码、页面尺寸、光标坐标（mm）、选区数量、帧时间（开发开关）。 */
export function StatusBar() {
  const commands = useShellCommands();
  const doc = useEditorStore(activeDocument);
  const pointer = useEditorStore((s) => s.pointer);
  const showFrameStats = useEditorStore((s) => s.showFrameStats);
  const stats = useEditorStore((s) => s.frameStats);
  const running = useEditorStore((s) => s.benchmarkRunning);

  const pageIndex = doc?.currentPage ?? 0;
  const page = doc?.snapshot.pages[pageIndex];
  const selected = doc?.selection ?? [];
  const objects = page?.objects.filter((o) => selected.includes(o.id)) ?? [];

  let selectionText = '';
  if (objects.length === 1) {
    const object = objects[0]!;
    if (object.type === 'component' && object.componentKind) {
      selectionText = format(t.status.selectionOneComponent, {
        kind: t.componentKinds[object.componentKind],
      });
    } else {
      selectionText = format(t.status.selectionOne, { type: t.objectTypes[object.type] });
    }
  } else if (objects.length > 1) {
    selectionText = format(t.status.selectionMany, { n: objects.length });
  }

  return (
    <footer className="mg-status">
      <span className="mg-status-zoom">
        {doc
          ? `${(doc.view.zoom * 100).toFixed(Math.round(doc.view.zoom * 100) === doc.view.zoom * 100 ? 0 : 1)}%`
          : '—'}
      </span>
      <span className="mg-status-sep" />
      <span>
        {doc && page
          ? `${format(t.status.page, { n: pageIndex + 1, total: doc.snapshot.pages.length })}  ·  ${format(
              t.status.pageSize,
              { w: ptToMm(page.width).toFixed(0), h: ptToMm(page.height).toFixed(0) },
            )}`
          : t.documents.none}
      </span>
      <span className="mg-status-grow" />
      {selectionText ? <span>{selectionText}</span> : null}
      {pointer ? (
        <span className="mg-status-pointer">
          {format(t.status.pointer, {
            x: ptToMm(pointer.x).toFixed(2),
            y: ptToMm(pointer.y).toFixed(2),
          })}
        </span>
      ) : null}
      <button
        type="button"
        className={`mg-status-btn${showFrameStats ? ' mg-on' : ''}`}
        onClick={() => commands.toggleFrameStats()}
        title={t.menuItems.frameStats}
      >
        {running
          ? t.status.benchmarkRunning
          : stats
            ? format(t.status.frameStats, {
                p50: formatMs(stats.intervalP50),
                p95: formatMs(stats.intervalP95),
                work95: formatMs(stats.workP95),
                n: stats.samples,
              })
            : t.status.frameStatsOff}
      </button>
      <button type="button" className="mg-status-btn" onClick={() => commands.runBenchmark()}>
        {t.menuItems.benchmark}
      </button>
    </footer>
  );
}
