//! Menu-bar presence: the icon, the live title, and the right-click menu.

use tauri::menu::{CheckMenuItem, MenuBuilder, MenuEvent, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Rect, Runtime};
use tauri_plugin_autostart::ManagerExt;
use usage_core::pricing::PricingOptions;
use usage_core::{PricingMap, QuotaOrigin, QuotaView, Report};

use crate::commands::TrayState;
use crate::engine::{EngineChannel, Msg, Shared};
use crate::lang::{self, Lang};
use crate::panel;
use crate::settings::{Settings, TrayMode};

#[cfg(target_os = "macos")]
mod macos_menu;

pub const TRAY_ID: &str = "kimo-token";

/// Fixed-ish width: the status bar must not jitter on every refresh.
fn compact(n: f64) -> String {
    let abs = n.abs();
    if abs >= 1e9 {
        format!("{:.2}B", n / 1e9)
    } else if abs >= 1e6 {
        format!("{:.2}M", n / 1e6)
    } else if abs >= 1e3 {
        format!("{:.1}K", n / 1e3)
    } else {
        format!("{:.0}", n)
    }
}

fn money(n: f64) -> String {
    if n > 0.0 && n < 0.01 {
        "<$0.01".into()
    } else {
        format!("${:.2}", n)
    }
}

fn countdown(ms: i64, now_ms: i64) -> String {
    let diff = (ms - now_ms).max(0) / 60_000;
    let (d, h, m) = (diff / 1440, (diff % 1440) / 60, diff % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else {
        format!("{m}m")
    }
}

/// The newest still-open quota window with the highest usage.
///
/// `resets_at_ms == 0` means the source never reports a reset (Qoder's credit
/// plan), which is "always open" — a plan that has run out is the one bar that
/// belongs in the menu bar, so dropping it for lack of a countdown is wrong.
fn live_quota(report: &Report, now_ms: i64) -> Option<&QuotaView> {
    report
        .quotas
        .iter()
        .filter(|q| q.resets_at_ms == 0 || q.resets_at_ms > now_ms)
        .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))
}

// 取剩余比例最低的厂商时间窗，排除预算、签到标记和过期样本。
fn remaining_quota(quotas: &[QuotaView], now_ms: i64) -> Option<&QuotaView> {
    quotas.iter()
        .filter(|q| q.origin != QuotaOrigin::Budget && q.window_minutes > 0)
        .filter(|q| q.used_percent.is_finite() && q.used_percent >= 0.0)
        .filter(|q| q.resets_at_ms == 0 || q.resets_at_ms > now_ms)
        .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent)
            .then_with(|| a.sampled_at_ms.cmp(&b.sampled_at_ms)))
}

// 将厂商的已用百分比转成可显示的剩余百分比。
fn remaining_percent(q: &QuotaView) -> String {
    let remaining = (100.0 - q.used_percent).clamp(0.0, 100.0);
    if remaining > 0.0 && remaining < 10.0 {
        format!("{remaining:.1}%")
    } else {
        format!("{remaining:.0}%")
    }
}

// 使用报告中的工具名称，缺少检测记录时显示工具 ID。
fn quota_tool_name<'a>(report: &'a Report, q: &'a QuotaView) -> &'a str {
    report.sources.iter()
        .find(|source| source.id == q.tool)
        .map_or(&q.tool, |source| &source.display)
}

// 拼接菜单栏配额提示，剩余模式额外标出对应窗口。
fn quota_tooltip(report: &Report, q: &QuotaView, mode: TrayMode, now_ms: i64, lang: Lang) -> String {
    let reset = if q.resets_at_ms > 0 {
        let duration = countdown(q.resets_at_ms, now_ms);
        match lang {
            Lang::Zh => format!(" · {duration} 后重置"),
            Lang::En => format!(" · resets in {duration}"),
        }
    } else {
        String::new()
    };
    if mode == TrayMode::TrayQuota {
        let window = countdown(q.window_minutes.saturating_mul(60_000), 0);
        let tool = quota_tool_name(report, q);
        let remaining = remaining_percent(q);
        let label = lang.str("剩余", "Remaining");
        return format!("\n{tool} · {window} · {label} {remaining}{reset}");
    }
    let label = lang.str("配额", "quota");
    format!("\n{} {label} {:.0}%{reset}", q.tool, q.used_percent)
}

/// The platform's real tray shape: Windows and Linux retain an icon even
/// with an old text-only preference, so both collapse to TrayOnly. One
/// helper instead of two cfg blocks that had to stay in sync.
fn effective_mode(mode: TrayMode) -> TrayMode {
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    { let _ = mode; TrayMode::TrayOnly }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    mode
}

/// `(title, tooltip)`. Icon-only trays repeat the numbers in the tooltip.
pub fn label_for(report: &Report, mode: TrayMode) -> (Option<String>, String) {
    let mode = effective_mode(mode);
    let l = lang::get();
    let now_ms = if mode == TrayMode::TrayQuota { usage_core::report::now_ms() } else { report.generated_at_ms };
    let day = &report.day;
    let quota = if mode == TrayMode::TrayQuota { remaining_quota(&report.quotas, now_ms) } else { live_quota(report, now_ms) };

    let tokens_seg = compact(day.summary.total_tokens);
    let cost_seg = money(day.summary.cost);
    let title = match mode {
        TrayMode::TrayOnly => None,
        TrayMode::TokensOnly | TrayMode::TrayTokens => Some(tokens_seg.clone()),
        TrayMode::CostOnly | TrayMode::TrayCost => Some(cost_seg.clone()),
        TrayMode::TrayTokensCost => Some(format!("{tokens_seg} {cost_seg}")),
        TrayMode::TrayQuota => Some(quota.map_or_else(
            || "—".to_string(),
            remaining_percent,
        )),
    };

    let mut tooltip = format!(
        "kimo-token · {}\n{} · {} tokens · {} {}",
        l.str("今日", "Today"),
        money(day.summary.cost),
        compact(day.summary.total_tokens),
        day.summary.requests,
        l.str("次", "requests"),
    );
    if day.summary.credits > 0.0 {
        tooltip.push_str(&format!(" · {:.2} credits", day.summary.credits));
    }
    tooltip.push_str(&format!(" · v{}", env!("CARGO_PKG_VERSION")));
    if let Some(q) = quota {
        tooltip.push_str(&quota_tooltip(report, q, mode, now_ms, l));
    } else if mode == TrayMode::TrayQuota {
        tooltip.push_str(l.str("\n暂无有效配额", "\nNo active quota data"));
    }
    tooltip.push_str(if report.pricing.stale {
        l.str("\n价格快照已过期，成本为估算", "\nPrice snapshot is stale; costs are estimates")
    } else {
        l.str("\n价格为实时估算，非账单", "\nPrices are live estimates, not billing")
    });
    (title, tooltip)
}

pub fn tray_state(app: &AppHandle) -> Option<TrayState> {
    let shared = app.state::<Shared>();
    let settings = shared.settings();
    let report = shared.report()?;
    let (title, tooltip) = label_for(&report, settings.tray_mode);
    Some(TrayState {
        label: title.unwrap_or_default(),
        tooltip,
        mode: settings.tray_mode,
    })
}

/// AppKit mutations (NSStatusItem, muda menu items) are main-thread-only on
/// macOS, but the tray is poked from the engine thread and from async commands
/// too. Every tray-touching body goes through here; calling this from the main
/// thread itself is fine — the body then runs after the current handler
/// returns. Skipping the update (instead of crashing the pool drain) when the
/// event loop is already gone, i.e. during shutdown.
fn on_main<R: Runtime, F: FnOnce() + Send + 'static>(app: &AppHandle<R>, body: F) {
    if let Err(e) = app.run_on_main_thread(body) {
        crate::logging::error(&format!("tray update dropped, event loop is gone: {e}"));
    }
}

/// Pushes the current numbers onto the status bar. Callable from any thread;
/// the painting itself is marshalled onto the main thread. Everything paint
/// needs is computed here, on the thread that already holds the report —
/// crossing to main carries two strings, not a deep clone of the report.
pub fn refresh<R: Runtime>(app: &AppHandle<R>, report: &Report, mode: TrayMode) {
    let handle = app.clone();
    let mode = effective_mode(mode);
    let (title, tooltip) = label_for(report, mode);
    on_main(app, move || paint(&handle, mode, title, tooltip));
}

fn paint<R: Runtime>(
    app: &AppHandle<R>,
    mode: TrayMode,
    title: Option<String>,
    tooltip: String,
) {
    #[cfg(target_os = "windows")]
    crate::windows_tray::refresh(app);
    let Some(tray) = app.tray_by_id(TRAY_ID) else { return };
    // Re-setting the image repaints the status item — visible as a flicker on
    // every engine cycle. The image only depends on the mode, so touch it
    // when the mode actually changes and leave it alone otherwise.
    let changed = match app.state::<LastMode>().0.lock() {
        Ok(mut last) => {
            let changed = *last != Some(mode);
            if changed {
                *last = Some(mode);
            }
            changed
        }
        Err(_) => true, // poisoned: repaint rather than risk a stale image
    };
    if changed {
        // 仅Token/仅花费 hide the icon entirely: a text-only status item.
        #[cfg(not(target_os = "windows"))]
        let icon = if mode.shows_icon() {
            app.try_state::<TrayAssets>().map(|a| a.0.clone())
        } else {
            None
        };
        #[cfg(target_os = "windows")]
        let icon = Some(crate::windows_tray::image(app));
        let _ = tray.set_icon(icon);
        // set_icon resets the template flag; without it macOS stops recoloring
        // the glyph and the black PNG ships as-is — invisible on a dark bar.
        let _ = tray.set_icon_as_template(tray_uses_template());
    }
    // tray-icon's set_title(None) is a no-op, so clearing means an empty
    // string — otherwise 仅托盘 keeps the previous mode's text forever.
    // Same-value writes are skipped entirely: see LastPaint.
    let title_str = title.clone().unwrap_or_default();
    let mut text_changed = true;
    if let Some(paint) = app.try_state::<LastPaint>() {
        if let Ok(mut last) = paint.0.lock() {
            if last.0 == title_str && last.1 == tooltip {
                text_changed = false;
            } else {
                *last = (title_str.clone(), tooltip.clone());
            }
        }
    }
    if text_changed {
        let _ = tray.set_title(Some(title_str.as_str()));
        let _ = tray.set_tooltip(Some(tooltip));
    }
    // AppKit 重排图标和标题后，把事件接收区重新对齐按钮边界。
    #[cfg(target_os = "macos")]
    if changed || text_changed {
        macos_menu::sync_hit_area(&tray);
    }
}

/// Keeps the checkable autostart row in sync with the settings file. The
/// option inside the mutex is swapped when the language changes rebuilds the
/// menu — a second `manage` would be ignored, leaving sync pointed at a dead
/// row.
struct MenuItems(std::sync::Mutex<Option<CheckMenuItem<tauri::Wry>>>);

impl MenuItems {
    fn set_checked(&self, on: bool) {
        if let Ok(guard) = self.0.lock() {
            if let Some(item) = guard.as_ref() {
                item.set_checked(on).ok();
            }
        }
    }
}

/// The dual-ring icon, kept so 仅Token/仅花费 can hide it and the 托盘 modes
/// can put it back without re-reading the file.
#[cfg(not(target_os = "windows"))]
struct TrayAssets(tauri::image::Image<'static>);

/// The last mode the status item was painted with — the guard that keeps
/// periodic refreshes from re-setting the image (and flickering) on no-op.
struct LastMode(std::sync::Mutex<Option<TrayMode>>);

/// The strings the status item currently carries. Re-writing the same title
/// and tooltip through AppKit on every engine cycle (the watcher can fire
/// several times a second) churns NSStatusItem internals for zero visual
/// change — and that churn path is where the pool drain meets KVO state.
/// Only real changes are pushed; a same-value write is skipped.
struct LastPaint(std::sync::Mutex<(String, String)>);

fn build_menu(app: &AppHandle) -> tauri::Result<(tauri::menu::Menu<tauri::Wry>, CheckMenuItem<tauri::Wry>)> {
    let l = lang::get();
    let quit = MenuItem::with_id(app, "quit", l.str("退出", "Quit"), true, None::<&str>)?;
    let autostart = CheckMenuItem::with_id(app, "autostart", l.str("开机自启", "Launch at Login"), true, false, None::<&str>)?;
    let open_data = MenuItem::with_id(app, "data", l.str("打开数据目录", "Open Data Folder"), true, None::<&str>)?;
    let week = MenuItem::with_id(app, "week", l.str("本周", "This Week"), true, None::<&str>)?;
    let today = MenuItem::with_id(app, "today", l.str("今日花费", "Today's Cost"), true, None::<&str>)?;
    let refresh_item = MenuItem::with_id(app, "refresh", l.str("刷新", "Refresh"), true, None::<&str>)?;
    #[cfg(target_os = "linux")]
    let open_panel = MenuItem::with_id(app, "open_panel", l.str("打开面板", "Open Panel"), true, None::<&str>)?;

    autostart
        .set_checked(app.state::<Shared>().settings().autostart)
        .ok();

    let menu = MenuBuilder::new(app);
    #[cfg(target_os = "linux")]
    let menu = menu.item(&open_panel).separator();
    let menu = menu
        .item(&refresh_item)
        .separator()
        .item(&today)
        .item(&week)
        .separator()
        .item(&open_data)
        .separator()
        .item(&autostart)
        .separator()
        .item(&quit)
        .build()?;
    Ok((menu, autostart))
}

/// The glyph at the size the tray actually paints. Explorer draws a tray icon
/// at GetSystemMetrics(SM_CXSMICON) physical pixels (16 at 100% scaling, 20 at
/// 125%, 24 at 150% ...), so the nearest pre-rendered size
/// (scripts/gen-tray-icons.py, rendered from design/tray-icon.svg) ships
/// sharp at every DPI — the 44px macOS menu-bar asset squashed down by
/// Explorer broke the thin arcs into uneven strokes. Read once at startup:
/// the tray lives on the primary display, and a mid-session scaling change
/// costs a restart to re-pick.
#[cfg(target_os = "windows")]
fn tray_image() -> tauri::image::Image<'static> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON};
    match unsafe { GetSystemMetrics(SM_CXSMICON) } {
        0..=17 => tauri::include_image!("icons/tray-icon-16.png"),
        18..=21 => tauri::include_image!("icons/tray-icon-20.png"),
        22..=25 => tauri::include_image!("icons/tray-icon-24.png"),
        26..=29 => tauri::include_image!("icons/tray-icon-28.png"),
        _ => tauri::include_image!("icons/tray-icon-32.png"),
    }
}

// macOS 菜单栏直接使用项目已内置的 Codex 厂商图标。
#[cfg(target_os = "macos")]
fn tray_image() -> tauri::image::Image<'static> {
    tauri::include_image!("../../../crates/usage-core/assets/codex.png")
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn tray_image() -> tauri::image::Image<'static> {
    tauri::include_image!("icons/tray-icon.png")
}

// 厂商图标自带底色，保留原色以在深浅菜单栏上都可辨认。
fn tray_uses_template() -> bool {
    !cfg!(target_os = "macos")
}

// AppIndicator preserves colors; the application icon reads on both themes.
#[cfg(target_os = "linux")]
fn tray_image() -> tauri::image::Image<'static> {
    tauri::include_image!("icons/32x32.png")
}

/// 创建菜单栏图标，并注册面板和菜单的入口。
pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let (menu, autostart) = build_menu(app)?;
    app.manage(MenuItems(std::sync::Mutex::new(Some(autostart))));
    let icon = tray_image();
    #[cfg(target_os = "windows")]
    let icon = crate::windows_tray::init(app, icon);
    #[cfg(not(target_os = "windows"))]
    app.manage(TrayAssets(icon.clone()));
    // The builder already painted the icon for the startup mode's default;
    // seeding None makes the first publish apply the real mode's imagery.
    app.manage(LastMode(std::sync::Mutex::new(None)));
    // Empty strings: build() painted icon-only, so the first publish always
    // writes whatever the mode resolves to.
    app.manage(LastPaint(std::sync::Mutex::new((String::new(), String::new()))));

    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("kimo-token")
        .icon(icon)
        .icon_as_template(tray_uses_template())
        .on_menu_event(on_menu_event)
        .on_tray_icon_event(on_tray_event)
        .build(app)?;
    // 保留菜单内容，解除 AppKit 按钮的默认菜单行为。
    #[cfg(target_os = "macos")]
    macos_menu::detach(&tray)?;
    #[cfg(not(target_os = "macos"))]
    let _ = tray;
    #[cfg(target_os = "windows")]
    {
        crate::windows_tray::watch(app);
        promote_taskbar_icon();
    }
    Ok(())
}

/// Win11 files every tray icon under HKCU\Control Panel\NotifyIconSettings,
/// keyed by a hash of the exe path, and a fresh entry starts hidden inside the
/// overflow flyout (IsPromoted absent) — every new install path would surface
/// its tray icon only after a manual trip through taskbar settings. The tray
/// is this app's whole interface, so an absent value is flipped to promoted;
/// a value that is present was set by the user (shown or hidden) and is never
/// touched. Explorer creates the entry only after the icon is first shown, so
/// the lookup retries on its own thread and stops at the first decision.
#[cfg(target_os = "windows")]
fn promote_taskbar_icon() {
    std::thread::Builder::new()
        .name("kimo-token-tray-promote".into())
        .spawn(|| {
            let exe = match std::env::current_exe() {
                Ok(p) => p,
                Err(_) => return,
            };
            for attempt in 0..24 {
                std::thread::sleep(std::time::Duration::from_millis(500));
                if promote_taskbar_icon_once(&exe, attempt) {
                    return;
                }
            }
        })
        .ok();
}

/// One lookup pass. `true` = decided (promoted, or the user already chose).
#[cfg(target_os = "windows")]
fn eq_ignore_ascii(a: &[u16], b: &[u16]) -> bool {
    fn lower(c: u16) -> u16 {
        if (0x41..=0x5A).contains(&c) {
            c + 0x20
        } else {
            c
        }
    }
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x == y || lower(*x) == lower(*y))
}

/// Explorer records ExecutablePath with an unexpanded known-folder GUID prefix
/// ("{6D809377-…}\kimo-token\…"), while current_exe() answers the real path —
/// RegGetValueW expands %vars% but not folder GUIDs, so the raw strings can
/// never compare equal. Rewrite the prefix through the environment copy of the
/// same folder; the three GUIDs cover everywhere this product installs. Plain
/// paths (dev entries) pass through untouched, and an unknown GUID returns
/// None so the entry is skipped rather than mismatched.
#[cfg(target_os = "windows")]
fn resolve_known_folder(entry: &[u16]) -> Option<Vec<u16>> {
    if entry.first() != Some(&0x7B) {
        return Some(entry.to_vec());
    }
    let close = entry.iter().position(|&c| c == 0x7D)?;
    let guid: String = entry[1..close]
        .iter()
        .map(|c| *c as u8 as char)
        .collect::<String>()
        .to_ascii_lowercase();
    let base = match guid.as_str() {
        // FOLDERID_ProgramFilesX64
        "6d809377-6af0-444b-8957-a3773f02200e" => std::env::var("ProgramW6432").ok()?,
        // FOLDERID_ProgramFilesX86
        "7c5a40ef-a0fb-4b58-944d-503c4ed9d452" => std::env::var("ProgramFiles(x86)").ok()?,
        // FOLDERID_LocalAppData
        "f1b32785-6fba-4fcf-9d55-7b8e7f157091" => std::env::var("LOCALAPPDATA").ok()?,
        _ => return None,
    };
    let mut out: Vec<u16> = base.encode_utf16().collect();
    out.extend_from_slice(&entry[close + 1..]);
    Some(out)
}

#[cfg(target_os = "windows")]
fn promote_taskbar_icon_once(exe: &std::path::Path, attempt: u32) -> bool {
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, RegSetValueExW,
        HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_DWORD, RRF_RT_REG_DWORD,
        RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    unsafe {
        let mut root = std::ptr::null_mut();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            wide("Control Panel\\NotifyIconSettings").as_ptr(),
            0,
            KEY_READ | KEY_SET_VALUE,
            &mut root,
        ) != 0
        {
            if attempt == 23 {
                crate::logging::error("tray: NotifyIconSettings root would not open");
            }
            return false;
        }
        let want: Vec<u16> = exe.to_string_lossy().encode_utf16().collect();
        let mut decided = false;
        let mut scanned: u32 = 0;
        let mut get_fail: Option<u32> = None;
        let mut skipped_unknown: u32 = 0;
        let mut index: u32 = 0;
        loop {
            let mut name = [0u16; 256];
            let mut len = 256u32;
            if RegEnumKeyExW(
                root,
                index,
                name.as_mut_ptr(),
                &mut len,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ) != 0
            {
                break;
            }
            index += 1;
            let mut sub = std::ptr::null_mut();
            if RegOpenKeyExW(root, name.as_ptr(), 0, KEY_READ | KEY_SET_VALUE, &mut sub) != 0 {
                continue;
            }
            let mut path = [0u16; 520];
            let mut path_len = std::mem::size_of_val(&path) as u32;
            let mut vtype: u32 = 0;
            let got = RegGetValueW(
                sub,
                std::ptr::null(),
                wide("ExecutablePath").as_ptr(),
                // Explorer stores the path as REG_EXPAND_SZ with an unexpanded
                // known-folder GUID prefix ({6D809377-…}\kimo-token\…); a plain
                // RRF_RT_REG_SZ rejects the type and every entry would be
                // silently skipped.
                RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
                &mut vtype,
                path.as_mut_ptr() as _,
                &mut path_len,
            );
            if got == 0 {
                scanned += 1;
                let entry = &path[..path.len() >> 1];
                let entry = &entry[..entry.iter().position(|&c| c == 0).unwrap_or(entry.len())];
                let resolved = resolve_known_folder(entry);
                if let Some(entry) = resolved {
                    // `want` carries no terminator: the whole string compares.
                    if eq_ignore_ascii(&entry, &want) {
                    let mut current: u32 = 0;
                    let mut cur_len = std::mem::size_of::<u32>() as u32;
                    let present = RegGetValueW(
                        sub,
                        std::ptr::null(),
                        wide("IsPromoted").as_ptr(),
                        RRF_RT_REG_DWORD,
                        std::ptr::null_mut(),
                        &mut current as *mut u32 as _,
                        &mut cur_len,
                    ) == 0;
                    if !present {
                        let one: u32 = 1;
                        RegSetValueExW(
                            sub,
                            wide("IsPromoted").as_ptr(),
                            0,
                            REG_DWORD,
                            &one as *const u32 as _,
                            4,
                        );
                        crate::logging::info("tray: promoted the taskbar icon (fresh entry)");
                    }
                    decided = true;
                    }
                } else {
                    skipped_unknown += 1;
                }
            } else {
                get_fail = Some(got);
            }
            RegCloseKey(sub);
            if decided {
                break;
            }
        }
        RegCloseKey(root);
        if !decided && attempt == 23 {
            crate::logging::error(&format!(
                "tray: no NotifyIconSettings entry matched {} — scanned {scanned}, value-read failures {}, unknown-guid skips {skipped_unknown}, last read error {:?}",
                exe.display(),
                get_fail.iter().count(),
                get_fail
            ));
        }
        decided
    }
}

// 分发菜单栏点击，左键开合用量面板，右键打开选项。
fn on_tray_event(tray: &TrayIcon<tauri::Wry>, event: TrayIconEvent) {
    let app = tray.app_handle().clone();
    // The positioner plugin tracks the tray rect for the Windows anchor.
    #[cfg(target_os = "windows")]
    tauri_plugin_positioner::on_tray_event(&app, &event);

    match event {
        #[cfg(target_os = "windows")]
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Down,
            rect,
            ..
        } => panel::remember_tray_rect(&rect),
        // 先收起面板，再在图标下方弹出原生菜单。
        #[cfg(target_os = "macos")]
        TrayIconEvent::Click {
            button: MouseButton::Right,
            button_state: MouseButtonState::Down,
            ..
        } => {
            panel::hide(&app);
            macos_menu::popup(tray);
        }
        // 左键只开合用量面板。
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            rect,
            ..
        } => {
            #[cfg(target_os = "windows")]
            panel::remember_tray_rect(&rect);
            #[cfg(target_os = "macos")]
            crate::logging::info("tray: left click toggling usage panel");
            panel::toggle(&app, Some(rect));
        }
        // On Windows a double click is preceded by the first left-button-up
        // click. Toggling here as well would immediately undo the first toggle.
        TrayIconEvent::DoubleClick { .. } => {}
        _ => {}
    }
}

fn on_menu_event(app: &AppHandle, event: MenuEvent) {
    match event.id().as_ref() {
        #[cfg(target_os = "linux")]
        "open_panel" => panel::show(app, None),
        "refresh" => {
            // The full refresh the panel's 刷新 runs: a fresh models.dev price
            // table first (the engine re-summarizes when it lands), then the
            // manual pass — this explicitly requests fresh quota.
            // The pricing fetch blocks on network,
            // so it runs off the menu-event thread.
            if let Some(channel) = app.try_state::<EngineChannel>() {
                let sender = channel.0.clone();
                std::thread::spawn(move || {
                    let map = PricingMap::refresh(&PricingOptions {
                        offline: false,
                        cache_dir: PricingOptions::default_cache_dir(),
                        overrides: Default::default(),
                    });
                    let _ = sender.send(Msg::Pricing(map));
                    let _ = sender.send(Msg::RefreshWithQuota);
                });
            }
        }
        "today" => panel::open_at(app, "day"),
        "week" => panel::open_at(app, "week"),
        "data" => open_data_dir(),
        "autostart" => toggle_autostart(app),
        "quit" => app.exit(0),
        _ => {}
    }
}

fn toggle_autostart(app: &AppHandle) {
    let next = !app.autolaunch().is_enabled().unwrap_or(false);
    set_autostart(app, next);
}

/// The one writer for the login-item switch: tray menu and panel command both
/// land here, so the plugin, the menu checkmark and the file can never drift.
/// Marshalled onto the main thread — the checkmark is an AppKit mutation and
/// the async command reaches this from the runtime thread.
pub fn set_autostart(app: &AppHandle, on: bool) {
    let handle = app.clone();
    on_main(app, move || apply_autostart(&handle, on));
}

fn apply_autostart(app: &AppHandle, on: bool) {
    let plugin = app.autolaunch();
    if on {
        plugin.enable().ok();
    } else {
        plugin.disable().ok();
    }
    if let Some(items) = app.try_state::<MenuItems>() {
        items.set_checked(on);
    }
    persist(app, |s| s.autostart = on);
}

/// Applies the persisted switch, and repairs drift if login items changed.
pub fn reconcile_autostart(app: &AppHandle) {
    let want = app.state::<Shared>().settings().autostart;
    let plugin = app.autolaunch();
    if plugin.is_enabled().unwrap_or(false) == want {
        return;
    }
    if want {
        plugin.enable().ok();
    } else {
        plugin.disable().ok();
    }
    if let Some(items) = app.try_state::<MenuItems>() {
        items.set_checked(want);
    }
}

/// Rebuild the context menu in the current UI language. muda items carry
/// their labels from construction, so a language switch means new items;
/// the autostart row moves into the fresh menu before the old one drops.
pub fn apply_lang(app: &AppHandle) {
    let handle = app.clone();
    on_main(app, move || {
        let Ok((menu, autostart)) = build_menu(&handle) else { return };
        if let Some(tray) = handle.tray_by_id(TRAY_ID) {
            // 替换语言菜单后同样解除 AppKit 默认的左键弹出。
            if tray.set_menu(Some(menu)).is_ok() {
                #[cfg(target_os = "macos")]
                if let Err(error) = macos_menu::detach(&tray) {
                    crate::logging::error(&format!("tray: unable to detach localized menu: {error}"));
                }
            }
        }
        if let Some(items) = handle.try_state::<MenuItems>() {
            *items.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(autostart);
        }
    });
}

fn persist(app: &AppHandle, edit: impl FnOnce(&mut Settings)) {
    let shared = app.state::<Shared>();
    let Ok(mut settings) = shared.settings.lock() else { return };
    edit(&mut settings);
    settings.clone().save().ok();
}

fn open_data_dir() {
    let Some(path) = usage_index::Index::default_path() else { return };
    let dir = if path.is_dir() {
        path
    } else {
        path.parent().map(|p| p.to_path_buf()).unwrap_or(path)
    };
    let mut command = match std::env::consts::OS {
        "macos" => {
            let mut c = std::process::Command::new("open");
            c.arg(&dir);
            c
        }
        "windows" => {
            let mut c = std::process::Command::new("explorer");
            c.arg(dir.to_string_lossy().to_string());
            c
        }
        _ => {
            let mut c = std::process::Command::new("xdg-open");
            c.arg(&dir);
            c
        }
    };
    // Spawning is best-effort: a missing file manager must not kill the app.
    command.spawn().ok();
}

/// Exposed for the panel's fallback anchor when no tray rect is available.
pub fn last_rect(app: &AppHandle) -> Option<Rect> {
    app.tray_by_id(TRAY_ID).and_then(|t| t.rect().ok().flatten())
}

#[cfg(test)]
mod tests {
    use super::*;

    // 构造一个 Codex 时间窗供菜单栏选取和格式化测试使用。
    fn quota(used_percent: f64, resets_at_ms: i64) -> QuotaView {
        QuotaView {
            tool: "codex".into(), used_percent, window_minutes: 300, resets_at_ms,
            sampled_at_ms: 100, label: None, id: None, origin: QuotaOrigin::Probe, cycle: None,
        }
    }

    // 验证预算、过期时间窗和无额度标记都不会盖住可用配额。
    #[test]
    fn picks_the_tightest_active_vendor_window() {
        let active = quota(91.5, 300);
        let weekly = QuotaView { window_minutes: 10_080, ..quota(65.0, 400) };
        let expired = quota(100.0, 200);
        let budget = QuotaView { origin: QuotaOrigin::Budget, ..quota(99.0, 300) };
        let marker = QuotaView { window_minutes: 0, ..quota(100.0, 0) };
        let samples = [weekly, expired, budget, marker, active.clone()];
        assert_eq!(remaining_quota(&samples, 200), Some(&active));
        assert!(remaining_quota(&samples, 400).is_none());
    }

    // 验证百分比方向、低剩余精度和超额时的下界。
    #[test]
    fn formats_vendor_usage_as_remaining_percent() {
        assert_eq!(remaining_percent(&quota(40.0, 300)), "60%");
        assert_eq!(remaining_percent(&quota(91.5, 300)), "8.5%");
        assert_eq!(remaining_percent(&quota(125.0, 300)), "0%");
    }

    // 验证 macOS 在同一份报告上能切换 Token、真实配额和空配额提示。
    #[cfg(target_os = "macos")]
    #[test]
    fn menu_bar_title_follows_the_saved_mode() {
        let now = usage_core::report::now_ms();
        let pricing = PricingMap::default();
        let mut report = usage_core::summarize(&[], &usage_core::ReportOptions::new(&pricing).with_now(now));
        report.day.summary.total_tokens = 2_000_000.0;
        assert_eq!(label_for(&report, TrayMode::TrayTokens).0.as_deref(), Some("2.00M"));
        assert!(label_for(&report, TrayMode::TrayQuota).0.unwrap().ends_with('—'));
        report.quotas.push(quota(40.0, now + 60_000));
        let (title, tooltip) = label_for(&report, TrayMode::TrayQuota);
        assert_eq!(title.as_deref(), Some("60%"));
        assert!(tooltip.contains("5h 00m"));
    }
}
