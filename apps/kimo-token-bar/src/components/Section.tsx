import { Card } from "@heroui/react";
import { t } from "../lib/i18n";
import { IconChevron } from "./Icons";

interface SectionProps {
  label: string;
  meta?: string;
  /** A compact control (e.g. the sort toggle) sitting beside the meta text. */
  trail?: React.ReactNode;
  /** Replaces the label/meta pair outright, for a section whose header is a
   *  control (the 活动 section's 今日/活动 view seg). */
  head?: React.ReactNode;
  children: React.ReactNode;
  /** Take the page's leftover height instead of the body's own. For a section
      that stands in for a whole list while it waits: the spinner then sits in
      the middle of the rows the list would have had. */
  fill?: boolean;
  className?: string;
}

/** A hairline-separated block. Deliberately not a card: no radius, no shadow.
 *  The hairline belongs to the block below a pair (`.sec + .sec`), so a page's
 *  last section cannot end in a line stranded over blank panel space. */
// 包装各页面的内容区，并允许概览为图表和卡片提供专属样式。
export function Section({ label, meta, trail, head, children, fill, className }: SectionProps) {
  return (
    <Card variant="transparent" role="region" aria-label={label} className={["sec", fill && "sec-fill", className].filter(Boolean).join(" ")}>
      <Card.Header className="sec-hd">
        {head ?? (
          <>
            <Card.Title className="sec-label">{label}</Card.Title>
            {meta || trail ? (
              <span className="sec-right">
                {trail}
                {meta ? <span className="sec-meta num">{meta}</span> : null}
              </span>
            ) : null}
          </>
        )}
      </Card.Header>
      <Card.Content className="sec-content">{children}</Card.Content>
    </Card>
  );
}

interface MoreProps {
  open: boolean;
  /** Everything the list has, including what the preview hides. */
  total: number;
  /** How many the collapsed list shows. */
  preview: number;
  onToggle: () => void;
  /** Non-count semantics: the 数据源 tail hides "undetected", not "more items". */
  closedLabel?: string;
  openLabel?: string;
}

/**
 * The one affordance for a truncated list, so no section ever quietly withholds
 * rows. It carries the hidden count before the click — "另 13" answers *how much*
 * is behind it, which a bare "更多" would not.
 */
export function MoreRow({ open, total, preview, onToggle, closedLabel, openLabel }: MoreProps) {
  if (total <= preview) return null;
  return (
    <button type="button" className="more-row" aria-expanded={open} onClick={onToggle}>
      <IconChevron size={12} className="more-icon" />
      <span>{open ? openLabel ?? t("more.open", { n: preview }) : closedLabel ?? t("more.close", { n: total })}</span>
      {open ? null : <span className="more-n num">+{total - preview}</span>}
    </button>
  );
}
