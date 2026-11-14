import { useLayoutEffect, useRef, type RefObject } from "react";
import { useReducedMotion } from "./hooks";

// Motion Prompt #002 · MIT License (c) 2026 Shohei Ohtaki · https://showheyohtaki.github.io/motion-prompt/#002
// Motion Prompt #001-024 · MIT License (c) 2026 Shohei Ohtaki · https://showheyohtaki.github.io/motion-prompt/#001

const CARD_WIDTH = 539;
const BAR_WIDTH = 44;
const GAP = 20;
const PITCH = BAR_WIDTH + GAP;
const MAX_BAR_WIDTH = 56;
const WEEK_COUNT = 7;
const CYCLE = 4.7;
const PASS_SECONDS = CYCLE / 2;
const FIRST_BAR = 0.15;
const BAR_STEP = 0.12;
const BAR_RISE = 0.5;
const DOT_DELAY = 0.34;
const EXIT_STEP = 0.045;
const BAR_EXIT = [1.78, 2.04] as const;
const DOT_EXIT = [1.7, 1.9] as const;
const DOT_RADIUS = 6.5;
const DOT_OFFSET = 16;
const END_SECONDS = PASS_SECONDS + FIRST_BAR + (WEEK_COUNT - 1) * BAR_STEP + DOT_DELAY + 1;
const PALETTE = ["#3B7BFF", "#FF6B5C", "#FFC83D", "#2EDBA8"] as const;
const BASELINE_COLOR = "#2A3D80";
const EMPTY_COLOR = "rgba(127, 144, 196, 0.28)";

export interface StaggerBucket {
  tokens: number;
  future: boolean;
}

interface PlotArea {
  width: number;
  height: number;
  base: number;
  scaleHeight: number;
  pixelRatio: number;
}

interface PlotFrame {
  x: number;
  width: number;
  unit: number;
  base: number;
  height: number;
  dot: number;
  color: string;
}

// 将时刻映射到参考实现的 0 到 1 进度。
const progress = (t: number, a: number, b: number) => Math.min(1, Math.max(0, (t - a) / (b - a)));

// 使用参考实现的回弹曲线。
const backOut = (x: number, s: number) => 1 + (s + 1) * Math.pow(x - 1, 3) + s * Math.pow(x - 1, 2);

// 使用参考实现的阻尼弹簧计算圆点半径。
const spring = (t: number, w: number, d: number) => t <= 0 ? 0 : 1 - Math.exp(-d * t) * Math.cos(w * t);

// 使用三次缓入和逐柱偏移计算退场倍率。
function exitScale(time: number, offset: number, [start, end]: readonly [number, number]) {
  const x = progress(time, start + offset, end + offset);
  return 1 - x * x * x;
}

// 完整播放参考实现的第一轮，第二次起柱后停在实际读数。
function staggerFrame(seconds: number | null, index: number) {
  if (seconds === null) return { bar: 1, dot: 1 };
  const local = seconds % PASS_SECONDS;
  const start = FIRST_BAR + index * BAR_STEP;
  const offset = index * EXIT_STEP;
  const exiting = seconds < PASS_SECONDS;
  return {
    bar: backOut(progress(local, start, start + BAR_RISE), 2.6) * (exiting ? exitScale(local, offset, BAR_EXIT) : 1),
    dot: spring(local - start - DOT_DELAY, 16, 7) * (exiting ? exitScale(local, offset, DOT_EXIT) : 1),
  };
}

// 与 DOM 刻度共用柱底位置和顶部留白，按设备像素分辨率绘制。
function measurePlot(canvas: HTMLCanvasElement): PlotArea {
  const { width, height } = canvas.getBoundingClientRect();
  const styles = getComputedStyle(canvas);
  const base = Math.max(0, height - parseFloat(styles.getPropertyValue("--axis-label-height")));
  const headroom = parseFloat(styles.getPropertyValue("--chart-rise-headroom"));
  const dotHeadroom = parseFloat(styles.getPropertyValue("--chart-dot-headroom"));
  return { width, height, base, scaleHeight: Math.max(0, base * (1 - headroom) - dotHeadroom), pixelRatio: window.devicePixelRatio || 1 };
}

// 填充圆角矩形；旧版 WebView 仍能绘制对应数值。
function fillRounded(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, radii: number | number[]) {
  ctx.beginPath();
  if (typeof ctx.roundRect === "function") ctx.roundRect(x, y, w, h, radii);
  else ctx.rect(x, y, w, h);
  ctx.fill();
}

// 按原版圆角和白点尺寸绘制单根柱。
function drawBar(ctx: CanvasRenderingContext2D, frame: PlotFrame) {
  const { x, width, unit, base, height, dot, color } = frame;
  if (height <= 0) return;
  ctx.fillStyle = color;
  fillRounded(ctx, x, base - height, width, height, [12 * unit, 12 * unit, 3 * unit, 3 * unit]);
  if (dot <= 0.01) return;
  ctx.fillStyle = "#fff";
  ctx.beginPath();
  ctx.arc(x + width / 2, base - height - DOT_OFFSET * unit, DOT_RADIUS * unit * dot, 0, 7);
  ctx.fill();
}

// 周图使用原版 539 像素构图，其余周期按实际分桶数量等比分配宽度。
function plotColumns(width: number, count: number, isWeek: boolean) {
  const referenceUnit = width / CARD_WIDTH;
  const pitch = isWeek ? PITCH * referenceUnit : width / count;
  const unit = Math.min(pitch / PITCH, referenceUnit);
  const barWidth = Math.min(MAX_BAR_WIDTH, pitch * BAR_WIDTH / PITCH);
  const span = (count - 1) * pitch + barWidth;
  return { unit, pitch, start: (width - span) / 2, width: barWidth, span };
}

// 使用真实用量代替演示高度，未来和零用量用短刻度标记。
function drawBuckets(ctx: CanvasRenderingContext2D, plot: PlotArea, buckets: readonly StaggerBucket[], ceiling: number,
  isWeek: boolean, seconds: number | null) {
  const columns = plotColumns(plot.width, buckets.length, isWeek);
  const { unit, pitch, start, width, span } = columns;
  const inset = Math.min(18 * unit, start);
  ctx.fillStyle = BASELINE_COLOR;
  fillRounded(ctx, start - inset, plot.base, span + inset * 2, 5 * unit, 2.5 * unit);
  buckets.forEach((bucket, index) => {
    const x = start + index * pitch;
    const empty = bucket.future || bucket.tokens <= 0;
    const frame = staggerFrame(seconds, index);
    if (!empty && frame.bar <= 0.001) return;
    const height = empty ? 2 * unit : Math.min(1, bucket.tokens / ceiling) * plot.scaleHeight * frame.bar;
    drawBar(ctx, { x, width, unit, base: plot.base, height, dot: empty ? 0 : frame.dot,
      color: empty ? EMPTY_COLOR : PALETTE[index % PALETTE.length] });
  });
}

interface CanvasSeries {
  buckets: readonly StaggerBucket[];
  ceiling: number;
  isWeek: boolean;
}

// Canvas 每帧读取最新数据，不因焦点变化或报表刷新重新开始播放。
function bindCanvas(canvas: HTMLCanvasElement, ctx: CanvasRenderingContext2D, readSeries: () => CanvasSeries, animate: boolean) {
  let plot = measurePlot(canvas);
  let time: number | null = animate ? 0 : null;
  let frameId = 0;
  const start = performance.now();

  // 使用现有画布绘制当前帧。
  const draw = () => {
    const { buckets, ceiling, isWeek } = readSeries();
    ctx.clearRect(0, 0, plot.width, plot.height);
    if (plot.width <= 0 || plot.height <= 0 || buckets.length === 0) return;
    drawBuckets(ctx, plot, buckets, ceiling, isWeek, time);
  };

  // 窗口大小变化时重设位图，维持与刻度一致的 CSS 坐标。
  const resize = () => {
    plot = measurePlot(canvas);
    const width = Math.max(1, Math.round(plot.width * plot.pixelRatio));
    const height = Math.max(1, Math.round(plot.height * plot.pixelRatio));
    if (canvas.width !== width) canvas.width = width;
    if (canvas.height !== height) canvas.height = height;
    ctx.setTransform(plot.pixelRatio, 0, 0, plot.pixelRatio, 0, 0);
    draw();
  };

  // 到达停留时刻后停止调度，只在数据或尺寸变化时重绘。
  const step = (now: number) => {
    const elapsed = (now - start) / 1000;
    time = elapsed < END_SECONDS ? elapsed : null;
    draw();
    if (time !== null) frameId = requestAnimationFrame(step);
  };

  resize();
  const observer = new ResizeObserver(resize);
  observer.observe(canvas);
  if (animate) frameId = requestAnimationFrame(step);
  return { draw: resize, dispose: () => { observer.disconnect(); cancelAnimationFrame(frameId); } };
}

// 挂载可访问图表的视觉层，交互按钮和刻度由组件保留。
export function useStaggerCanvas(ref: RefObject<HTMLCanvasElement | null>, series: CanvasSeries, animate: boolean) {
  const reducedMotion = useReducedMotion();
  const latest = useRef(series);
  const repaint = useRef<(() => void) | null>(null);
  useLayoutEffect(() => {
    latest.current = series;
    repaint.current?.();
  }, [series]);

  useLayoutEffect(() => {
    const canvas = ref.current;
    const ctx = canvas?.getContext("2d");
    if (!canvas || !ctx) return;
    // 绑定动画和尺寸监听，并在切换视图时清理。
    const binding = bindCanvas(canvas, ctx, () => latest.current, animate && !reducedMotion);
    repaint.current = binding.draw;
    return () => { repaint.current = null; binding.dispose(); };
  }, [ref, animate, reducedMotion]);
}
