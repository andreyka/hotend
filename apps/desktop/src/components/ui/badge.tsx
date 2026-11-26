// Tag: provenance, Simulated, slice warnings (DESIGN_SPEC 8). 20 px high, radius 4, an
// icon and a few words. The tone names a state; `sim` is for simulated and stand-in only.
import type { ReactNode } from "react";

import { cn } from "@/lib/utils";

export type Tone = "neutral" | "ok" | "warn" | "fail" | "sim" | "accent";

const TONES: Record<Tone, string> = {
  neutral: "bg-app-subtle text-app-text-2",
  ok: "bg-app-ok-wash text-app-ok",
  warn: "bg-app-warn-wash text-app-warn",
  fail: "bg-app-fail-wash text-app-fail",
  sim: "bg-app-sim-wash text-app-sim",
  accent: "bg-app-accent-wash text-app-accent",
};

export function Badge({
  tone = "neutral",
  className,
  children,
  title,
}: {
  tone?: Tone;
  className?: string;
  children: ReactNode;
  title?: string;
}) {
  return (
    <span
      title={title}
      className={cn(
        "t-caption inline-flex min-h-5 max-w-full items-center gap-1 rounded-tag px-1.5 py-0.5 align-middle [&_svg]:size-3.5 [&_svg]:shrink-0",
        TONES[tone],
        className,
      )}
    >
      {children}
    </span>
  );
}
