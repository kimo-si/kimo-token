import type { QuotaView } from "../types";
import { clockStamp, etaDuration, freshness, toolDisplay } from "../lib/format";
import { lang, t } from "../lib/i18n";
import { IconNext } from "./Icons";

const STAGES = [
  { key: "low", label: "quota.legend.low" },
  { key: "mid", label: "quota.legend.mid" },
  { key: "high", label: "quota.legend.high" },
] as const;
export const QUOTA_THRESHOLDS = { attention: 50, warning: 80, limit: 100 } as const;
type Stage = typeof STAGES[number]["key"];
export type QuotaFilter = Stage | "all";

const resetTime = new Intl.DateTimeFormat(lang === "zh" ? "zh-CN" : "en-GB", {
  hour: "2-digit", minute: "2-digit", hourCycle: "h23",
});
const resetDate = new Intl.DateTimeFormat(lang === "zh" ? "zh-CN" : "en-GB", {
  month: "short", day: "numeric", hour: "2-digit", minute: "2-digit", hourCycle: "h23",
});

/** Windows are named by length unless the source gave a better bucket name.
 *  A source-provided label passes through verbatim — it is the vendor's text. */
export function quotaWindowName(q: QuotaView): string {
  if (q.label && q.label.length > 0) return q.label;
  if (q.window_minutes >= 43_200) return t("quota.win.month");
  if (q.window_minutes >= 1_440) return t("quota.win.day", { n: Math.round(q.window_minutes / 1440) });
  if (q.window_minutes >= 60) return t("quota.win.hour", { n: Math.round(q.window_minutes / 60) });
  return q.window_minutes > 0 ? t("quota.win.min", { n: q.window_minutes }) : t("quota.win.unknown");
}

// 配额状态始终由已用比例决定。
export function quotaStage(used: number): Stage | "over" {
  if (used > QUOTA_THRESHOLDS.limit) return "over";
  if (used >= QUOTA_THRESHOLDS.warning) return "high";
  return used >= QUOTA_THRESHOLDS.attention ? "mid" : "low";
}

// 只有实测到百分比的配额才能参与紧迫度排序。
export function hasQuotaMeter(row: QuotaView): boolean {
  return Number.isFinite(row.used_percent) && row.used_percent >= 0
    && row.id !== "dsh-balance" && !row.label?.includes("无可查限额");
}

// 已用比例更高的窗口优先，比例相同时优先展示即将重置的窗口。
export function compareQuotaUrgency(a: QuotaView, b: QuotaView): number {
  const usage = (hasQuotaMeter(b) ? b.used_percent : -1) - (hasQuotaMeter(a) ? a.used_percent : -1);
  return usage || (a.resets_at_ms || Infinity) - (b.resets_at_ms || Infinity) || 0;
}

// 将超额窗口纳入告急筛选，余额等非百分比窗口保留在全部状态。
export function matchesQuotaFilter(row: QuotaView, filter: QuotaFilter): boolean {
  if (filter === "all") return true;
  if (!hasQuotaMeter(row)) return false;
  const stage = quotaStage(row.used_percent);
  return filter === (stage === "over" ? "high" : stage);
}

// 在窄面板里同时给出重置时刻和剩余时间。
export function quotaResetClock(at: number, now: number): string {
  const date = new Date(at);
  const sameDay = date.toDateString() === new Date(now).toDateString();
  return (sameDay ? resetTime : resetDate).format(date);
}

// 概览只推荐仍有效且有重置时间的供应商窗口。
function attentionWindow(rows: QuotaView[], now: number): QuotaView | undefined {
  return rows.filter((row) => row.origin !== "budget" && row.window_minutes > 0
    && (row.resets_at_ms === 0 || row.resets_at_ms > now) && hasQuotaMeter(row)).sort(compareQuotaUrgency)[0];
}

// 概览提醒使用本机采样，并直接链接到配额页。
export function QuotaAttention({ rows, now, pending, polling, onOpen }: {
  rows: QuotaView[]; now: number; pending: boolean; polling: boolean; onOpen: () => void;
}) {
  const quota = pending ? undefined : attentionWindow(rows, now);
  const warning = polling && quota !== undefined && quotaStage(quota.used_percent) !== "low";
  const fallback = pending ? "quota.pending" : polling ? "quota.overview.unavailable" : "quota.overview.paused";
  const resetAt = quota?.resets_at_ms ?? 0;
  const reset = resetAt > now ? quotaResetClock(resetAt, now) : "";
  return (
    <button type="button" className="quota-attention" data-warning={warning || undefined} onClick={onOpen}
      title={quota ? t("quota.sampled", { time: clockStamp(quota.sampled_at_ms, now) }) : undefined}>
      <span>
        <strong>{quota ? toolDisplay(quota.tool) : t("quota.section")}</strong>
        <small>{quota
          ? polling ? t("quota.overview.used", { window: quotaWindowName({ ...quota, label: null }), p: Math.round(quota.used_percent) })
            : t("quota.stopped", { age: freshness(quota.sampled_at_ms, now) })
          : t(fallback)}</small>
      </span>
      <span className="quota-attention-end">
        {reset ? <>
          <time dateTime={new Date(resetAt).toISOString()}>{t("quota.reset.at", { time: reset })}</time>
          <small>{t("quota.reset.in", { duration: etaDuration(resetAt - now) })}</small>
        </> : <span>{t("quota.overview.open")}</span>}
      </span>
      <IconNext size={13} />
    </button>
  );
}

// 以原生选择控件切换配额状态。
export function QuotaBoardControls({ filter, onFilter, meta }: {
  filter: QuotaFilter; onFilter: (filter: QuotaFilter) => void; meta: string;
}) {
  return (
    <div className="quota-board-tools">
      <h1>{t("page.quota.title")}</h1>
      <select aria-label={t("quota.board.status")} value={filter}
        onChange={(event) => onFilter(event.currentTarget.value as QuotaFilter)}>
        <option value="all">{t("quota.board.all")}</option>
        {STAGES.map(({ key, label }) => <option key={key} value={key}>{t(label)}</option>)}
      </select>
      <span className="sec-meta">{meta}</span>
    </div>
  );
}
