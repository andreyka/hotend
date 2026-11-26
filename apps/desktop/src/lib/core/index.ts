// The typed client of the core. Nothing outside this folder imports from @tauri-apps.
import type { CoreClient } from "./client";
import { tauriClient } from "./tauri";

export type * from "./bindings";
export type { CoreClient, CoreCommands, EventHandler, Unsubscribe } from "./client";
export { isApiError, toApiError } from "./errors";
export { agentEvent, type AgentEvent } from "./agent-events";

/**
 * The client for where the interface is running: Tauri's IPC in the window, the
 * development bridge in a browser. The bridge transport is compiled only into the
 * bridge build.
 */
export async function connect(): Promise<CoreClient> {
  if (__HOTEND_BRIDGE__) {
    const { bridgeAddress, bridgeClient } = await import("./bridge");
    const address = bridgeAddress();
    if (!address) {
      throw new Error(
        "This build talks to hotend-bridge. Open it with ?bridge=<url>&token=<token>, " +
          "the two values hotend-bridge prints when it starts.",
      );
    }
    return bridgeClient(address);
  }
  return tauriClient();
}
