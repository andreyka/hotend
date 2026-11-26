// A CoreClient for tests of the interface code. Only the commands a test sets answer;
// every other command fails the test that calls it.
import { vi } from "vitest";

import type { CoreClient, EventHandler, ShellEvent } from "@/lib/core";
import { commands } from "@/lib/core/bindings";

export type FakeClient = CoreClient & {
  /** Delivers an event to every subscriber, as the core's stream would. */
  emit(event: ShellEvent): void;
  subscribers(): number;
};

export function fakeClient(answers: Partial<CoreClient>): FakeClient {
  const handlers = new Set<EventHandler>();
  const refused = Object.fromEntries(
    Object.keys(commands)
      .filter((name) => name !== "events_subscribe")
      .map((name) => [
        name,
        vi.fn(() => Promise.reject(new Error(`the test did not expect ${name}`))),
      ]),
  ) as unknown as CoreClient;
  return {
    ...refused,
    transport: "bridge",
    subscribe: (handler) => {
      handlers.add(handler);
      return Promise.resolve(() => {
        handlers.delete(handler);
      });
    },
    ...answers,
    emit(event) {
      for (const handler of handlers) handler(event);
    },
    subscribers: () => handlers.size,
  };
}
