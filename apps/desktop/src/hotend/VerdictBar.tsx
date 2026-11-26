// The verdict bar (DESIGN_SPEC 3.4): a capsule at the top left of the stage with a status
// icon, one sentence and at most one primary and one quiet button. Its content is a pure
// function of the project's state (./verdict.ts).
import { Button } from "@/components/ui/button";

import { StatusIcon } from "./bits";
import type { Verdict, VerdictAction } from "./verdict";

export function VerdictBar({
  verdict,
  onAction,
  onAbout,
}: {
  verdict: Verdict;
  onAction(action: VerdictAction): void;
  /** A click on the sentence: scroll the inspector to what it speaks about. */
  onAbout?: () => void;
}) {
  // New words fade in; a number that counts (a layer, a clock) changes in place.
  const swap = verdict.sentence.replace(/\d+/g, "0");
  return (
    <div
      role="status"
      aria-live="polite"
      data-testid="verdict"
      data-kind={verdict.icon}
      // The sentence is never cut: where the stage is narrow it runs on to a second line
      // and the bar grows with it (its corners stay those of the one-line capsule).
      // The buttons go under the sentence where the text is set larger or the stage is narrow.
      className="flex min-h-10 max-w-full min-w-0 flex-wrap items-center gap-x-2 gap-y-1 rounded-[20px] border border-app-line bg-app-raised py-1 pr-1.5 pl-3 shadow-1"
    >
      <StatusIcon kind={verdict.icon} />
      {verdict.about && onAbout ? (
        <button
          key={swap}
          type="button"
          onClick={onAbout}
          data-testid="verdict-sentence"
          className="app-swap t-ui-strong inline-flex min-h-7 min-w-[20ch] flex-1 items-center rounded-tag text-left text-app-text"
        >
          {verdict.sentence}
        </button>
      ) : (
        <span
          key={swap}
          data-testid="verdict-sentence"
          className="app-swap t-ui-strong min-w-[20ch] flex-1 pr-1.5 text-app-text"
        >
          {verdict.sentence}
        </span>
      )}
      {verdict.primary ? (
        <Button
          size="sm"
          variant={verdict.primary.calm ? "outline" : "default"}
          className="shrink-0 rounded-pill px-3"
          data-testid="verdict-primary"
          onClick={() => {
            if (verdict.primary) onAction(verdict.primary.action);
          }}
        >
          {verdict.primary.label}
        </Button>
      ) : null}
      {verdict.quiet ? (
        <Button
          variant="ghost"
          size="sm"
          className="shrink-0 rounded-pill px-2.5 text-app-text-2"
          data-testid="verdict-quiet"
          onClick={() => {
            if (verdict.quiet) onAction(verdict.quiet.action);
          }}
        >
          {verdict.quiet.label}
        </Button>
      ) : null}
    </div>
  );
}
