import type { ReactNode } from "react";

/** A key hint in a tooltip or in the Shortcuts table. */
export function Kbd({ children }: { children: ReactNode }) {
  return (
    <kbd className="t-caption inline-flex h-5 min-w-5 items-center justify-center rounded-tag border border-app-line bg-app-subtle px-1 font-sans text-app-text-2">
      {children}
    </kbd>
  );
}
