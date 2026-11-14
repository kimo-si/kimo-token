import { useLayoutEffect, useRef } from "react";
import { useReducedMotion } from "../lib/hooks";

// Motion Prompt #010 · MIT License (c) 2026 Shohei Ohtaki · https://showheyohtaki.github.io/motion-prompt/#010
// Motion Prompt #001-024 · MIT License (c) 2026 Shohei Ohtaki · https://showheyohtaki.github.io/motion-prompt/#001

// 保留参考仪表的尺寸比例，裁掉两侧空白以适配窄面板。
const WIDTH = 368;
const HEIGHT = 208;
const X = WIDTH / 2;
const Y = HEIGHT - 26;
const RADIUS = 128;
const ARC_WIDTH = 26;
const START_SECONDS = 0.25;
const HOLD_SECONDS = 3.7;
const MAX_NEEDLE = 1.06;
const COLORS = {
  navy: "#1B2A5E", blue: "#3B7BFF", mint: "#2EDBA8", coral: "#FF6B5C", yellow: "#FFC83D", white: "#FFFFFF",
} as const;

// 将配额比例映射为从左端开始的半圆角度。
const angle = (ratio: number) => Math.PI + ratio * Math.PI;

// 使用参考实现的阻尼公式，让指针越过目标再回到实际读数。
function overshoot(target: number, seconds: number) {
  const time = Math.max(0, seconds - START_SECONDS);
  return target * (1 - Math.exp(-4.6 * time) * Math.cos(10.5 * time));
}

// 绘制参考仪表的圆弧，并在满量程处截住填色。
function strokeArc(ctx: CanvasRenderingContext2D, start: number, end: number, color: string | CanvasGradient) {
  ctx.strokeStyle = color;
  ctx.beginPath();
  ctx.arc(X, Y, RADIUS, angle(start), angle(end));
  ctx.stroke();
}

// 在实际读数处绘制黄色标线和圆点。
function drawTarget(ctx: CanvasRenderingContext2D, target: number) {
  const a = angle(target);
  const dx = Math.cos(a);
  const dy = Math.sin(a);
  ctx.strokeStyle = COLORS.yellow;
  ctx.lineWidth = 7;
  ctx.beginPath();
  ctx.moveTo(X + dx * (RADIUS - 22), Y + dy * (RADIUS - 22));
  ctx.lineTo(X + dx * (RADIUS + 30), Y + dy * (RADIUS + 30));
  ctx.stroke();
  ctx.fillStyle = COLORS.yellow;
  ctx.beginPath();
  ctx.arc(X + dx * (RADIUS + 38), Y + dy * (RADIUS + 38), 8, 0, 7);
  ctx.fill();
}

// 画出白针和珊瑚色轴心，给浅色面板上的白针加对比。
function drawNeedle(ctx: CanvasRenderingContext2D, value: number) {
  const a = angle(Math.min(value, MAX_NEEDLE));
  ctx.save();
  ctx.shadowColor = "rgba(15,26,61,.65)";
  ctx.shadowBlur = 3;
  ctx.strokeStyle = COLORS.white;
  ctx.lineWidth = 9;
  ctx.beginPath();
  ctx.moveTo(X, Y);
  ctx.lineTo(X + Math.cos(a) * (RADIUS - 26), Y + Math.sin(a) * (RADIUS - 26));
  ctx.stroke();
  ctx.fillStyle = COLORS.white;
  ctx.beginPath();
  ctx.arc(X, Y, 17, 0, 7);
  ctx.fill();
  ctx.restore();
  ctx.fillStyle = COLORS.coral;
  ctx.beginPath();
  ctx.arc(X, Y, 8, 0, 7);
  ctx.fill();
}

// 合成一帧仪表，回弹时超过目标的区段用珊瑚色标记。
function drawGauge(ctx: CanvasRenderingContext2D, target: number, value: number) {
  const visible = Math.min(1, value);
  ctx.clearRect(0, 0, WIDTH, HEIGHT);
  ctx.lineWidth = ARC_WIDTH;
  ctx.lineCap = "round";
  strokeArc(ctx, 0, 1, COLORS.navy);
  if (visible > 0.005) {
    const gradient = ctx.createLinearGradient(X - RADIUS, 0, X + RADIUS, 0);
    gradient.addColorStop(0, COLORS.blue);
    gradient.addColorStop(1, COLORS.mint);
    strokeArc(ctx, 0, visible, gradient);
  }
  if (visible > target + 0.004) {
    ctx.lineCap = "butt";
    strokeArc(ctx, target, visible, COLORS.coral);
    ctx.lineCap = "round";
  }
  drawTarget(ctx, target);
  drawNeedle(ctx, value);
}

// 仪表第一次进入可见范围时播放一次，并在离开或关闭时取消绘制。
function animateGauge(canvas: HTMLCanvasElement, ctx: CanvasRenderingContext2D, target: number) {
  let frameId = 0;
  let start: number | null = null;
  const step = (now: number) => {
    start ??= now;
    const seconds = (now - start) / 1000;
    const settled = seconds >= HOLD_SECONDS;
    drawGauge(ctx, target, settled ? target : overshoot(target, seconds));
    if (!settled) frameId = requestAnimationFrame(step);
  };
  const observer = new IntersectionObserver((entries) => {
    if (!entries.some((entry) => entry.isIntersecting)) return;
    observer.disconnect();
    frameId = requestAnimationFrame(step);
  });
  observer.observe(canvas);
  return () => { observer.disconnect(); cancelAnimationFrame(frameId); };
}

// 以真实百分比作为停留读数，并向辅助技术报告相同的值。
export function QuotaGauge({ percent, label, present }: { percent: number; label: string; present: boolean }) {
  const ref = useRef<HTMLCanvasElement>(null);
  const reducedMotion = useReducedMotion();
  const target = Math.max(0, Math.min(100, percent)) / 100;
  useLayoutEffect(() => {
    const canvas = ref.current;
    const ctx = canvas?.getContext("2d");
    if (!canvas || !ctx || !present) return;
    const animate = !reducedMotion && target > 0;
    drawGauge(ctx, target, animate ? 0 : target);
    if (animate) return animateGauge(canvas, ctx, target);
  }, [present, reducedMotion, target]);

  return (
    <span className="quota-gauge" role="meter" aria-label={label}
      aria-valuemin={0} aria-valuemax={100} aria-valuenow={target * 100}>
      <canvas ref={ref} width={WIDTH} height={HEIGHT} aria-hidden="true" />
    </span>
  );
}
