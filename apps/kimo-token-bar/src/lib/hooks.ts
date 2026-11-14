import { useEffect, useLayoutEffect, useState } from "react";

const REDUCED = "(prefers-reduced-motion: reduce)";

export function prefersReducedMotion(): boolean {
  return typeof window !== "undefined" && window.matchMedia(REDUCED).matches;
}

/** Media-query live region used for the two motion moments. */
export function useReducedMotion(): boolean {
  const [reduced, setReduced] = useState(prefersReducedMotion);
  useEffect(() => {
    const mq = window.matchMedia(REDUCED);
    const onChange = () => setReduced(mq.matches);
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);
  return reduced;
}

// Motion Prompt #007 · MIT License (c) 2026 Shohei Ohtaki · https://showheyohtaki.github.io/motion-prompt/#007
// Motion Prompt #001-024 · MIT License (c) 2026 Shohei Ohtaki · https://showheyohtaki.github.io/motion-prompt/#001
const COUNT_LAND_SECONDS = 2.9;
const COUNT_HOLD_SECONDS = 4;

// 将时间映射到参考实现中各阶段的 0 到 1 进度。
const countProgress = (time: number, start: number, end: number) =>
  Math.min(1, Math.max(0, (time - start) / (end - start)));

// 用同一时钟计算数字、箭头和进度条，停在落点以保留用量读数。
function countUpFrame(target: number, time: number | null) {
  if (time === null) return {
    value: target, alpha: 1, landed: true, pop: 1, arrow: 1, bob: 0, fill: target > 0 ? 1 : 0,
  };
  const progress = countProgress(time, 0.45, COUNT_LAND_SECONDS);
  const value = Math.round(target * (1 - Math.pow(1 - progress, 4)));
  const land = time - COUNT_LAND_SECONDS;
  const arrowTime = time - 0.35;
  return {
    value,
    alpha: countProgress(time, 0.1, 0.4),
    landed: land > 0,
    pop: 1 + (land > 0 ? 0.07 * Math.exp(-7 * land) * Math.cos(15 * land) : 0),
    arrow: arrowTime > 0 ? 1 - Math.exp(-7 * arrowTime) * Math.cos(14 * arrowTime) : 0,
    bob: Math.pow(1 - progress, 3) * 6 * Math.sin(time * 30),
    fill: value / target,
  };
}

// 首次展示和切换周期时播放；同一周期刷新只更新读数。
export function useCountUp(target: number, period: string) {
  const reduced = useReducedMotion();
  const enabled = !reduced && target > 0;
  const [time, setTime] = useState<number | null>(null);
  useLayoutEffect(() => {
    if (!enabled) {
      setTime(null);
      return;
    }
    const start = performance.now();
    let frameId = 0;
    setTime(0);
    // 落点回弹结束后释放动画帧。
    const step = (now: number) => {
      const elapsed = (now - start) / 1000;
      setTime(elapsed < COUNT_HOLD_SECONDS ? elapsed : null);
      if (elapsed < COUNT_HOLD_SECONDS) frameId = requestAnimationFrame(step);
    };
    frameId = requestAnimationFrame(step);
    return () => cancelAnimationFrame(frameId);
  }, [enabled, period]);
  return countUpFrame(target, enabled ? time : null);
}

/** Re-render on a slow tick so "12 分钟前" and quota countdowns stay honest. */
export function useTicker(intervalMs = 30_000): number {
  const [tick, setTick] = useState(0);
  useEffect(() => {
    const id = window.setInterval(() => setTick((n) => n + 1), intervalMs);
    return () => window.clearInterval(id);
  }, [intervalMs]);
  return tick;
}

/** Escape hides the panel; the core owns the actual window. */
export function useEscape(onEscape: () => void): void {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onEscape();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onEscape]);
}
