import { LoaderCircle } from "lucide-react";

import { cn } from "@/lib/utils";

/** The busy mark (DESIGN_SPEC 8): `loader-circle`, one turn per 1.2 s. */
export function Spinner({ className, label }: { className?: string; label?: string }) {
  return (
    <LoaderCircle
      role={label ? "img" : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
      strokeWidth={1.5}
      className={cn("app-spin size-4 shrink-0", className)}
    />
  );
}
