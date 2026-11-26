import { createContext, use, useEffect, useMemo, useState, type ReactNode } from "react";
import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";

import { useCore, useHost } from "@/host/core-context";
import { canTalk } from "@/host/store";

import { rememberedRun } from "./run";
import { createHotendStore, type HotendState, type HotendStore } from "./store";
import { ACTIVE_JOB } from "./store";
import type { VerdictInput } from "./verdict";

const HotendContext = createContext<HotendStore | null>(null);

/** Creates the module's store and feeds it the core's events for as long as it is shown. */
export function HotendProvider({ children }: { children: ReactNode }) {
  const client = useCore();
  const [store] = useState(() => createHotendStore(client));

  useEffect(() => {
    let stop: (() => void) | null = null;
    let left = false;
    void client
      .subscribe((event) => {
        store.getState().receive(event);
      })
      .then(
        (unsubscribe) => {
          if (left) unsubscribe();
          else stop = unsubscribe;
        },
        (thrown: unknown) => {
          store.setState({
            error: {
              kind: "internal",
              message: `Live updates could not start: ${String(thrown)}`,
              next_step: "Restart Hotend.",
            },
          });
        },
      );
    void store.getState().loadPrinters();
    return () => {
      left = true;
      stop?.();
    };
  }, [client, store]);

  return <HotendContext value={store}>{children}</HotendContext>;
}

export function useHotendStore(): HotendStore {
  const store = use(HotendContext);
  if (!store) throw new Error("useHotend needs a HotendProvider above it.");
  return store;
}

export function useHotend<T>(selector: (state: HotendState) => T): T {
  return useStore(useHotendStore(), selector);
}

/** A clock that ticks once a second while `running`, for sentences that count time. */
export function useNow(running: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!running) return;
    const timer = setInterval(() => {
      setNow(Date.now());
    }, 1000);
    return () => {
      clearInterval(timer);
    };
  }, [running]);
  return now;
}

/** Everything the verdict bar and the stage marks are computed from. */
export function useVerdictInput(): VerdictInput {
  const state = useHotend(
    useShallow((s) => ({
      project: s.project,
      results: s.results,
      look: s.look,
      run: s.run,
      rebuild: s.rebuild,
      refused: s.refused,
      rebuildError: s.rebuildError,
      preparation: s.preparation,
      preparing: s.preparing,
      prepareError: s.prepareError,
      plate: s.plate,
      quality: s.quality,
      slice: s.slice,
      sliceStale: s.sliceStale,
      slicing: s.slicing,
      sliceError: s.sliceError,
      jobs: s.jobs,
      review: s.review,
      reviewing: s.reviewing,
      reviewError: s.reviewError,
      confirming: s.confirming,
      wantReview: s.wantReview,
      printers: s.printers,
    })),
  );
  // A first message that was stopped before anything was built: known from the run, or
  // after a reload from what the window remembers of it.
  const stoppedEmpty = useHotend((s) => {
    if (s.project === null || s.project.versions.length > 0) return false;
    if (s.run !== null) return s.run.status === "cancelled";
    const last = s.project.transcript.at(-1);
    return last !== undefined && rememberedRun(last.run_id)?.status === "cancelled";
  });
  const talk = useHost(canTalk);
  const previews = useHost((host) => host.settings?.previews ?? null);
  const counting = state.rebuild !== null || state.jobs.some((job) => job.status === "starting" && ACTIVE_JOB.has(job.status));
  const now = useNow(counting);
  return useMemo(() => ({ ...state, canTalk: talk, now, stoppedEmpty, previews }), [state, talk, now, stoppedEmpty, previews]);
}
