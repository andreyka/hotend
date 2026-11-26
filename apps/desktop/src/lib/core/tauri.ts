// The window's transport: Tauri's IPC. Events arrive on a Tauri channel.
import { Channel } from "@tauri-apps/api/core";

import { commands, type ShellEvent } from "./bindings";
import { fanOut, type CoreClient } from "./client";

export function tauriClient(): CoreClient {
  const { events_subscribe, ...rest } = commands;
  return {
    ...rest,
    transport: "tauri",
    subscribe: fanOut(async (deliver) => {
      const channel = new Channel<ShellEvent>();
      channel.onmessage = deliver;
      await events_subscribe(channel);
      // The shell keeps one subscription per window and ends it when the next one
      // starts or the window closes. A channel without a handler drops what arrives.
      return () => {
        channel.onmessage = () => undefined;
      };
    }),
  };
}
