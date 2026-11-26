// Tooltip on every icon button and every disabled control (DESIGN_SPEC 8), on Base UI.
import { Tooltip as TooltipPrimitive } from "@base-ui/react/tooltip";
import type { ReactElement, ReactNode } from "react";

export function TooltipProvider({ children }: { children: ReactNode }) {
  return (
    <TooltipPrimitive.Provider delay={600} closeDelay={0} timeout={400}>
      {children}
    </TooltipPrimitive.Provider>
  );
}

/**
 * Wraps one control. `label` is what the tooltip says; with no label the control is
 * returned as it is. A disabled control still shows its tooltip: the trigger is a span
 * around it.
 */
export function Tooltip({
  label,
  children,
  side = "bottom",
  wrap = false,
}: {
  label: ReactNode;
  children: ReactElement;
  side?: "top" | "bottom" | "left" | "right";
  /** Put the trigger on a span around the control (for a disabled control); "block" for one as wide as its row. */
  wrap?: boolean | "block";
}) {
  if (label === null || label === undefined || label === "") return children;
  return (
    <TooltipPrimitive.Root>
      {wrap ? (
        <TooltipPrimitive.Trigger render={<span className={wrap === "block" ? "flex w-full" : "inline-flex"} tabIndex={0} />}>
          {children}
        </TooltipPrimitive.Trigger>
      ) : (
        <TooltipPrimitive.Trigger render={children} />
      )}
      <TooltipPrimitive.Portal>
        <TooltipPrimitive.Positioner side={side} sideOffset={6} className="z-50">
          <TooltipPrimitive.Popup className="app-popup t-ui max-w-[320px] rounded-control px-2 py-1">
            {label}
          </TooltipPrimitive.Popup>
        </TooltipPrimitive.Positioner>
      </TooltipPrimitive.Portal>
    </TooltipPrimitive.Root>
  );
}
