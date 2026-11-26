import { createContext, use, useEffect, useMemo, useState, type ReactNode } from "react";
import { useStore } from "zustand";

import type { CoreClient } from "@/lib/core";
import { pendingCommands, type PendingCommands } from "@/lib/core/pending";

import { createHostStore, type HostState, type HostStore } from "./store";

export const CoreContext = createContext<CoreClient | null>(null);

/** The typed client of the core. The only way a screen reaches the core. */
export function useCore(): CoreClient {
  const client = use(CoreContext);
  if (!client) throw new Error("useCore needs a CoreContext above it.");
  return client;
}

/** Stands in for the pending commands (tests). Without it they come from the core's client. */
export const PendingContext = createContext<PendingCommands | null>(null);

/** The commands the shell does not carry yet (@/lib/core/pending). */
export function usePending(): PendingCommands {
  const given = use(PendingContext);
  const client = use(CoreContext);
  return useMemo(() => given ?? pendingCommands(client), [given, client]);
}

const HostContext = createContext<HostStore | null>(null);

/** Creates the host's store and reads the settings once. */
export function HostProvider({ children }: { children: ReactNode }) {
  const client = useCore();
  const [store] = useState(() => createHostStore(client));
  useEffect(() => {
    void store.getState().load();
  }, [store]);
  return <HostContext value={store}>{children}</HostContext>;
}

export function useHostStore(): HostStore {
  const store = use(HostContext);
  if (!store) throw new Error("useHost needs a HostProvider above it.");
  return store;
}

export function useHost<T>(selector: (state: HostState) => T): T {
  return useStore(useHostStore(), selector);
}
