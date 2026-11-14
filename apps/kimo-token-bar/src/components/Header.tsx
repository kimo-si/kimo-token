import { Chip } from "@heroui/react";
import { useLayoutEffect, useRef, useState, type CSSProperties, type KeyboardEvent } from "react";
import kimoTokenWordmark from "../assets/kimo-token-wordmark.webp";
import type { MachineScope, PageKey, PeriodKey, Report, Summary, SyncDevicesSnapshot } from "../types";
import {
  PERIOD_LABEL,
  PERIOD_PREV,
  PERIOD_SHORT,
  credits as creditText,
  deltaDirection,
  formatOverviewTokens,
  money,
  percent,
  count,
  signedPercent,
  splitHeadlineTokens,
} from "../lib/format";
import { useCountUp } from "../lib/hooks";
import { useMoney } from "../lib/display";
import { t } from "../lib/i18n";
import { IconArrowDown, IconArrowUp, IconClose, IconFlat, IconRefresh, IconSettings } from "./Icons";
import { ScopeDropdown } from "./ScopeDropdown";

const ORDER = ["day", "week", "month", "year"] as const;

const PAGES: { key: PageKey; label: () => string; hint: () => string }[] = [
  { key: "overview", label: () => t("page.overview"), hint: () => t("page.overview.hint") },
  { key: "quota", label: () => t("page.quota"), hint: () => t("page.quota.hint") },
  { key: "analysis", label: () => t("page.analysis"), hint: () => t("page.analysis.hint") },
  { key: "speed", label: () => t("page.speed"), hint: () => t("page.speed.hint") },
];

interface Props {
  report: Report;
  page: PageKey;
  onPage: (page: PageKey) => void;
  onClose?: () => void;
  onOpenSettings: () => void;
  /** Machine-sync health: the newest bundle merged from each origin machine
   *  that still exists (a paired device or a configured server). Null when no
   *  such machine ever delivered one (most users). */
  sync?: {
    latest: { origin: string; rows: number; file: string };
    age: string;
    more: number;
    /** Warn hue: the newest merge is not from today. */
    stale: boolean;
    /** Nothing merged today → the badge says so instead of yesterday's rows. */
    mergedToday: boolean;
    rows: { origin: string; file: string; rows: number; window: string; age: string }[];
  } | null;
  /** The scope the current report was folded under (its echoed value). */
  scope: MachineScope;
  onScope: (scope: MachineScope) => void;
  /** The scope menu's open state lives in App so Escape can order
   *  menu → settings sheet → panel. */
  scopeMenuOpen: boolean;
  onScopeMenuOpen: (open: boolean) => void;
  /** Origins backed by a configured server; the scope menu hides other
   *  non-device origins as historical export/import data. */
  serverOrigins?: Set<string>;
  /** Registered strict-sync devices; read-only in this slice. */
  syncDevices?: SyncDevicesSnapshot;
  /** The shared 30 s tick, so the menu's sync ages stay honest. */
  now: number;
}

// 用语义化状态标签显示用量相对上一周期的变化。
function DeltaChip({ pct, caption }: { pct: number; caption: string }) {
  const dir = deltaDirection(pct);
  const Glyph = dir === "up" ? IconArrowUp : dir === "down" ? IconArrowDown : IconFlat;
  const value = signedPercent(pct);
  // 比较周期通过提示提供，徽标显示变化方向和比例。
  return (
    <Chip size="sm" variant="soft" color={dir === "up" ? "danger" : "accent"} className="delta-chip" data-dir={dir} title={`${caption} ${value}`}>
      <Glyph size={11} />
      <Chip.Label className="chip-v num">{value}</Chip.Label>
    </Chip>
  );
}

/** One line per origin for the badge tooltip: window and file included so a
 *  wrong or half-transported bundle is identifiable without opening the folder. */
function syncTip(sync: NonNullable<Props["sync"]>): string {
  const lines = [sync.mergedToday ? t("hdr.sync.tip") : t("hdr.sync.idle.tip")];
  for (const r of sync.rows) {
    lines.push(
      `${r.origin} · ${r.age} · ${t("hdr.sync.rows", { n: count(r.rows) })} · ${r.window} · ${r.file}`,
    );
  }
  return lines.join("\n");
}

// 将请求、会话和缓存摘要排在用量数字下方。
function HeaderStats({ summary }: { summary: Summary }) {
  return (
    <dl className="overview-stats">
      <div><dt>{t("hdr.requests.label")}</dt><dd>{count(summary.requests)}</dd></div>
      <div><dt>{t("hdr.sessions")}</dt><dd>{count(summary.sessions)}</dd></div>
      <div><dt>{t("hdr.cache.label")}</dt><dd>{percent(summary.cached_pct, 1)}</dd></div>
    </dl>
  );
}

// 将积分补充信息放在摘要标题旁，避免占用额外一行。
function HeaderCredits({ summary, showMoney }: { summary: Summary; showMoney: boolean }) {
  if (summary.credits <= 0) return null;
  return <span className="hdr-credits">
    {creditText(summary.credits)}
    {showMoney && summary.credit_cost > 0 && <span title={t("hdr.credit.note")}>{t("hdr.credit.included", { c: money(summary.credit_cost) })}</span>}
  </span>;
}

// 方向键和 Home、End 同时移动选中项与键盘焦点。
export function moveSelection(event: KeyboardEvent, index: number, size: number, select: (next: number) => void) {
  const offsets = { ArrowLeft: -1, ArrowRight: 1 } as const;
  const key = event.key;
  const next = key === "Home" ? 0 : key === "End" ? size - 1
    : key === "ArrowLeft" || key === "ArrowRight" ? (index + offsets[key] + size) % size : null;
  if (next === null) return;
  event.preventDefault();
  select(next);
  event.currentTarget.parentElement?.querySelectorAll("button")[next]?.focus();
}

// 四页共享一个滑动指示条，始终跟随实际选中的标签。
function PanelTabs({ page, onPage }: Pick<Props, "page" | "onPage">) {
  return (
    <div className="tabs-row">
      <div className="page-tabs" role="tablist" aria-label={t("page.nav")}
        style={{ "--selected-index": PAGES.findIndex(({ key }) => key === page) } as CSSProperties}>
        {PAGES.map(({ key, label, hint }, index) => (
          <button key={key} id={`panel-tab-${key}`} type="button" role="tab" className="page-tab"
            aria-selected={page === key} aria-controls={page === key ? "panel-content" : undefined}
            tabIndex={page === key ? 0 : -1} title={hint()} onClick={() => onPage(key)}
            onKeyDown={(event) => moveSelection(event, index, PAGES.length, (next) => onPage(PAGES[next].key))}>
            {label()}
          </button>
        ))}
      </div>
    </div>
  );
}

// 在用量相关页面使用同一个日、周、月、年选择器。
export function PeriodControl({ period, onPeriod }: { period: PeriodKey; onPeriod: (period: PeriodKey) => void }) {
  return (
    <div className="period-control" role="radiogroup" aria-label={t("hdr.period.a11y")}
      style={{ "--selected-index": ORDER.indexOf(period) } as CSSProperties}>
      {ORDER.map((key, index) => (
        <button key={key} type="button" role="radio" aria-checked={key === period}
          tabIndex={key === period ? 0 : -1} onClick={() => onPeriod(key)}
          onKeyDown={(event) => moveSelection(event, index, ORDER.length, (next) => onPeriod(ORDER[next]))}>
          {PERIOD_SHORT[key]}
        </button>
      ))}
    </div>
  );
}

// 汇总所有分项，用同一份提示提供精确值。
function tokenBreakdown(summary: Summary) {
  return {
    total: count(summary.total_tokens),
    in: count(summary.counts.input + summary.counts.cache_creation),
    cached: count(summary.counts.cache_read),
    out: count(summary.counts.output),
    reasoning: count(summary.counts.reasoning),
  };
}

// 按输入、缓存读取、输出三列展示用量。
function TokenSplit({ summary, onClose }: { summary: Summary; onClose: () => void }) {
  const parts = [
    { key: "in", value: summary.counts.input + summary.counts.cache_creation, label: "hdr.io.in" },
    { key: "cached", value: summary.counts.cache_read, label: "hdr.io.cached" },
    { key: "out", value: summary.counts.output, label: "hdr.io.out" },
  ] as const;
  const title = t("hdr.io.title", tokenBreakdown(summary));
  return (
    <div className="hero-io-row">
      {parts.map(({ key, value, label }) => {
        const part = splitHeadlineTokens(value);
        return (
          <button key={key} type="button" className="hero-io-col" data-io={key} title={title} onClick={onClose}>
            <span className="hero-io-line">
              <span className="hero-io-num">{part.value}</span>
              {part.unit && <span className="hero-io-unit">{part.unit}</span>}
            </span>
            <span className="hero-io-label">{t(label)}</span>
          </button>
        );
      })}
    </div>
  );
}

// 使用 Count-up 展示精确用量，数字按钮保留分项入口。
function TokenHeadline({ window, period, showMoney, frame, onOpen }: {
  window: Report[PeriodKey]; period: PeriodKey; showMoney: boolean;
  frame: ReturnType<typeof useCountUp>; onOpen: () => void;
}) {
  const { summary } = window;
  const total = count(summary.total_tokens);
  const final = formatOverviewTokens(summary.total_tokens);
  const shown = formatOverviewTokens(frame.value, summary.total_tokens);
  return (
    <div className="hero-main">
      <div className="hero-count-row">
        <span className="hero-count-arrow" aria-hidden="true"
          style={{ transform: `translateY(${frame.bob * 0.5}px) scale(${frame.arrow})` }}>
          <IconArrowUp size={19} />
        </span>
        <div className="hero-count-amount">
          <button type="button" className="hero-value" aria-label={`${total} tokens`}
            title={t("hdr.io.title", tokenBreakdown(summary))} onClick={onOpen}
            style={{ "--count-chars": final.value.length + final.unit.length } as CSSProperties}>
            <span className="hero-count-sizer" aria-hidden="true">
              {final.value}<small className="hero-count-unit">{final.unit}</small>
            </span>
            <span className="hero-count-number" aria-hidden="true" data-landed={frame.landed || undefined}
              style={{ opacity: frame.alpha, transform: `scale(${frame.pop})` }}>
              {shown.value}<small className="hero-count-unit">{shown.unit}</small>
            </span>
          </button>
          <span className="hero-unit-word">tokens</span>
        </div>
      </div>
      <HeadlineCaption window={window} period={period} showMoney={showMoney} />
      <div className="hero-count-progress" aria-hidden="true" style={{ opacity: frame.alpha }}>
        <span style={{ width: `${frame.fill * 100}%` }} />
      </div>
    </div>
  );
}

// 将费用和周期对比靠右对齐。
function HeadlineCaption({ window, period, showMoney }: {
  window: Report[PeriodKey]; period: PeriodKey; showMoney: boolean;
}) {
  const { summary } = window;
  const cost = money(summary.cost);
  return (
    <div className="hero-side">
      {showMoney && <span className="hero-cost"
        title={summary.credit_cost > 0 ? `${cost} · ${t("hdr.credit.title", { c: money(summary.credit_cost) })}` : cost}>
        {summary.credit_cost > 0 ? "≈" : ""}{cost}
      </span>}
      <DeltaChip pct={showMoney ? window.delta_cost_pct : window.delta_tokens_pct} caption={PERIOD_PREV[period]} />
    </div>
  );
}

// 提供稳定的摘要给辅助技术，跳过数字动画的中间帧。
function usageAnnouncement(window: Report[PeriodKey], period: PeriodKey, showMoney: boolean): string {
  const context = {
    label: PERIOD_LABEL[period],
    tokens: count(window.summary.total_tokens),
    prev: PERIOD_PREV[period],
    pct: signedPercent(showMoney ? window.delta_cost_pct : window.delta_tokens_pct),
  };
  const withCost = { ...context, cost: money(window.summary.cost) };
  return showMoney ? t("hdr.sr.money", withCost) : t("hdr.sr.tokens", context);
}

// 在概览组织周期、用量和输入输出明细。
export function OverviewSummary({ report, period, onPeriod }: { report: Report; period: PeriodKey; onPeriod: (period: PeriodKey) => void }) {
  const showMoney = useMoney();
  const [showSplit, setShowSplit] = useState(false);
  const hero = useRef<HTMLDivElement>(null);
  const restoreFocus = useRef(false);
  const window = report[period];
  // 分项切换不重新开始同一周期的 Count-up。
  const frame = useCountUp(window.summary.total_tokens, period);

  // 总览和分项切换后，将键盘焦点移到当前可用的用量按钮。
  useLayoutEffect(() => {
    if (!restoreFocus.current) return;
    restoreFocus.current = false;
    hero.current?.querySelector("button")?.focus();
  }, [showSplit]);

  // 记录是否由键盘打开或关闭分项。
  const toggleSplit = () => {
    restoreFocus.current = Boolean(document.activeElement?.matches(".hero button:focus-visible"));
    setShowSplit((shown) => !shown);
  };

  return (
    <section className="overview-summary" aria-labelledby="overview-summary-title">
      <div className="overview-summary-heading">
        <h1 id="overview-summary-title">{t("hdr.total", { period: PERIOD_LABEL[period] })}</h1>
        <PeriodControl period={period} onPeriod={onPeriod} />
      </div>
      <div className="hero" ref={hero}>
        {showSplit ? <TokenSplit summary={window.summary} onClose={toggleSplit} />
          : <TokenHeadline window={window} period={period} showMoney={showMoney} frame={frame} onOpen={toggleSplit} />}
        <span className="sr" aria-live="polite">{usageAnnouncement(window, period, showMoney)}</span>
      </div>
      <div className="overview-summary-meta">
        <HeaderStats summary={window.summary} />
        <HeaderCredits summary={window.summary} showMoney={showMoney} />
      </div>
    </section>
  );
}

// 提供四页导航、设备范围和设置入口。
export function Header({ report, page, onPage, onClose, onOpenSettings, sync, scope, onScope, scopeMenuOpen, onScopeMenuOpen, serverOrigins, syncDevices, now }: Props) {
  return (
    <header className="hdr">
      <div className="hdr-bar">
        <img className="brand" src={kimoTokenWordmark} alt="kimo" width={240} height={82} />
        <div className="hdr-actions">
          {page !== "quota" && <ScopeDropdown
            scope={scope}
            onScope={onScope}
            machines={report.machines ?? []}
            syncs={report.syncs}
            devices={syncDevices?.devices}
            isSecondary={syncDevices?.role === "secondary"}
            now={now}
            open={scopeMenuOpen}
            onOpen={onScopeMenuOpen}
            serverOrigins={serverOrigins}
          />}
          <span className="header-local">{t("scope.local")}</span>
          <button type="button" className="panel-settings" title={t("status.settings")}
            aria-label={t("status.settings")} onClick={onOpenSettings}>
            <IconSettings size={15} />
          </button>
          {onClose && <button type="button" className="panel-close" onClick={onClose}
            title={t("hdr.close")} aria-label={t("hdr.close")}><IconClose size={13} /></button>}
        </div>
      </div>
      <div className="hdr-sync">
        {page !== "quota" && scope.kind === "all" && sync ? (
          <span className="sync" data-stale={sync.stale || undefined} title={syncTip(sync)}>
            <IconRefresh size={11} />
            {t("hdr.sync")}
            <span className="dot-sep" aria-hidden="true" />
            {sync.mergedToday ? (
              <>
                <span>{sync.latest.origin}</span>
                <span className="dot-sep" aria-hidden="true" />
                <span>{sync.age}</span>
                {sync.latest.file === "device-sync" ? (
                  // The live session channel has no row count: its exchange
                  // is a re-verified aggregate snapshot, so a "0 rows" here
                  // would be a number about nothing.
                  null
                ) : (
                  <>
                    <span className="dot-sep" aria-hidden="true" />
                    <span className="num">{t("hdr.sync.rows", { n: count(sync.latest.rows) })}</span>
                  </>
                )}
                {sync.more > 0 ? <span className="sync-more">{t("hdr.sync.more", { n: sync.more })}</span> : null}
              </>
            ) : (
              <span>{t("hdr.sync.idle")}</span>
            )}
          </span>
        ) : null}
      </div>
      <PanelTabs page={page} onPage={onPage} />
    </header>
  );
}
