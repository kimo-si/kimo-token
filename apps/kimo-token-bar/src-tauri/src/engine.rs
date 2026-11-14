//! The engine: one background thread owns the `Index` and the `PricingMap`.
//!
//! Nothing here runs on the main thread. Ingest is serialised by a `try_lock`
//! so a burst of watcher events can never stack two passes on top of each
//! other — the second one is simply skipped and the next tick catches up.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, RecvTimeoutError};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager, Runtime};
use usage_core::pricing::PricingOptions;
use usage_core::report::{now_ms, AggregatePlan, QuotaOrigin, QuotaView, ReportOptions};
use usage_core::{DateFilter, DetectedSource, MachineScope, PricingMap, Report, SourceAdapter, SourceFile, SourceStatus};
use usage_index::{Index, IngestOptions, Watcher};

use crate::notify;
use crate::settings::Settings;
use crate::snapshot;
use crate::tray;

/// Emitted on every recomputed report; the payload is a serialised `Report`.
pub const REPORT_EVENT: &str = "report-updated";
/// Emitted when a tray menu entry asks the panel to focus a period.
pub const PERIOD_EVENT: &str = "tray-period";

/// The debounce window that absorbs a burst of write events as one re-index.
/// The idle cadence itself is the persisted `refresh_secs` setting.
const DEBOUNCE: Duration = Duration::from_millis(800);

/// Floor between ingest passes. A full pass re-walks every adapter's tree
/// (codex alone holds hundreds of rollouts inside the retention window), so a
/// writer that touches its store every second — ZCode's own telemetry db —
/// would otherwise keep the engine at a permanent percent-level duty cycle.
/// Scoped passes only restat the previous snapshot, but the floor keeps even
/// those from spinning. A finished call surfaces at most this much later; the
/// manual refresh bypasses the floor because the user asked for "now".
const MIN_PASS_GAP: Duration = Duration::from_secs(3);
/// The adaptive floor widens with the last pass's cost (×3) and never exceeds
/// this — a pathological pass must not silence the watcher for minutes.
const MAX_PASS_COST: Duration = Duration::from_secs(5);
/// With the panel hidden the ingest pass backs off to this floor.
const HIDDEN_PASS_GAP: Duration = Duration::from_secs(15);

/// Minimum gap between report rebuilds on file-change wakes. A rebuild reads
/// the day rollup plus a few live slices, so it is cheap — but every one still
/// emits a report and repaints the panel, so drip writers (one new event per
/// second) get at most one per gap. Ingestion itself stays per-wake and cheap;
/// the cadence timer and the manual refresh publish regardless, so quiet
/// machines still see the time-shaped windows move.
const PUBLISH_GAP: Duration = Duration::from_secs(10);

/// How long one discovered-file snapshot is reused (restatted, not re-walked)
/// before the engine pays a full `discover` again. The watcher covers every
/// incremental change — new, grown and vanished files all surface on the next
/// wake — so the TTL is only the backstop for a file the watcher never saw:
/// eight minutes is the longest one can stay invisible, long enough that idle
/// passes never pay a full re-walk of every adapter tree on a schedule. Any
/// wake naming a declared root forces a full walk for every tool, ahead of
/// the TTL.
const SNAPSHOT_TTL: Duration = Duration::from_secs(480);

pub enum Msg {
    /// A watched root changed; the payload is the root the watcher named.
    Wake(PathBuf),
    /// An internal change needs an immediate usage re-index.
    Refresh,
    /// The user pressed Refresh; re-index and query the vendor.
    RefreshWithQuota,
    /// Swap in a freshly downloaded price table.
    Pricing(PricingMap),
    /// The panel switched the machine scope (全部/本机/远程). Cheap by design:
    /// the fold over the rollup is re-run, no ingest, no vendor probes.
    Scope,
}

/// Cloned into managed state so commands can poke the engine thread.
pub struct EngineChannel(pub Sender<Msg>);

/// Shared between the engine thread, the commands and the tray.
pub struct Shared {
    pub report: Mutex<Option<Report>>,
    /// A startup/open failure is separate from the report: a cold panel has
    /// no report yet, and returning the generic `indexing` sentinel forever
    /// hides a failure that the user can actually retry.
    pub startup_error: Mutex<Option<String>>,
    pub settings: Mutex<Settings>,
    ingesting: Mutex<()>,
    /// The machine scope every publish folds with. Read at publish time (never
    /// cached in the engine), so a switch applies to the very next report even
    /// if it lands while a slow pass is mid-flight.
    scope: Mutex<MachineScope>,
    scope_refresh_queued: AtomicBool,
}

impl Shared {
    pub fn new(settings: Settings) -> Self {
        Self {
            report: Mutex::new(None),
            startup_error: Mutex::new(None),
            settings: Mutex::new(settings),
            ingesting: Mutex::new(()),
            scope: Mutex::new(MachineScope::All),
            scope_refresh_queued: AtomicBool::new(false),
        }
    }

    /// Updates the tray mode in memory only; the caller decides when the
    /// settings file is written, so a slow save never delays the switch.
    pub fn set_tray_mode(&self, mode: crate::settings::TrayMode) -> Result<(), String> {
        let mut settings = self.settings.lock().map_err(|_| "settings busy".to_string())?;
        settings.tray_mode = mode;
        Ok(())
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().map(|s| s.clone()).unwrap_or_default()
    }

    // 供独立采样器和手动工具操作读取实时开关。
    pub fn agent_enabled(&self, id: &str) -> bool {
        self.settings.lock().is_ok_and(|settings| settings.agent_enabled(id))
    }

    /// The current scope, falling back to `All` on a poisoned lock: showing
    /// every machine is the report the user can least be surprised by.
    pub fn scope(&self) -> MachineScope {
        self.scope.lock().map(|s| s.clone()).unwrap_or(MachineScope::All)
    }

    pub fn set_scope(
        &self,
        scope: MachineScope,
        tx: Option<&Sender<Msg>>,
    ) -> Result<(bool, bool), String> {
        let mut slot = self.scope.lock().map_err(|_| "scope busy".to_string())?;
        if *slot == scope { return Ok((false, false)); }
        *slot = scope;
        let queued = match tx {
            Some(tx) => self.queue_scope_fold(tx)?,
            None => false,
        };
        Ok((true, queued))
    }

    // 去重排队，在现有索引上重新折叠报告，不触发额度查询。
    pub fn queue_scope_fold(&self, tx: &Sender<Msg>) -> Result<bool, String> {
        if self.scope_refresh_queued.swap(true, Ordering::AcqRel) {
            return Ok(false);
        }
        if tx.send(Msg::Scope).is_err() {
            self.scope_refresh_queued.store(false, Ordering::Release);
            return Err("engine channel is closed".into());
        }
        Ok(true)
    }

    pub fn report(&self) -> Option<Report> {
        self.report.lock().ok().and_then(|r| r.clone())
    }

    pub fn set_startup_error(&self, error: impl Into<String>) {
        if let Ok(mut slot) = self.startup_error.lock() {
            *slot = Some(error.into());
        }
    }

    pub fn clear_startup_error(&self) {
        if let Ok(mut slot) = self.startup_error.lock() {
            *slot = None;
        }
    }

    pub fn startup_error(&self) -> Option<String> {
        self.startup_error.lock().ok().and_then(|error| error.clone())
    }
}

/// Runs the engine on its own thread; `tx` is the same sender commands hold so
/// the file-watcher wake-ups can be merged into one queue.
///
/// The loop is supervised. `run()` returning means the engine *cannot* keep
/// working, and historically it did exactly that — silently — whenever a
/// price-table swap landed while a watcher burst was being debounced; the panel
/// then served frozen numbers forever with nothing in the log to say so. Nothing
/// here can end the application (quitting is `app.exit`, which kills the
/// process), so an unexpected exit is always a defect: log it, restart, and back
/// off so a persistent failure cannot spin the CPU.
pub fn start<R: Runtime>(app: AppHandle<R>, rx: Receiver<Msg>, tx: Sender<Msg>) {
    std::thread::Builder::new()
        .name("kimo-token-engine".into())
        .spawn(move || {
            // `run` borrows both ends instead of owning them: a panic unwinds its
            // frame, and a `Receiver` consumed by that frame would leave the
            // restart with nothing to listen to (and every command's send failing
            // for the rest of the session).
            let mut attempt = 0u32;
            loop {
                // An engine panic must not take the whole panel down, and must not
                // end the updates either: catch it, log it, restart.
                let started = Instant::now();
                let outcome =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(app.clone(), &rx, &tx)));
                if matches!(outcome, Ok(Exit::Disconnected)) {
                    // Every sender is gone, which happens only while the app is
                    // being torn down. There is nothing left to serve.
                    crate::logging::info("engine loop ended with the command channel; engine thread exiting");
                    return;
                }
                let reason = match &outcome {
                    Ok(exit) => exit.reason().to_string(),
                    Err(panic) => panic_text(panic),
                };
                // A run that survived the settle window proved the restart works,
                // so the next failure starts counting again.
                if started.elapsed() > RESTART_SETTLE {
                    attempt = 0;
                }
                attempt += 1;
                let wait = restart_delay(attempt);
                crate::logging::error(&format!(
                    "engine stopped ({reason}) — restarting in {}s (attempt {attempt})",
                    wait.as_secs()
                ));
                std::thread::sleep(wait);
            }
        })
        .expect("failed to spawn the kimo-token engine thread");
}

/// How long a restarted engine must stay alive before the failure counter resets.
const RESTART_SETTLE: Duration = Duration::from_secs(60);
const RESTART_BACKOFF: Duration = Duration::from_secs(2);
const RESTART_CAP: Duration = Duration::from_secs(300);

/// How long to wait before restart number `attempt` (1-based): 2 s, 4 s, 8 s …
/// until [`RESTART_CAP`] binds, so an engine that fails the instant it starts
/// cannot become a CPU burner — and, capped rather than abandoned, it still
/// comes back once whatever broke it heals.
fn restart_delay(attempt: u32) -> Duration {
    (RESTART_BACKOFF * (1u64 << (attempt - 1).min(10)) as u32).min(RESTART_CAP)
}

/// The panic payload, as far as it can be read. A panic carries `&str` or
/// `String` most of the time; anything else still has to say *that* the engine
/// died, which is the whole point of logging it.
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    let detail = payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".into());
    format!("panicked: {detail}")
}

/// How the pass loop ended. Exactly one way is left: the command channel dying,
/// which means the app is going away. Everything that used to end it — a pricing
/// swap mid-debounce, a wake that outranked the window — is now a pass.
enum Exit {
    Disconnected,
}

impl Exit {
    fn reason(&self) -> &'static str {
        match self {
            Exit::Disconnected => "the command channel is gone (every sender dropped)",
        }
    }
}

struct AgentSources {
    ids: Vec<&'static str>,
    adapters: Vec<Box<dyn SourceAdapter>>,
    detected: Vec<DetectedSource>,
    _watcher: Option<Watcher>,
}

impl AgentSources {
    // 只检测和监听当前启用的适配器，空列表也表示全部关闭。
    fn load(ids: Vec<&'static str>, wakes: &Sender<PathBuf>) -> Self {
        let adapters = usage_adapter_all::builtin_adapters().into_iter()
            .filter(|adapter| ids.contains(&adapter.id())).collect::<Vec<_>>();
        let detected: Vec<_> = adapters.iter().filter_map(|adapter| adapter.probe()).collect();
        let roots: Vec<_> = detected.iter().flat_map(|source| source.roots.iter().cloned()).collect();
        // 替换来源时销毁旧 watcher，停掉已关闭工具的文件监听。
        let watcher = if roots.is_empty() { None } else { Watcher::spawn(&roots, wakes.clone()).ok() };
        Self { ids, adapters, detected, _watcher: watcher }
    }
}

// 将文件监听事件送到现有引擎频道，来源切换时继续复用这个线程。
fn watcher_channel(tx: Sender<Msg>) -> Sender<PathBuf> {
    let (wake_tx, wake_rx) = mpsc::channel();
    std::thread::Builder::new().name("kimo-token-wake".into()).spawn(move || {
        while let Ok(root) = wake_rx.recv() {
            if tx.send(Msg::Wake(root)).is_err() {
                break;
            }
        }
    }).ok();
    wake_tx
}

fn run<R: Runtime>(app: AppHandle<R>, rx: &Receiver<Msg>, tx: &Sender<Msg>) -> Exit {
    // Cold start is the one latency the user feels directly, so the boot path
    // is measured rather than guessed: this clock is what the "boot:" line
    // below breaks down, and it is where a slow open gets attributed.
    let boot = Instant::now();
    // Last-known numbers from the previous run, before anything is read. The
    // whole boot path below (detect, pricing, the aggregation) costs 6.8–24.9 s
    // measured on this machine, and every number it produces was already on disk
    // when the process started; a file read costs milliseconds. The panel labels
    // this report as restored, so the first frame is faster rather than younger.
    publish_restored(&app);
    let at_restore = boot.elapsed();
    let wake_tx = watcher_channel(tx.clone());
    // 启动时从已保存的列表初始化适配器和文件监听。
    let mut agents = AgentSources::load(app.state::<Shared>().settings().active_agent_ids(), &wake_tx);
    let at_detect = boot.elapsed();
    let mut pricing = PricingMap::load(&PricingOptions {
        offline: false,
        cache_dir: PricingOptions::default_cache_dir(),
        overrides: Default::default(),
    });
    let at_pricing = boot.elapsed();

    // An index that cannot be opened is not a quiet day either: every pass would
    // return without publishing and the webview would sit on "indexing" forever.
    let mut index = open_index_or_set_error(
        &app,
        "integrity: the index could not be opened; the original image was preserved and a new index was created for local re-scan",
        "index could not be opened; local usage is unavailable until it is repaired: ",
        "index could not be opened — no data can be published until it is: ",
    );
    let at_index = boot.elapsed();

    // The second frame: this run's own fold over the index, before the scan has
    // read a single source file. The snapshot above may already have covered it;
    // this is what replaces it, and on a fresh install (no snapshot, empty index)
    // it is the boot screen's job until the scan lands. Both use cached quota.
    if let Some(index) = index.as_mut() {
        let events = index.has_events().unwrap_or(false);
        let at_count = boot.elapsed();
        if events {
            let sources = index.source_statuses(&agents.detected)
                .unwrap_or_else(|_| fallback_sources(&agents.detected));
            let at_sources = boot.elapsed();
            let _ = publish(&app, index, &sources, &pricing, QuotaMode::Cached);
            let ms = |d: Duration| d.as_millis();
            crate::logging::info(&format!(
                "boot: published the index fold before the scan — restore {} ms, detect {} ms, \
                 pricing {} ms, index open {} ms, has-events {} ms, sources {} ms, report {} ms \
                 ({} ms of engine startup spent before the panel could render)",
                ms(at_restore),
                ms(at_detect - at_restore),
                ms(at_pricing - at_detect),
                ms(at_index - at_pricing),
                ms(at_count - at_index),
                ms(at_sources - at_count),
                ms(boot.elapsed() - at_sources),
                ms(boot.elapsed()),
            ));
        }
    }

    // SQLite WAL already guarantees crash-safe atomic transactions.
    // Running full `PRAGMA integrity_check` on every launch is an anti-pattern:
    // it scales linearly with DB size (~10s on 623MB, ~30s on 1GB).
    // The handle opened above is used directly; corruption is caught on actual
    // read/write failure and handled via existing recovery paths.

    // The last discovered file list per tool. Passes that are neither
    // privileged nor TTL-expired restat this snapshot instead of walking every
    // adapter tree again — the discover scoping that keeps idle passes down to
    // a couple of stats per file.
    let mut snapshot: HashMap<String, Vec<SourceFile>> = HashMap::new();
    let mut snapshot_at = Instant::now();
    let mut wake_roots: Vec<PathBuf> = Vec::new();

    let mut last_publish = Instant::now();
    ingest(&app, &mut index, &agents.adapters, &agents.detected, &pricing, IngestReason::Initial, &mut last_publish,
        &mut snapshot, &mut snapshot_at, &mut wake_roots);
    let mut last_pass = Instant::now();
    let mut last_pass_cost = Duration::from_secs(1);
    // The cadence clock runs on its own: a machine whose tools log every
    // second would otherwise reset `recv_timeout` with every wake and starve
    // the timer — measured here, a 15s setting drifted to 30-60s between
    // publishes, which is exactly the "set 15s, data still moves every
    // minute" report. `remaining` shrinks across intervening passes, so the
    // cadence pass lands on schedule however many wakes cut in line.
    let mut last_cadence = Instant::now();

    loop {
        // Read per pass, so a cadence change from the panel applies on the next
        // wait without restarting the engine.
        let fallback = {
            let secs = app.state::<Shared>().settings().refresh_secs;
            Duration::from_secs(secs.clamp(10, 3600))
        };
        let remaining = fallback.saturating_sub(last_cadence.elapsed());
        match wait_for_work(rx, &mut pricing, remaining, &mut wake_roots) {
            Work::Ingest(mut reason) => {
                if reason == IngestReason::FileChange {
                    let since = last_pass.elapsed();
                    // Adaptive floor: the wider the last pass, the longer the
                    // breather — a 700ms index pass at 18 wakes a minute was a
                    // third of a core. With the panel tucked away nobody is
                    // watching the numbers, so the floor widens further.
                    let mut gap = MIN_PASS_GAP.max(last_pass_cost * 3);
                    if crate::panel::is_panel_hidden() {
                        gap = gap.max(HIDDEN_PASS_GAP);
                    }
                    if since < gap {
                        let shared = app.state::<Shared>();
                        // 文件变化可以等一等，但等待不能越过下一次定时刷新。
                        let wait = (gap - since).min(fallback.saturating_sub(last_cadence.elapsed()));
                        if wait_for_ingest_gap(&shared.scope_refresh_queued, wait) {
                            fold_scope(&app, &mut index, &agents.detected, &pricing);
                        }
                    }
                    if last_cadence.elapsed() >= fallback {
                        reason = IngestReason::Cadence;
                    }
                }
                // 定时或手动刷新后重新计时；普通报告更新不改动时钟。
                if reason.quota_mode() != QuotaMode::Cached {
                    last_cadence = Instant::now();
                }
                let pass_started = Instant::now();
                last_pass = Instant::now();
                let selected = app.state::<Shared>().settings().active_agent_ids();
                if agents.ids != selected {
                    // 来源切换后重新发现日志，旧监听和文件快照不能沿用。
                    agents = AgentSources::load(selected, &wake_tx);
                    snapshot.clear();
                    wake_roots.clear();
                }
                ingest(&app, &mut index, &agents.adapters, &agents.detected, &pricing, reason, &mut last_publish,
                    &mut snapshot, &mut snapshot_at, &mut wake_roots);
                last_pass_cost = pass_started.elapsed().min(MAX_PASS_COST);
            }
            Work::Resummarize => {
                crate::logging::info("pricing refreshed — re-summarizing indexed events");
                resummarize(&app, &mut index, &agents.detected, &pricing);
                // Wakes collected before the table landed are still pending:
                // `ingest` clears `wake_roots`, this arm leaves them for the next
                // pass, which then walks those tools' trees instead of restating
                // a snapshot built from the old prices.
            }
            Work::Scope => {
                fold_scope(&app, &mut index, &agents.detected, &pricing);
            }
            Work::Stopped => return Exit::Disconnected,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum QuotaMode {
    Cached,
    Scheduled,
    Manual,
}

impl QuotaMode {
    // 区分真正的查询来源，普通发布只读缓存。
    fn request_source(self) -> Option<&'static str> {
        match self {
            Self::Cached => None,
            Self::Scheduled => Some("scheduled"),
            Self::Manual => Some("manual"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IngestReason {
    Initial,
    FileChange,
    Cadence,
    Refresh,
    ManualRefresh,
}

impl IngestReason {
    // 将工作来源转换为日志中使用的名称。
    fn label(self) -> &'static str {
        match self {
            Self::Initial => "initial scan",
            Self::FileChange => "file change",
            Self::Cadence => "cadence timer",
            Self::Refresh => "report refresh",
            Self::ManualRefresh => "manual refresh",
        }
    }

    // 显式刷新可跳过常规文件变动的节流。
    fn is_immediate(self) -> bool {
        matches!(self, Self::Refresh | Self::ManualRefresh)
    }

    // 定时到期或用户主动刷新才允许查询厂商配额。
    fn quota_mode(self) -> QuotaMode {
        match self {
            Self::Cadence => QuotaMode::Scheduled,
            Self::ManualRefresh => QuotaMode::Manual,
            _ => QuotaMode::Cached,
        }
    }
}

enum Work {
    Ingest(IngestReason),
    Resummarize,
    /// The panel picked another machine scope; fold and republish.
    Scope,
    /// Every sender dropped. The only way this loop can end on its own.
    Stopped,
}

// shortcut: scope priority can lag by 25ms; use a wakeable wait if more controls need to interrupt this gap.
fn wait_for_ingest_gap(scope_queued: &AtomicBool, remaining: Duration) -> bool {
    let deadline = Instant::now() + remaining;
    loop {
        if scope_queued.load(Ordering::Acquire) { return true; }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() { return false; }
        std::thread::sleep(remaining.min(Duration::from_millis(25)));
    }
}

// 使用新范围重算本地统计，同时沿用上次配额。
fn fold_scope<R: Runtime>(
    app: &AppHandle<R>,
    index: &mut Option<Index>,
    detected: &[DetectedSource],
    pricing: &PricingMap,
) {
    let shared = app.state::<Shared>();
    if !shared.scope_refresh_queued.swap(false, Ordering::AcqRel) { return; }
    let scope = shared.scope();
    let started = Instant::now();
    crate::logging::info(&format!("scope_fold: phase=start scope={}", scope_kind(&scope)));
    let published = resummarize(app, index, detected, pricing);
    crate::logging::info(&format!(
        "scope_fold: phase=finish scope={} outcome={} total_ms={}",
        scope_kind(&shared.scope()),
        if published { "published" } else { "failed" },
        started.elapsed().as_millis(),
    ));
}

/// Idle until something happens, then debounce a burst of wake-ups into one pass.
fn wait_for_work(
    rx: &Receiver<Msg>,
    pricing: &mut PricingMap,
    fallback: Duration,
    wake_roots: &mut Vec<PathBuf>,
) -> Work {
    let cadence_at = Instant::now() + fallback;
    match rx.recv_timeout(fallback) {
        Ok(Msg::Pricing(map)) => {
            *pricing = map;
            Work::Resummarize
        }
        Ok(Msg::Wake(root)) => {
            wake_roots.push(root);
            // 记录已收到的文件变化，但防抖窗口不得推迟定时刷新。
            match drain_debounce(rx, pricing, wake_roots, cadence_at) {
                Drained::Wakes => Work::Ingest(IngestReason::FileChange),
                Drained::Cadence => Work::Ingest(IngestReason::Cadence),
                Drained::Pricing => Work::Resummarize,
                Drained::Scope => Work::Scope,
                Drained::Refresh(reason) => Work::Ingest(reason),
                Drained::Disconnected => Work::Stopped,
            }
        }
        Ok(Msg::Refresh) => Work::Ingest(IngestReason::Refresh),
        Ok(Msg::RefreshWithQuota) => Work::Ingest(IngestReason::ManualRefresh),
        Ok(Msg::Scope) => Work::Scope,
        Err(RecvTimeoutError::Timeout) => Work::Ingest(IngestReason::Cadence),
        Err(RecvTimeoutError::Disconnected) => Work::Stopped,
    }
}

/// What ended a debounce window.
enum Drained {
    /// The window closed; every wake in it is collected.
    Wakes,
    /// The configured refresh became due during the debounce window.
    Cadence,
    /// An immediate refresh interrupted the debounce.
    Refresh(IngestReason),
    /// A price table landed mid-window.
    Pricing,
    /// The panel switched machine scope mid-window.
    Scope,
    /// Every sender dropped.
    Disconnected,
}

/// Keeps pushing the deadline out while events keep arriving. Whatever woke the
/// engine before the interrupt stays in `wake_roots` — a file change the watcher
/// reported is never dropped on the floor, and a refresh is never downgraded to
/// a plain file-change pass (which can early-return without publishing, leaving
/// the button looking like it did nothing).
fn drain_debounce(
    rx: &Receiver<Msg>,
    pricing: &mut PricingMap,
    wake_roots: &mut Vec<PathBuf>,
    cadence_at: Instant,
) -> Drained {
    let mut deadline = Instant::now() + DEBOUNCE;
    loop {
        if Instant::now() >= cadence_at {
            return Drained::Cadence;
        }
        let remaining = deadline.min(cadence_at).saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(Msg::Wake(root)) => {
                wake_roots.push(root);
                deadline = Instant::now() + DEBOUNCE;
            }
            Ok(Msg::Pricing(map)) => {
                *pricing = map;
                return Drained::Pricing;
            }
            Ok(Msg::Scope) => return Drained::Scope,
            Ok(Msg::Refresh) => return Drained::Refresh(IngestReason::Refresh),
            Ok(Msg::RefreshWithQuota) => return Drained::Refresh(IngestReason::ManualRefresh),
            Err(RecvTimeoutError::Timeout) => {
                return if Instant::now() >= cadence_at { Drained::Cadence } else { Drained::Wakes };
            }
            Err(RecvTimeoutError::Disconnected) => return Drained::Disconnected,
        }
    }
}

// 收集本地日志，再根据此次触发原因决定是否查询额度。
fn ingest<R: Runtime>(
    app: &AppHandle<R>,
    index: &mut Option<Index>,
    adapters: &[Box<dyn SourceAdapter>],
    detected: &[DetectedSource],
    pricing: &PricingMap,
    cause: IngestReason,
    last_publish: &mut Instant,
    snapshot: &mut HashMap<String, Vec<SourceFile>>,
    snapshot_at: &mut Instant,
    wake_roots: &mut Vec<PathBuf>,
) {
    let reason = cause.label();
    let shared = app.state::<Shared>();
    // A second ingest while one is running adds nothing but latency.
    let Ok(_guard) = shared.ingesting.try_lock() else { return };
    if index.is_none() {
        match open_index_or_set_error(
            &app,
            "integrity: the index was repaired on retry; the original image was preserved",
            "index could not be repaired; local usage is unavailable until it is repaired: ",
            "index retry failed: ",
        ) {
            Some(opened) => {
                *index = Some(opened);
                shared.clear_startup_error();
            }
            None => return,
        }
    }
    let Some(index) = index.as_mut() else { return };

    let cutoff = cutoff_ms();
    let filter = DateFilter::new(Some(cutoff), None);
    // Discover scoping: privileged passes (initial scan, manual refresh) and
    // expired snapshots walk every tree; cadence timer restats the cached list
    // unless the TTL expired; a file-change wake walks only the tool that owns
    // the woken root. Every other tool restats its previous list — a couple of
    // stats per file, no walk. A file the watcher missed surfaces within SNAPSHOT_TTL.
    let full = cause == IngestReason::Initial || cause.is_immediate()
        || snapshot_at.elapsed() >= SNAPSHOT_TTL
        || snapshot.is_empty();
    let woken: HashSet<&str> = detected
        .iter()
        .filter(|d| wake_roots.iter().any(|r| d.roots.contains(r)))
        .map(|d| d.id.as_str())
        .collect();
    let prepared: HashMap<String, Vec<SourceFile>> = adapters
        .iter()
        .map(|a| {
            let walk = full || woken.contains(a.id());
            let prev =
                if walk { None } else { Some(snapshot.get(a.id()).cloned().unwrap_or_default()) };
            (a.id().to_string(), a.discover_cached(&filter, prev))
        })
        .collect();
    *snapshot = prepared.clone();
    *snapshot_at = Instant::now();
    wake_roots.clear();
    let hints = |id: &str| prepared.get(id).cloned();
    // A failed pass keeps the previous report on screen rather than blanking it.
    let ingest_started = Instant::now();
    let report = match index.ingest_with_hints(adapters, &filter, &IngestOptions::default(), &hints)
    {
        Ok(r) => {
            // An idle file-change pass (nothing moved) stays out of panel.log.
            if r.files_changed > 0 || r.new_events > 0 || cause != IngestReason::FileChange {
                crate::logging::info(&format!(
                    "ingest({reason}): scanned {} changed {} new {} deduped {} purged {} — {} ms",
                    r.files_scanned, r.files_changed, r.new_events, r.deduped, r.purged, r.took_ms
                ));
            }
            if r.new_events > 0 {
                let per_tool: Vec<String> = r.per_tool.iter().map(|(t, n)| format!("{t}:{n}")).collect();
                crate::logging::info(&format!("ingest indexed per tool: {}", per_tool.join(", ")));
            }
            // The pass's own diagnostics: a failed read (the errors also mark
            // their file for a re-read) and the adapters' silent-failure notes.
            // Rare by design — their presence is the signal.
            if !r.errors.is_empty() {
                crate::logging::error(&format!("ingest({reason}) errors: {}", r.errors.join(" | ")));
            }
            if !r.notes.is_empty() {
                crate::logging::info(&format!("ingest({reason}) notes: {}", r.notes.join(" | ")));
            }
            Some(r)
        }
        Err(e) => {
            crate::logging::error(&format!(
                "ingest({reason}) failed total_ms={}: {e}",
                ingest_started.elapsed().as_millis(),
            ));
            None
        }
    };
    let _ = index.prune(cutoff);
    // Planner-statistics refresh only on the idle pass: it reads enough of the
    // index that per-wake runs were measurable overhead for no benefit.
    if cause == IngestReason::Cadence {
        index.optimize();
    }
    // Sync bundles from other machines land before the publish decision below,
    // so a fresh merge publishes on this same pass. A missing directory is the
    // normal no-sync case; a bad bundle is logged and left for the next round.
    let sync_merged = import_sync_dir(index);
    // A file wake that added nothing new would republish an identical report —
    // on a working machine that is most wakes (editors touch files constantly).
    // A wake that DID add events recomputes the entire report (all indexed
    // history, priced) and that costs hundreds of milliseconds, so drip writers
    // get at most one rebuild per PUBLISH_GAP. The cadence timer and manual
    // refresh always publish, keeping the time-shaped numbers moving. A merged
    // bundle publishes immediately: merges are rare, and the badge's freshness
    // must not wait out a drip writer's gap.
    if cause == IngestReason::FileChange {
        let quiet = report.as_ref().is_some_and(|r| r.new_events == 0);
        if quiet && !sync_merged {
            return;
        }
        if !sync_merged && last_publish.elapsed() < PUBLISH_GAP {
            return;
        }
    }
    *last_publish = Instant::now();
    let sources = index.source_statuses(detected).unwrap_or_else(|_| fallback_sources(detected));
    if report.is_none() {
        crate::logging::error(&format!("ingest({reason}) failed pass — previous report stays on screen"));
    }
    // 定时或手动刷新才更新厂商额度，其余发布复用上次结果。
    let _ = publish(app, index, &sources, pricing, cause.quota_mode());
    // The audit trail runs AFTER the publish, never in front of it: the hourly
    // heartbeat re-parses every codex rollout (tens of seconds on a heavy
    // machine) and the panel must not sit on the boot screen for diagnostics.
    if let Some(r) = &report {
        crate::scan_log::pass(reason, detected, r, index);
        // The DSH paper trail: when the ledger moved, one line saying what the
        // index now holds. (The per-session audit lives in scan.log — this used
        // to re-log every historical row on each append, flooding panel.log.)
        if r.new_events > 0 && r.per_tool.iter().any(|(t, _)| t == "dsh") {
            let dsh_count = r.per_tool.get("dsh").copied().unwrap_or(0);
            let newest = index
                .newest_ts_ms("dsh")
                .ok()
                .flatten()
                .and_then(|ts| chrono::DateTime::from_timestamp_millis(ts))
                .map(|d| d.format("%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "none".into());
            crate::logging::info(&format!("dsh ledger: {dsh_count} events indexed, newest at {newest}"));
        }
    }
}

/// Process-local sha256 cache for sync bundles, keyed by (size, mtime_ns, path).
/// The on-disk `sync_file_memo` only saves the *import*; the sha it is compared
/// against still had to come from a full read of every bundle on every pass.
/// Stat first and reuse the hash while the stat is unchanged, so a busy pass
/// pays a couple of stats per bundle instead of re-reading each one in full; a
/// bundle whose stat moved re-hashes on its next pass, and a restart re-reads
/// once. Deliberately process-local — the memo table stays the library's, and
/// import semantics (memo consulted, memo written by `import_sync`) are
/// unchanged.
static SYNC_SHA_CACHE: LazyLock<Mutex<HashMap<(u64, i64, PathBuf), String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Merges every bundle sitting in `~/kimo-token-sync` that this index has not
/// seen at its current content hash yet. Same function `kimo-token import` uses —
/// hash check, key-based reconciliation, one transaction — so a bundle that
/// fails validation is rejected whole and stays on disk for the next pass,
/// while the panel keeps serving the previous numbers. Returns whether any
/// bundle merged, so the caller knows to publish.
fn import_sync_dir(index: &mut Index) -> bool {
    // 兼容还在往旧收件目录发送文件的采集端。
    let mut entries: Vec<_> = usage_index::sync_import_dirs().into_iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flat_map(|entries| entries.flatten())
        .collect();
    entries.sort_by_key(|e| e.file_name());
    let mut merged = false;
    let mut live: HashSet<PathBuf> = HashSet::new();
    for entry in entries {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            continue;
        };
        // Manifests, `.tmp` artifacts and anything else are not payloads.
        if !name.ends_with(".jsonl.gz") {
            continue;
        }
        live.insert(path.clone());
        // Stat identity for the sha cache: an unchanged (size, mtime) reuses the
        // hash a previous pass computed instead of reading the bundle again. A
        // stat-less or pre-epoch mtime bypasses the cache and hashes as before —
        // the odd case pays, the common one does not.
        let stat = std::fs::metadata(&path).ok().and_then(|meta| {
            let ns = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
            Some((meta.len(), ns.as_nanos() as i64, path.clone()))
        });
        let sha = match stat.as_ref().and_then(|key| SYNC_SHA_CACHE.lock().ok()?.get(key).cloned()) {
            Some(sha) => sha,
            None => match usage_index::sha256_file(&path) {
                Ok(sha) => {
                    if let Some(key) = stat {
                        if let Ok(mut cache) = SYNC_SHA_CACHE.lock() {
                            cache.insert(key, sha.clone());
                        }
                    }
                    sha
                }
                Err(e) => {
                    crate::logging::error(&format!("sync: cannot hash {name}: {e}"));
                    continue;
                }
            },
        };
        match index.sync_file_memo(&name) {
            Ok(Some(known)) if known.eq_ignore_ascii_case(&sha) => continue,
            Ok(_) => {}
            Err(e) => {
                crate::logging::error(&format!("sync: cannot read the memo for {name}: {e}"));
                continue;
            }
        }
        match index.import_sync(&usage_index::ImportOptions { file: path.clone(), dry_run: false })
        {
            Ok(rep) => {
                crate::logging::info(&format!(
                    "sync: merged {name} from {} — {} rows ({} new, {} updated, {} deduped), calls +{}{}",
                    rep.origin, rep.rows, rep.inserted, rep.updated, rep.deduped, rep.calls_added,
                    if rep.self_import { " — origin equals this machine's hostname" } else { "" }
                ));
                if rep.stale_rows > 0 {
                    crate::logging::info(&format!(
                        "sync: {} older row(s) of {} are outside the bundle window (purged upstream); kept",
                        rep.stale_rows, rep.origin
                    ));
                }
                merged = true;
            }
            Err(e) => {
                crate::logging::error(&format!("sync: {name} rejected (kept for retry): {e}"));
            }
        }
    }
    // The cache keys on paths; drop entries for bundles that left the directory
    // so renamed or deleted files cannot grow it without bound.
    if let Ok(mut cache) = SYNC_SHA_CACHE.lock() {
        cache.retain(|(_, _, p), _| live.contains(p));
    }
    merged
}

/// Re-runs the aggregation over already-indexed events using cached quota.
fn resummarize<R: Runtime>(
    app: &AppHandle<R>,
    index: &mut Option<Index>,
    detected: &[DetectedSource],
    pricing: &PricingMap,
) -> bool {
    let Some(index) = index.as_mut() else { return false };
    let sources = index
        .source_statuses(detected)
        .unwrap_or_else(|_| fallback_sources(detected));
    publish(app, index, &sources, pricing, QuotaMode::Cached)
}

/// The first frame: the previous run's own published report, read off disk
/// before this run has touched a source file, a price table or a vendor.
///
/// Restoring it is the difference between a first report at milliseconds and one
/// at 6.8–24.9 s measured on this machine, and every number in it was true when
/// it was folded. The panel labels it as restored, and quota also remains at
/// the last known sample.
fn publish_restored<R: Runtime>(app: &AppHandle<R>) {
    let shared = app.state::<Shared>();
    let settings = shared.settings();
    let Some(mut report) = snapshot::read(&shared.scope(), &settings.active_agent_ids()) else {
        crate::logging::info("boot: nothing to restore — the panel waits for the index fold");
        return;
    };
    let published_at = report.generated_at_ms;
    report.from_previous_run = true;
    // 恢复时只合并本地额度快照，预算继续用已保存的报告。
    let quotas = cached_quota(&report.quotas, &settings);
    report.quotas.retain(|row| row.origin == QuotaOrigin::Budget);
    report.quotas.extend(quotas);
    report.quotas_pending = false;
    if let Ok(mut slot) = app.state::<Shared>().report.lock() {
        *slot = Some(report.clone());
    }
    app.state::<Shared>().clear_startup_error();
    let _ = app.emit(REPORT_EVENT, &report);
    let mode = app.state::<Shared>().settings().tray_mode;
    tray::refresh(app, &report, mode);
    crate::logging::info(&format!(
        "boot: restored last-known report folded {} ms ago",
        now_ms() - published_at
    ));
}

// 发布最新统计；仅定时和手动刷新可以查询配额。
fn publish<R: Runtime>(
    app: &AppHandle<R>,
    index: &mut Index,
    sources: &[SourceStatus],
    pricing: &PricingMap,
    quota_mode: QuotaMode,
) -> bool {
    let shared = app.state::<Shared>();
    let settings = shared.settings();
    let agents = settings.active_agent_ids();
    // 根据刷新来源读取缓存或查询额度。
    let quota = quota_for_report(&shared, &settings, quota_mode);
    let mut scope = shared.scope();
    let folded_at = usage_core::report::instant_after_polling(false, now_ms(), &quota.rows);
    // The fold runs at most twice: an Origin scope naming a machine the fresh
    // fold no longer holds resets to All and re-folds once, so the panel
    // self-corrects within one cadence instead of stranding the user on a
    // machine that has no way back (the scope menu collapses when no remote
    // row remains). `report.machines` is unscoped by construction — the rollup
    // spans the whole index whatever the report's own scope — so a missing
    // origin means the index truly holds no rows for it, not a filtered view.
    // Which origins have left the sync promise (a revoked device on the
    // primary, a severed downlink on the secondary). One read-only probe per
    // publish: any detach or re-pair lands within one cadence, no deletes —
    // the fold just stops counting the rows until the pairing is live again.
    let exclude_origins = usage_device_sync::SyncStore::read_detached_origins(&index_path());
    let mut report = loop {
        let opts = ReportOptions {
            pricing,
            // After the probes, not before them: a vendor CLI that answers late must
            // not leave the report stamped older than a window it is showing.
            now_ms: folded_at,
            sources: sources.iter().filter(|source| settings.agent_enabled(&source.id)).cloned().collect(),
            recent_session_limit: 12,
            // The rare second fold re-uses the pass's samples; the vec is a
            // dozen rows, noise next to the fold itself.
            polled_quota: quota.rows.clone(),
            budgets: settings.budgets.iter().filter(|(tool, _)| settings.agent_enabled(tool))
                .map(|(tool, budget)| (tool.clone(), budget.clone())).collect(),
            scope: scope.clone(),
            exclude_origins: exclude_origins.clone(),
        };
        // Every period's numbers fold from the day rollup (~2k rows) plus the
        // plan's live slices over today — never from re-reading all events. The
        // scope travels into the fetch as well: the sessions' top-N is applied in
        // SQL, so it must already be the top-N of the chosen scope — and of the
        // detached set, or a departed origin would fill the slots.
        let plan = AggregatePlan::build(opts.now_ms, opts.recent_session_limit);
        let facts_started = Instant::now();
        let mut facts = match index.report_facts_for_tools(&plan, &scope, &exclude_origins, Some(&agents)) {
            Ok(facts) => facts,
            Err(e) => {
                crate::logging::error(&format!(
                    "publish: facts query failed scope={} total_ms={} — previous report stays on screen: {e}",
                    scope_kind(&scope), facts_started.elapsed().as_millis(),
                ));
                return false;
            }
        };
        if facts.rebuilt {
            crate::logging::info(
                "publish: rebuilt the day rollup from the event table (first publish after an \
                 upgrade, or after a fail-safe invalidation)",
            );
        }
        // 文件变动可更新用量，额度样本仅在定时或手动刷新时纳入。
        facts.quotas.retain(|sample| quota.queried_tools.contains(&sample.tool.as_str()));
        let folded = usage_core::report::summarize_facts(&facts, &opts);
        if let MachineScope::Origin { name } = &scope {
            // Correct only the scope this fold actually used: a pick that
            // landed mid-fold has already replaced the stored scope and queued
            // its own Msg::Scope, and clobbering it with All would silently
            // drop the user's click (the same stale-read race the tray-mode
            // refresh guards below). If the newer pick names a dead machine
            // too, its own pass resets it the same way.
            if !folded.machines.iter().any(|machine| &machine.origin == name) && shared.scope() == scope {
                let _ = shared.set_scope(MachineScope::All, None);
                crate::logging::info(&format!(
                    "scope_reset: origin {name:?} matches no machine in the fold — scope back to all"
                ));
                scope = MachineScope::All;
                continue;
            }
        }
        break folded;
    };
    let cycle_usage_started = Instant::now();
    if let Err(e) = index.attach_cycle_usage(&mut report.quotas, report.generated_at_ms) {
        crate::logging::error(&format!(
            "publish: cycle token usage failed total_ms={} — quota rows stay visible without the summary: {e}",
            cycle_usage_started.elapsed().as_millis(),
        ));
    }
    // 查询额度或折叠期间可能保存了新选择，交给队列里的刷新发布最新范围。
    if shared.settings().active_agent_ids() != agents {
        return false;
    }
    if let Ok(mut slot) = shared.report.lock() {
        *slot = Some(report.clone());
    }
    // The next launch's first frame. Written here rather than on the boot paths so
    // that whatever the panel has been shown is also what gets remembered — a
    // snapshot of a report nobody saw would make "last known" mean two things.
    snapshot::write(&report, &agents);
    // The notify worker owns threshold deduplication and delivery settings.
    let displays: HashMap<String, String> =
        sources.iter().map(|s| (s.id.clone(), s.display.clone())).collect();
    let gate = notify::Gate::from(&app.state::<Shared>().settings());
    notify::observe(&report.quotas, &displays, crate::lang::get(), gate);
    let _ = app.emit(REPORT_EVENT, &report);
    // 手动查询期间可能切换了菜单栏模式，发布前读取最新设置。
    let mode = app.state::<Shared>().settings().tray_mode;
    tray::refresh(app, &report, mode);
    true
}

/// The host-exit half of the pass gate: whether the host process test lets a
/// tool through, or the pause is off altogether.
///
/// A claim-bearing tool with `auto_checkin` on is never host-paused: its daily
/// grant is the one number that still moves while the host app is closed, and
/// the claim itself is an HTTPS call against the account — not a read of the
/// host's state — so a closed IDE must not silence it, and its rows stay live
/// in the strip. The user's own switches compose on top in the caller; this
/// answers the host half alone so the refusal log can ask the same function
/// and never name a tool the pass actually asked.
fn host_gate_open(tool: &str, auto_checkin: bool, alive: &Option<HashSet<String>>) -> bool {
    if auto_checkin && usage_quota::is_checkin_tool(tool) {
        return true;
    }
    match alive {
        Some(names) => usage_quota::host::any_host_running(tool, names),
        None => true,
    }
}

struct QuotaForReport {
    rows: Vec<QuotaView>,
    queried_tools: Vec<&'static str>,
}

// 继续展示启用工具的缓存，重新启用时可从磁盘恢复上次结果。
fn cached_quota(previous: &[QuotaView], settings: &Settings) -> Vec<QuotaView> {
    settings.active_agent_ids().into_iter()
        .filter(|tool| !settings.quota_probes_off.iter().any(|off| off == tool))
        .flat_map(|tool| {
            let rows: Vec<_> = previous.iter().filter(|row| row.tool == tool && row.origin != QuotaOrigin::Budget)
                .cloned().collect();
            if rows.is_empty() { usage_quota::cached_for_tool(tool) } else { rows }
        }).collect()
}

// 普通发布沿用快照；定时或手动刷新读取厂商与日志里的新额度。
fn quota_for_report(shared: &Shared, settings: &Settings, mode: QuotaMode) -> QuotaForReport {
    let previous = shared.report.lock().ok().and_then(|slot| {
        slot.as_ref().map(|report| report.quotas.clone())
    });
    let mut rows = cached_quota(previous.as_deref().unwrap_or_default(), settings);
    let mut queried_tools = Vec::new();
    if let Some(source) = mode.request_source() {
        queried_tools = quota_request_tools(settings);
        if !queried_tools.is_empty() {
            let fresh = refresh_quota(source, &queried_tools);
            rows.retain(|row| !queried_tools.contains(&row.tool.as_str()));
            rows.extend(fresh);
        }
    }
    QuotaForReport { rows, queried_tools }
}

// 尊重总开关、厂商开关及宿主退出暂停设置。
fn quota_request_tools(settings: &Settings) -> Vec<&'static str> {
    if !settings.quota_polling {
        return Vec::new();
    }
    let agents = settings.active_agent_ids();
    if agents.is_empty() {
        return agents;
    }
    let alive = settings.host_exit_pause.then(usage_quota::host::running_process_names);
    agents.into_iter()
        .filter(|tool| settings.probe_allowed(tool) && host_gate_open(tool, settings.auto_checkin, &alive))
        .collect()
}

// 在后台引擎线程查询配额，失败时由缓存层保留上次成功的结果。
fn refresh_quota(source: &str, tools: &[&str]) -> Vec<QuotaView> {
    let started = Instant::now();
    let rows = usage_quota::refresh_tools(tools);
    let notes: Vec<_> = usage_quota::take_probe_notes().into_iter()
        .filter(|(tool, _)| tools.contains(tool))
        .map(|(_, note)| note)
        .collect();
    crate::logging::info(&format!(
        "quota refresh: {source} request returned {} windows in {} ms{}",
        rows.len(), started.elapsed().as_millis(),
        if notes.is_empty() { String::new() } else { format!(" ({})", notes.join(", ")) },
    ));
    rows
}

fn scope_kind(scope: &MachineScope) -> &'static str {
    match scope {
        MachineScope::All => "all",
        MachineScope::Local => "local",
        MachineScope::Origin { .. } => "origin",
    }
}

fn fallback_sources(detected: &[DetectedSource]) -> Vec<SourceStatus> {
    detected
        .iter()
        .map(|d| SourceStatus {
            id: d.id.clone(),
            display: d.display.clone(),
            detected: true,
            roots: d.roots.clone(),
            hint: d.hint.clone(),
            events_ingested: 0,
        })
        .collect()
}

/// The one open of the index shared by the boot (`run`) and the mid-run retry
/// (`ingest`): `Ok(_, true)` is the integrity-preserving recreate, `Err` names
/// the failure through the startup-error slot and the log. The three message
/// tails keep the boot and the retry distinguishable in `panel.log` while the
/// branch shape exists exactly once; the error text is appended by the helper.
fn open_index_or_set_error<R: Runtime>(
    app: &AppHandle<R>,
    recovered_log: &'static str,
    failed_startup: &'static str,
    failed_log: &'static str,
) -> Option<Index> {
    match Index::open_recovering_corruption(index_path()) {
        Ok((index, true)) => {
            crate::logging::error(recovered_log);
            Some(index)
        }
        Ok((index, false)) => Some(index),
        Err(e) => {
            app.state::<Shared>().set_startup_error(format!("{failed_startup}{e}"));
            crate::logging::error(&format!("{failed_log}{e}"));
            None
        }
    }
}

/// `Index::default_path()` wins; a missing platform dir falls back to cache.
pub(crate) fn index_path() -> PathBuf {
    Index::default_path().unwrap_or_else(|| {
        usage_core::paths::data_dir()
            .unwrap_or_else(|| PathBuf::from("kimo-token"))
            .join("usage.sqlite")
    })
}

fn cutoff_ms() -> i64 {
    usage_index::retention_cutoff(now_ms())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two tables the tests can tell apart without fetching anything: `cache_dir`
    /// is echoed into the meta, and an offline load with no dir falls back to the
    /// bundled snapshot.
    fn table(mark: Option<&str>) -> PricingMap {
        PricingMap::load(&PricingOptions {
            offline: true,
            cache_dir: mark.map(PathBuf::from),
            overrides: Default::default(),
        })
    }

    fn marked(pricing: &PricingMap) -> bool {
        pricing.meta().cache_dir.is_some()
    }

    /// The exact shape that froze the panel on 2026-10-04: the watcher wakes the
    /// engine and a price table lands before the debounce window closes. The loop
    /// has to survive it, apply the table, and still owe the wake a read.
    #[test]
    fn a_price_table_mid_debounce_never_ends_the_loop() {
        let (tx, rx) = mpsc::channel();
        let mut pricing = table(None);
        let mut wakes = Vec::new();
        tx.send(Msg::Wake(PathBuf::from("/root/codex"))).unwrap();
        tx.send(Msg::Pricing(table(Some("/tmp/pricing-swap")))).unwrap();

        let work = wait_for_work(&rx, &mut pricing, DEBOUNCE * 10, &mut wakes);
        assert!(matches!(work, Work::Resummarize), "a pricing swap is a pass, not an exit");
        assert!(marked(&pricing), "the table that landed must be the live one");
        assert_eq!(wakes, vec![PathBuf::from("/root/codex")], "the wake is still owed a read");
        // The channel must still be usable afterwards — that is the whole
        // difference between "restarts" and "the panel goes quiet forever".
        tx.send(Msg::Refresh).unwrap();
        assert!(matches!(
            wait_for_work(&rx, &mut pricing, DEBOUNCE, &mut wakes),
            Work::Ingest(IngestReason::Refresh)
        ));
    }

    #[test]
    fn a_burst_of_wakes_is_one_pass_and_keeps_every_root() {
        let (tx, rx) = mpsc::channel();
        let mut pricing = table(None);
        let mut wakes = Vec::new();
        for root in ["/root/a", "/root/b", "/root/c"] {
            tx.send(Msg::Wake(PathBuf::from(root))).unwrap();
        }
        let started = Instant::now();
        let work = wait_for_work(&rx, &mut pricing, DEBOUNCE * 10, &mut wakes);
        assert!(matches!(work, Work::Ingest(IngestReason::FileChange)));
        assert_eq!(wakes.len(), 3, "the debounce merges passes, never events");
        assert!(started.elapsed() >= DEBOUNCE, "the window has to actually hold");
    }

    /// A refresh arriving inside the window used to be answered with a plain
    /// `file change` pass — which is allowed to early-return without publishing,
    /// so the button the user pressed when the numbers looked frozen could do
    /// nothing at all.
    // 确认普通更新和手动刷新在防抖期间不会混用额度策略。
    #[test]
    fn a_refresh_inside_the_debounce_window_is_still_a_manual_pass() {
        let (tx, rx) = mpsc::channel();
        let mut pricing = table(None);
        let mut wakes = Vec::new();
        for (msg, expected) in [(Msg::Refresh, QuotaMode::Cached), (Msg::RefreshWithQuota, QuotaMode::Manual)] {
            tx.send(Msg::Wake(PathBuf::from("/root/a"))).unwrap();
            tx.send(msg).unwrap();
            let work = wait_for_work(&rx, &mut pricing, DEBOUNCE * 10, &mut wakes);
            assert!(matches!(work, Work::Ingest(reason) if reason.is_immediate() && reason.quota_mode() == expected));
        }
        assert_eq!(wakes.len(), 2);
    }

    // 到期的定时刷新优先于文件防抖，已收到的文件变化仍由本轮处理。
    #[test]
    fn quota_cadence_survives_a_pending_file_wake() {
        let (tx, rx) = mpsc::channel();
        let mut pricing = table(None);
        let mut wakes = Vec::new();
        let root = PathBuf::from("/root/codex");
        tx.send(Msg::Wake(root.clone())).unwrap();

        let work = wait_for_work(&rx, &mut pricing, Duration::from_millis(5), &mut wakes);
        assert!(matches!(work, Work::Ingest(reason) if reason.quota_mode() == QuotaMode::Scheduled));
        assert_eq!(wakes, vec![root]);
    }

    #[test]
    fn only_a_lost_channel_stops_the_loop() {
        let (tx, rx) = mpsc::channel::<Msg>();
        drop(tx);
        let mut pricing = table(None);
        let mut wakes = Vec::new();
        assert!(matches!(
            wait_for_work(&rx, &mut pricing, DEBOUNCE, &mut wakes),
            Work::Stopped
        ));

        // Everything else stays a pass: a table on its own, then quiet.
        let (tx, rx) = mpsc::channel();
        tx.send(Msg::Pricing(table(Some("/tmp/other")))).unwrap();
        assert!(matches!(
            wait_for_work(&rx, &mut pricing, DEBOUNCE, &mut wakes),
            Work::Resummarize
        ));
        assert!(matches!(
            wait_for_work(&rx, &mut pricing, Duration::from_millis(1), &mut wakes),
            Work::Ingest(IngestReason::Cadence)
        ));
    }

    #[test]
    fn a_panic_always_reads_as_a_reason() {
        let text: Box<dyn std::any::Any + Send> = Box::new("index went away");
        assert_eq!(panic_text(&*text), "panicked: index went away");
        let owned: Box<dyn std::any::Any + Send> = Box::new(String::from("lock poisoned"));
        assert_eq!(panic_text(&*owned), "panicked: lock poisoned");
        // A payload nobody can read still has to say that the engine died.
        let opaque: Box<dyn std::any::Any + Send> = Box::new(7u8);
        assert_eq!(panic_text(&*opaque), "panicked: non-string panic payload");
    }

    #[test]
    fn startup_error_is_cleared_after_a_successful_index_open() {
        let shared = Shared::new(Settings::default());
        assert!(shared.startup_error().is_none());
        shared.set_startup_error("index could not be opened");
        assert_eq!(shared.startup_error().as_deref(), Some("index could not be opened"));
        shared.clear_startup_error();
        assert!(shared.startup_error().is_none());
    }

    #[test]
    fn scope_switch_burst_queues_once_and_keeps_latest_scope() {
        let shared = Shared::new(Settings::default());
        let (tx, rx) = mpsc::channel();
        let remote = MachineScope::Origin { name: "remote-device".into() };

        assert_eq!(shared.set_scope(MachineScope::Local, Some(&tx)), Ok((true, true)));
        assert_eq!(shared.set_scope(remote.clone(), Some(&tx)), Ok((true, false)));
        assert_eq!(shared.scope(), remote.clone());
        assert!(matches!(rx.try_recv(), Ok(Msg::Scope)));
        assert!(rx.try_recv().is_err(), "a scope burst must queue only one fold");

        shared.scope_refresh_queued.store(false, Ordering::Release);
        assert_eq!(shared.set_scope(remote, Some(&tx)), Ok((false, false)));
        assert!(rx.try_recv().is_err(), "selecting the active scope must be a no-op");
    }

    #[test]
    fn queued_scope_interrupts_adaptive_ingest_gap() {
        let queued = std::sync::Arc::new(AtomicBool::new(false));
        let wake = std::sync::Arc::clone(&queued);
        let setter = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(25));
            wake.store(true, Ordering::Release);
        });
        let started = Instant::now();
        assert!(wait_for_ingest_gap(&queued, Duration::from_secs(15)));
        assert!(started.elapsed() < Duration::from_millis(500));
        setter.join().unwrap();
    }

    /// The `auto_checkin` exemption is exactly as wide as the claim registry:
    /// with the switch on, a check-in host that is NOT running still passes
    /// the host gate — the claim is HTTPS and a closed IDE is no reason to
    /// miss the day — while every other tool keeps following its process.
    #[test]
    fn auto_checkin_exempts_only_the_claim_bearers_from_the_host_pause() {
        let alive = Some(HashSet::new());
        assert!(!host_gate_open("trae_cn", false, &alive), "off keeps today's rule");
        assert!(host_gate_open("trae_cn", true, &alive), "on: the claim fires with the IDE closed");
        assert!(host_gate_open("qoder", true, &alive));
        assert!(host_gate_open("qoder_cn", true, &alive));
        assert!(!host_gate_open("qoder_cn", false, &alive));
        assert!(!host_gate_open("claude", true, &alive), "not a claim bearer");
        assert!(host_gate_open("zcode", true, &alive), "zcode claims over HTTPS — the IDE need not run");
        assert!(host_gate_open("copilot", true, &alive), "no host mapping — never paused");
        assert!(host_gate_open("trae_cn", false, &None), "pause off means everything passes");

        let mut running: HashSet<String> = HashSet::new();
        running.insert("Trae CN Helper".to_string());
        assert!(host_gate_open("trae_cn", false, &Some(running)), "a running host passes as before");
    }

    #[test]
    fn restart_delay_grows_and_caps() {
        assert_eq!(restart_delay(1), Duration::from_secs(2));
        assert_eq!(restart_delay(4), Duration::from_secs(16));
        // The cap has to bind — a decorative `RESTART_CAP` would leave a
        // hard-failing engine restarting every 512 s forever — and an unbounded
        // attempt counter must not overflow the shift.
        assert_eq!(restart_delay(9), RESTART_CAP);
        assert_eq!(restart_delay(u32::MAX), RESTART_CAP);
    }
}
