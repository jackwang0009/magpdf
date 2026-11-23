import { useState } from 'react';
import { format, t } from '../i18n';
import type { BenchmarkResult } from '../canvas/benchmark';
import { formatMs } from '../canvas/frameStats';
import type { HostNotice, RecoverableDocument } from './editorHost';

export interface RecoveryDialogProps {
  readonly documents: readonly RecoverableDocument[];
  /** 用户答复了：要重新打开的那几份（可以是空的） */
  readonly onResolve: (ids: readonly string[]) => void;
}

/** 崩溃恢复（《交互设计》6.9）：列出上次开着的文档，逐个勾选要不要重新打开。 */
export function RecoveryDialog({ documents, onResolve }: RecoveryDialogProps) {
  const [chosen, setChosen] = useState<ReadonlySet<string>>(
    () => new Set(documents.map((d) => d.id)),
  );
  const toggle = (id: string) =>
    setChosen((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  return (
    <div
      className="mg-dialog-backdrop"
      role="alertdialog"
      aria-modal="true"
      aria-label={t.recovery.title}
    >
      <div className="mg-dialog mg-notice mg-recovery">
        <header className="mg-dialog-title">{t.recovery.title}</header>
        <p className="mg-notice-message">{t.recovery.message}</p>
        <table className="mg-recovery-table">
          <thead>
            <tr>
              <th />
              <th>{t.recovery.name}</th>
              <th>{t.recovery.modified}</th>
            </tr>
          </thead>
          <tbody>
            {documents.map((doc) => (
              <tr key={doc.id}>
                <td>
                  <input
                    type="checkbox"
                    aria-label={doc.name}
                    checked={chosen.has(doc.id)}
                    onChange={() => toggle(doc.id)}
                  />
                </td>
                <td>{doc.name}</td>
                <td>{doc.modifiedMs ? new Date(doc.modifiedMs).toLocaleString('zh-CN') : ''}</td>
              </tr>
            ))}
          </tbody>
        </table>
        <div className="mg-dialog-actions">
          <button
            type="button"
            onClick={() => onResolve(documents.filter((d) => chosen.has(d.id)).map((d) => d.id))}
            disabled={chosen.size === 0}
            autoFocus
          >
            {t.recovery.reopen}
          </button>
          <button type="button" onClick={() => onResolve([])}>
            {t.recovery.discard}
          </button>
        </div>
      </div>
    </div>
  );
}

export interface UnsavedDialogProps {
  /** 有没保存的修改的文档名 */
  readonly names: readonly string[];
  /** 正在保存：按钮都不能点 */
  readonly busy: boolean;
  readonly onSave: () => void;
  readonly onDiscard: () => void;
  readonly onCancel: () => void;
}

/**
 * 关闭前保存（《交互设计》9.1、9.2）：关标签页、退出程序时有没保存的修改，问“保存”“不保存”“取消”。
 * 一份文档时标题里写文件名；几份时标题写份数，下面列出文件名，“保存”把它们都存了。
 * 对话框里的按键不往外传：免得 Esc、方向键、工具键落到后面的画布上；Esc 等于“取消”。
 */
export function UnsavedDialog({ names, busy, onSave, onDiscard, onCancel }: UnsavedDialogProps) {
  const title =
    names.length === 1
      ? format(t.unsaved.titleOne, { name: names[0]! })
      : format(t.unsaved.titleMany, { n: names.length });
  return (
    <div
      className="mg-dialog-backdrop"
      role="alertdialog"
      aria-modal="true"
      aria-label={title}
      onKeyDown={(event) => {
        event.stopPropagation();
        if (event.key === 'Escape' && !busy) onCancel();
      }}
    >
      <div className="mg-dialog mg-notice">
        <header className="mg-dialog-title">{title}</header>
        <p className="mg-notice-message">{busy ? t.unsaved.saving : t.unsaved.message}</p>
        {names.length > 1 ? (
          <ul className="mg-unsaved-list">
            {names.map((name, index) => (
              <li key={`${name}-${index}`}>{name}</li>
            ))}
          </ul>
        ) : null}
        <div className="mg-dialog-actions">
          <button type="button" onClick={onSave} disabled={busy} autoFocus>
            {t.unsaved.save}
          </button>
          <button type="button" onClick={onDiscard} disabled={busy}>
            {t.unsaved.discard}
          </button>
          <button type="button" onClick={onCancel} disabled={busy}>
            {t.unsaved.cancel}
          </button>
        </div>
      </div>
    </div>
  );
}

export interface NoticeDialogProps {
  /** 要告诉用户的事；一次显示一条，点“知道了”再显示下一条 */
  readonly notice: HostNotice | null;
  readonly onDismiss: () => void;
}

/** 提示框：文件打不开、引擎重启过之类用户必须知道的事。 */
export function NoticeDialog({ notice, onDismiss }: NoticeDialogProps) {
  if (!notice) return null;
  const title = notice.kind === 'error' ? t.host.errorTitle : t.host.noticeTitle;
  return (
    <div className="mg-dialog-backdrop" role="alertdialog" aria-modal="true" aria-label={title}>
      <div className="mg-dialog mg-notice" data-kind={notice.kind}>
        <header className="mg-dialog-title">{title}</header>
        <p className="mg-notice-message">{notice.message}</p>
        <div className="mg-dialog-actions">
          <button type="button" onClick={onDismiss} autoFocus>
            {t.host.ok}
          </button>
        </div>
      </div>
    </div>
  );
}

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
