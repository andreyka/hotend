// shadcn/ui `textarea` (apps/v4/registry/bases/base/ui/textarea.tsx of the shadcn-ui
// repository, MIT). Its classes are in src/styles/shadcn.css.
import type * as React from "react";

import { cn } from "@/lib/utils";

function Textarea({ className, ...props }: React.ComponentProps<"textarea">) {
  return (
    <textarea
      data-slot="textarea"
      className={cn(
        "cn-textarea flex field-sizing-content min-h-10 w-full placeholder:text-muted-foreground disabled:cursor-not-allowed disabled:opacity-40",
        className,
      )}
      {...props}
    />
  );
}

export { Textarea };
