/**
 * 帧时间统计：环形缓冲记录最近 N 帧的“帧间隔”（rAF 回调之间的时间，反映用户感受到的流畅度）
 * 与“脚本耗时”（我们在 rAF 回调里实际花的时间）。P50/P95 由排序后的样本取分位数。
 * 纯计算，不依赖 DOM，便于单测与基准脚本复用。
 */
export interface FrameStatsSummary {
  readonly samples: number;
  /** 帧间隔分位数（ms） */
  readonly intervalP50: number;
  readonly intervalP95: number;
  readonly intervalMax: number;
  /** rAF 回调内脚本耗时分位数（ms） */
  readonly workP50: number;
  readonly workP95: number;
  readonly workMax: number;
  /** 超过 16.7 ms 预算的帧数 */
  readonly longFrames: number;
}

export class FrameStats {
  private readonly intervals: Float64Array;
  private readonly work: Float64Array;
  private head = 0;
  private size = 0;
  private lastFrameAt: number | null = null;

  constructor(readonly capacity = 240) {
    this.intervals = new Float64Array(capacity);
    this.work = new Float64Array(capacity);
  }

  /** 在 rAF 回调开始时调用，返回本帧时间戳 */
  beginFrame(now: number): void {
    if (this.lastFrameAt !== null) {
      this.push(now - this.lastFrameAt, 0);
    }
    this.lastFrameAt = now;
  }

  /** 在 rAF 回调结束时调用，记录脚本耗时到当前帧 */
  endFrame(now: number): void {
    if (this.lastFrameAt === null || this.size === 0) return;
    const index = (this.head - 1 + this.capacity) % this.capacity;
    this.work[index] = Math.max(this.work[index]!, now - this.lastFrameAt);
  }

  /** 直接写入一帧样本（基准脚本或测试使用） */
  push(interval: number, work: number): void {
    this.intervals[this.head] = interval;
    this.work[this.head] = work;
    this.head = (this.head + 1) % this.capacity;
    if (this.size < this.capacity) this.size++;
  }

  /** 交互中断（鼠标抬起、停止滚动）后调用，避免把空闲时间算成一帧 */
  breakSequence(): void {
    this.lastFrameAt = null;
  }

  reset(): void {
    this.head = 0;
    this.size = 0;
    this.lastFrameAt = null;
  }

  get count(): number {
    return this.size;
  }

  summary(budgetMs = 16.7): FrameStatsSummary {
    const intervals = this.collect(this.intervals);
    const work = this.collect(this.work);
    let longFrames = 0;
    for (const v of intervals) if (v > budgetMs) longFrames++;
    return {
      samples: this.size,
      intervalP50: percentile(intervals, 0.5),
      intervalP95: percentile(intervals, 0.95),
      intervalMax: intervals.length ? intervals[intervals.length - 1]! : 0,
      workP50: percentile(work, 0.5),
      workP95: percentile(work, 0.95),
      workMax: work.length ? work[work.length - 1]! : 0,
      longFrames,
    };
  }

  private collect(source: Float64Array): number[] {
    const out: number[] = [];
    for (let i = 0; i < this.size; i++) out.push(source[i]!);
    out.sort((a, b) => a - b);
    return out;
  }
}

/** 最近秩法分位数：sorted 升序，p ∈ [0, 1] */
export function percentile(sorted: readonly number[], p: number): number {
  if (sorted.length === 0) return 0;
  const rank = Math.min(sorted.length - 1, Math.max(0, Math.ceil(p * sorted.length) - 1));
  return sorted[rank]!;
}

export function formatMs(value: number): string {
  return value.toFixed(1);
}
