import { Checkbox as CheckboxPrimitive } from "@base-ui/react/checkbox";
import { Check } from "lucide-react";

import { cn } from "@/lib/utils";

/** Checkbox: export formats (DESIGN_SPEC 8). */
export function Checkbox({ className, ...props }: CheckboxPrimitive.Root.Props) {
  return (
    <CheckboxPrimitive.Root
      className={cn(
        "inline-flex size-4 shrink-0 items-center justify-center rounded-tag border border-app-control bg-app-raised data-[checked]:border-app-accent-fill data-[checked]:bg-app-accent-fill data-[checked]:text-app-on-fill",
        className,
      )}
      {...props}
    >
      <CheckboxPrimitive.Indicator>
        <Check className="size-3" strokeWidth={2} />
      </CheckboxPrimitive.Indicator>
    </CheckboxPrimitive.Root>
  );
}
