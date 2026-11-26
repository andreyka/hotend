// The version rail (DESIGN_SPEC 5.5): one dot per version under the 3D view, oldest left,
// and a card that compares a version with the one before it. Arrow keys walk the dots,
// Tab enters the card of the dot in focus.
import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router";

import { Button } from "@/components/ui/button";
import type { EngineResult, Version } from "@/lib/core";
import { cn } from "@/lib/utils";

import { useHotend, useNow } from "./context";
import { agoInline, sized } from "./format";
import { countsText, proofOf } from "./proof";
import { sizesOf } from "./sizes";
import { ACTIVE_JOB, printedVersion, versionOf } from "./store";
import { SnapshotsButton } from "./Snapshots";
import { S } from "./strings";
import { shownJob, type Stage } from "./verdict";
import { diffVersions, dotName, sizeChanges, versionLabel } from "./versions";

export type Dot = { version: Version; run: number };

/**
 * The dots of the rail: a run of changes to one size collapses to its last dot with a
 * count. A run is a chain of `set_parameter` versions, each made from the one before it,
 * that all change the same one size.
 */
export function railDots(versions: readonly Version[]): Dot[] {
  const byNumber = new Map(versions.map((version) => [version.number, version]));
  const changed = (version: Version): string | null => {
    if (version.origin !== "set_parameter" || version.parent === null) return null;
    const names = sizeChanges(byNumber.get(version.parent), version).map((change) => change.name);
    return names.length === 1 ? (names[0] ?? null) : null;
  };
  const dots: Dot[] = [];
  for (const version of versions) {
    const last = dots.at(-1);
    const name = changed(version);
    if (last && name !== null && version.parent === last.version.number && changed(last.version) === name) {
      dots[dots.length - 1] = { version, run: last.run + 1 };
    } else {
      dots.push({ version, run: 1 });
    }
  }
  return dots;
}

const CARD_WIDTH = 360;
/** What the card leaves free at the right end of the stage: the overall-size chip. */
const CHIP_ROOM = 148;

function Card({
  version,
  before,
  result,
  beforeResult,
  current,
  onStage,
  now,
  onLook,
  onUse,
}: {
  version: Version;
  before: Version | null;
  result: EngineResult | undefined;
  beforeResult: EngineResult | undefined;
  current: number | null;
  onStage: number | null;
  now: number;
  /** `byKey`: the button was pressed from the keyboard, so the focus needs a place to go. */
  onLook(byKey: boolean): void;
  onUse(): void;
}) {
  const diff = diffVersions(before ? { version: before, result: beforeResult } : null, { version, result });
  const proof = proofOf(result);
  const first = before === null;
  const firstSizes = first ? sizesOf(result).slice(0, 6) : [];
  const isCurrent = version.number === current;
  return (
    <div className="space-y-1.5" data-testid="version-card" data-version={version.number}>
      <p className="t-ui-strong text-app-text">
        {versionLabel(version, before, result)}
        <span className="font-normal text-app-text-2"> · {agoInline(version.created_at_ms, now)}</span>
      </p>
      {version.origin !== "set_parameter" && version.note ? (
        <p className="t-ui line-clamp-3 text-app-text-2">“{version.note}”</p>
      ) : null}
      {first ? (
        <>
          {firstSizes.map((size) => (
            <p key={size.name} className="t-ui num flex justify-between gap-3">
              <span className="min-w-0 truncate text-app-text-2">{size.label}</span>
              <span>{sized(size.value, size.unit)}</span>
            </p>
          ))}
          {result ? <p className="t-ui num text-app-text-2">{S.rail.proofCount(countsText(proof.counts))}</p> : null}
        </>
      ) : (
        <>
          {diff.sizes.map((size) => (
            <p key={size.name} className="t-ui num flex justify-between gap-3">
              <span className="min-w-0 truncate text-app-text-2">{size.label}</span>
              <span className="shrink-0 whitespace-nowrap">
                {sized(size.from, size.unit).replace(/ mm$/, "")} → {sized(size.to, size.unit)}
              </span>
            </p>
          ))}
          {/* A pair of values is never broken: one row for the size, one for the weight. */}
          {diff.parts.flatMap((part) =>
            [
              part.size ? { key: `${part.name}:size`, label: S.rail.partSize(part.name), value: part.size } : null,
              part.mass ? { key: `${part.name}:mass`, label: S.rail.partWeight(part.name), value: part.mass } : null,
            ]
              .filter((row) => row !== null)
              .map((row) => (
                <p key={row.key} className="t-ui num flex justify-between gap-3" data-testid="card-part-row">
                  <span className="min-w-0 truncate text-app-text-2">{row.label}</span>
                  <span className="shrink-0 whitespace-nowrap">{row.value}</span>
                </p>
              )),
          )}
          {diff.nowFails.length > 0 ? (
            <p className="t-ui text-app-fail">{S.marker.nowFails(diff.nowFails.join(", "))}</p>
          ) : null}
          {diff.nowPasses.length > 0 ? (
            <p className="t-ui text-app-text">{S.marker.nowPasses(diff.nowPasses.join(", "))}</p>
          ) : null}
          {diff.sameProof ? <p className="t-ui num text-app-text-2">{diff.sameProof}</p> : null}
        </>
      )}
      {isCurrent ? (
        <p className="t-caption text-app-text-2">{S.rail.current}</p>
      ) : (
        <div className="flex justify-between gap-2 pt-1.5">
          {version.number === onStage ? (
            <span />
          ) : (
            <Button
              variant="outline"
              size="sm"
              onClick={(event) => {
                onLook(event.detail === 0);
              }}
              data-testid="look-at"
            >
              {S.rail.lookAt(version.number)}
            </Button>
          )}
          <Button variant="outline" size="sm" onClick={onUse} data-testid="use-version">
            {S.rail.use(version.number)}
          </Button>
        </div>
      )}
    </div>
  );
}

export function VersionRail({ stage, projectId }: { stage: Stage; projectId: string }) {
  const navigate = useNavigate();
  const detail = useHotend((state) => state.project);
  const results = useHotend((state) => state.results);
  const look = useHotend((state) => state.look);
  // On Print the stage shows the version that is printed, which the design may have left behind.
  const printJob = useHotend((state) => (stage === "print" ? shownJob(state) : null));
  const lookVersion = useHotend((state) => state.lookAt);
  const makeCurrent = useHotend((state) => state.useVersion);
  const now = useNow(true);

  // Looking at a version happens in Design: from another stage the window goes there.
  const lookAt = (version: number | null) => {
    if (stage !== "design") void navigate(`/hotend/p/${projectId}/design`);
    return lookVersion(version);
  };

  const root = useRef<HTMLDivElement>(null);
  const strip = useRef<HTMLDivElement>(null);
  const [card, setCard] = useState<{ version: number; left: number } | null>(null);
  const [focusOn, setFocusOn] = useState<number | null>(null);

  const versions = useMemo(() => detail?.versions ?? [], [detail]);
  const dots = useMemo(() => railDots(versions), [versions]);
  const current = detail?.project.current_version ?? null;
  const onStage = stage === "design" ? (look ?? current) : stage === "print" ? printedVersion(printJob, current) : current;

  // After "Look at v1" from the keyboard the card is gone: the focus goes to the dot of
  // the version that is now on the stage, not to the page.
  const focusStage = useRef(false);
  useEffect(() => {
    if (!focusStage.current) return;
    focusStage.current = false;
    strip.current?.querySelector<HTMLButtonElement>("button[data-on-stage]")?.focus();
  }, [onStage]);

  // The newest dots are in view.
  const count = dots.length;
  useEffect(() => {
    const element = strip.current;
    if (element) element.scrollLeft = element.scrollWidth;
  }, [count]);

  if (versions.length === 0 || current === null) {
    return <div className="h-8 shrink-0 border-t border-app-line bg-app-panel" data-testid="version-rail" />;
  }

  const open = (version: number, dot: HTMLElement) => {
    const frame = root.current?.getBoundingClientRect();
    if (!frame) return;
    const at = dot.getBoundingClientRect();
    const left = Math.max(8, Math.min(frame.width - CARD_WIDTH - CHIP_ROOM, at.left + at.width / 2 - frame.left - 24));
    setCard({ version, left });
  };
  const stageVersion = versionOf(detail, onStage);
  const tabStop = focusOn ?? onStage;
  const stageDot = dots.find((dot) => dot.version.number === onStage) ?? dots.findLast((dot) => dot.version.number <= (onStage ?? 0));
  const shown = card ? versionOf(detail, card.version) : null;
  const shownBefore = shown ? versionOf(detail, shown.parent ?? null) ?? (versions[versions.indexOf(shown) - 1] ?? null) : null;

  const move = (event: React.KeyboardEvent, index: number) => {
    const next =
      event.key === "ArrowLeft" ? index - 1 : event.key === "ArrowRight" ? index + 1 : event.key === "Home" ? 0 : event.key === "End" ? dots.length - 1 : null;
    if (next === null) return;
    event.preventDefault();
    strip.current?.querySelectorAll<HTMLButtonElement>("button[data-dot]")[Math.max(0, Math.min(dots.length - 1, next))]?.focus();
  };

  return (
    <div
      ref={root}
      className="relative flex h-8 shrink-0 items-center gap-3 border-t border-app-line bg-app-panel px-3"
      data-testid="version-rail"
      onMouseLeave={() => {
        if (!root.current?.contains(document.activeElement)) setCard(null);
      }}
      onBlur={(event) => {
        if (!root.current?.contains(event.relatedTarget)) setCard(null);
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape" && card) {
          event.stopPropagation();
          setCard(null);
        }
      }}
    >
      <div
        ref={strip}
        role="toolbar"
        aria-label={S.rail.label}
        className="flex h-full max-w-[55%] shrink-0 items-center overflow-x-auto [scrollbar-width:none]"
      >
        {dots.map((dot, index) => {
          const { version } = dot;
          const number = version.number;
          const result = results[number];
          const failing = result ? proofOf(result).problems > 0 : false;
          const small = version.origin === "set_parameter";
          const here = stageDot?.version.number === number;
          const newer = number > current;
          const name = [
            dotName(version, result),
            dot.run > 1 ? S.rail.collapsedName(dot.run) : null,
            newer ? S.rail.newer : null,
            here ? S.rail.onStage : null,
          ]
            .filter(Boolean)
            .join(", ");
          return (
            <button
              key={number}
              type="button"
              data-dot={number}
              data-testid={`dot-${number}`}
              data-current={number === current || undefined}
              data-on-stage={here || undefined}
              aria-label={name}
              aria-current={here ? "true" : undefined}
              tabIndex={(tabStop === number || (focusOn === null && here)) ? 0 : -1}
              // The ring is drawn inside the button: the strip scrolls and would cut one outside it.
              className={cn("app-ring-inset group flex h-8 shrink-0 items-center justify-center gap-0.5 rounded-tag px-1.5", newer && "opacity-40")}
              onMouseEnter={(event) => {
                open(number, event.currentTarget);
              }}
              onFocus={(event) => {
                setFocusOn(number);
                open(number, event.currentTarget);
              }}
              onKeyDown={(event) => {
                move(event, index);
              }}
              onClick={() => {
                void lookAt(number === current ? null : number);
              }}
            >
              <span className={cn("grid size-4 place-items-center rounded-pill", here && "ring-2 ring-app-accent ring-inset")}>
                <span
                  className={cn(
                    "rounded-pill",
                    small ? "size-1.5" : "size-2",
                    failing ? "bg-app-fail" : small ? "bg-app-text-3" : "bg-app-text-2",
                  )}
                />
              </span>
              {dot.run > 1 ? (
                <span className="t-caption num text-app-text-3" title={S.rail.collapsedHint(dot.run)} data-testid="dot-collapsed">
                  {S.rail.collapsed(dot.run)}
                </span>
              ) : null}
            </button>
          );
        })}
      </div>
      {printJob && current !== null && printJob.version !== current ? (
        <p className="t-caption num min-w-0 flex-1 truncate text-app-text-2" data-testid="rail-label" data-job-version={printJob.version}>
          {ACTIVE_JOB.has(printJob.status)
            ? S.print.jobVersion.printing(printJob.version, current)
            : S.print.jobVersion.printed(printJob.version, current)}
        </p>
      ) : stageVersion ? (
        <p className="t-caption num min-w-0 flex-1 truncate text-app-text-2" data-testid="rail-label">
          {versionLabel(stageVersion, versionOf(detail, stageVersion.parent), results[stageVersion.number])}
          {" · "}
          {agoInline(stageVersion.created_at_ms, now)}
        </p>
      ) : null}
      <SnapshotsButton projectId={projectId} />

      {card && shown ? (
        <div className="absolute bottom-full z-30 max-w-[calc(100%-16px)] pb-2" style={{ left: card.left, width: CARD_WIDTH }}>
          <div className="app-popup p-3">
            <Card
              version={shown}
              before={shownBefore}
              result={results[shown.number]}
              beforeResult={shownBefore ? results[shownBefore.number] : undefined}
              current={current}
              onStage={onStage}
              now={now}
              onLook={(byKey) => {
                focusStage.current = byKey;
                void lookAt(shown.number);
                setCard(null);
              }}
              onUse={() => {
                void makeCurrent(shown.number);
                setCard(null);
              }}
            />
          </div>
        </div>
      ) : null}
    </div>
  );
}
