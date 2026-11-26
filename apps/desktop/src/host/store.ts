// The host's own state: the settings, whether a key is stored, and the health check of
// this computer. Everything in it came from the core.
import { createStore, type StoreApi } from "zustand/vanilla";

import {
  toApiError,
  type ApiError,
  type CoreClient,
  type DoctorReport,
  type ProviderKind,
  type Settings,
} from "@/lib/core";

export type HostState = {
  /** Null until read. */
  settings: Settings | null;
  /** Whether a key for the chosen provider is stored (or found). Null until asked. */
  keyStored: boolean | null;
  doctor: DoctorReport | null;
  doctorBusy: boolean;
  /** The last settings or key command that failed. */
  error: ApiError | null;

  load(): Promise<void>;
  /** Saves the settings with some fields changed. False when the core refused. */
  change(patch: Partial<Settings>): Promise<boolean>;
  /** Reads the settings again, after a command that changed them (a slicer choice). */
  refresh(): Promise<void>;
  saveKey(kind: ProviderKind, key: string): Promise<boolean>;
  removeKey(kind: ProviderKind): Promise<void>;
  checkComputer(): Promise<void>;
  /** The health check without its slow self-tests, once, when none has run yet. */
  quickCheck(): Promise<void>;
  dismissError(): void;
};

export type HostStore = StoreApi<HostState>;

export function createHostStore(client: CoreClient): HostStore {
  return createStore<HostState>()((set, get) => {
    const readKey = async (settings: Settings | null) => {
      const kind = settings?.provider?.kind;
      if (kind === undefined || kind === "ollama") {
        set({ keyStored: kind === "ollama" ? true : null });
        return;
      }
      try {
        set({ keyStored: await client.provider_key_status(kind) });
      } catch (thrown) {
        set({ keyStored: false, error: toApiError(thrown) });
      }
    };

    return {
      settings: null,
      keyStored: null,
      doctor: null,
      doctorBusy: false,
      error: null,

      async load() {
        try {
          const settings = await client.settings_get();
          set({ settings });
          await readKey(settings);
        } catch (thrown) {
          set({ error: toApiError(thrown) });
        }
      },

      async change(patch) {
        const current = get().settings;
        if (!current) return false;
        const next = { ...current, ...patch };
        try {
          await client.settings_set(next);
          set({ settings: next, error: null });
          if (patch.provider !== undefined) await readKey(next);
          return true;
        } catch (thrown) {
          set({ error: toApiError(thrown) });
          return false;
        }
      },

      async refresh() {
        await get().load();
      },

      async saveKey(kind, key) {
        try {
          await client.provider_key_set(kind, key);
          set({ error: null });
          await readKey(get().settings);
          return true;
        } catch (thrown) {
          set({ error: toApiError(thrown) });
          return false;
        }
      },

      async removeKey(kind) {
        try {
          await client.provider_key_delete(kind);
          await readKey(get().settings);
        } catch (thrown) {
          set({ error: toApiError(thrown) });
        }
      },

      async checkComputer() {
        if (get().doctorBusy) return;
        set({ doctorBusy: true });
        try {
          set({ doctor: await client.doctor_report(false) });
        } catch (thrown) {
          set({ error: toApiError(thrown) });
        } finally {
          set({ doctorBusy: false });
        }
      },

      async quickCheck() {
        if (get().doctor !== null || get().doctorBusy) return;
        try {
          const doctor = await client.doctor_report(true);
          if (get().doctor === null) set({ doctor });
        } catch {
          // The full check on its own page says what is wrong.
        }
      },

      dismissError() {
        set({ error: null });
      },
    };
  });
}

/** A check Hotend cannot work without has failed: AI-written models will not run. */
export function healthFailed(state: Pick<HostState, "doctor">): boolean {
  return state.doctor?.checks.some((check) => check.required && check.status === "fail") ?? false;
}

/** True when the maker can talk to the AI: a provider is chosen and it has its key. */
export function canTalk(state: Pick<HostState, "settings" | "keyStored">): boolean {
  const provider = state.settings?.provider;
  if (!provider) return false;
  return provider.kind === "ollama" || state.keyStored === true;
}
