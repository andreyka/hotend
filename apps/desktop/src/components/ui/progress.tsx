import { Progress as ProgressPrimitive } from "@base-ui/react/progress";

/** Print progress (DESIGN_SPEC 8). `value` is 0 to 100, or null while it is not known. */
export function Progress({ value, label }: { value: number | null; label: string }) {
  return (
    <ProgressPrimitive.Root value={value} aria-label={label}>
      <ProgressPrimitive.Track className="h-1 w-full overflow-hidden rounded-pill bg-app-active">
        <ProgressPrimitive.Indicator className="h-full rounded-pill bg-app-text" />
      </ProgressPrimitive.Track>
    </ProgressPrimitive.Root>
  );
}
