// The development transport: the same commands over HTTP to `hotend-bridge` on this
// computer (apps/desktop/src-tauri/src/bridge.rs). It exists so that the interface can be
// driven in a normal browser by Playwright. This file is only reachable in the bridge
// build; `scripts/check-dist.mjs` fails the window's build if any of it is in there.
//
// Wire format: POST <url>/invoke/<command> with the command's named arguments as JSON
// and `Authorization: Bearer <token>`; the answer is {ok: true, data} or
// {ok: false, error}. GET <url>/events is a server-sent event stream of ShellEvent.
import { mockIPC } from "@tauri-apps/api/mocks";

import { commands, type ApiError, type ShellEvent } from "./bindings";
import { fanOut, type CoreClient, type EventHandler } from "./client";

export type BridgeAddress = { url: string; token: string };

const STORAGE_KEY = "hotend.bridge";

/**
 * Where the bridge is. Tests open the page with `?bridge=<url>&token=<token>`; the pair
 * is moved into the tab's session storage and taken out of the address, so that it
 * survives a reload and is not left in the address bar.
 */
export function bridgeAddress(
  location: Pick<Location, "search" | "pathname" | "hash"> = window.location,
  storage: Pick<Storage, "getItem" | "setItem"> = window.sessionStorage,
  history: Pick<History, "replaceState"> = window.history,
): BridgeAddress | null {
  const query = new URLSearchParams(location.search);
  const url = query.get("bridge");
  const token = query.get("token");
  if (url && token) {
    const address = { url: url.replace(/\/+$/, ""), token };
    if (!isLoopback(address.url)) return null;
    storage.setItem(STORAGE_KEY, JSON.stringify(address));
    history.replaceState(null, "", `${location.pathname}${location.hash}`);
    return address;
  }
  const stored = storage.getItem(STORAGE_KEY);
  if (!stored) return null;
  const address = JSON.parse(stored) as BridgeAddress;
  return isLoopback(address.url) ? address : null;
}

/** The bridge listens on this computer only, and the page refuses any other address. */
export function isLoopback(url: string): boolean {
  return /^http:\/\/127\.0\.0\.1:\d{1,5}$/.test(url);
}

type Answer = { ok: true; data: unknown } | { ok: false; error: ApiError };

export async function invokeOverBridge(
  address: BridgeAddress,
  command: string,
  args: unknown,
): Promise<unknown> {
  const response = await fetch(`${address.url}/invoke/${command}`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${address.token}`,
      "content-type": "application/json",
    },
    body: JSON.stringify(args ?? {}),
  });
  if (response.status !== 200) {
    throw bridgeFailure(`The bridge answered ${response.status} to ${command}.`);
  }
  const answer = (await response.json()) as Answer;
  if (answer.ok) return answer.data;
  // The same value the window's transport rejects with: the core's own error.
  throw answer.error;
}

function bridgeFailure(message: string): ApiError {
  return {
    kind: "internal",
    message,
    next_step: "Start hotend-bridge again and reload the page with its address.",
  };
}

/** Reads server-sent events from a stream of bytes, one `data:` payload per call. */
export async function readEvents(
  body: ReadableStream<Uint8Array>,
  deliver: (data: string) => void,
): Promise<void> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let pending = "";
  for (;;) {
    const { done, value } = await reader.read();
    if (done) return;
    pending += decoder.decode(value, { stream: true });
    for (;;) {
      const end = pending.indexOf("\n\n");
      if (end < 0) break;
      const block = pending.slice(0, end);
      pending = pending.slice(end + 2);
      const data = block
        .split("\n")
        .filter((line) => line.startsWith("data:"))
        .map((line) => line.slice(5).trimStart())
        .join("\n");
      if (data) deliver(data);
    }
  }
}

async function openEvents(address: BridgeAddress, deliver: EventHandler) {
  const stop = new AbortController();
  const response = await fetch(`${address.url}/events`, {
    headers: { authorization: `Bearer ${address.token}` },
    signal: stop.signal,
  });
  if (response.status !== 200 || !response.body) {
    throw bridgeFailure(`The bridge answered ${response.status} to the event stream.`);
  }
  void readEvents(response.body, (data) => {
    deliver(JSON.parse(data) as ShellEvent);
  }).catch(() => {
    // The stream ended: the bridge stopped or the page is closing. Commands will say so.
  });
  return () => {
    stop.abort();
  };
}

export function bridgeClient(address: BridgeAddress): CoreClient {
  // The generated commands call Tauri's `invoke`; this routes that one function to the
  // bridge, so both transports run the same generated code with the same arguments.
  mockIPC((command, args) => invokeOverBridge(address, command, args));
  // eslint-disable-next-line @typescript-eslint/no-unused-vars -- left out on purpose
  const { events_subscribe, ...rest } = commands;
  return {
    ...rest,
    transport: "bridge",
    subscribe: fanOut((deliver) => openEvents(address, deliver)),
  };
}
