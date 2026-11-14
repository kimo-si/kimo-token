import type { Item, MachineScope, PeriodKey, Report } from "../types";
import { t, type StrKey } from "../lib/i18n";
import { CallTabs } from "./CallTabs";
import { EmptyState } from "./EmptyState";
import { moveSelection, PeriodControl } from "./Header";
import { Loading } from "./Loading";
import { RankedList } from "./RankedList";
import { Sessions } from "./Sessions";
import { Sources } from "./Sources";
import { ToolsSection } from "./ToolsSection";

const VIEWS = [
  { key: "tools", label: "page.tools", hint: "page.tools.hint" },
  { key: "ranks", label: "page.ranks", hint: "page.ranks.hint" },
  { key: "sessions", label: "page.sessions", hint: "page.detail.hint" },
  { key: "sources", label: "page.sources", hint: "page.sources" },
] as const satisfies ReadonlyArray<{ key: string; label: StrKey; hint: StrKey }>;
export type AnalysisView = typeof VIEWS[number]["key"];

interface Props {
  report: Report;
  period: PeriodKey;
  scope: MachineScope;
  tools: Item[];
  view: AnalysisView;
  now: number;
  empty: boolean;
  loading: boolean;
  onView: (view: AnalysisView) => void;
  onPeriod: (period: PeriodKey) => void;
  onRefresh: () => void;
}

// 将现有排行放进分析页的排行视图。
function Rankings({ window }: { window: Report[PeriodKey] }) {
  return (
    <>
      <RankedList label={t("sec.tools")} items={window.breakdown.tools} limit={8} weight="tokens" />
      <RankedList label={t("sec.models")} items={window.breakdown.models} limit={8} unpriced={window.summary.unpriced} />
      <RankedList label={t("sec.projects")} items={window.breakdown.projects} limit={8} />
      <CallTabs mcps={window.breakdown.mcps} skills={window.breakdown.skills} />
    </>
  );
}

// 保留原有分析内容并让数据源在无用量时仍可检查。
function AnalysisContent({ report, period, scope, tools, view, now, empty, loading, onRefresh }: Props) {
  if (view === "sources") return <Sources sources={report.sources} allTime={report.all_time} scope={scope} />;
  if (empty) return loading
    ? <div className="empty empty-rescan"><Loading size={34} label={t("empty.scanning")} /></div>
    : <EmptyState onRefresh={onRefresh} />;
  if (view === "tools") return <ToolsSection tools={tools} />;
  if (view === "sessions") return <Sessions rows={report.recent_sessions} now={now} />;
  return <Rankings window={report[period]} />;
}

// 分析页为工具、排行、会话和数据源提供同一个入口。
export function AnalysisPage(props: Props) {
  const { period, view, onPeriod, onView } = props;
  const hasPeriod = view === "tools" || view === "ranks";
  return (
    <>
      <div className="analysis-heading">
        <div className="overview-summary-heading">
          <h1>{t("page.analysis.title")}</h1>
          {hasPeriod && <PeriodControl period={period} onPeriod={onPeriod} />}
        </div>
        <div className="analysis-tabs" role="tablist" aria-label={t("page.analysis")}>
          {VIEWS.map(({ key, label, hint }, index) => (
            <button key={key} id={`analysis-tab-${key}`} type="button" role="tab" title={t(hint)}
              aria-selected={key === view} aria-controls={key === view ? "analysis-content" : undefined}
              tabIndex={key === view ? 0 : -1} onClick={() => onView(key)}
              onKeyDown={(event) => moveSelection(event, index, VIEWS.length, (next) => onView(VIEWS[next].key))}>
              {t(label)}
            </button>
          ))}
        </div>
      </div>
      <div id="analysis-content" role="tabpanel" aria-labelledby={`analysis-tab-${view}`}>
        <AnalysisContent {...props} />
      </div>
    </>
  );
}
