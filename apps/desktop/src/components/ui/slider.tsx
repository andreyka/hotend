// Size slider (DESIGN_SPEC 5.3): a 4 px track, the filled part in the accent fill, a
// 16 px white thumb. The hollow tick that marks the built value is drawn by the caller.
import { Slider as SliderPrimitive } from "@base-ui/react/slider";
import type { ReactNode } from "react";

import { cn } from "@/lib/utils";

export function Slider({
  className,
  label,
  unit,
  children,
  ...props
}: Omit<SliderPrimitive.Root.Props<number>, "children"> & {
  label: string;
  /** The unit after the value as a screen reader says it: "40 mm", never a bare "40". */
  unit?: string;
  /** Marks drawn on the track, positioned by the caller in percent. */
  children?: ReactNode;
}) {
  return (
    <SliderPrimitive.Root
      thumbAlignment="edge"
      className={cn("relative w-full data-[disabled]:opacity-40", className)}
      {...props}
    >
      <SliderPrimitive.Control className="flex h-5 w-full touch-none items-center select-none">
        <SliderPrimitive.Track className="relative h-1 w-full rounded-pill bg-app-control">
          <SliderPrimitive.Indicator className="rounded-pill bg-app-accent-fill" />
          {children}
          <SliderPrimitive.Thumb
            aria-label={label}
            getAriaValueText={unit ? (formatted) => `${formatted} ${unit}` : undefined}
            data-slot="slider-thumb"
            // The thumb is 16 px to look at and 28 px to hit (6.2).
            className="app-ring-group size-4 rounded-pill border border-app-control bg-app-on-fill shadow-1 before:absolute before:-inset-1.5 before:content-['']"
          />
        </SliderPrimitive.Track>
      </SliderPrimitive.Control>
    </SliderPrimitive.Root>
  );
}
