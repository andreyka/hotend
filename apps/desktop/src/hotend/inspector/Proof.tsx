// PROOF (DESIGN_SPEC 5.2): one line per kind of check, in three groups, failing lines
// first. Only the first problem is open. An open line says what it found in one sentence
// of the window's; the engine's own sentences are behind "Details".
import { ChevronDown, ChevronRight } from "lucide-react";
import { useNavigate } from "react-router";

import { Button } from "@/components/ui/button";
import { Tooltip } from "@/components/ui/tooltip";
import { useHost } from "@/host/core-context";
import { canTalk } from "@/host/store";
import { H } from "@/host/strings";
import type { EngineResult } from "@/lib/core";
import { cn } from "@/lib/utils";

import { Flash, SectionLabel, StatusIcon } from "../bits";
import { quantityWords } from "../calibration";
import { useHotend } from "../context";
import { partWords } from "../format";
import { proofOf, readable, type ProofCounts, type ProofGroup, type ProofLine } from "../proof";
import { versionOf } from "../store";
import { S } from "../strings";
import { useStudioUi } from "../ui";

/** "1 problem · 10 pass · 3 notes": the problems first and in the colour of a problem. */
export function Counts({ counts, className, short = false }: { counts: ProofCounts; className?: string; short?: boolean }) {
  const open = S.proof.rest(counts.measure, counts.note) || null;
  // A group has one line of room: what is open, and the passes only where nothing is.
  const rest = (short && (counts.problem > 0 || open !== null) ? [open] : [counts.pass > 0 ? S.proof.passNumber(counts.pass) : null, open]).filter(
    (part): part is string => part !== null,
  );
  return (
    <span className={className}>
      {counts.problem > 0 ? <span className="font-medium text-app-fail">{S.proof.problems(counts.problem)}</span> : null}
      {counts.problem > 0 && rest.length > 0 ? " · " : null}
      {rest.join(" · ")}
    </span>
  );
}

function Line({
  line,
  busy,
  specs,
  first,
}: {
  line: ProofLine;
  busy: boolean;
  specs: EngineResult["param_specs"];
  /** The first problem of the list: the one line that is open by itself. */
  first: boolean;
}) {
  const navigate = useNavigate();
  const override = useStudioUi((state) => state.openLines[line.key]);
  const toggleLine = useStudioUi((state) => state.toggleLine);
  const openSizeRow = useStudioUi((state) => state.openSizeRow);
  const scrollTo = useStudioUi((state) => state.scrollTo);
  const fillComposer = useStudioUi((state) => state.fillComposer);
  const solo = useStudioUi((state) => state.solo);
  const shown = useStudioUi((state) => state.spot?.line === line.key);
  const showOnly = useStudioUi((state) => state.showOnly);
  const sendMessage = useHotend((state) => state.sendMessage);
  const running = useHotend((state) => state.run?.status === "running");
  const reading = useHotend((state) => state.look !== null);
  const rebuilding = useHotend((state) => state.rebuild !== null);
  const rebuildCurrent = useHotend((state) => state.rebuildCurrent);
  // Whether this printer was measured, and whether the version on the stage was built since.
  const calibration = useHotend((state) => state.calibration ?? null);
  const measuredAt = calibration?.at ?? null;
  const builtAt = useHotend((state) => versionOf(state.project, state.look ?? state.project?.project.current_version ?? null)?.created_at_ms ?? null);
  const fits: "general" | "stale" | "measured" =
    !line.uncalibrated || measuredAt === null ? "general" : builtAt !== null && builtAt < measuredAt ? "stale" : "measured";
  const talk = useHost(canTalk);

  const detailsKey = `details:${line.key}`;
  const details = useStudioUi((state) => state.openLines[detailsKey] ?? false);
  const open = override ?? first;
  const failing = line.status === "fail" || line.unmet;
  const part = line.spot?.part ?? (line.parts.length === 1 ? (line.parts[0] ?? null) : null);
  const askLocked = running ? S.proof.whileRun : talk ? null : S.proof.noRun;
  const skipped = line.status === "skipped";

  return (
    <div
      data-testid="proof-line"
      data-key={line.key}
      data-status={line.status}
      data-standing={line.unmet ? "problem" : undefined}
      data-scroll={failing ? "problem" : line.icon === "note" ? "note" : undefined}
    >
      <button
        type="button"
        aria-expanded={open}
        onClick={() => {
          toggleLine(line.key, !open);
        }}
        className="flex min-h-7 w-full items-start gap-2 rounded-control py-[5px] text-left hover:bg-app-hover"
      >
        <StatusIcon
          kind={busy ? "busy" : line.icon}
          label={busy ? S.proof.status.busy : S.proof.status[line.icon]}
          className="mt-px"
        />
        <span className={cn("t-ui min-w-0 flex-1", skipped ? "text-app-text-3" : "text-app-text")}>
          {line.uncalibrated && fits === "measured" ? S.proof.calibratedTitle((calibration?.guessed.length ?? 0) > 0) : line.title}
        </span>
        {open ? (
          <ChevronDown className="mt-px size-4 shrink-0 text-app-text-3" strokeWidth={1.5} aria-hidden />
        ) : (
          <ChevronRight className="mt-px size-4 shrink-0 text-app-text-3" strokeWidth={1.5} aria-hidden />
        )}
      </button>
      {open ? (
        <div className="space-y-1.5 pr-1 pb-2 pl-6">
          {line.summary ? (
            <p className="t-ui text-app-text" data-testid="proof-summary">
              {line.summary}
            </p>
          ) : null}
          {line.fixed ? <p className={cn("t-ui", line.summary ? "text-app-text-2" : "text-app-text")}>{line.fixed}</p> : null}
          {line.guessed.length > 0 ? (
            <p className="flex flex-wrap gap-x-3 gap-y-1">
              {line.guessed.map((name) => (
                <button
                  key={name}
                  type="button"
                  className="t-ui inline-flex min-h-7 items-center rounded-tag text-app-accent underline-offset-2 hover:underline"
                  onClick={() => {
                    openSizeRow(name, true);
                    scrollTo("measure");
                  }}
                >
                  {specs?.[name]?.label ?? name}
                </button>
              ))}
            </p>
          ) : null}
          {line.uncalibrated ? (
            <p className="t-ui text-app-text-2" data-testid="fits-state" data-fits={fits}>
              {fits === "general"
                ? S.proof.uncalibrated
                : fits === "stale"
                  ? S.proof.calibratedSince
                  : // Only what the printer's table says was measured is called measured.
                    S.proof.calibratedPartly(
                      line.unsure ?? 1,
                      quantityWords(calibration?.measured ?? [], H.calibration.quantities),
                      calibration && calibration.guessed.length > 0 ? quantityWords(calibration.guessed, H.calibration.quantities) : null,
                    )}
            </p>
          ) : null}
          {failing || line.uncalibrated || (line.asksMeasurement && !reading) ? (
            <div className="flex flex-wrap gap-2 pt-1" data-testid="proof-actions">
              {failing && !reading ? (
                <Tooltip label={askLocked} wrap>
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={askLocked !== null}
                    onClick={() => {
                      const sentence = line.summary ?? line.entries[0]?.message ?? line.title;
                      void sendMessage(S.proof.fixMessage(sentence));
                    }}
                  >
                    {S.verdict.askFix}
                  </Button>
                </Tooltip>
              ) : null}
              {failing && part !== null ? (
                <Button
                  variant="ghost"
                  size="sm"
                  aria-pressed={shown && solo === part}
                  data-testid="proof-show-me"
                  onClick={() => {
                    if (shown && solo === part) showOnly(null);
                    else showOnly(part, { line: line.key, part, at: line.spot?.at ?? null, label: line.spot?.label ?? null });
                  }}
                >
                  {shown && solo === part ? S.verdict.showAll : S.verdict.showMe}
                </Button>
              ) : null}
              {line.uncalibrated && fits === "stale" && !reading ? (
                <Button variant="outline" size="sm" disabled={rebuilding || running} onClick={rebuildCurrent} data-testid="rebuild-measured">
                  {S.proof.rebuildWithMeasurements}
                </Button>
              ) : null}
              {line.uncalibrated && fits !== "stale" ? (
                <Button variant="outline" size="sm" onClick={() => void navigate("/settings/calibration")}>
                  {S.proof.openCalibration}
                </Button>
              ) : null}
              {line.asksMeasurement && !reading ? (
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => {
                    fillComposer(S.proof.tellMeasuredText);
                  }}
                >
                  {S.proof.tellMeasured}
                </Button>
              ) : null}
            </div>
          ) : null}
          {/* The engine's own sentences come last: the explanation and what to do are read first. */}
          {line.entries.length > 0 ? (
            <div>
              <button
                type="button"
                aria-expanded={details}
                data-testid="proof-details"
                onClick={() => {
                  toggleLine(detailsKey, !details);
                }}
                className="t-caption inline-flex min-h-7 items-center gap-0.5 rounded-tag pr-1 text-app-text-2 hover:text-app-text"
              >
                {details ? (
                  <ChevronDown className="size-3.5" strokeWidth={1.5} aria-hidden />
                ) : (
                  <ChevronRight className="size-3.5" strokeWidth={1.5} aria-hidden />
                )}
                {S.proof.details}
              </button>
              {details
                ? line.entries.map((entry, index) => (
                    <p key={index} className="t-caption break-words pb-1 text-app-text-3" data-testid="proof-entry">
                      {entry.part ? <span className="font-medium text-app-text-2">{partWords(entry.part)}: </span> : null}
                      {readable(entry.message)}
                    </p>
                  ))
                : null}
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

function Group({
  group,
  busy,
  specs,
  firstProblem,
}: {
  group: ProofGroup;
  busy: boolean;
  specs: EngineResult["param_specs"];
  firstProblem: string | null;
}) {
  const key = `group:${group.id}`;
  const override = useStudioUi((state) => state.openLines[key]);
  const toggleLine = useStudioUi((state) => state.toggleLine);
  const allPass = group.shown > 0 && group.passed === group.shown;
  const counts = group.counts;
  const open = override ?? !allPass;
  return (
    <div data-testid={`proof-group-${group.id}`}>
      <button
        type="button"
        aria-expanded={open}
        onClick={() => {
          toggleLine(key, !open);
        }}
        className="flex h-7 w-full items-center justify-between gap-2 rounded-control text-left hover:bg-app-hover"
      >
        <span className="t-caption shrink-0 whitespace-nowrap text-app-text-2">{group.title}</span>
        {busy ? (
          <span className="t-caption num text-app-text-3">{S.proof.busy}</span>
        ) : (
          <Counts counts={counts} short className="t-caption num min-w-0 truncate text-app-text-3" />
        )}
      </button>
      {open
        ? group.lines.map((line) => <Line key={line.key} line={line} busy={busy} specs={specs} first={line.key === firstProblem} />)
        : null}
    </div>
  );
}

export function Proof({ result }: { result: EngineResult | undefined }) {
  const busy = useHotend((state) => state.rebuild !== null);
  const reading = useHotend((state) => state.look !== null);
  const fillComposer = useStudioUi((state) => state.fillComposer);
  const proof = proofOf(result);
  return (
    <section aria-labelledby="proof-title" data-testid="proof">
      <SectionLabel
        id="proof-title"
        aside={
          <Flash
            of={busy ? "busy" : JSON.stringify(proof.counts)}
            className="t-caption num -ml-1 rounded-tag px-1 whitespace-nowrap text-app-text-2"
            data-testid="proof-count"
          >
            {busy ? S.proof.busy : proof.shown > 0 ? <Counts counts={proof.counts} /> : null}
          </Flash>
        }
      >
        {S.proof.title}
      </SectionLabel>
      {proof.groups.map((group) =>
        group.id === "must" && !proof.hasContract ? (
          <div key={group.id} className="pb-2" data-testid="no-contract">
            <p className="t-caption flex h-7 items-center text-app-text-2">{group.title}</p>
            <p className="t-ui flex items-start gap-2 py-[5px] text-app-text-2">
              <StatusIcon kind="skipped" label={S.proof.status.skipped} className="mt-px" />
              <span className="min-w-0 flex-1">{S.proof.noContract}</span>
            </p>
            {!reading ? (
              <div className="pl-6">
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => {
                    fillComposer(S.proof.askContractText);
                  }}
                >
                  {S.proof.askContract}
                </Button>
              </div>
            ) : null}
          </div>
        ) : (
          <Group key={group.id} group={group} busy={busy} specs={result?.param_specs} firstProblem={proof.firstProblem?.key ?? null} />
        ),
      )}
    </section>
  );
}
