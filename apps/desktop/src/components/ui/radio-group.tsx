// Radio rows: Send to, Turn (DESIGN_SPEC 8). The sentence under a choice is part of its
// label.
import { Radio } from "@base-ui/react/radio";
import { RadioGroup as RadioGroupPrimitive } from "@base-ui/react/radio-group";
import type { ReactNode } from "react";

import { cn } from "@/lib/utils";

export function RadioGroup({ className, ...props }: RadioGroupPrimitive.Props) {
  return <RadioGroupPrimitive className={cn("flex flex-col gap-1", className)} {...props} />;
}

export function RadioRow({
  value,
  children,
  disabled,
  className,
}: {
  value: string;
  children: ReactNode;
  disabled?: boolean;
  className?: string;
}) {
  return (
    <label
      className={cn(
        "t-ui flex cursor-default items-start gap-2 rounded-control px-2 py-1.5 hover:bg-app-hover has-[[data-disabled]]:hover:bg-transparent",
        className,
      )}
    >
      <Radio.Root
        value={value}
        disabled={disabled}
        className="mt-px inline-flex size-4 shrink-0 items-center justify-center rounded-pill border border-app-control bg-app-raised data-[checked]:border-app-accent data-[disabled]:opacity-40"
      >
        <Radio.Indicator className="size-2 rounded-pill bg-app-accent" />
      </Radio.Root>
      <span className="min-w-0 flex-1">{children}</span>
    </label>
  );
}
