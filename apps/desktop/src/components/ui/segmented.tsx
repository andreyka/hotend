// Segmented choice: provider, theme, the direction of Cut open (DESIGN_SPEC 8). One is
// always selected. Built on Base UI's ToggleGroup.
import { Toggle } from "@base-ui/react/toggle";
import { ToggleGroup } from "@base-ui/react/toggle-group";
import type { ReactNode } from "react";

import { cn } from "@/lib/utils";

export type Segment<T extends string> = { value: T; label: ReactNode; name?: string };

export function Segmented<T extends string>({
  value,
  onChange,
  segments,
  label,
  className,
  size = "default",
}: {
  value: T;
  onChange(value: T): void;
  segments: readonly Segment<T>[];
  label: string;
  className?: string;
  size?: "default" | "sm";
}) {
  return (
    <ToggleGroup
      aria-label={label}
      value={[value]}
      onValueChange={(next) => {
        const chosen = next[0] as T | undefined;
        // A click on the selected segment leaves it selected.
        if (chosen !== undefined) onChange(chosen);
      }}
      className={cn(
        "inline-flex max-w-full flex-wrap gap-0.5 rounded-control border border-app-line bg-app-subtle p-0.5",
        className,
      )}
    >
      {segments.map((segment) => (
        <Toggle
          key={segment.value}
          value={segment.value}
          aria-label={segment.name}
          className={cn(
            "t-ui-strong inline-flex items-center justify-center gap-1.5 rounded-[4px] border border-transparent px-2.5 text-app-text-2 select-none hover:text-app-text [&_svg]:size-4",
            // The selected one is a raised chip (styles/app.css), not a bordered field.
            "app-segment",
            size === "sm" ? "h-6" : "h-7",
          )}
        >
          {segment.label}
        </Toggle>
      ))}
    </ToggleGroup>
  );
}
