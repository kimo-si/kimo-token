import { useEffect, useState } from "react";
import { PANEL_THEME_KEYS } from "../types";
import type { BubbleSkin, DiagnosticsResponse, McpClientConfig, NotifyState, NotifyTierKey, PanelSettings, ProbeTool, SyncDevicesSnapshot, ThemeKey, TrayMode, UpdateStatus } from "../types";
import { bridge, inTauri, isMac, isWindows } from "../lib/bridge";
import { t, type StrKey } from "../lib/i18n";
import { toolDisplay, TRAY_MODE_LABEL, TRAY_MODES } from "../lib/format";
import { IconClose } from "./Icons";
import { PetSkinDropdown } from "./PetSkinDropdown";

const INTERVALS: { secs: number; label: () => string }[] = [
  { secs: 15, label: () => t("set.15s") },
  { secs: 30, label: () => t("set.30s") },
  { secs: 60, label: () => t("set.1m") },
  { secs: 300, label: () => t("set.5m") },
];

const PANEL_SKIN_LABELS: Record<ThemeKey, StrKey> = {
  dark: "set.theme.dark",
  light: "set.theme.light",
  cobalt: "set.theme.cobalt",
  violet: "set.theme.violet",
  amber: "set.theme.amber",
  rose: "set.theme.rose",
  charcoal: "set.theme.charcoal",
  system: "set.theme.system",
};

// 使用各套皮肤自己的颜色变量绘制预览，预览随配色一起更新。
function PanelSkinOptions({ theme, busy, failed, onChange }: {
  theme: ThemeKey; busy: boolean; failed: boolean; onChange: (skin: ThemeKey) => void;
}) {
  return (
    <div className="sheet-row panel-skin-row">
      <div className="sheet-label" id="panel-skin-label">{t("set.panel-skin")}</div>
      {failed && <div className="sheet-hint" role="alert">{t("set.panel-skin.failed")}</div>}
      <div className="panel-skin-options" role="radiogroup" aria-labelledby="panel-skin-label" aria-busy={busy}>
        {PANEL_THEME_KEYS.map((skin) => (
          <label className="panel-skin-choice" key={skin}>
            <input type="radio" name="panel-skin" value={skin} checked={theme === skin}
              aria-disabled={busy || undefined} onChange={() => onChange(skin)} />
            <span className="panel-skin-tile">
              <span className="panel-skin-preview" data-skin={skin} aria-hidden="true"><i /><i /><i /><i /></span>
              <span className="panel-skin-name">{t(PANEL_SKIN_LABELS[skin])}</span>
            </span>
          </label>
        ))}
      </div>
    </div>
  );
}

const TIERS: { key: NotifyTierKey; label: () => string }[] = [
  { key: "both", label: () => t("set.tiers.both") },
  { key: "exhausted", label: () => t("set.tiers.exhausted") },
  { key: "off", label: () => t("set.tiers.off") },
];

const PLAN_EXPIRY_DAYS: { value: string; label: () => string }[] = [
  { value: "", label: () => t("set.plan-expiry.off") },
  { value: "5", label: () => t("set.plan-expiry.days", { n: 5 }) },
  { value: "3", label: () => t("set.plan-expiry.days", { n: 3 }) },
  { value: "1", label: () => t("set.plan-expiry.days", { n: 1 }) },
];

async function copyText(value: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(value);
    return true;
  } catch {
    const node = document.createElement("textarea");
    node.value = value;
    node.style.position = "fixed";
    node.style.opacity = "0";
    document.body.appendChild(node);
    node.select();
    const ok = document.execCommand("copy");
    node.remove();
    return ok;
  }
}

type Tab = "general" | "pet" | "alert" | "advanced" | "about";
// Label keys are literals rather than a `set.tab.${k}` template so the parity
// gate (scripts/check-i18n-parity.py) can see each tab's copy is reached.
const TABS: { key: Tab; label: () => string }[] = [
  { key: "general", label: () => t("set.tab.general") },
  { key: "pet", label: () => t("set.tab.pet") },
  { key: "alert", label: () => t("set.tab.alert") },
  { key: "advanced", label: () => t("set.tab.advanced") },
  { key: "about", label: () => t("set.tab.about") },
];
type List = "probe" | "mute";

/** The engine's own name when it has one (妙手, not the frontend's fallback);
 *  the probes with no log adapter — copilot, gemini — fall back to the map. */
function nameOf(p: ProbeTool, displays: Record<string, string>): string {
  return displays[p.id] ?? toolDisplay(p.id);
}

/**
 * The panel's knobs as a bottom sheet of a fixed 65%: four tabs, and the two
 * settings whose value is a list of tools open a page inside the same sheet
 * (`›`) rather than expanding in place — an inline expansion would push the
 * sheet past 460 px of body and take the tab strip off screen with it.
 *
 * The height is written, not capped: a `max-height` sheet changes height when a
 * tab is shorter than the last, and a panel that jumps under the cursor loses
 * the muscle memory the whole sheet depends on. The footer (version · update ·
 * quit) sits outside the tabs for the same reason — those are state and a
 * global action, not a group of settings.
 */
export function SettingsSheet({
  onClose,
  onEmptyTools,
  onRefreshSecs,
  onMoney,
  onQuotaPolling,
  onTrayModeChange,
  displays,
}: {
  onClose: () => void;
  onEmptyTools: (on: boolean) => void;
  /** The panel times its "updates stopped" line against this cadence. */
  onRefreshSecs: (secs: number) => void;
  /** Dollar figures on/off, so the panel repaints without waiting for a report. */
  onMoney: (on: boolean) => void;
  onQuotaPolling: (on: boolean) => void;
  onTrayModeChange: (mode: TrayMode) => Promise<void>;
  displays: Record<string, string>;
}) {
  const [settings, setSettings] = useState<PanelSettings | null>(null);
  const [skinBusy, setSkinBusy] = useState(false);
  const [skinError, setSkinError] = useState<string | null>(null);
  const [panelSkinBusy, setPanelSkinBusy] = useState(false);
  const [panelSkinError, setPanelSkinError] = useState(false);
  const [update, setUpdate] = useState<UpdateStatus | null>(null);
  const [updateBusy, setUpdateBusy] = useState(false);
  // 0–100 while the artifact streams in; the button reads the set.dl label.
  const [progress, setProgress] = useState<number | null>(null);
  const [tab, setTab] = useState<Tab>("general");
  const [list, setList] = useState<List | null>(null);
  const [quotaPollingBusy, setQuotaPollingBusy] = useState(false);
  const [quotaPollingError, setQuotaPollingError] = useState(false);
  const [probes, setProbes] = useState<ProbeTool[]>([]);
  const [showAll, setShowAll] = useState(false);
  const [notify, setNotify] = useState<NotifyState>("unknown");
  const [petBusy, setPetBusy] = useState(false);
  const [petError, setPetError] = useState<string | null>(null);
  const [trayModeBusy, setTrayModeBusy] = useState(false);
  const [trayModeError, setTrayModeError] = useState(false);
  const [ttftBusy, setTtftBusy] = useState(false);
  const [ttftError, setTtftError] = useState(false);
  const [performanceBusy, setPerformanceBusy] = useState(false);
  const [performanceError, setPerformanceError] = useState(false);
  const [mcpSync, setMcpSync] = useState<SyncDevicesSnapshot | null>(null);
  const [mcpStatus, setMcpStatus] = useState<DiagnosticsResponse | null>(null);
  const [mcpConfig, setMcpConfig] = useState<McpClientConfig | null>(null);
  const [mcpBusy, setMcpBusy] = useState(false);
  const [mcpError, setMcpError] = useState<string | null>(null);
  const [mcpCopied, setMcpCopied] = useState(false);

  useEffect(() => bridge.onUpdateProgress((p) => setProgress(p.percent)), []);

  useEffect(() => {
    let alive = true;
    void bridge.panelSettings().then((s) => {
      if (!alive) return;
      setSettings(s);
      onRefreshSecs(s.refresh_secs);
      // 打开设置就问一次（开关开着时）：底部的版本行不必等到用户切开关才有答案。
      if (s.auto_update_check && s.version !== "dev") {
        setUpdateBusy(true);
        void bridge
          .checkUpdate()
          .then((status) => {
            // quiet = 检查失败或已是最新：没有可行动的信息就不渲染任何行。
            if (alive) setUpdate(status.phase === "available" || status.phase === "downloaded" ? status : null);
          })
          .catch(() => alive && setUpdate(null))
          .finally(() => alive && setUpdateBusy(false));
      }
    });
    void bridge.notifyStatus().then((s) => alive && setNotify(s));
    void bridge.probeTools().then((p) => alive && setProbes(p));
    return () => {
      alive = false;
    };
  }, []);

  const refreshMcp = async () => {
    setMcpBusy(true);
    setMcpError(null);
    try {
      const [sync, diagnostics] = await Promise.all([
        bridge.syncDevices({ action: "status" }),
        bridge.diagnosticsSession({ action: "status" }),
      ]);
      setMcpSync(sync);
      setMcpStatus(diagnostics);
      try {
        setMcpConfig(await bridge.mcpClientConfig());
      } catch (error) {
        setMcpConfig(null);
        setMcpError(error instanceof Error ? error.message : String(error));
      }
      if (diagnostics.error) setMcpError(diagnostics.error.message);
    } catch (error) {
      setMcpError(error instanceof Error ? error.message : String(error));
    } finally {
      setMcpBusy(false);
    }
  };

  useEffect(() => {
    if (tab !== "advanced" || list !== null) return;
    void refreshMcp();
  }, [tab, list]);

  const setMcpEnabled = async (enabled: boolean) => {
    if (mcpBusy) return;
    setMcpBusy(true);
    setMcpError(null);
    try {
      const response = await bridge.diagnosticsSession({ action: "configure", enabled });
      setMcpStatus(response);
      if (enabled) {
        try {
          setMcpConfig(await bridge.mcpClientConfig());
        } catch (error) {
          setMcpConfig(null);
          setMcpError(error instanceof Error ? error.message : String(error));
        }
      }
      if (response.error) setMcpError(response.error.message);
    } catch (error) {
      setMcpError(error instanceof Error ? error.message : String(error));
    } finally {
      setMcpBusy(false);
    }
  };

  const patch = (fn: (s: PanelSettings) => PanelSettings) =>
    setSettings((s) => (s ? fn(s) : s));

  const setRefresh = (secs: number) => {
    patch((s) => ({ ...s, refresh_secs: secs }));
    onRefreshSecs(secs);
    void bridge.setRefreshSecs(secs);
  };

  // 保存面板皮肤，并同步设置页中的选中状态。
  const setPanelSkin = async (skin: ThemeKey) => {
    if (panelSkinBusy || settings?.theme === skin) return;
    setPanelSkinBusy(true);
    setPanelSkinError(false);
    try {
      // 等待设置写入成功后再更新选择状态。
      await bridge.setTheme(skin);
      patch((s) => ({ ...s, theme: skin }));
    } catch {
      setPanelSkinError(true);
    } finally {
      setPanelSkinBusy(false);
    }
  };

  const setMoney = (on: boolean) => {
    patch((s) => ({ ...s, show_money: on }));
    onMoney(on);
    void bridge.setShowMoney(on);
  };

  // 等待菜单栏设置保存后，再同步选择器状态。
  const setTrayMode = async (mode: TrayMode) => {
    if (trayModeBusy || settings?.tray_mode === mode) return;
    setTrayModeBusy(true);
    setTrayModeError(false);
    try {
      await onTrayModeChange(mode);
      patch((s) => ({ ...s, tray_mode: mode }));
    } catch {
      setTrayModeError(true);
    } finally {
      setTrayModeBusy(false);
    }
  };

  const setTtft = async (on: boolean) => {
    if (ttftBusy || settings?.ttft_enabled === on) return;
    setTtftBusy(true);
    setTtftError(false);
    try {
      await bridge.setTtftEnabled(on);
      patch((s) => ({ ...s, ttft_enabled: on }));
    } catch {
      setTtftError(true);
    } finally {
      setTtftBusy(false);
    }
  };

  const setPerformanceMonitoring = async (on: boolean) => {
    if (performanceBusy || settings?.performance_monitoring === on) return;
    setPerformanceBusy(true);
    setPerformanceError(false);
    try {
      await bridge.setPerformanceMonitoring(on);
      patch((s) => ({ ...s, performance_monitoring: on }));
    } catch {
      setPerformanceError(true);
    } finally {
      setPerformanceBusy(false);
    }
  };

  const setHideUnusedTools = (on: boolean) => {
    patch((s) => ({ ...s, hide_unused_tools: on }));
    onEmptyTools(on);
    void bridge.setHideUnusedTools(on);
  };

  const setBubble = (on: boolean) => {
    patch((s) => ({ ...s, bubble_enabled: on }));
    void bridge.setBubbleEnabled(on);
  };

  const setBubbleSkin = async (skin: BubbleSkin) => {
    if (skinBusy || settings?.bubble_skin === skin) return;
    setSkinBusy(true);
    setSkinError(null);
    try {
      await bridge.setBubbleSkin(skin);
      patch((s) => ({ ...s, bubble_skin: skin }));
    } catch (e) {
      setSkinError(String(e));
    } finally {
      setSkinBusy(false);
    }
  };

  const setPetPreference = async (changes: Partial<PanelSettings["pet_companion"]>) => {
    if (!settings || petBusy) return;
    const next = { ...settings.pet_companion, ...changes };
    setPetBusy(true); setPetError(null);
    try { await bridge.setPetPreferences(next); patch((s) => ({ ...s, pet_companion: next })); }
    catch { setPetError(t("pet.save.failed")); }
    finally { setPetBusy(false); }
  };

  const setAutostart = (on: boolean) => {
    patch((s) => ({ ...s, autostart: on }));
    void bridge.setAutostart(on);
  };

  const setHostExitPause = (on: boolean) => {
    patch((s) => ({ ...s, host_exit_pause: on }));
    void bridge.setHostExitPause(on);
  };

  /* 其他厂商的自动签到切换保留，Codex 版不挂载该操作。
  const setAutoCheckin = (on: boolean) => {
    patch((s) => ({ ...s, auto_checkin: on }));
    void bridge.setAutoCheckin(on);
  };
  */

  // 保存总开关后同步配额页和逐工具状态。
  const setQuotaPolling = async (on: boolean) => {
    if (quotaPollingBusy) return;
    setQuotaPollingBusy(true);
    setQuotaPollingError(false);
    try {
      // 等待采集设置写入成功，避免界面显示未生效的状态。
      await bridge.setQuotaPolling(on);
      patch((s) => ({ ...s, quota_polling: on }));
      setProbes((items) => items.map((item) => ({ ...item, polling: on && !isOff(item.id) })));
      onQuotaPolling(on);
    } catch {
      setQuotaPollingError(true);
    } finally {
      setQuotaPollingBusy(false);
    }
  };

  const isOff = (id: string) => (settings?.quota_probes_off ?? []).includes(id);

  const setToolPolling = (id: string, on: boolean) => {
    patch((s) => ({
      ...s,
      quota_probes_off: on ? s.quota_probes_off.filter((t0) => t0 !== id) : [...s.quota_probes_off, id].sort(),
    }));
    setProbes((ps) => ps.map((p) => (p.id === id ? { ...p, polling: (settings?.quota_polling ?? true) && on } : p)));
    void bridge.setToolPolling(id, on);
  };

  const setTiers = (tiers: NotifyTierKey) => {
    patch((s) => ({ ...s, notify_tiers: tiers }));
    void bridge.setNotifyTiers(tiers);
  };

  const setNotifyDevicePresence = (on: boolean) => {
    patch((s) => ({ ...s, notify_device_presence: on }));
    void bridge.setNotifyDevicePresence(on);
  };

  const setNotifySyncStatus = (on: boolean) => {
    patch((s) => ({ ...s, notify_sync_status: on }));
    void bridge.setNotifySyncStatus(on);
  };

  const setPlanExpiryNotifyDays = (value: string) => {
    const days = value ? Number(value) : null;
    patch((s) => ({ ...s, plan_expiry_notify_days: days }));
    void bridge.setPlanExpiryNotifyDays(days);
  };

  const isMuted = (id: string) => (settings?.notify_muted ?? []).includes(id);

  const setToolMuted = (id: string, on: boolean) => {
    patch((s) => ({
      ...s,
      notify_muted: on ? [...new Set([...s.notify_muted, id])].sort() : s.notify_muted.filter((t0) => t0 !== id),
    }));
    void bridge.setToolMuted(id, on);
  };

  const setAutoUpdateCheck = (on: boolean) => {
    patch((s) => ({ ...s, auto_update_check: on }));
    void bridge.setAutoUpdateCheck(on);
    // 开启的一刻就问一次：用户不必等到下次启动才知道有没有新版。
    if (on) {
      setUpdateBusy(true);
      void bridge
        .checkUpdate()
        .then((status) => {
          // quiet = 检查失败或已是最新：没有可行动的信息就不渲染任何行，
          // 否则会留下一个空白的弹窗条（用户报修的那块）。
          setUpdate(status.phase === "available" || status.phase === "downloaded" ? status : null);
        })
        .catch(() => setUpdate(null))
        .finally(() => setUpdateBusy(false));
    } else {
      setUpdate(null);
    }
  };

  /** 关于页的“立即检查”：只做版本比较。“下载并安装 / 重启”是检查结果
   *  在底栏升起的按钮，检查本身上是当前版本时绝不能换包。 */
  const checkNow = async () => {
    setUpdateBusy(true);
    try {
      const status = await bridge.checkUpdate();
      // message 为空的应答没有可渲染的行；其余（含 quiet 的失败说明）都进底栏。
      setUpdate(status.message ? status : null);
    } catch (e) {
      setUpdate({ phase: "error", message: String(e), version: null });
    } finally {
      setUpdateBusy(false);
    }
  };

  const runUpdate = async () => {
    setUpdateBusy(true);
    try {
      const downloaded = await bridge.downloadUpdate();
      setUpdate(downloaded);
      if (downloaded.phase === "downloaded") {
        await bridge.installUpdate();
      }
    } catch (e) {
      setUpdate({ phase: "error", message: String(e), version: null });
    } finally {
      setUpdateBusy(false);
      setProgress(null);
    }
  };

  const quit = () => {
    void bridge.quit();
  };

  /** `›` opens a page inside this sheet; `↗` opens a different sheet. The two
   *  glyphs are the whole contract and must not be interchanged. */
  const navRow = (key: List, label: string, hint: string, count: string) => (
    <div
      className="sheet-row sheet-row-nav"
      role="button"
      tabIndex={0}
      onClick={() => setList(key)}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          setList(key);
        }
      }}
    >
      <div>
        <div className="sheet-label">{label}</div>
        <div className="sheet-hint">{hint}</div>
      </div>
      <div className="sheet-nav-end">
        <span className="sheet-value">{count}</span>
        <span className="sheet-chev" aria-hidden="true">
          ›
        </span>
      </div>
    </div>
  );

  const switchRow = (
    label: string,
    hint: string | null,
    on: boolean,
    flip: (next: boolean) => void,
    disabled = false,
    tag: string | null = null,
  ) => (
    <div className={`sheet-row${disabled ? " sheet-row-locked" : ""}`}>
      <div>
        <div className="sheet-label">
          {label}
          {tag ? <span className="sheet-tag">{tag}</span> : null}
        </div>
        {hint ? <div className="sheet-hint">{hint}</div> : null}
      </div>
      <button
        type="button"
        role="switch"
        className="switch"
        aria-checked={on}
        aria-label={label}
        disabled={disabled}
        onClick={() => flip(!on)}
      />
    </div>
  );

  const offCount = probes.filter((p) => settings?.quota_probes_off.includes(p.id)).length;
  const mutedCount = settings?.notify_muted.length ?? 0;
  const local = probes.filter((p) => p.answered_here);
  const listed = showAll ? probes : local;
  const listedOn = list === "probe" ? listed.filter((p) => p.polling).length : listed.filter((p) => !isMuted(p.id)).length;
  const probeLocked = settings ? !settings.quota_polling : false;
  const muteLocked = notify !== "granted";
  // MCP is a local debugging capability. Sync role only controls whether this
  // device can issue a remote grant; it must not disable the local switch on
  // a secondary device.
  const mcpEnabled = mcpStatus?.enabled === true;
  const mcpControlEnabled = mcpStatus?.lan_control_enabled === true;
  const mcpHint = !mcpSync
    ? t("set.mcp.loading")
    : t("set.mcp.local.hint");

  const copyMcpConfig = async () => {
    if (!mcpConfig?.config_json) return;
    const ok = await copyText(mcpConfig.config_json);
    if (ok) {
      setMcpCopied(true);
      window.setTimeout(() => setMcpCopied(false), 1600);
    }
  };

  const setMcpControlEnabled = async (enabled: boolean) => {
    if (mcpBusy || !mcpEnabled) return;
    setMcpBusy(true);
    setMcpError(null);
    try {
      const response = await bridge.diagnosticsSession({ action: "configure_lan_control", enabled });
      setMcpStatus(response);
      const config = await bridge.mcpClientConfig();
      setMcpConfig(config);
      if (response.error) setMcpError(response.error.message);
      else if (config.reason && enabled) setMcpError(config.reason);
    } catch (error) {
      setMcpError(error instanceof Error ? error.message : String(error));
    } finally {
      setMcpBusy(false);
    }
  };

  const body = () => {
    if (!settings) return null;
    if (list === "probe" || list === "mute") {
      const probing = list === "probe";
      const locked = probing ? probeLocked : muteLocked;
      return (
        <>
          {/* The count is this page's live state — under a lock nothing is live, and
              a "0 polling" line would contradict the greyed switches below it. */}
          {locked ? (
            <p className="sheet-locknote">{probing ? t("set.probe.locked") : t("set.mute.locked")}</p>
          ) : (
            <p className="sheet-summary num">
              {t(probing ? "set.probe.summary" : "set.mute.summary", {
                n: listed.length,
                on: listedOn,
                off: listed.length - listedOn,
              })}
            </p>
          )}
          <div className={`sheet-list${locked ? " sheet-list-locked" : ""}`}>
            {probing
              ? switchRow(t("set.pause"), t("set.pause.hint"), settings.host_exit_pause, setHostExitPause, locked)
              : null}
            {listed.map((p) =>
              probing
                ? switchRow(
                    nameOf(p, displays),
                    null,
                    !isOff(p.id),
                    (next) => setToolPolling(p.id, next),
                    locked,
                    // The badge is the page's one piece of real advice: these
                    // four are the probes nothing else can stop.
                    p.host_gated ? null : t("set.probe.nohost"),
                  )
                : switchRow(nameOf(p, displays), null, isMuted(p.id), (next) => setToolMuted(p.id, next), locked),
            )}
            <button type="button" className="sheet-scope" onClick={() => setShowAll((v) => !v)}>
              {showAll
                ? t("set.probe.showlocal", { n: local.length })
                : t("set.probe.showall", { n: probes.length, m: probes.length - local.length })}
            </button>
          </div>
        </>
      );
    }
    return (
      <>
        <div className="sheet-tabs" role="tablist" onKeyDown={(e) => {
          const at = TABS.findIndex((x) => x.key === tab);
          const step = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
          if (!step) return;
          e.preventDefault();
          setTab(TABS[(at + step + TABS.length) % TABS.length].key);
        }}>
          {TABS.map(({ key, label }) => (
            <button
              key={key}
              type="button"
              role="tab"
              className="sheet-tab"
              aria-selected={tab === key}
              onClick={() => setTab(key)}
            >
              {label()}
            </button>
          ))}
        </div>
        {tab === "general" ? (
          <>
            <div className="sheet-row">
              <div>
                <div className="sheet-label">{t("set.refresh")}</div>
                <div className="sheet-hint">{t("set.refresh.hint")}</div>
              </div>
              <div className="seg" role="radiogroup" aria-label={t("set.interval.a11y")}>
                {INTERVALS.map((i) => (
                  <button
                    key={i.secs}
                    type="button"
                    role="radio"
                    className="seg-btn"
                    aria-checked={settings.refresh_secs === i.secs}
                    onClick={() => setRefresh(i.secs)}
                  >
                    {i.label()}
                  </button>
                ))}
              </div>
            </div>
            <PanelSkinOptions theme={settings.theme} busy={panelSkinBusy} failed={panelSkinError}
              onChange={(skin) => void setPanelSkin(skin)} />
            {isMac || !inTauri ? (
              <div className="sheet-row">
                <div>
                  <label className="sheet-label" htmlFor="menu-bar-display">{t("set.tray.mode")}</label>
                  <div className="sheet-hint" id="menu-bar-display-hint">
                    {trayModeError ? <span role="alert">{t("set.tray.failed")}</span> : t("set.tray.hint")}
                  </div>
                </div>
                <select
                  id="menu-bar-display"
                  className="sheet-select"
                  aria-describedby="menu-bar-display-hint"
                  value={settings.tray_mode}
                  disabled={trayModeBusy}
                  onChange={(event) => void setTrayMode(event.currentTarget.value as TrayMode)}
                >
                  {TRAY_MODES.map((mode) => <option key={mode} value={mode}>{TRAY_MODE_LABEL[mode]}</option>)}
                </select>
              </div>
            ) : null}
            {switchRow(t("set.money"), t("set.money.hint"), settings.show_money, setMoney)}
            {switchRow(t("set.hideunused.tools"), t("set.hideunused.tools.hint"), settings.hide_unused_tools, setHideUnusedTools)}
          </>
        ) : null}
        {tab === "pet" ? (
          <>
            {isWindows || isMac ? (
              <>
                {switchRow(t("set.bubble"), t("set.bubble.hint"), settings.bubble_enabled, setBubble)}
                <div className="sheet-row">
                  <div>
                    <label className="sheet-label" htmlFor="bubble-skin">{t("set.bubble.skin")}</label>
                    <div className="sheet-hint" id="bubble-skin-hint">{t("set.bubble.skin.hint")}</div>
                  </div>
                  <PetSkinDropdown
                    value={settings.bubble_skin}
                    disabled={skinBusy}
                    onChange={(skin) => void setBubbleSkin(skin)}
                  />
                </div>
                {switchRow(t("set.pet.greetings"), t("set.pet.greetings.hint"), settings.pet_companion.greetings,
                  (on) => void setPetPreference({ greetings:on }), petBusy || !settings.bubble_enabled)}
                {switchRow(t("set.pet.rest"), t("set.pet.rest.hint"), settings.pet_companion.rest_reminders,
                  (on) => void setPetPreference({ rest_reminders:on }), petBusy || !settings.bubble_enabled)}
                <div className="sheet-row">
                  <div><label className="sheet-label" htmlFor="pet-rest-interval">{t("set.pet.interval")}</label>
                    <div className="sheet-hint">{t("set.pet.interval.hint")}</div></div>
                  <select id="pet-rest-interval" className="sheet-select" value={settings.pet_companion.rest_interval_minutes}
                    disabled={petBusy || !settings.bubble_enabled || !settings.pet_companion.rest_reminders}
                    onChange={(event) => void setPetPreference({ rest_interval_minutes:Number(event.target.value) })}>
                    {[30,45,60,90,120].map((minutes) => <option key={minutes} value={minutes}>{t("set.pet.minutes", { minutes })}</option>)}
                  </select>
                </div>
                {switchRow(t("set.pet.motions"), t("set.pet.motions.hint"), settings.pet_companion.motions,
                  (on) => void setPetPreference({ motions:on }), petBusy || !settings.bubble_enabled)}
                <div className="sheet-row">
                  <div><div className="sheet-label">{t("set.pet.preview")}</div><div className="sheet-hint">{t("set.pet.preview.hint")}</div></div>
                  <button type="button" className="sheet-action" disabled={!settings.bubble_enabled || petBusy}
                    onClick={() => { void bridge.previewPetNotice().then((notice) => { if (!notice) setPetError(t("pet.preview.wait")); }).catch(() => setPetError(t("pet.save.failed"))); }}>
                    {t("set.pet.preview.button")}
                  </button>
                </div>
                {petError && <p className="sheet-hint sheet-pet-error" role="alert">{petError}</p>}
              </>
            ) : (
              <p className="sheet-hint">{t("set.pet.none")}</p>
            )}
          </>
        ) : null}
        {tab === "alert" ? (
          <>
            <div className="sheet-row">
              <div>
                <div className="sheet-label">{t("set.notify")}</div>
                <div className="sheet-hint">{t("set.notify.hint")}</div>
              </div>
              {notify === "granted" ? (
                <span className="sheet-status">
                  <span className="sheet-status-dot" aria-hidden="true" />
                  {t("set.notify.granted")}
                </span>
              ) : (
                <button
                  type="button"
                  className="sheet-action sheet-action-warn"
                  onClick={() => void bridge.notifyEnable().then((s) => setNotify(s))}
                >
                  {t("set.notify.enable")}
                </button>
              )}
            </div>
            <div className="sheet-block">
              <div className="sheet-label">{t("set.tiers")}</div>
              <div className="sheet-hint">{t("set.tiers.hint")}</div>
              <div className="seg seg-wide" role="radiogroup" aria-label={t("set.tiers")}>
                {TIERS.map((tr) => (
                  <button
                    key={tr.key}
                    type="button"
                    role="radio"
                    className="seg-btn"
                    aria-checked={settings.notify_tiers === tr.key}
                    onClick={() => setTiers(tr.key)}
                  >
                    {tr.label()}
                  </button>
                ))}
              </div>
            </div>
            <div className="sheet-block">
              <div className="sheet-label">{t("set.plan-expiry")}</div>
              <div className="sheet-hint">{t("set.plan-expiry.hint")}</div>
              <div className="seg seg-wide" role="radiogroup" aria-label={t("set.plan-expiry")}>
                {PLAN_EXPIRY_DAYS.map((option) => {
                  const current = settings.plan_expiry_notify_days == null ? "" : String(settings.plan_expiry_notify_days);
                  return (
                    <button
                      key={option.value || "off"}
                      type="button"
                      role="radio"
                      className="seg-btn"
                      aria-checked={current === option.value}
                      onClick={() => setPlanExpiryNotifyDays(option.value)}
                    >
                      {option.label()}
                    </button>
                  );
                })}
              </div>
            </div>
            {switchRow(t("set.notify.presence"), t("set.notify.presence.hint"), settings.notify_device_presence ?? true, setNotifyDevicePresence, mutedCount > 0 ? false : false)}
            {switchRow(t("set.notify.sync"), t("set.notify.sync.hint"), settings.notify_sync_status ?? true, setNotifySyncStatus)}
            {navRow("mute", t("set.muted"), t("set.muted.hint"), mutedCount ? t("set.muted.count", { n: mutedCount }) : t("set.none"))}
          </>
        ) : null}
        {tab === "advanced" ? (
          <>
            {switchRow(
              t("set.mcp"),
              mcpHint,
              mcpEnabled,
              (on) => void setMcpEnabled(on),
              mcpBusy,
            )}
            {switchRow(
              t("set.mcp.control"),
              mcpEnabled ? t("set.mcp.control.hint") : t("set.mcp.control.disabled"),
              mcpControlEnabled,
              (on) => void setMcpControlEnabled(on),
              mcpBusy || !mcpEnabled,
            )}
            <div className="sheet-block sheet-mcp-block">
              <div className="sheet-row sheet-mcp-heading">
                <div>
                  <div className="sheet-label">{t("set.mcp.client.config")}</div>
                  <div className="sheet-hint">{t("set.mcp.client.config.hint")}</div>
                </div>
              </div>
              {mcpEnabled && mcpConfig?.config_json ? (
                <div className="sheet-mcp-config" aria-label={t("set.mcp.client.config")}>
                  <textarea
                    className="sheet-mcp-json"
                    readOnly
                    value={mcpConfig.config_json}
                    aria-label={t("set.mcp.client.config")}
                    onFocus={(event) => event.currentTarget.select()}
                  />
                  <button type="button" className="sheet-action" onClick={() => void copyMcpConfig()}>
                    {mcpCopied ? t("set.mcp.client.copied") : t("set.mcp.client.copy")}
                  </button>
                  <p className="sheet-hint">
                    {mcpConfig.expires_at_ms
                      ? t("set.mcp.client.expires", { time: new Date(mcpConfig.expires_at_ms).toLocaleString() })
                      : t("set.mcp.client.persistent")}
                  </p>
                  {mcpConfig.reason ? <p className="sheet-hint" role="alert">{mcpConfig.reason}</p> : null}
                </div>
              ) : (
                <p className="sheet-hint" role={mcpError ? "alert" : undefined}>
                  {mcpError ?? t("set.mcp.client.unavailable")}
                </p>
              )}
            </div>
            {mcpError && mcpEnabled && mcpConfig?.config_json ? <p className="sheet-hint" role="alert">{mcpError}</p> : null}
            {switchRow(t("set.polling"), t("set.polling.hint"), settings.quota_polling, (on) => void setQuotaPolling(on), quotaPollingBusy)}
            {quotaPollingError && <p className="sheet-hint" role="alert">{t("quota.polling.error")}</p>}
            {switchRow(t("set.ttft"), t("set.ttft.hint"), settings.ttft_enabled, (on) => void setTtft(on), ttftBusy)}
            {ttftError ? <p className="sheet-hint" role="alert">{t("set.ttft.failed")}</p> : null}
            {/* Master off ⇒ no claim can fire at all, so the guarantee would be
                a promise the app cannot keep; the row locks like set.pause does. */}
            {/* Codex 无签到接口。
            {switchRow(t("set.checkin"), t("set.checkin.hint"), settings.auto_checkin, setAutoCheckin, !settings.quota_polling)}
            */}
            {navRow("probe", t("set.perprobe"), t("set.perprobe.hint"), offCount ? t("set.perprobe.off", { n: offCount }) : t("set.perprobe.all"))}
          </>
        ) : null}
        {tab === "about" ? (
          <>
            {switchRow(t("set.autostart"), null, settings.autostart, setAutostart)}
            {switchRow(t("set.autoupdate"), t("set.autoupdate.hint"), settings.auto_update_check, setAutoUpdateCheck)}
            {switchRow(
              t("set.performance"),
              t("set.performance.hint"),
              settings.performance_monitoring,
              (on) => void setPerformanceMonitoring(on),
              performanceBusy || !inTauri,
            )}
            {performanceError ? <p className="sheet-hint" role="alert">{t("set.performance.failed")}</p> : null}
            {/* The footer already signs the sheet "kimo-token v…" on every tab;
                a version row here said it twice. The build number — the one
                thing the footer doesn't carry — lives in the check row. */}
            <div className="sheet-row">
              <div>
                <div className="sheet-label">{t("set.check")}</div>
                <div className="sheet-hint">{t("set.check.hint", { b: settings.build })}</div>
              </div>
              <button type="button" className="sheet-action" disabled={updateBusy} onClick={() => void checkNow()}>
                {updateBusy ? t("set.busy") : t("set.check.now")}
              </button>
            </div>
            <div className="sheet-row">
              <div>
                <div className="sheet-label">{t("set.logs")}</div>
                <div className="sheet-hint">{t("set.logs.tip")}</div>
              </div>
              <button type="button" className="sheet-action" onClick={() => void bridge.openLogDir()}>
                {t("set.logs.open")}
              </button>
            </div>
          </>
        ) : null}
      </>
    );
  };

  return (
    <div className="sheet-backdrop" onClick={onClose}>
      <div
        className="sheet sheet-paged"
        role="dialog"
        aria-modal="true"
        aria-label={t("set.title")}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="sheet-hd">
          {/* A subpage promotes its back button into the title slot: one row
              instead of two, and "设置" was never information. */}
          <div className="sheet-hd-main">
            {list ? (
              <>
                <button type="button" className="sheet-back" onClick={() => setList(null)}>
                  <span aria-hidden="true">‹</span>
                  {t("set.back")}
                </button>
                <h2 className="sec-label">{list === "probe" ? t("set.perprobe") : t("set.muted")}</h2>
              </>
            ) : (
              <h2 className="sec-label">{t("set.title")}</h2>
            )}
          </div>
          <button type="button" className="sheet-close" aria-label={t("set.close.a11y")} onClick={onClose}>
            <IconClose size={12} />
          </button>
        </div>
        <div className="sheet-body">{body()}</div>
        {skinError ? (
          <div className="sheet-notice" role="alert">
            {skinError}
            <button type="button" className="sheet-close" aria-label={t("set.close.a11y")} onClick={() => setSkinError(null)}>
              <IconClose size={10} />
            </button>
          </div>
        ) : null}
        <p className="sheet-foot num">
          <span>kimo-token v{settings?.version ?? "…"}</span>
          {update ? (
            <span className="sheet-foot-update">
              <span className={update.phase === "error" ? "update-error" : undefined}>{update.message}</span>
              {update.phase === "available" || update.phase === "downloaded" ? (
                <button
                  type="button"
                  className="sheet-action"
                  disabled={updateBusy}
                  onClick={() => void runUpdate()}
                >
                  {updateBusy
                    ? progress != null
                      ? t("set.dl", { p: Math.round(progress) })
                      : t("set.busy")
                    : update.phase === "available"
                      ? t("set.download")
                      : t("set.reboot")}
                </button>
              ) : null}
            </span>
          ) : null}
          {bridge.live ? (
            <button type="button" className="quit-btn" onClick={quit} title={t("set.quit.hint")}>
              {t("set.quit")}
            </button>
          ) : null}
        </p>
      </div>
    </div>
  );
}
