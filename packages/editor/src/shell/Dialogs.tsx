import { useState } from 'react';
import { t } from '../i18n';
import type { BenchmarkResult } from '../canvas/benchmark';
import { formatMs } from '../canvas/frameStats';

export interface BenchmarkDialogProps {
  readonly result: BenchmarkResult | null;
  readonly running: boolean;
  readonly onClose: () => void;
}

/**
 * 性能基准结果（窗口 →「运行性能基准（开发）」）。显示每个阶段的 P50/P95 与瓦片统计，
 * 同时打印到控制台，便于复制进报告。
 */
export function BenchmarkDialog({ result, running, onClose }: BenchmarkDialogProps) {
  const [copied, setCopied] = useState(false);
  if (!running && !result) return null;
  const copy = async () => {
    if (!result) return;
    await navigator.clipboard?.writeText(JSON.stringify(result, null, 2));
    setCopied(true);
  };
  return (
    <div
      className="mg-dialog-backdrop"
      role="dialog"
      aria-modal="true"
      aria-label={t.menuItems.benchmark}
    >
      <div className="mg-dialog">
        <header className="mg-dialog-title">
          {t.menuItems.benchmark}
          <button
            type="button"
            className="mg-dialog-close"
            onClick={onClose}
            aria-label={t.window.close}
          >
            ×
          </button>
        </header>
        {running ? (
          <p className="mg-empty">{t.status.benchmarkRunning}</p>
        ) : result ? (
          <>
            <p className="mg-dialog-meta">
              {result.pages} 页 · 视口 {Math.round(result.viewport.width)}×
              {Math.round(result.viewport.height)} · DPR {result.devicePixelRatio} · 缓存{' '}
              {result.tiles.cacheCount} 块 / {result.tiles.cacheMB} MB
            </p>
            <table className="mg-bench-table">
              <thead>
                <tr>
                  <th>阶段</th>
                  <th>帧数</th>
                  <th>P50</th>
                  <th>P95</th>
                  <th>最大</th>
                  <th>脚本 P95</th>
                  <th>长帧</th>
                </tr>
              </thead>
              <tbody>
                {result.phases.map((phase) => (
                  <tr key={phase.name}>
                    <td>{phase.name}</td>
                    <td>{phase.samples}</td>
                    <td>{formatMs(phase.intervalP50)}</td>
                    <td>{formatMs(phase.intervalP95)}</td>
                    <td>{formatMs(phase.intervalMax)}</td>
                    <td>{formatMs(phase.workP95)}</td>
                    <td>{phase.longFrames}</td>
                  </tr>
                ))}
              </tbody>
            </table>
            <div className="mg-dialog-actions">
              <button type="button" onClick={copy} disabled={copied}>
                {copied ? t.benchmark.copied : t.benchmark.copy}
              </button>
              <button type="button" onClick={onClose}>
                {t.benchmark.close}
              </button>
            </div>
          </>
        ) : null}
      </div>
    </div>
  );
}
