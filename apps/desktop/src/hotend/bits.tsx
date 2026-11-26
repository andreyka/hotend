// Small pieces every Hotend screen shares: the status icons (a shape and a colour per
// state, never colour alone), the "Simulated" tag, the error card, an icon button with
// its tooltip, and a section label.
import {
  ChevronDown,
  ChevronRight,
  CircleCheck,
  CircleDashed,
  CircleQuestionMark,
  CircleX,
  FlaskConical,
  History,
  TriangleAlert,
  type LucideIcon,
} from "lucide-react";
import { Fragment, useState, type ReactNode } from "react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { Tooltip } from "@/components/ui/tooltip";
import type { ApiError } from "@/lib/core";
import { cn } from "@/lib/utils";

import { S } from "./strings";

export type StatusKind = "ok" | "note" | "problem" | "question" | "skipped" | "busy" | "history" | "sim" | "none";

const ICONS: Record<Exclude<StatusKind, "busy" | "none">, { icon: LucideIcon; tone: string }> = {
  ok: { icon: CircleCheck, tone: "text-app-ok" },
  note: { icon: TriangleAlert, tone: "text-app-warn" },
  problem: { icon: CircleX, tone: "text-app-fail" },
  question: { icon: CircleQuestionMark, tone: "text-app-warn" },
  skipped: { icon: CircleDashed, tone: "text-app-text-3" },
  history: { icon: History, tone: "text-app-text-2" },
  // Something simulated or stood in for: never the green tick (DESIGN_SPEC 11).
  sim: { icon: FlaskConical, tone: "text-app-sim" },
};

/** The icon of a state. `label` names it for a screen reader; without one it is decoration. */
export function StatusIcon({ kind, label, className }: { kind: StatusKind; label?: string; className?: string }) {
  if (kind === "none") return null;
  if (kind === "busy") return <Spinner className={cn("text-app-text-2", className)} label={label} />;
  const { icon: Icon, tone } = ICONS[kind];
  return (
    <Icon
      strokeWidth={1.5}
      role={label ? "img" : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
      data-status={kind}
      className={cn("size-4 shrink-0", tone, className)}
    />
  );
}

/** The tag for everything simulated: the reserved colour, a flask, the word. */
export function SimTag({ children = S.printerTag, className }: { children?: ReactNode; className?: string }) {
  return (
    <Badge tone="sim" className={className}>
      <FlaskConical strokeWidth={1.5} aria-hidden />
      {children}
    </Badge>
  );
}

/** An icon that stands alone: always with a tooltip and an accessible name. */
export function IconButton({
  label,
  hint,
  children,
  className,
  pressed,
  side,
  ...props
}: {
  label: string;
  /** A key hint shown after the label in the tooltip. */
  hint?: ReactNode;
  children: ReactNode;
  className?: string;
  pressed?: boolean;
  side?: "top" | "bottom" | "left" | "right";
} & Omit<React.ComponentProps<typeof Button>, "children" | "className">) {
  return (
    <Tooltip
      side={side}
      label={
        <span className="inline-flex items-center gap-2">
          {label}
          {hint}
        </span>
      }
    >
      <Button
        variant="ghost"
        size="icon-sm"
        aria-label={label}
        aria-pressed={pressed}
        className={cn("text-app-text-2 hover:text-app-text [&_svg]:size-[18px]", pressed && "bg-app-active text-app-text", className)}
        {...props}
      >
        {children}
      </Button>
    </Tooltip>
  );
}

/** The 11 px uppercase label of an inspector section. */
export function SectionLabel({ children, aside, id }: { children: ReactNode; aside?: ReactNode; id?: string }) {
  return (
    // It stays at the top of the column while its section scrolls under it: with twelve
    // sizes the Proof count would otherwise be a screen away.
    <div
      className="sticky -top-3 z-[1] -mx-4 flex min-h-7 flex-wrap items-center justify-between gap-x-2 bg-app-panel px-4"
      data-section-label
    >
      <h2 id={id} className="t-label flex h-7 items-center text-app-text-2">
        {children}
      </h2>
      {aside}
    </div>
  );
}

/**
 * Error anatomy (DESIGN_SPEC 1.2, item 9): what happened, what to do, one button. It
 * stays until it is dealt with; nothing disappears by itself.
 */
export function ErrorCard({
  title,
  detail,
  nextStep,
  code,
  raw,
  action,
  className,
  ...props
}: {
  title: string;
  detail?: string | null;
  nextStep?: string | null;
  /** The core's code for the failure: for a bug report, so behind "Details". */
  code?: string | null;
  /** What the other side said word for word, also behind "Details". */
  raw?: string | null;
  action?: ReactNode;
  className?: string;
} & Omit<React.ComponentProps<"div">, "title" | "children" | "className" | "role">) {
  const [open, setOpen] = useState(false);
  return (
    // Never wider than a column of text: across a wide window it would be a red slab.
    <div role="alert" className={cn("app-enter flex max-w-[720px] gap-2 rounded-card bg-app-fail-wash p-4", className)} {...props}>
      <StatusIcon kind="problem" className="mt-0.5" />
      <div className="min-w-0 flex-1 space-y-1.5">
        <p className="t-ui-strong text-app-text">{title}</p>
        {detail ? <p className="t-ui break-words text-app-text-2">{detail}</p> : null}
        {nextStep ? <p className="t-ui text-app-text-2">{nextStep}</p> : null}
        {action ? <div className="pt-1">{action}</div> : null}
        {code || raw ? (
          <div>
            <button
              type="button"
              aria-expanded={open}
              data-testid="error-details"
              onClick={() => {
                setOpen(!open);
              }}
              className="t-caption inline-flex min-h-7 items-center gap-0.5 rounded-tag pr-1 text-app-text-2 hover:text-app-text"
            >
              {open ? <ChevronDown className="size-3.5" strokeWidth={1.5} aria-hidden /> : <ChevronRight className="size-3.5" strokeWidth={1.5} aria-hidden />}
              {S.error.details}
            </button>
            {open ? (
              <div className="space-y-1" data-testid="error-code">
                {raw ? <p className="t-caption break-words text-app-text-2">{raw}</p> : null}
                {code ? <p className="t-mono break-words text-app-text-2">{code}</p> : null}
              </div>
            ) : null}
          </div>
        ) : null}
      </div>
    </div>
  );
}

/** A command that failed, in the core's own words. */
export function ApiErrorCard({
  error,
  action,
  className,
}: {
  error: ApiError;
  action?: ReactNode;
  className?: string;
}) {
  // A project that is gone is said in the window's words wherever it is said; the core's
  // sentence, with the project's id in it, is for a bug report.
  const gone = error.kind === "not_found";
  return (
    <ErrorCard
      title={gone ? S.studio.gone : error.message}
      nextStep={error.next_step}
      raw={gone && error.message !== S.studio.gone ? error.message : null}
      code={"code" in error ? `${error.kind} · ${error.code}` : error.kind}
      action={action}
      className={className}
    />
  );
}

/**
 * A number that flashes once when it changes (DESIGN_SPEC 9), and not when it first
 * appears: `of` is what is watched.
 */
export function Flash({
  of,
  className,
  children,
  ...props
}: { of: string | number; className?: string; children: ReactNode } & Omit<React.ComponentProps<"span">, "children" | "className">) {
  const [first] = useState(of);
  return (
    <span key={of} className={cn(of !== first && "app-flash", className)} {...props}>
      {children}
    </span>
  );
}

/**
 * A path on this computer, in the mono face. It breaks after a slash and nowhere else,
 * unless one name alone is wider than the line.
 */
export function PathText({ path, className }: { path: string; className?: string }) {
  const pieces = path.split(/(?<=[\\/])/);
  return (
    <p className={cn("t-mono [overflow-wrap:break-word] break-normal text-app-text-2", className)} data-testid="path" title={path}>
      {pieces.map((piece, index) => (
        <Fragment key={index}>
          {piece}
          {index < pieces.length - 1 ? <wbr /> : null}
        </Fragment>
      ))}
    </p>
  );
}
