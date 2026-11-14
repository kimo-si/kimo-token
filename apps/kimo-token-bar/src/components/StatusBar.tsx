import { Button } from "@heroui/react";
import { useMemo } from "react";
import type { PricingMeta, ServerStatus, ServerView, TrayMode } from "../types";
import { bridge, isWindows } from "../lib/bridge";
import { clockStamp, freshness, relativeTime, TRAY_MODE_LABEL } from "../lib/format";
import { useTicker } from "../lib/hooks";
import { t } from "../lib/i18n";
import type { UpdateInfo } from "../lib/update";
import { IconClock, IconDevices, IconRefresh, IconServer, IconStack, IconWarn } from "./Icons";

/** One dot for the whole fleet: the most actionable state wins. */
function aggServerStatus(servers: ServerView[]): ServerStatus {
  for (const s of ["syncing", "error", "warn", "ok"] as const) {
    if (servers.some((v) => v.status === s)) return s;
  }
  return "none";
}

interface ReportFreshness {
  publishedAt: number;
  restored?: { at: number } | null;
  pendingSince: number | null;
  refreshSecs: number;
  frozen?: { ago: string; afterSecs: number } | null;
}

// 状态栏显示当前用量的采集状态与时间。
function reportStatus({ restored, pendingSince, frozen, publishedAt, refreshSecs }: ReportFreshness, now: number) {
  if (restored) {
    return { tone: "restored", Icon: IconRefresh, text: t("status.restored"), detail: t("status.restored.tip", { a: relativeTime(restored.at, now) }) };
  }
  // 引擎没有返回时，刷新动画最多保留两分钟。
  if (pendingSince !== null && now - pendingSince < 120_000) {
    const text = t("status.refreshing");
    return { tone: "busy", Icon: IconRefresh, text, detail: text };
  }
  if (frozen) {
    return { tone: "frozen", Icon: IconWarn, text: t("status.frozen"), detail: t("status.frozen.tip", { s: frozen.afterSecs }) };
  }
  const detail = t("status.fresh.tip", { time: clockStamp(publishedAt, now), secs: refreshSecs });
  return { tone: "", Icon: IconClock, text: t("status.fresh.age", { age: freshness(publishedAt, now) }), detail };
}

// 用秒级时间更新状态栏读数。
function FreshnessStatus(props: ReportFreshness) {
  const tick = useTicker(1000);
  const now = useMemo(() => Date.now(), [tick]);
  const { tone, Icon, text, detail } = reportStatus(props, now);
  return (
    <span className={`fresh ${tone}`} title={detail}>
      <Icon size={12} />
      <span>{text}</span>
    </span>
  );
}

// 仅在有过期价格或可用更新时展示独立提示行。
function StatusNotices({ stale, update }: { stale: boolean; update?: UpdateInfo | null }) {
  if (!stale && !update) return null;
  return (
    <div className="status-notices">
      {stale && <span className="stale" title={t("status.stale.tip")}><IconWarn size={12} />{t("status.stale")}</span>}
      {update && (
        <Button size="sm" variant="ghost" type="button" className="update-chip num"
          render={(props) => <button {...props} title={t("status.update.go", { v: update.latest })} />}
          onPress={() => void bridge.openExternal(update.url)}>
          {t("status.update.chip", { v: update.latest })}
        </Button>
      )}
    </div>
  );
}

// 菜单栏模式作为轻量文字入口，完整名称仍可通过提示和辅助技术读取。
function TrayModeButton({ mode, onCycle }: { mode: TrayMode; onCycle: () => void }) {
  const label = TRAY_MODE_LABEL[mode];
  const description = `${t("status.tray.tip")} · ${label}`;
  return (
    <Button size="sm" variant="ghost" type="button" className="tray-mode" onPress={onCycle}
      aria-label={description} render={(props) => <button {...props} title={description} />}>
      <IconStack size={12} />
      <span>{label}</span>
    </Button>
  );
}

// 将服务器和设备同步入口放在同一组。
function StatusActions({ loading, servers, onOpenServers, onOpenSync, onRefresh }: {
  loading: boolean;
  servers: ServerView[];
  onOpenServers: () => void;
  onOpenSync: () => void;
  onRefresh: () => void;
}) {
  return (
    <div className="status-actions">
      <div className="status-tools">
        <Button size="sm" variant="ghost" type="button" className="icon-btn" data-status={aggServerStatus(servers)}
          onPress={onOpenServers} aria-label={t("srv.title")}
          render={(props) => <button {...props} title={servers.length ? t("srv.title.count", { n: servers.length }) : t("srv.title")} />}>
          <IconServer size={15} />
          <span className="srv-dot" aria-hidden="true" />
        </Button>
        <Button size="sm" variant="ghost" type="button" className="icon-btn" onPress={onOpenSync}
          render={(props) => <button {...props} title={t("sync.title")} />} aria-label={t("sync.title")}>
          <IconDevices size={15} />
        </Button>
      </div>
      <Button size="sm" variant="primary" type="button" className="refresh" onPress={onRefresh} isDisabled={loading} isPending={loading}>
        <IconRefresh size={12} />
        {t("status.refresh")}
      </Button>
    </div>
  );
}

/** Persistent status line: what the numbers on screen are, and the refresh control. */
export function StatusBar({
  pricing,
  loading,
  onRefresh,
  onOpenServers,
  onOpenSync,
  servers,
  tray,
  update,
  restored,
  publishedAt,
  pendingSince,
  refreshSecs,
  frozen,
}: ReportFreshness & {
  pricing: PricingMeta;
  loading: boolean;
  onRefresh: () => void;
  onOpenServers: () => void;
  onOpenSync: () => void;
  /** The fleet behind the server icon; the dot aggregates one state for all. */
  servers: ServerView[];
  /** Present only inside Tauri: a browser has no menu-bar title to configure. */
  tray?: { mode: TrayMode; onCycle: () => void } | null;
  /** A newer release answered the manifest; the chip opens its download page. */
  update?: UpdateInfo | null;
}) {
  return (
    <footer className="status">
      <StatusNotices stale={pricing.stale} update={update} />
      <div className="status-main">
        <div className="status-info">
          <FreshnessStatus
            publishedAt={publishedAt} restored={restored} pendingSince={pendingSince}
            refreshSecs={refreshSecs} frozen={frozen}
          />
          {tray && !isWindows && <TrayModeButton mode={tray.mode} onCycle={tray.onCycle} />}
        </div>
        <StatusActions
          loading={loading} servers={servers} onRefresh={onRefresh}
          onOpenServers={onOpenServers} onOpenSync={onOpenSync}
        />
      </div>
    </footer>
  );
}
