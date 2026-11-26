// Inspector of the Prepare stage (DESIGN_SPEC 4.4): the plate, the material, the slice.
// There is no Slice button here: the verdict bar carries it.
import { FlaskConical, TriangleAlert } from "lucide-react";
import { useState } from "react";
import { Link } from "react-router";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { NumberField } from "@/components/ui/number-field";
import { Popover, PopoverContent, PopoverTitle, PopoverTrigger } from "@/components/ui/popover";
import { RadioGroup, RadioRow } from "@/components/ui/radio-group";
import { Select } from "@/components/ui/select";
import { Tooltip } from "@/components/ui/tooltip";
import { useHost } from "@/host/core-context";
import { H } from "@/host/strings";
import type { CheckResult, Orientation, Value } from "@/lib/core";
import { cn } from "@/lib/utils";

import { ApiErrorCard, SectionLabel, StatusIcon } from "../bits";
import { useHotend } from "../context";
import { area, duration, partWords, plain, sentence, weight } from "../format";
import { proofOf } from "../proof";
import { currentSlice, slicedBeforePreviews, slicedPlate } from "../store";
import { S } from "../strings";
import { readable } from "../proof";
import { useStudioUi } from "../ui";
import { sliceFailure } from "../verdict";

export type Way = { key: string; turned: string; overhang: number; contact: number; height: number };

const AXES: [string, readonly [number, number, number]][] = [
  ["+x", [1, 0, 0]],
  ["-x", [-1, 0, 0]],
  ["+y", [0, 1, 0]],
  ["-y", [0, -1, 0]],
  ["+z", [0, 0, 1]],
  ["-z", [0, 0, -1]],
];

function axisOf(up: unknown): string | null {
  if (!Array.isArray(up) || up.length !== 3) return null;
  const found = AXES.find(([, axis]) => axis.every((value, index) => Math.abs(value - Number(up[index])) < 1e-6));
  return found ? found[0] : null;
}

/** The ways a part can lie that point one of the six axis directions up, best first. */
export function waysOf(orientation: Pick<Orientation, "candidates">): Way[] {
  const seen = new Set<string>();
  return orientation.candidates.flatMap((candidate: Value) => {
    if (candidate === null || typeof candidate !== "object" || Array.isArray(candidate)) return [];
    const key = axisOf(candidate.up);
    if (key === null || seen.has(key)) return [];
    seen.add(key);
    const number = (value: Value | undefined) => (typeof value === "number" ? value : 0);
    return [
      {
        key,
        turned: typeof candidate.turned === "string" ? candidate.turned : key,
        overhang: number(candidate.overhang),
        contact: number(candidate.contact),
        height: number(candidate.height),
      },
    ];
  });
}

/** How much of a part hangs in the air in one way of lying, in a word a maker can weigh. */
function supportWord(overhang: number): string {
  if (overhang < 0.5) return S.prepare.support.none;
  if (overhang < 100) return S.prepare.support.little;
  return overhang < 1500 ? S.prepare.support.some : S.prepare.support.much;
}

/** "As modelled", "Upside down", "On its left side": a way of lying, by what points up. */
function wayName(way: Pick<Way, "key" | "turned">): string {
  return S.prepare.ways[way.key] ?? S.prepare.turned(way.turned);
}

function Turn({ orientation, busy }: { orientation: Orientation; busy: boolean }) {
  const layOut = useHotend((state) => state.layOut);
  const [open, setOpen] = useState(false);
  const ways = waysOf(orientation);
  const now = axisOf(orientation.up);
  if (ways.length < 2) return null;
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger
        render={
          <Button variant="ghost" size="sm" className="text-app-accent" disabled={busy} data-testid={`turn-${orientation.part}`}>
            {S.prepare.turn}
          </Button>
        }
      />
      <PopoverContent side="left" align="start" className="w-[360px] space-y-2" data-testid="turn-popover">
        <PopoverTitle className="t-ui-strong">{S.prepare.turnTitle(partWords(orientation.part))}</PopoverTitle>
        <RadioGroup
          aria-label={S.prepare.turnTitle(partWords(orientation.part))}
          value={now ?? ""}
          onValueChange={(value) => {
            if (typeof value !== "string" || value === now) return;
            setOpen(false);
            void layOut({ orientations: { [orientation.part]: value } });
          }}
        >
          {ways.map((way, index) => (
            <RadioRow key={way.key} value={way.key}>
              <span className="flex flex-wrap items-center gap-x-2">
                <span className="t-ui-strong">{wayName(way)}</span>
                {/* The list is best first: the first way is the one Hotend would take. */}
                {index === 0 ? <Badge tone="ok">{S.prepare.recommended}</Badge> : null}
              </span>
              <span className="block text-app-text-2">
                {supportWord(way.overhang)} · <span className="num whitespace-nowrap">{S.prepare.tall(`${plain(way.height, 1)} mm`)}</span>
              </span>
              <span className="t-caption num block text-app-text-3">
                {way.overhang >= 0.5 ? (
                  <>
                    <span className="whitespace-nowrap">{S.prepare.supportUnder(area(way.overhang))}</span>
                    {" · "}
                  </>
                ) : null}
                <span className="whitespace-nowrap">{S.prepare.onBed(area(way.contact))}</span>
              </span>
            </RadioRow>
          ))}
        </RadioGroup>
      </PopoverContent>
    </Popover>
  );
}

/** The area a check says hangs in the air, in mm²; 0 for any other check. */
function hangingArea(check: CheckResult): number {
  const data = check.data;
  if (check.id !== "overhangs" || check.status === "pass" || data === null || typeof data !== "object" || Array.isArray(data)) return 0;
  return typeof data.overhang_area === "number" ? data.overhang_area : 0;
}

/** What a check of the layout says, in the window's words where it has them. */
function checkWords(check: CheckResult): string {
  const data = check.data;
  const hanging = hangingArea(check);
  if (hanging > 0 && data !== null && typeof data === "object" && !Array.isArray(data) && Array.isArray(data.faces) && data.faces.length > 0) {
    return S.prepare.hangs(data.faces.length, area(hanging));
  }
  return sentence(readable(check.message));
}

function WarnLine({ children }: { children: string }) {
  return (
    <p className="t-ui flex gap-2 py-0.5 text-app-text-2">
      <StatusIcon kind="note" className="mt-px" />
      <span className="min-w-0 flex-1">{children}</span>
    </p>
  );
}

function PlateSection() {
  const preparation = useHotend((state) => state.preparation);
  const preparing = useHotend((state) => state.preparing);
  const plate = useHotend((state) => state.plate);
  const setPlate = useHotend((state) => state.setPlate);
  const layOut = useHotend((state) => state.layOut);
  const setPiece = useStudioUi((state) => state.setPiece);
  if (!preparation) return null;
  const plates = Math.max(1, preparation.plates.length);
  const warnings = preparation.checks.filter((check) => check.status === "warn" || check.status === "fail");
  const named = new Set(preparation.orientations.map((orientation) => orientation.part));
  const loose = warnings.filter((check) => check.part === null || !named.has(check.part));
  return (
    <section aria-labelledby="plate-title" data-testid="plate-section">
      <SectionLabel
        id="plate-title"
        aside={
          plates > 1 ? (
            <span className="flex items-center gap-0.5">
              {preparation.plates.map((entry) => (
                <button
                  key={entry.index}
                  type="button"
                  aria-label={S.prepare.plateButton(entry.index)}
                  aria-pressed={entry.index === plate}
                  onClick={() => {
                    setPlate(entry.index);
                  }}
                  className={cn(
                    "t-caption num grid size-7 place-items-center rounded-control text-app-text-2 hover:bg-app-hover",
                    entry.index === plate && "bg-app-active text-app-text",
                  )}
                >
                  {entry.index}
                </button>
              ))}
            </span>
          ) : null
        }
      >
        {S.prepare.plate(Math.min(plate, plates), plates)}
      </SectionLabel>
      <ul className="space-y-3 pt-1">
        {preparation.orientations.map((orientation) => {
          const unplaced = preparation.unplaced.filter((piece) => piece.part === orientation.part);
          return (
            <li
              key={orientation.part}
              data-testid={`piece-${orientation.part}`}
              // The row in use outlines its piece on the plate.
              onPointerEnter={() => {
                setPiece(orientation.part);
              }}
              onPointerLeave={() => {
                setPiece(null);
              }}
              onFocus={() => {
                setPiece(orientation.part);
              }}
              onBlur={(event) => {
                if (!event.currentTarget.contains(event.relatedTarget)) setPiece(null);
              }}
            >
              {/* One shape for every piece: the name on its own line; under it how the piece lies, then Turn and Copies at the right. */}
              <Tooltip label={sentence(orientation.explanation)} side="left">
                <span className="t-ui-strong block min-w-0 truncate" tabIndex={0} title={partWords(orientation.part)} data-testid="piece-name">
                  {partWords(orientation.part)}
                </span>
              </Tooltip>
              {/* With large text the controls go under how the piece lies, and the stepper under Turn: nothing leaves the column. */}
              <div className="flex min-h-8 flex-wrap items-center gap-x-2 gap-y-1">
                <p className="t-caption min-w-[8ch] flex-1 truncate text-app-text-2" data-testid={`lies-${orientation.part}`}>
                  {S.prepare.ways[axisOf(orientation.up) ?? ""] ?? S.prepare.turned(orientation.turned)}
                </p>
                <div className="ml-auto flex max-w-full flex-wrap items-center justify-end gap-x-2 gap-y-1" data-testid="piece-controls">
                  <Turn orientation={orientation} busy={preparing} />
                  <span className="t-caption text-app-text-2" aria-hidden>
                    {S.prepare.copiesLabel}
                  </span>
                  <NumberField
                    label={S.prepare.copies(partWords(orientation.part))}
                    stepper
                    min={1}
                    max={64}
                    step={1}
                    disabled={preparing}
                    value={orientation.copies}
                    onValueCommitted={(value) => {
                      if (value !== null && value !== orientation.copies) void layOut({ copies: { [orientation.part]: value } });
                    }}
                  />
                </div>
              </div>
              {unplaced.length > 0 ? (
                <div className="flex gap-2 py-0.5" role="alert">
                  <StatusIcon kind="problem" className="mt-px" />
                  <p className="t-ui min-w-0 flex-1">
                    <span className="t-ui-strong">{S.prepare.doesNotFit}.</span> {sentence(unplaced[0]?.reason ?? "")}
                  </p>
                </div>
              ) : null}
              {warnings
                .filter((check) => check.part === orientation.part)
                .map((check) => (
                  <WarnLine key={check.id}>{checkWords(check)}</WarnLine>
                ))}
            </li>
          );
        })}
      </ul>
      {loose.map((check) => (
        <WarnLine key={`${check.id}:${check.part ?? ""}`}>{checkWords(check)}</WarnLine>
      ))}
      {preparation.warnings.map((warning) => (
        <WarnLine key={warning}>{sentence(readable(warning))}</WarnLine>
      ))}
    </section>
  );
}

function SliceSection() {
  const state = useHotend((s) => s);
  const { slice, slicing, quality, setQuality } = state;
  const version = state.project?.project.current_version ?? null;
  const shown = slice && slice.record.version === version ? slice : null;
  const stale = shown !== null && currentSlice(state) === null;
  const previews = useHost((host) => host.settings?.previews ?? null);
  // Made before Hotend-made previews were switched on: Bambu Connect cannot import it.
  const outdated = !stale && slicedBeforePreviews(shown, previews);
  const failure = sliceFailure(state);
  const noSlicer = state.sliceError?.kind === "not_configured" || failure?.code === "slicer_not_installed";
  const summary = shown?.summary;
  const codes = new Set(summary?.warnings.map((warning) => warning.code));
  const grams = summary?.filament.reduce((sum, use) => sum + use.grams, 0) ?? 0;
  const quiet = new Set(["time_unknown", "layers_unknown", "filament_use_unknown"]);
  const plates = state.preparation?.plates.length ?? 1;
  // What the layout says hangs in the air, over every piece as it lies now.
  const hanging = (state.preparation?.checks ?? []).reduce((sum, check) => sum + hangingArea(check), 0);
  const standIn = codes.has("fake_slicer");

  return (
    <section aria-labelledby="slice-title" data-testid="slice-section" data-stand-in={codes.has("fake_slicer") || undefined}>
      <SectionLabel id="slice-title">{S.prepare.slice}</SectionLabel>
      <div className="flex min-h-9 items-center justify-between gap-3">
        <span className="t-ui text-app-text-2">{S.prepare.quality}</span>
        <Select
          label={S.prepare.quality}
          value={quality}
          onChange={setQuality}
          options={S.prepare.qualities}
          disabled={slicing}
          className="w-[176px]"
        />
      </div>
      {state.preparation ? (
        <div className="py-1" data-testid="supports-row">
          <div className="flex min-h-7 items-baseline justify-between gap-3">
            <span className="t-ui text-app-text-2">{S.prepare.supports.label}</span>
            <span className="t-ui num min-w-0 text-right">{hanging < 0.5 ? S.prepare.supports.none : S.prepare.supports.needed(area(hanging))}</span>
          </div>
          {hanging >= 0.5 && summary ? (
            <p className={cn("t-caption", standIn ? "text-app-sim" : "text-app-text-2")} data-testid="supports-note">
              {standIn ? S.prepare.supports.standIn : S.prepare.supports.real}
            </p>
          ) : null}
        </div>
      ) : null}
      <div className="min-h-[104px] pt-2" data-testid="slice-summary" data-stale={stale || undefined}>
        {noSlicer ? (
          <p className="t-ui text-app-text-2">{S.verdict.noSlicer}</p>
        ) : failure ? (
          <div className="flex gap-2" role="alert">
            <StatusIcon kind="problem" className="mt-px" />
            <div className="min-w-0 flex-1 space-y-1">
              <p className="t-ui">{failure.sentence}</p>
              {/* What the slicer itself said belongs here, and only here. */}
              {failure.said !== failure.sentence ? (
                <p className="t-ui break-words text-app-text-2" data-testid="slice-said">
                  {sentence(failure.said)}
                </p>
              ) : null}
              {state.sliceError?.next_step ? <p className="t-ui text-app-text-2">{state.sliceError.next_step}</p> : null}
              {failure.code ? <p className="t-mono text-app-text-2">{failure.code}</p> : null}
            </div>
          </div>
        ) : summary ? (
          <>
            {stale ? <p className="t-ui pb-1 text-app-text-2">{S.prepare.changed}</p> : null}
            {outdated ? (
              <p className="t-ui pb-1 text-app-text-2" data-testid="slice-previews-outdated">
                {S.prepare.previewsOutdated}
              </p>
            ) : null}
            {plates > 1 ? (
              <p className="t-ui pb-1 text-app-text-2" data-testid="slice-plate">
                {S.prepare.slicedPlate(slicedPlate(state.slice) ?? state.plate, plates)}
              </p>
            ) : null}
            <div className={cn("space-y-1", (stale || slicing) && "opacity-60")}>
              <p className="t-numeric-lg num" data-testid="slice-time">
                {codes.has("time_unknown") ? <span className="t-ui text-app-text-2">{S.prepare.notStated}</span> : duration(summary.time_s)}
              </p>
              <p className="t-ui num">
                {codes.has("filament_use_unknown") ? S.prepare.notStated : weight(grams)}
                {" · "}
                {codes.has("layers_unknown") ? S.prepare.notStated : S.prepare.layers(summary.layers)}
              </p>
              <p className="t-caption text-app-text-2">
                {codes.has("fake_slicer") ? S.prepare.standInName : summary.slicer.name}
                {summary.slicer.version ? ` ${summary.slicer.version}` : ""}
              </p>
              <div className="flex flex-col items-start gap-1 pt-1">
                {summary.warnings
                  .filter((warning) => !quiet.has(warning.code))
                  .map((warning) =>
                    warning.code === "fake_slicer" ? (
                      // A tag is one short line; what it means is the sentence under it.
                      <div key={warning.code} className="space-y-1" title={warning.message}>
                        <Badge tone="sim">
                          <FlaskConical strokeWidth={1.5} />
                          <span>{S.prepare.standInTag}</span>
                        </Badge>
                        <p className="t-caption text-app-sim">{S.prepare.tags.fake_slicer}</p>
                        <Link
                          to="/settings/slicer"
                          className="t-ui inline-flex min-h-7 items-center rounded-tag text-app-accent underline-offset-2 hover:underline"
                          data-testid="use-studio"
                        >
                          {S.print.useStudio}
                        </Link>
                      </div>
                    ) : (
                      <Badge key={warning.code} tone="warn" title={warning.message} className="items-start">
                        <TriangleAlert strokeWidth={1.5} />
                        <span>{S.prepare.tags[warning.code] ?? sentence(warning.message)}</span>
                      </Badge>
                    ),
                  )}
                {codes.has("demo_profile") || codes.has("unverified_printer_facts") ? (
                  <p className="t-caption text-app-text-2" data-testid="tags-note">
                    {S.prepare.tagsNote}
                  </p>
                ) : null}
              </div>
            </div>
          </>
        ) : (
          <p className="t-ui text-app-text-2">{S.prepare.notSliced}</p>
        )}
      </div>
    </section>
  );
}

export function PreparePanel() {
  const current = useHotend((state) => state.project?.project.current_version ?? null);
  const result = useHotend((state) => (current === null ? undefined : state.results[current]));
  const preparation = useHotend((state) => state.preparation);
  const preparing = useHotend((state) => state.preparing);
  const prepareError = useHotend((state) => state.prepareError);
  const material = useHost((state) => state.settings?.material ?? null);
  const pieces = useHotend((state) => state.preparation?.plates.reduce((sum, plate) => sum + plate.items.length, 0) ?? 0);

  if (current === null) return <p className="t-ui pt-1 text-app-text-2">{S.prepare.nothing}</p>;
  if (proofOf(result).blocking > 0) return <p className="t-ui pt-1 text-app-text-2">{S.prepare.opens}</p>;
  const label = H.materials.find((entry) => entry.id === material?.id)?.label ?? material?.id ?? "";

  return (
    <div className="space-y-4" data-testid="prepare-panel" data-pieces={pieces}>
      {prepareError ? <ApiErrorCard error={prepareError} /> : null}
      {preparation ? <PlateSection /> : preparing ? <p className="t-ui text-app-text-2">{S.prepare.layingOut}</p> : null}
      <hr className="border-app-line" />
      <section aria-labelledby="material-title">
        <SectionLabel id="material-title">{S.prepare.material}</SectionLabel>
        <p className="t-ui py-1">
          {label} · {S.prepare.mainNozzle}
        </p>
        <Link to="/settings/printer" className="t-ui inline-flex min-h-7 items-center rounded-tag text-app-accent underline-offset-2 hover:underline">
          {S.prepare.changeInSettings}
        </Link>
      </section>
      <hr className="border-app-line" />
      <SliceSection />
    </div>
  );
}

