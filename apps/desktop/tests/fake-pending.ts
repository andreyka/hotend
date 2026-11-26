// Stand-in answers for the commands of @/lib/core/pending.
// Only the commands a test sets answer; every other one fails the test that calls it.
import { vi } from "vitest";

import { pendingCommands, type LanPrinterStatus, type PendingCommands } from "@/lib/core/pending";

export type StandInPending = PendingCommands & {
  /** Delivers a printer report to whoever listens to `printer_events`. */
  report(status: LanPrinterStatus): void;
};

export function standInPending(answers: Partial<PendingCommands>): StandInPending {
  const listeners = new Set<(status: LanPrinterStatus) => void>();
  const names = Object.keys(pendingCommands(null)).filter((name) => name !== "available");
  const refused = Object.fromEntries(
    names.map((name) => [name, vi.fn(() => Promise.reject(new Error(`the test did not expect ${name}`)))]),
  ) as unknown as PendingCommands;
  return {
    ...refused,
    printer_events: vi.fn((handler: (status: LanPrinterStatus) => void) => {
      listeners.add(handler);
      return Promise.resolve(() => {
        listeners.delete(handler);
      });
    }),
    ...answers,
    available: true,
    report(status) {
      for (const listener of listeners) listener(status);
    },
  };
}
