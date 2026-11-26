// The one interface the interface code uses to reach the core. It has two transports
// (./tauri.ts for the window, ./bridge.ts for a browser during development) and both
// carry exactly the commands of ./bindings.ts, which tauri-specta generates from the
// Rust command layer.
import type { commands, ShellEvent } from "./bindings";

/** Every command of the shell except the event subscription, which `subscribe` wraps. */
export type CoreCommands = Omit<typeof commands, "events_subscribe">;

export type EventHandler = (event: ShellEvent) => void;
export type Unsubscribe = () => void;

export interface CoreClient extends CoreCommands {
  /** Which transport carries the calls. For display and tests only. */
  readonly transport: "tauri" | "bridge";
  /**
   * Agent and print events, in order. The stream is opened with the first handler and
   * closed with the last. An event `{channel: "lagged"}` says some were missed: read the
   * state again.
   */
  subscribe(handler: EventHandler): Promise<Unsubscribe>;
}

/**
 * Shares one stream of events between any number of handlers. `open` starts the stream
 * and returns how to stop it.
 */
export function fanOut(
  open: (deliver: EventHandler) => Promise<Unsubscribe>,
): CoreClient["subscribe"] {
  const handlers = new Set<EventHandler>();
  let stream: Promise<Unsubscribe> | null = null;
  const deliver: EventHandler = (event) => {
    for (const handler of [...handlers]) handler(event);
  };
  return async (handler) => {
    handlers.add(handler);
    stream ??= open(deliver);
    const opened = stream;
    try {
      await opened;
    } catch (error) {
      handlers.delete(handler);
      if (stream === opened) stream = null;
      throw error;
    }
    return () => {
      if (!handlers.delete(handler) || handlers.size > 0 || stream !== opened) return;
      stream = null;
      void opened.then((close) => close());
    };
  };
}
