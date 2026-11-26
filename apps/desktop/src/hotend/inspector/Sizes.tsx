// SIZES (DESIGN_SPEC 5.3, 5.4, 6.1): one row per parameter of the model, grouped by
// where its number comes from. A guess is a question with a field; a choice is a slider
// that rebuilds; a fixed number says who fixed it.
import { BookMarked, ChevronDown, ChevronRight, Gauge, Ruler } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { NumberField } from "@/components/ui/number-field";
import { Slider } from "@/components/ui/slider";
import { Spinner } from "@/components/ui/spinner";
import { Tooltip } from "@/components/ui/tooltip";
import { useHost } from "@/host/core-context";
import { canTalk } from "@/host/store";
import type { EngineResult } from "@/lib/core";
import { cn } from "@/lib/utils";

import { ApiErrorCard, Flash, SectionLabel, StatusIcon } from "../bits";
import { useHotend } from "../context";
import { partWords, plain, sized, unitLabel } from "../format";
import { outOfRange, percentOf, sizesOf, snapped, spanOf, stepOf, type Size, type SizeSpan } from "../sizes";
import { S } from "../strings";
import { useStudioUi } from "../ui";

type RowProps = {
  size: Size;
  /** Why the row cannot be changed now, or null. */
  locked: string | null;
};

const FORMAT: Intl.NumberFormatOptions = { maximumFractionDigits: 2, useGrouping: false };
/** How many of "Your choices" are shown before "Show all 12 sizes". */
const CHOICES_SHOWN = 6;

/** A slider with a number field: a choice of the maker's that rebuilds the part. */
export function ChoiceRow({ size, locked }: RowProps) {
  const setSize = useHotend((state) => state.setSize);
  const rebuild = useHotend((state) => state.rebuild);
  const queued = useHotend((state) => state.queued[size.name]);
  const refused = useHotend((state) => state.refused);
  const rebuildError = useHotend((state) => state.rebuildError);
  const clearRefused = useHotend((state) => state.clearRefused);
  const navigate = useNavigate();
  const [more, setMore] = useState(false);
  const [draft, setDraft] = useState<number | null>(null);
  // A typed value outside the range: it stays in the field, said as what it is, and
  // nothing is rebuilt until it is inside the range.
  const [beyond, setBeyond] = useState<number | null>(null);
  const dragging = useRef(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const pending = queued ?? (rebuild?.name === size.name ? rebuild.value : undefined);
  const shown = beyond ?? draft ?? pending ?? size.value;
  const busy = pending !== undefined;
  const hasRange = size.min !== null && size.max !== null;
  const step = stepOf(size);
  const isRefused = refused?.name === size.name;
  const failed = rebuildError?.name === size.name ? rebuildError : null;
  const onSlider = Math.min(size.max ?? shown, Math.max(size.min ?? shown, shown));

  useEffect(
    () => () => {
      if (timer.current) clearTimeout(timer.current);
    },
    [],
  );

  // The next edit of the row answers its last refusal: the message goes, so that two
  // never stack and an old one is never read as the answer to a new value.
  const edited = () => {
    if (isRefused || failed) clearRefused();
    setMore(false);
  };

  const commit = (value: number) => {
    if (timer.current) clearTimeout(timer.current);
    timer.current = null;
    if (outOfRange(value, size)) {
      setBeyond(value);
      setDraft(null);
      return;
    }
    setBeyond(null);
    const next = snapped(value, size);
    setDraft(null);
    if (next !== (pending ?? size.value)) setSize(size.name, next);
  };

  return (
    <div className="py-1.5" data-testid={`size-${size.name}`} data-busy={busy || undefined} data-beyond={beyond !== null || undefined}>
      {/* Where the text is set large the field goes under the name, at the right, and stays in the column. */}
      <div className="flex min-h-7 flex-wrap items-center justify-between gap-x-2 gap-y-1">
        <span className="t-ui min-w-[8ch] flex-1 truncate text-app-text" title={size.label}>
          {size.label}
        </span>
        <span className="ml-auto flex shrink-0 items-center gap-1.5">
          <NumberField
            label={size.label}
            value={shown}
            // The range is not given to the field: it would change a typed 200 into 80
            // without a word. The row says the range instead.
            step={step}
            largeStep={step * 10}
            format={FORMAT}
            disabled={locked !== null}
            stepper={size.unit === "count"}
            invalid={beyond !== null}
            onValueChange={(value) => {
              if (value === null) return;
              edited();
              setBeyond(null);
              setDraft(value);
            }}
            onValueCommitted={(value) => {
              if (value !== null) commit(value);
            }}
          />
          {unitLabel(size.unit) ? <span className="t-ui-strong w-6 text-app-text-2">{unitLabel(size.unit)}</span> : null}
        </span>
      </div>
      {hasRange && size.unit !== "count" ? (
        <Tooltip label={locked} wrap="block">
          <div
            className="w-full"
            onPointerDown={() => {
              dragging.current = true;
            }}
          >
            <Slider
              label={size.label}
              unit={unitLabel(size.unit) === "°" ? "degrees" : unitLabel(size.unit) || undefined}
              value={onSlider}
              min={size.min ?? 0}
              max={size.max ?? 0}
              step={step}
              largeStep={step * 10}
              disabled={locked !== null}
              onValueChange={(value) => {
                edited();
                setBeyond(null);
                setDraft(value);
              }}
              onValueCommitted={(value) => {
                if (dragging.current) {
                  // On release: one rebuild.
                  dragging.current = false;
                  commit(value);
                  return;
                }
                // Arrow keys: 400 ms after the last one.
                if (timer.current) clearTimeout(timer.current);
                timer.current = setTimeout(() => {
                  commit(value);
                }, 400);
              }}
            >
              {/* The hollow tick marks the value of the version on the stage. */}
              {/* It is drawn only where the thumb is not: under the thumb it reads as a stray ring. */}
              {Math.abs(percentOf(onSlider, size.min ?? 0, size.max ?? 0) - percentOf(size.value, size.min ?? 0, size.max ?? 0)) > 4 ? (
                <span
                  aria-hidden
                  data-testid={`tick-${size.name}`}
                  className="pointer-events-none absolute top-1/2 size-2 -translate-x-1/2 -translate-y-1/2 rounded-pill border border-app-text-2 bg-app-panel"
                  style={{ left: `${percentOf(size.value, size.min ?? 0, size.max ?? 0)}%` }}
                />
              ) : null}
            </Slider>
          </div>
        </Tooltip>
      ) : null}
      <div className="t-caption flex min-h-4 items-center gap-1.5 text-app-text-3">
        {busy ? (
          <Spinner className="size-3.5" />
        ) : hasRange ? (
          <span className="num">{S.sizes.range(plain(size.min ?? 0), plain(size.max ?? 0))}</span>
        ) : null}
      </div>
      {beyond !== null && hasRange ? (
        <p className="t-caption mt-1 text-app-fail" role="alert" data-testid="size-beyond">
          {S.sizes.outOfRange(size.label, plain(size.min ?? 0), plain(size.max ?? 0), unitLabel(size.unit))}
        </p>
      ) : null}
      {/* One message per row: a value outside the range, else a build that could not run, else a refusal. */}
      {beyond !== null && hasRange ? null : failed ? (
        // The size is not at fault: the computer or the core is, and the card says what to do.
        <div className="mt-1 space-y-1" data-testid="size-rebuild-error">
          <p className="t-caption text-app-fail" role="alert">
            {S.sizes.rebuildFailed(failed.label, sized(failed.value, failed.unit), sized(failed.still, failed.unit))}
          </p>
          <ApiErrorCard
            error={failed.error}
            action={
              "code" in failed.error && failed.error.code === "sandbox_unavailable" ? (
                <Button variant="outline" size="sm" onClick={() => void navigate("/settings/computer")}>
                  {S.sizes.openComputer}
                </Button>
              ) : undefined
            }
          />
        </div>
      ) : isRefused && refused ? (
        <div className="mt-1" role="alert">
          {/* What the model answered can be a paragraph: three lines, and the rest on request. */}
          <p className={cn("t-caption break-words text-app-fail", !more && "line-clamp-3")} data-testid="size-refused">
            {S.sizes.refused(
              refused.label,
              sized(refused.value, refused.unit),
              refused.reason.replace(/\s*\.?$/, "."),
              sized(refused.still, refused.unit),
            )}
          </p>
          {refused.reason.length > 120 ? (
            <button
              type="button"
              aria-expanded={more}
              data-testid="size-refused-details"
              onClick={() => {
                setMore(!more);
              }}
              className="t-caption inline-flex min-h-7 items-center rounded-tag text-app-text-2 hover:text-app-text"
            >
              {more ? S.sizes.refusedLess : S.sizes.refusedDetails}
            </button>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

/** A guessed number: a question the maker answers with a measurement. */
function MeasureRow({ size, locked, open, span }: RowProps & { open: boolean; span: SizeSpan | null }) {
  const openRow = useStudioUi((state) => state.openSizeRow);
  const setDimension = useStudioUi((state) => state.setDimension);
  const focusTick = useStudioUi((state) => state.focusSize);
  const sendMessage = useHotend((state) => state.sendMessage);
  const notApplied = useHotend((state) => (state.notApplied?.name === size.name ? state.notApplied : null));
  const clearNotApplied = useHotend((state) => state.clearNotApplied);
  const [value, setValue] = useState<number | null>(size.value);
  const row = useRef<HTMLDivElement>(null);
  const wrong = value !== null && value <= 0;
  const focused = useRef(focusTick);

  useEffect(() => {
    if (!open || focusTick === focused.current) return;
    focused.current = focusTick;
    const input = row.current?.querySelector("input");
    input?.focus();
    input?.select();
  }, [open, focusTick]);

  const send = (measured: number) => {
    if (locked !== null || measured <= 0) return;
    void sendMessage(S.sizes.measuredMessage(size.label, sized(measured, size.unit)), { name: size.name, value: measured });
  };
  const use = () => {
    if (value !== null) send(value);
  };
  // While the row is in use (the pointer over it, or the keyboard in it) the size is
  // drawn on the part: the sentence names a side, the line shows which.
  const point = (on: boolean) => {
    if (on && span) setDimension({ name: size.name, ...span, label: sized(size.value, size.unit) });
    else if (!on && useStudioUi.getState().dimension?.name === size.name) setDimension(null);
  };
  useEffect(
    () => () => {
      if (useStudioUi.getState().dimension?.name === size.name) useStudioUi.getState().setDimension(null);
    },
    [size.name],
  );
  const side = size.ask && size.ask.axis !== null ? S.sizes.side(size.ask.axis, size.ask.side, sized(size.value, size.unit)) : null;
  const part = size.ask?.part ? partWords(size.ask.part) : null;

  if (!open) {
    return (
      <button
        type="button"
        data-testid={`measure-${size.name}`}
        aria-expanded={false}
        onClick={() => {
          openRow(size.name, true);
        }}
        className="flex h-8 w-full items-center gap-2 rounded-control text-left hover:bg-app-hover"
      >
        <StatusIcon kind="question" />
        <span className="t-ui min-w-0 flex-1 truncate" title={size.label}>
          {size.label}
        </span>
        <Flash of={size.value} className="t-ui-strong num rounded-tag px-1 text-app-text">
          {sized(size.value, size.unit)}
        </Flash>
      </button>
    );
  }
  return (
    <div
      ref={row}
      className="py-1"
      data-testid={`measure-${size.name}`}
      data-open
      onPointerEnter={() => {
        point(true);
      }}
      onPointerLeave={() => {
        if (!row.current?.contains(document.activeElement)) point(false);
      }}
      onFocus={() => {
        point(true);
      }}
      onBlur={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget)) point(false);
      }}
    >
      {/* Where the text is set large the tag goes under the name, which keeps a width it can be read in. */}
      <div className="flex min-h-8 flex-wrap items-center gap-x-2 gap-y-0.5">
        <StatusIcon kind="question" />
        {/* The open row is the question being asked: its name may run to two lines. */}
        <span className="t-ui-strong line-clamp-2 min-w-[8ch] flex-1" title={size.label}>
          {size.label}
        </span>
        <Badge tone="warn">{S.sizes.guess}</Badge>
      </div>
      <div className="flex flex-wrap items-center gap-2 pt-1 pl-6">
        <NumberField
          label={size.label}
          value={value}
          step={0.1}
          largeStep={1}
          format={FORMAT}
          onValueChange={setValue}
          inputClassName="w-16"
          invalid={wrong}
        />
        <span className="t-ui-strong text-app-text-2">{unitLabel(size.unit)}</span>
        <Tooltip label={locked} wrap>
          <Button
            variant="outline"
            size="sm"
            // Its words may take two lines where the column is narrow for the text size.
            className="h-auto min-h-7 max-w-full whitespace-normal"
            disabled={locked !== null || value === null || wrong}
            onClick={use}
            data-testid={`use-measurement-${size.name}`}
          >
            {S.sizes.useMeasurement}
          </Button>
        </Tooltip>
      </div>
      {wrong ? (
        <p className="t-caption pt-1 pl-6 text-app-fail" role="alert" data-testid="measure-wrong">
          {S.sizes.moreThanZero}
        </p>
      ) : size.ask ? (
        <p className="t-caption pt-1 pl-6 text-app-text-2" data-testid={`measure-ask-${size.name}`}>
          {size.ask.kind === "inside" || size.ask.kind === "outside"
            ? S.sizes.ask[size.ask.kind](part, side)
            : S.sizes.ask[size.ask.kind](part)}
        </p>
      ) : null}
      {notApplied ? (
        <div className="space-y-1 pt-1.5 pl-6" data-testid="not-applied">
          <p className="t-caption text-app-warn" role="status">
            {S.sizes.notApplied(sized(size.value, size.unit))}
          </p>
          <div className="flex flex-wrap gap-2">
            <Button
              variant="outline"
              size="sm"
              disabled={locked !== null}
              onClick={() => {
                send(notApplied.value);
              }}
            >
              {S.sizes.sendAgain}
            </Button>
            <Button variant="ghost" size="sm" onClick={clearNotApplied}>
              {S.sizes.dismiss}
            </Button>
          </div>
        </div>
      ) : null}
    </div>
  );
}

/** A number that is fixed: measured by the maker, a standard part, or from the printer. */
function FixedRow({ size, locked }: RowProps) {
  const setSize = useHotend((state) => state.setSize);
  const [editing, setEditing] = useState(false);
  const tag =
    size.provenance === "measured"
      ? { icon: Ruler, text: S.sizes.measured }
      : size.provenance === "calibrated"
        ? { icon: Gauge, text: S.sizes.calibrated }
        : { icon: BookMarked, text: S.sizes.standard };
  const Icon = tag.icon;
  return (
    <div className="py-1.5">
      <div className="flex min-h-7 items-center justify-between gap-2">
        <span className="t-ui min-w-0 truncate">{size.label}</span>
        {editing ? (
          <span className="flex items-center gap-1.5">
            <NumberField
              label={size.label}
              defaultValue={size.value}
              format={FORMAT}
              step={stepOf(size)}
              onValueCommitted={(value) => {
                setEditing(false);
                if (value !== null && value !== size.value) setSize(size.name, value);
              }}
            />
            <span className="t-ui-strong text-app-text-2">{unitLabel(size.unit)}</span>
          </span>
        ) : (
          <span className="flex items-center gap-2">
            <span className="t-ui-strong num">{sized(size.value, size.unit)}</span>
            {size.provenance === "measured" && locked === null ? (
              <Button
                variant="ghost"
                size="xs"
                className="text-app-accent"
                onClick={() => {
                  setEditing(true);
                }}
              >
                {S.sizes.change}
              </Button>
            ) : null}
          </span>
        )}
      </div>
      <Badge>
        <Icon strokeWidth={1.5} aria-hidden />
        {tag.text}
      </Badge>
    </div>
  );
}

export function Sizes({ result, readOnly }: { result: EngineResult | undefined; readOnly: number | null }) {
  const running = useHotend((state) => state.run?.status === "running");
  const talk = useHost(canTalk);
  const openSize = useStudioUi((state) => state.openSize);
  const [showFixed, setShowFixed] = useState(false);
  const [showAll, setShowAll] = useState(false);
  const sizes = sizesOf(result);
  const measure = sizes.filter((size) => size.group === "measure");
  const choices = sizes.filter((size) => size.group === "choices");
  const fixed = sizes.filter((size) => size.group === "fixed");
  const locked = running ? S.sizes.working : readOnly !== null ? S.sizes.readOnly(readOnly) : null;
  const measureLocked = locked ?? (talk ? null : S.sizes.needsKey);
  // The first row is open when the project opens; after that the maker's choice holds.
  const notApplied = useHotend((state) => state.notApplied);
  // A measurement that was not applied shows its row, with the model's value back in it.
  const open = notApplied && measure.some((size) => size.name === notApplied.name)
    ? notApplied.name
    : openSize === undefined
      ? (measure[0]?.name ?? null)
      : openSize;

  return (
    <section aria-labelledby="sizes-title" data-testid="sizes">
      <SectionLabel id="sizes-title">{S.sizes.title}</SectionLabel>
      {sizes.length === 0 ? <p className="t-ui py-1 text-app-text-2">{S.sizes.none}</p> : null}

      {measure.length > 0 ? (
        <div data-scroll="measure" className="pb-2">
          <h3 className="t-caption py-1 text-app-text-2">{S.sizes.groups.measure}</h3>
          {/* What the group is for comes before its first row, not after its last. */}
          <p className="t-caption pb-1 text-app-text-3">{S.sizes.measureHelp}</p>
          {measure.map((size) => (
            <MeasureRow
              key={`${size.name}:${size.value}:${notApplied?.name === size.name ? notApplied.value : ""}`}
              size={size}
              locked={measureLocked}
              open={open === size.name}
              span={readOnly === null ? spanOf(size, result) : null}
            />
          ))}
        </div>
      ) : null}

      {choices.length > 0 ? (
        <div className={cn("pb-2", locked !== null && "opacity-100")}>
          <h3 className="t-caption py-1 text-app-text-2">{S.sizes.groups.choices}</h3>
          {/* A model with a dozen sliders shows six: Proof stays within reach. */}
          {(showAll ? choices : choices.slice(0, CHOICES_SHOWN)).map((size) => (
            <ChoiceRow key={size.name} size={size} locked={locked} />
          ))}
          {choices.length > CHOICES_SHOWN ? (
            <Button
              variant="outline"
              size="sm"
              aria-expanded={showAll}
              data-testid="sizes-show-all"
              onClick={() => {
                setShowAll(!showAll);
              }}
            >
              {showAll ? S.sizes.showFewer : S.sizes.showAll(choices.length)}
            </Button>
          ) : null}
        </div>
      ) : null}

      {fixed.length > 0 ? (
        <div className="pb-2">
          <button
            type="button"
            aria-expanded={showFixed}
            onClick={() => {
              setShowFixed(!showFixed);
            }}
            className="t-caption flex h-7 w-full items-center gap-1 rounded-control text-app-text-2 hover:text-app-text"
          >
            {showFixed ? <ChevronDown className="size-3.5" strokeWidth={1.5} /> : <ChevronRight className="size-3.5" strokeWidth={1.5} />}
            {S.sizes.groups.fixed(fixed.length)}
          </button>
          {showFixed ? fixed.map((size) => <FixedRow key={size.name} size={size} locked={locked} />) : null}
        </div>
      ) : null}
    </section>
  );
}
