// The two transports are one interface: the same commands, generated from the Rust
// command layer, and one event stream shared by every subscriber.
import { clearMocks } from "@tauri-apps/api/mocks";
import { afterEach, describe, expect, it, vi } from "vitest";

import { commands, type ShellEvent } from "@/lib/core/bindings";
import { bridgeAddress, bridgeClient, invokeOverBridge, isLoopback, readEvents } from "@/lib/core/bridge";
import { fanOut, type EventHandler } from "@/lib/core/client";
import { isApiError, toApiError } from "@/lib/core/errors";
import { tauriClient } from "@/lib/core/tauri";

const ADDRESS = { url: "http://127.0.0.1:4455", token: "s3cret" };

function answer(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

afterEach(() => {
  clearMocks();
  vi.unstubAllGlobals();
});

describe("the two transports", () => {
  it("carry exactly the same commands", () => {
    vi.stubGlobal("fetch", vi.fn());
    const names = Object.keys(commands)
      .filter((name) => name !== "events_subscribe")
      .sort();
    const own = (client: object) =>
      Object.keys(client)
        .filter((name) => name !== "transport" && name !== "subscribe")
        .sort();
    expect(names.length).toBeGreaterThan(40);
    expect(own(tauriClient())).toEqual(names);
    expect(own(bridgeClient(ADDRESS))).toEqual(names);
    expect(tauriClient().transport).toBe("tauri");
    expect(bridgeClient(ADDRESS).transport).toBe("bridge");
  });

  it("offer no command that reads a provider key", () => {
    const names = Object.keys(commands).filter((name) => name.startsWith("provider_key"));
    expect(names.sort()).toEqual(["provider_key_delete", "provider_key_set", "provider_key_status"]);
  });
});

describe("the bridge transport", () => {
  it("sends a command's named arguments with the token, and gives back the data", async () => {
    const fetch = vi.fn().mockResolvedValue(answer({ ok: true, data: { project: { id: "p1" } } }));
    vi.stubGlobal("fetch", fetch);
    const client = bridgeClient(ADDRESS);

    expect(await client.project_get("p1")).toEqual({ project: { id: "p1" } });
    const [url, request] = fetch.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("http://127.0.0.1:4455/invoke/project_get");
    expect(request.method).toBe("POST");
    expect(request.headers).toMatchObject({ authorization: "Bearer s3cret" });
    expect(JSON.parse(request.body as string)).toEqual({ projectId: "p1" });
  });

  it("sends an empty object for a command without arguments", async () => {
    const fetch = vi.fn().mockResolvedValue(answer({ ok: true, data: [] }));
    vi.stubGlobal("fetch", fetch);
    await bridgeClient(ADDRESS).projects_list();
    expect((fetch.mock.calls[0] as [string, RequestInit])[1].body).toBe("{}");
  });

  it("rejects with the core's own error, as the window's transport does", async () => {
    const error = { kind: "not_found", message: "No such project.", next_step: "Open Home." };
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(answer({ ok: false, error })));
    await expect(bridgeClient(ADDRESS).project_get("nope")).rejects.toEqual(error);
  });

  it("says so when the bridge itself refuses", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(answer({}, 401)));
    const thrown: unknown = await invokeOverBridge(ADDRESS, "projects_list", {}).catch((error: unknown) => error);
    expect(isApiError(thrown)).toBe(true);
    expect(thrown).toMatchObject({ kind: "internal", message: "The bridge answered 401 to projects_list." });
  });

  it("takes its address from the page's query once, then from the tab's storage", () => {
    const stored = new Map<string, string>();
    const storage = {
      getItem: (key: string) => stored.get(key) ?? null,
      setItem: (key: string, value: string) => void stored.set(key, value),
    };
    const replaceState = vi.fn();
    const location = {
      search: "?bridge=http%3A%2F%2F127.0.0.1%3A4455%2F&token=s3cret",
      pathname: "/",
      hash: "#/hotend",
    };
    expect(bridgeAddress(location, storage, { replaceState })).toEqual(ADDRESS);
    // The token is taken out of the address bar; the route stays.
    expect(replaceState).toHaveBeenCalledWith(null, "", "/#/hotend");
    expect(bridgeAddress({ search: "", pathname: "/", hash: "" }, storage, { replaceState })).toEqual(ADDRESS);
  });

  it("talks to this computer only", () => {
    expect(isLoopback("http://127.0.0.1:4455")).toBe(true);
    for (const other of ["https://127.0.0.1:4455", "http://127.0.0.1.example.com:80", "http://example.com:4455", "http://127.0.0.1:4455/x", "http://localhost:4455"]) {
      expect(isLoopback(other), other).toBe(false);
    }
    const storage = { getItem: () => null, setItem: vi.fn() };
    const location = { search: "?bridge=http%3A%2F%2Fexample.com%3A80&token=t", pathname: "/", hash: "" };
    expect(bridgeAddress(location, storage, { replaceState: vi.fn() })).toBeNull();
    expect(storage.setItem).not.toHaveBeenCalled();
  });

  it("reads server-sent events however the bytes are cut", async () => {
    const text = 'data: {"a":1}\n\n: keep-alive\n\ndata: {"b":\ndata: 2}\n\n';
    const bytes = new TextEncoder().encode(text);
    for (const size of [1, 3, 7, bytes.length]) {
      const body = new ReadableStream<Uint8Array>({
        start(controller) {
          for (let at = 0; at < bytes.length; at += size) controller.enqueue(bytes.slice(at, at + size));
          controller.close();
        },
      });
      const seen: string[] = [];
      await readEvents(body, (data) => seen.push(data));
      expect(seen).toEqual(['{"a":1}', '{"b":\n2}']);
    }
  });

  it("delivers the bridge's events to subscribers and closes the stream after the last", async () => {
    const event: ShellEvent = { channel: "lagged", missed: 3 };
    let close: (() => void) | undefined;
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode(`data: ${JSON.stringify(event)}\n\n`));
        close = () => {
          controller.close();
        };
      },
    });
    const fetch = vi.fn().mockResolvedValue(new Response(body, { status: 200 }));
    vi.stubGlobal("fetch", fetch);
    const client = bridgeClient(ADDRESS);
    const seen: ShellEvent[] = [];
    const stop = await client.subscribe((received) => seen.push(received));
    await vi.waitFor(() => {
      expect(seen).toEqual([event]);
    });
    const [url, request] = fetch.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("http://127.0.0.1:4455/events");
    expect(request.headers).toMatchObject({ authorization: "Bearer s3cret" });
    expect(request.signal?.aborted).toBe(false);
    stop();
    await vi.waitFor(() => {
      expect(request.signal?.aborted).toBe(true);
    });
    close?.();
  });
});

describe("one stream for every subscriber", () => {
  it("opens with the first handler, closes with the last, and opens again", async () => {
    let deliver: EventHandler = () => undefined;
    const close = vi.fn();
    const open = vi.fn((handler: EventHandler) => {
      deliver = handler;
      return Promise.resolve(close);
    });
    const subscribe = fanOut(open);
    const first: ShellEvent[] = [];
    const second: ShellEvent[] = [];
    const stopFirst = await subscribe((event) => first.push(event));
    const stopSecond = await subscribe((event) => second.push(event));
    expect(open).toHaveBeenCalledTimes(1);

    deliver({ channel: "lagged", missed: 1 });
    stopFirst();
    stopFirst();
    deliver({ channel: "lagged", missed: 2 });
    expect(first).toHaveLength(1);
    expect(second).toHaveLength(2);
    expect(close).not.toHaveBeenCalled();

    stopSecond();
    await vi.waitFor(() => {
      expect(close).toHaveBeenCalledTimes(1);
    });
    await subscribe(() => undefined);
    expect(open).toHaveBeenCalledTimes(2);
  });

  it("can be tried again after the stream failed to open", async () => {
    const open = vi.fn().mockRejectedValueOnce(new Error("down")).mockResolvedValueOnce(() => undefined);
    const subscribe = fanOut(open);
    await expect(subscribe(() => undefined)).rejects.toThrow("down");
    await subscribe(() => undefined);
    expect(open).toHaveBeenCalledTimes(2);
  });
});

describe("errors", () => {
  it("recognises the core's error and wraps anything else", () => {
    expect(isApiError({ kind: "busy", message: "m", next_step: "n" })).toBe(true);
    expect(isApiError({ kind: "busy" })).toBe(false);
    expect(isApiError(null)).toBe(false);
    expect(toApiError(new Error("boom"))).toMatchObject({ kind: "internal", message: "boom" });
    expect(toApiError("text")).toMatchObject({ kind: "internal", message: "text" });
  });
});
