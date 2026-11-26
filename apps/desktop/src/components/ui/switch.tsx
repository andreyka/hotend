import { Switch as SwitchPrimitive } from "@base-ui/react/switch";

import { cn } from "@/lib/utils";

/** Switch: the stand-in slicer (DESIGN_SPEC 8). */
export function Switch({ className, ...props }: SwitchPrimitive.Root.Props) {
  return (
    <SwitchPrimitive.Root
      className={cn(
        "relative inline-flex h-5 w-9 shrink-0 items-center rounded-pill border border-app-control bg-app-subtle transition-colors data-[checked]:border-app-accent-fill data-[checked]:bg-app-accent-fill data-[disabled]:opacity-40",
        className,
      )}
      {...props}
    >
      <SwitchPrimitive.Thumb className="block size-3.5 translate-x-0.5 rounded-pill bg-app-text transition-transform duration-[var(--app-dur-fast)] data-[checked]:translate-x-[18px] data-[checked]:bg-app-on-fill" />
    </SwitchPrimitive.Root>
  );
}
