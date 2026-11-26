// The key field of first run and Settings (DESIGN_SPEC 4.1): write-only. A key goes to
// the core once and is never read back or shown again.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CoreContext, HostProvider } from "@/host/core-context";
import { AiSection } from "@/host/sections";

import { settled } from "./cable-box";
import { fakeClient } from "./fake-client";
import type { CoreClient, DoctorReport } from "@/lib/core";

afterEach(cleanup);

const KEY = "not-a-real-key-1234";

async function show(stored: boolean, more: Partial<CoreClient> = {}) {
  let has = stored;
  const client = fakeClient({
    ...more,
    settings_get: vi.fn().mockRejectedValue(new Error("no settings in this test")),
    provider_key_status: vi.fn(() => Promise.resolve(has)),
    provider_key_set: vi.fn(() => {
      has = true;
      return Promise.resolve(null);
    }),
    provider_key_delete: vi.fn(() => {
      has = false;
      return Promise.resolve(null);
    }),
  });
  const view = render(
    <CoreContext value={client}>
      <HostProvider>
        <AiSection value={{ kind: "anthropic", model: "a-model" }} onChange={() => undefined} scripted={false} />
      </HostProvider>
    </CoreContext>,
  );
  await act(async () => {
    await settled();
  });
  return { client, view };
}

describe("the key field", () => {
  it("sends the key to the core once and never shows it again", async () => {
    const { client, view } = await show(false);
    const field = screen.getByTestId<HTMLInputElement>("key-field");
    expect(field.type).toBe("password");
    expect((screen.getByTestId("key-save") as HTMLButtonElement).disabled).toBe(true);

    fireEvent.change(field, { target: { value: `  ${KEY} ` } });
    await act(async () => {
      fireEvent.click(screen.getByTestId("key-save"));
      await settled();
    });

    expect(client.provider_key_set).toHaveBeenCalledTimes(1);
    expect(client.provider_key_set).toHaveBeenCalledWith("anthropic", KEY);
    // The saved state has no field, and the key is nowhere in the page.
    expect(screen.getByTestId("key-saved")).toBeTruthy();
    expect(screen.queryByTestId("key-field")).toBeNull();
    expect(view.container.innerHTML).not.toContain(KEY);
  });

  it("shows a stored key only as stored, and replaces it through an empty field", async () => {
    const { view } = await show(true);
    expect(screen.getByTestId("key-saved").textContent).toContain("Key saved");
    expect(screen.queryByTestId("key-field")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Replace" }));
    expect(screen.getByTestId<HTMLInputElement>("key-field").value).toBe("");
    expect(view.container.querySelectorAll("input[type=password]")).toHaveLength(1);
  });

  it("removes a stored key and asks for one again", async () => {
    const { client } = await show(true);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Remove" }));
      await settled();
    });
    expect(client.provider_key_delete).toHaveBeenCalledWith("anthropic");
    expect(screen.getByTestId<HTMLInputElement>("key-field").value).toBe("");
  });

  it("never shows the field on a computer with no keychain, also while the health report is read", async () => {
    let answer: (report: DoctorReport) => void = () => undefined;
    const report: DoctorReport = {
      ok: true,
      hotend: "0.1.0",
      debug_build: true,
      checks: [{ id: "secrets.keyring", label: "Keychain", status: "warn", required: false, detail: "No keychain on this computer.", fix: null }],
      sandbox: null,
      slicers: [],
      keys_from_environment: [],
    };
    await show(false, { doctor_report: vi.fn(() => new Promise<DoctorReport>((resolve) => (answer = resolve))) });
    // The report is on its way: a placeholder, not a field that would be taken away again.
    expect(screen.getByTestId("key-waiting")).toBeTruthy();
    expect(screen.queryByTestId("key-field")).toBeNull();
    await act(async () => {
      answer(report);
      await settled();
    });
    expect(screen.getByTestId("key-no-keychain").textContent).toContain("HOTEND_SECRETS=env");
    expect(screen.getByTestId("key-no-keychain").textContent).toContain("HOTEND_SECRETS=file");
    expect(screen.queryByTestId("key-field")).toBeNull();
  });

  const fileReport = (fromEnvironment: DoctorReport["keys_from_environment"]): DoctorReport => ({
    ok: true,
    hotend: "0.1.0",
    debug_build: true,
    checks: [
      { id: "secrets.keyring", label: "Keychain", status: "info", required: false, detail: "not available; not used: HOTEND_SECRETS=file", fix: null },
      { id: "secrets.file", label: "key store: file", status: "ok", required: false, detail: "/tmp/x/secrets.json", fix: null },
    ],
    sandbox: null,
    slicers: [],
    keys_from_environment: fromEnvironment,
  });

  it("names the variable, with no Replace or Remove, when the provider's key comes from the environment", async () => {
    await show(true, { doctor_report: vi.fn(() => Promise.resolve(fileReport(["anthropic"]))) });
    expect(screen.getByTestId("key-env").textContent).toContain("ANTHROPIC_API_KEY");
    expect(screen.getByTestId("key-env").textContent).toContain("instead of any key saved in Hotend's secrets file");
    expect(screen.queryByTestId("key-saved")).toBeNull();
    expect(screen.queryByRole("button", { name: "Replace" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Remove" })).toBeNull();
    expect(screen.queryByTestId("key-field")).toBeNull();
  });

  it("shows a key saved in the file as saved when another provider's key is in the environment", async () => {
    await show(true, { doctor_report: vi.fn(() => Promise.resolve(fileReport(["openai"]))) });
    expect(screen.getByTestId("key-saved").textContent).toContain("secrets file");
    expect(screen.getByRole("button", { name: "Remove" })).toBeTruthy();
  });

  it("shows the field without a keychain when Hotend keeps keys in its secrets file", async () => {
    const { client } = await show(false, { doctor_report: vi.fn(() => Promise.resolve(fileReport([]))) });
    expect(screen.queryByTestId("key-no-keychain")).toBeNull();
    const field = screen.getByTestId<HTMLInputElement>("key-field");
    fireEvent.change(field, { target: { value: KEY } });
    await act(async () => {
      fireEvent.click(screen.getByTestId("key-save"));
      await settled();
    });
    expect(client.provider_key_set).toHaveBeenCalledWith("anthropic", KEY);
    expect(screen.getByTestId("key-saved").textContent).toContain("secrets file");
  });
});
