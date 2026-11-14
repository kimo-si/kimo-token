import { lang, t } from "../lib/i18n";
import type { CodexResetCredit, CodexResetCredits } from "../types";
import { IconRefresh } from "./Icons";
import { Section } from "./Section";

const MINUTE_MS = 60_000;
const MINUTES_PER_HOUR = 60;
const HOURS_PER_DAY = 24;
const DAY_MS = HOURS_PER_DAY * MINUTES_PER_HOUR * MINUTE_MS;
const locale = lang === "zh" ? "zh-CN" : "en-GB";
const dateTime = new Intl.DateTimeFormat(locale, {
  year: "numeric", month: "2-digit", day: "2-digit",
  hour: "2-digit", minute: "2-digit", hourCycle: "h23",
});
const units = (["day", "hour", "minute"] as const).map((unit) => new Intl.NumberFormat(locale, {
  style: "unit", unit, unitDisplay: "narrow",
}));

// 倒计时最多显示两个单位，避免小窗口内挤占日期列。
function expiryCountdown(remainingMinutes: number): string {
  const hours = Math.floor(remainingMinutes / MINUTES_PER_HOUR);
  const days = Math.floor(hours / HOURS_PER_DAY);
  const values = [days, hours % HOURS_PER_DAY, remainingMinutes % MINUTES_PER_HOUR];
  const first = values.findIndex((value) => value > 0);
  return values.slice(first, first + 2)
    .flatMap((value, index) => value ? [units[first + index].format(value)] : [])
    .join(" ");
}

// 标记本地时间，并保留机器可读的原始时间供辅助技术使用。
function CreditTime({ value }: { value: number }) {
  return <time dateTime={new Date(value).toISOString()}>{dateTime.format(value)}</time>;
}

// 每行优先显示到期时间，其次显示领取时间和剩余时长。
function CreditRow({ card, now }: { card: CodexResetCredit; now: number }) {
  const expiry = card.expires_at_ms;
  const remainingMs = expiry === null ? null : expiry - now;
  const remaining = remainingMs === null ? null : Math.max(1, Math.floor(remainingMs / MINUTE_MS));
  const expiringSoon = remainingMs !== null && remainingMs <= DAY_MS;
  return (
    <li className="reset-credit-row">
      <div className="reset-credit-dates">
        <div className="reset-credit-expiry">
          {expiry === null ? t("reset-credits.no-expiry") : <><span>{t("reset-credits.expiry")}</span> <CreditTime value={expiry} /></>}
        </div>
        <div className="reset-credit-granted">{t("reset-credits.granted")} <CreditTime value={card.granted_at_ms} /></div>
      </div>
      {remaining === null ? null : (
        <span className="reset-credit-badge" data-soon={expiringSoon || undefined}>
          {t("reset-credits.left", { duration: expiryCountdown(remaining) })}
        </span>
      )}
    </li>
  );
}

const stateLabels = {
  idle: "reset-credits.idle",
  loading: "reset-credits.loading",
  ready: "reset-credits.empty",
  unavailable: "reset-credits.unavailable",
  sign_in_required: "reset-credits.sign-in",
} as const;

export type ResetCreditsState = CodexResetCredits | { status: "idle" | "loading"; cards: CodexResetCredit[] | null };

// 将可用卡片放进独立滚动区域，让图表和底栏始终保留在首屏。
export function ResetCreditsBoard({ state, now, onRefresh }: {
  state: ResetCreditsState;
  now: number;
  onRefresh: () => void;
}) {
  const cards = state.cards?.filter((card) => card.expires_at_ms === null || card.expires_at_ms > now) ?? [];
  const busy = state.status === "loading";
  const refreshLabel = t("reset-credits.refresh");
  const actions = (
    <>
      {state.cards !== null ? <span className="sec-meta num">{t("reset-credits.count", { n: cards.length })}</span> : null}
      <button type="button" className="icon-btn reset-credit-refresh" title={refreshLabel}
        aria-label={refreshLabel} aria-busy={busy} disabled={busy} onClick={onRefresh}>
        <IconRefresh size={14} />
      </button>
    </>
  );
  return (
    <Section className="reset-credit-board" label={t("reset-credits.title")} trail={actions}>
      {cards.length ? (
        <ul className="reset-credit-list" tabIndex={0} aria-label={t("reset-credits.title")}>
          {cards.map((card, index) => <CreditRow key={`${card.granted_at_ms}-${card.expires_at_ms}-${index}`} card={card} now={now} />)}
        </ul>
      ) : null}
      {(!cards.length || state.status === "unavailable" || state.status === "sign_in_required") ? (
        <p className="reset-credit-state" role="status">{t(stateLabels[state.status])}</p>
      ) : null}
    </Section>
  );
}
