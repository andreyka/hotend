// Select: material, quality (DESIGN_SPEC 8), on Base UI's Select.
import { Select as SelectPrimitive } from "@base-ui/react/select";
import { Check, ChevronDown } from "lucide-react";

import { cn } from "@/lib/utils";

export type SelectOption = { value: string; label: string };

export function Select({
  value,
  onChange,
  options,
  label,
  className,
  disabled,
}: {
  value: string;
  onChange(value: string): void;
  options: readonly SelectOption[];
  /** The accessible name. */
  label: string;
  className?: string;
  disabled?: boolean;
}) {
  return (
    <SelectPrimitive.Root
      value={value}
      onValueChange={(next) => {
        if (typeof next === "string") onChange(next);
      }}
      items={options as SelectOption[]}
      disabled={disabled}
    >
      <SelectPrimitive.Trigger
        aria-label={label}
        className={cn(
          "t-ui flex h-8 min-w-0 items-center justify-between gap-2 rounded-control border border-app-control bg-app-raised px-2.5 text-left select-none hover:bg-app-hover data-[popup-open]:bg-app-hover data-[disabled]:opacity-40",
          className,
        )}
      >
        <SelectPrimitive.Value className="min-w-0 truncate" />
        <SelectPrimitive.Icon className="shrink-0 text-app-text-2">
          <ChevronDown className="size-4" strokeWidth={1.5} />
        </SelectPrimitive.Icon>
      </SelectPrimitive.Trigger>
      <SelectPrimitive.Portal>
        <SelectPrimitive.Positioner sideOffset={6} alignItemWithTrigger={false} className="z-40">
          <SelectPrimitive.Popup className="app-popup min-w-[var(--anchor-width)] p-1">
            {options.map((option) => (
              <SelectPrimitive.Item
                key={option.value}
                value={option.value}
                className="t-ui flex h-7 cursor-default items-center gap-2 rounded-tag pr-2 pl-7 select-none data-[highlighted]:bg-app-hover"
              >
                <SelectPrimitive.ItemIndicator className="absolute left-2 inline-flex">
                  <Check className="size-4" strokeWidth={1.5} />
                </SelectPrimitive.ItemIndicator>
                <SelectPrimitive.ItemText>{option.label}</SelectPrimitive.ItemText>
              </SelectPrimitive.Item>
            ))}
          </SelectPrimitive.Popup>
        </SelectPrimitive.Positioner>
      </SelectPrimitive.Portal>
    </SelectPrimitive.Root>
  );
}
