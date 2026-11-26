// Settings sections for the printer on the local network and where web research comes
// from, against stand-in answers and through the shell's own commands. Secrets are write-only; trusting a certificate and agreeing to
// research are asked in the shell's own dialog, never decided by a control of the page.
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CoreContext, PendingContext } from "@/host/core-context";
import { PrinterLanSection, ResearchSection } from "@/host/pending-sections";
import { H } from "@/host/strings";
import { NOT_AVAILABLE, type PendingCommands, type PrinterLanSetup, type SearchSource, type SearchSourceOption } from "@/lib/core/pending";

import { settled } from "./cable-box";
import { fakeClient } from "./fake-client";
import { standInPending } from "./fake-pending";

afterEach(cleanup);

async function show(pending: PendingCommands | null, children: React.ReactNode) {
  const view = render(
    <CoreContext value={fakeClient({})}>
      <PendingContext value={pending}>{children}</PendingContext>
    </CoreContext>,
  );
  await act(async () => {
    await settled();
  });
  return view;
}

async function click(element: HTMLElement) {
  await act(async () => {
    fireEvent.click(element);
    await settled();
  });
}

const button = (testId: string) => screen.getByTestId<HTMLButtonElement>(testId);

const CODE = "12345678";
const KEY = "tvly-not-a-real-key";

describe("Settings, Printer on your network", () => {
  const lan = (change: Partial<PrinterLanSetup> = {}): PrinterLanSetup => ({
    address: null,
    trust: "unconfirmed",
    ca_file: null,
    pin: null,
    access_code: "missing",
    access_code_store: "keychain",
    watching: false,
    access_code_present: change.access_code === "environment" || change.access_code === "secret_store",
    ...change,
  });

  it("calls the shell's own printer commands through the client", async () => {
    // No stand-in: the client's commands, as the window has them.
    const keySet = vi.fn(() => Promise.resolve(null));
    const check = vi.fn(() => Promise.resolve({ confirmed: true, setup: lan({ address: { ip: "192.168.1.40", serial: "S1234567" }, trust: "pinned" }) }));
    const client = {
      printer_lan_get: vi.fn(() => Promise.resolve(lan({ address: { ip: "192.168.1.40", serial: "S1234567" } }))),
      printer_lan_key_set: keySet,
      printer_certificate_check: check,
    };
    render(
      <CoreContext value={fakeClient(client)}>
        <PendingContext value={null}>
          <PrinterLanSection />
        </PendingContext>
      </CoreContext>,
    );
    await act(async () => {
      await settled();
    });
    expect(client.printer_lan_get).toHaveBeenCalled();
    fireEvent.change(screen.getByTestId("printer-lan-code-field"), { target: { value: CODE } });
    await click(button("printer-lan-code-save"));
    expect(keySet).toHaveBeenCalledWith(CODE);
    await click(button("printer-lan-confirm"));
    // The window sends nothing with the check: the shell takes the fingerprint itself.
    expect(check).toHaveBeenCalledWith();
    expect(screen.getByTestId("printer-lan-trust").textContent).toBe(H.printerLan.trust.pinned);
  });

  it("shows its rows switched off in a window that cannot set it up", async () => {
    await show(standInPending({ printer_lan_get: vi.fn(() => Promise.reject(NOT_AVAILABLE)) }), <PrinterLanSection />);
    expect(screen.getByTestId("printer-lan").getAttribute("data-state")).toBe("unavailable");
    expect(screen.getByTestId("printer-lan-unavailable").textContent).toBe(H.printerLan.notAvailable);
    for (const id of ["printer-lan-ip", "printer-lan-serial", "printer-lan-code-field"]) {
      expect(screen.getByTestId<HTMLInputElement>(id).disabled).toBe(true);
    }
    for (const id of ["printer-lan-save", "printer-lan-code-save", "printer-lan-confirm"]) expect(button(id).disabled).toBe(true);
  });

  it("saves the address and serial", async () => {
    const set = vi.fn((ip: string, serial: string) => Promise.resolve(lan({ address: { ip, serial: serial.toUpperCase() } })));
    await show(standInPending({ printer_lan_get: vi.fn(() => Promise.resolve(lan())), printer_lan_set: set }), <PrinterLanSection />);
    expect(screen.getByTestId("printer-lan-none").textContent).toBe(H.printerLan.notSet);
    expect(button("printer-lan-confirm").disabled).toBe(true);
    fireEvent.change(screen.getByTestId("printer-lan-ip"), { target: { value: " 192.168.1.40 " } });
    fireEvent.change(screen.getByTestId("printer-lan-serial"), { target: { value: "01p00a123456789" } });
    await click(button("printer-lan-save"));
    expect(set).toHaveBeenCalledWith("192.168.1.40", "01p00a123456789");
    expect(screen.getByTestId<HTMLInputElement>("printer-lan-serial").value).toBe("01P00A123456789");
    expect(button("printer-lan-confirm").disabled).toBe(false);
    expect(button("printer-lan-forget")).toBeTruthy();
  });

  it("sends the access code once, empties the field and shows only that a code is set", async () => {
    let source: PrinterLanSetup["access_code"] = "missing";
    const keySet = vi.fn(() => {
      source = "secret_store";
      return Promise.resolve(null);
    });
    const view = await show(
      standInPending({ printer_lan_get: vi.fn(() => Promise.resolve(lan({ access_code: source }))), printer_lan_key_set: keySet }),
      <PrinterLanSection />,
    );
    expect(screen.getByTestId("printer-lan-code").textContent).toBe(H.printerLan.codeMissing);
    const field = screen.getByTestId<HTMLInputElement>("printer-lan-code-field");
    expect(field.type).toBe("password");
    fireEvent.change(field, { target: { value: ` ${CODE} ` } });
    await click(button("printer-lan-code-save"));
    expect(keySet).toHaveBeenCalledWith(CODE);
    expect(field.value).toBe("");
    expect(screen.getByTestId("printer-lan-code").textContent).toBe(H.printerLan.codeSet);
    expect(view.container.innerHTML).not.toContain(CODE);
    expect(button("printer-lan-code-remove")).toBeTruthy();
  });

  it("empties the access code field even when saving the code is refused", async () => {
    const keySet = vi.fn(() => Promise.reject(new Error("the key store is locked")));
    const view = await show(
      standInPending({ printer_lan_get: vi.fn(() => Promise.resolve(lan())), printer_lan_key_set: keySet }),
      <PrinterLanSection />,
    );
    const field = screen.getByTestId<HTMLInputElement>("printer-lan-code-field");
    fireEvent.change(field, { target: { value: CODE } });
    await click(button("printer-lan-code-save"));
    expect(keySet).toHaveBeenCalledWith(CODE);
    expect(field.value).toBe("");
    expect(screen.getByTestId("printer-lan-code").textContent).toBe(H.printerLan.codeMissing);
    expect(view.container.innerHTML).not.toContain(CODE);
  });

  it("says before anything is typed that a read-only key store keeps no code, and the way out", async () => {
    const keySet = vi.fn(() => Promise.resolve(null));
    await show(
      standInPending({ printer_lan_get: vi.fn(() => Promise.resolve(lan({ access_code_store: "environment" }))), printer_lan_key_set: keySet }),
      <PrinterLanSection />,
    );
    const field = screen.getByTestId<HTMLInputElement>("printer-lan-code-field");
    expect(field.disabled).toBe(true);
    expect(button("printer-lan-code-save").disabled).toBe(true);
    const note = screen.getByTestId("printer-lan-code-read-only").textContent;
    expect(note).toBe(H.secretStore.readOnly("BAMBU_LAN_ACCESS_CODE"));
    expect(note).toContain("HOTEND_SECRETS=file");
    expect(note).toContain("BAMBU_LAN_ACCESS_CODE=");
    expect(screen.queryByTestId("printer-lan-code-where")).toBeNull();
    // There is no form to send: nothing typed could leave the page.
    expect(field.closest("form")).toBeNull();
    expect(keySet).not.toHaveBeenCalled();
    // The address and serial are not secrets and can still be set.
    expect(screen.getByTestId<HTMLInputElement>("printer-lan-ip").disabled).toBe(false);
  });

  it("takes a code with the file store and says where it is kept", async () => {
    const keySet = vi.fn(() => Promise.resolve(null));
    await show(
      standInPending({ printer_lan_get: vi.fn(() => Promise.resolve(lan({ access_code_store: "file" }))), printer_lan_key_set: keySet }),
      <PrinterLanSection />,
    );
    expect(screen.queryByTestId("printer-lan-code-read-only")).toBeNull();
    const field = screen.getByTestId<HTMLInputElement>("printer-lan-code-field");
    expect(field.disabled).toBe(false);
    expect(screen.getByTestId("printer-lan-code-where").textContent).toBe(`${H.printerLan.codeStoredIn.file} ${H.secretStore.fileNote}`);
    fireEvent.change(field, { target: { value: CODE } });
    await click(button("printer-lan-code-save"));
    expect(keySet).toHaveBeenCalledWith(CODE);
    expect(field.value).toBe("");
  });

  it("asks the shell's own dialog to confirm the certificate and never sends a fingerprint", async () => {
    const check = vi.fn(() => Promise.resolve({ confirmed: false, setup: lan({ address: { ip: "192.168.1.40", serial: "S1234567" } }) }));
    await show(
      standInPending({
        printer_lan_get: vi.fn(() => Promise.resolve(lan({ address: { ip: "192.168.1.40", serial: "S1234567" } }))),
        printer_certificate_check: check,
      }),
      <PrinterLanSection />,
    );
    expect(button("printer-lan-confirm").textContent).toBe("Confirm printer…");
    await click(button("printer-lan-confirm"));
    expect(check).toHaveBeenCalledWith();
    expect(screen.getByTestId("printer-lan-declined").textContent).toBe(H.printerLan.notConfirmed);
    // Nothing on the page can trust a certificate: no switch, no box to tick.
    expect(screen.queryAllByRole("checkbox")).toHaveLength(0);
    expect(screen.queryAllByRole("switch")).toHaveLength(0);
  });

  it("shows a confirmed certificate, and forgets it or the printer when asked", async () => {
    let state = lan({
      address: { ip: "192.168.1.40", serial: "S1234567" },
      trust: "pinned",
      pin: { ip: "192.168.1.40", serial: "S1234567", sha256: "0123456789abcdef".repeat(4), subject: "CN=S1234567", confirmed_at_ms: 1 },
    });
    const forgetPin = vi.fn(() => {
      state = { ...state, trust: "unconfirmed", pin: null };
      return Promise.resolve(null);
    });
    const clear = vi.fn(() => {
      state = { ...state, address: null };
      return Promise.resolve(null);
    });
    await show(
      standInPending({ printer_lan_get: vi.fn(() => Promise.resolve(state)), printer_certificate_forget: forgetPin, printer_lan_clear: clear }),
      <PrinterLanSection />,
    );
    expect(screen.getByTestId("printer-lan-trust").textContent).toBe(H.printerLan.trust.pinned);
    expect(screen.getByTestId("printer-lan").textContent).toContain("Fingerprint 0123456789abcdef…");
    await click(button("printer-lan-forget-certificate"));
    expect(forgetPin).toHaveBeenCalledTimes(1);
    expect(screen.getByTestId("printer-lan-trust").textContent).toBe(H.printerLan.trust.unconfirmed);
    await click(button("printer-lan-forget"));
    expect(clear).toHaveBeenCalledTimes(1);
    expect(screen.getByTestId("printer-lan-none")).toBeTruthy();
  });
});

describe("Settings, Web research", () => {
  const option = (source: SearchSource, change: Partial<SearchSourceOption> = {}): SearchSourceOption => ({
    source,
    available: true,
    reason: "",
    host: source === "keenable" ? "api.keenable.ai" : source === "tavily" ? "api.tavily.com" : "",
    needs_key: source === "tavily",
    key_present: false,
    key_from_environment: false,
    key_store: "keychain",
    keyless: source === "keenable",
    suggested: false,
    chosen: false,
    agreed: false,
    ...change,
  });

  it("calls the shell's own research commands through the client", async () => {
    // No stand-in: the client's commands, as the window has them.
    const options = [option("built_in"), option("keenable", { chosen: true }), option("tavily"), option("off")];
    const ask = vi.fn(() => Promise.resolve({ agreed: true, options: options.map((one) => (one.chosen ? { ...one, agreed: true } : one)) }));
    const keySet = vi.fn(() => Promise.resolve(null));
    const client = {
      research_source_options: vi.fn(() => Promise.resolve(options)),
      research_consent_ask: ask,
      search_key_set: keySet,
    };
    render(
      <CoreContext value={fakeClient(client)}>
        <PendingContext value={null}>
          <ResearchSection />
        </PendingContext>
      </CoreContext>,
    );
    await act(async () => {
      await settled();
    });
    expect(client.research_source_options).toHaveBeenCalled();
    await click(button("research-turn-on"));
    // The window names the source only; the shell's dialog decides.
    expect(ask).toHaveBeenCalledWith("keenable");
    expect(screen.getByTestId("research-consent-keenable").textContent).toBe(H.research.agreed);
    fireEvent.change(screen.getByTestId("research-key-tavily-field"), { target: { value: KEY } });
    await click(button("research-key-tavily-save"));
    expect(keySet).toHaveBeenCalledWith("tavily", KEY);
  });

  it("shows the choices switched off in a window that cannot change them", async () => {
    await show(standInPending({ research_source_options: vi.fn(() => Promise.reject(NOT_AVAILABLE)) }), <ResearchSection />);
    expect(screen.getByTestId("research").getAttribute("data-state")).toBe("unavailable");
    expect(screen.getByTestId("research-unavailable").textContent).toBe(H.research.notAvailable);
    const radios = screen.getAllByRole("radio");
    expect(radios).toHaveLength(4);
    for (const radio of radios) expect(radio.getAttribute("aria-disabled") ?? radio.getAttribute("data-disabled")).not.toBeNull();
    expect(button("research-turn-on").disabled).toBe(true);
    expect(screen.getByTestId<HTMLInputElement>("research-key-tavily-field").disabled).toBe(true);
  });

  it("lists every source with where its queries go, why one cannot be chosen, and the agreement", async () => {
    const options = [
      option("built_in", { available: false, reason: "OpenAI has no built-in search in Hotend" }),
      option("keenable", { suggested: true, chosen: true }),
      option("tavily", { agreed: false }),
      option("off"),
    ];
    await show(standInPending({ research_source_options: vi.fn(() => Promise.resolve(options)) }), <ResearchSection />);
    expect(screen.getByTestId("research-source-built_in").textContent).toContain("Not available: OpenAI has no built-in search in Hotend");
    expect(screen.getByTestId("research-source-keenable").textContent).toContain("Queries go to api.keenable.ai.");
    expect(screen.getByTestId("research-source-keenable").textContent).toContain(H.research.suggested);
    expect(screen.getByTestId("research-source-keenable").textContent).toContain(H.research.keyless);
    expect(screen.getByTestId("research-consent-keenable").textContent).toBe(H.research.notAgreed);
    expect(screen.getByTestId("research-key-tavily-status").textContent).toBe(H.research.keyNeeded);
    expect(screen.getByTestId("research-key-keenable-status").textContent).toBe(H.research.keyMissing);
  });

  it("turns research on only through the shell's own dialog, for the chosen source", async () => {
    const ask = vi.fn((source: SearchSource) =>
      Promise.resolve({ agreed: false, options: [option("built_in"), option("keenable", { chosen: source === "keenable" }), option("tavily"), option("off")] }),
    );
    await show(
      standInPending({
        research_source_options: vi.fn(() => Promise.resolve([option("built_in"), option("keenable", { chosen: true }), option("tavily"), option("off")])),
        research_consent_ask: ask,
      }),
      <ResearchSection />,
    );
    // Nothing on the page agrees by itself: no switch, no box to tick.
    const section = screen.getByTestId("research");
    expect(within(section).queryAllByRole("checkbox")).toHaveLength(0);
    expect(within(section).queryAllByRole("switch")).toHaveLength(0);
    expect(button("research-turn-on").textContent).toBe("Turn on…");
    await click(button("research-turn-on"));
    expect(ask).toHaveBeenCalledWith("keenable");
    expect(screen.getByTestId("research-declined").textContent).toBe(H.research.notAgreedNow);
  });

  it("withdraws an agreement with one button", async () => {
    let agreed = true;
    const withdraw = vi.fn(() => {
      agreed = false;
      return Promise.resolve(null);
    });
    await show(
      standInPending({
        research_source_options: vi.fn(() => Promise.resolve([option("built_in", { chosen: true, agreed }), option("keenable"), option("tavily"), option("off")])),
        research_consent_withdraw: withdraw,
      }),
      <ResearchSection />,
    );
    expect(screen.getByTestId("research-consent-built_in").textContent).toBe(H.research.agreed);
    await click(button("research-turn-off"));
    expect(withdraw).toHaveBeenCalledTimes(1);
    expect(button("research-turn-on")).toBeTruthy();
  });

  it("chooses another source, which then waits for its own agreement", async () => {
    let chosen: SearchSource = "built_in";
    const set = vi.fn((source: SearchSource) => {
      chosen = source;
      return Promise.resolve({} as never);
    });
    const options = () => (["built_in", "keenable", "tavily", "off"] as SearchSource[]).map((one) => option(one, { chosen: one === chosen, agreed: one === "built_in" }));
    await show(standInPending({ research_source_options: vi.fn(() => Promise.resolve(options())), research_source_set: set }), <ResearchSection />);
    const tavily = screen.getAllByRole("radio").find((radio) => radio.closest("label")?.contains(screen.getByTestId("research-source-tavily")));
    await click(tavily as HTMLElement);
    expect(set).toHaveBeenCalledWith("tavily");
    expect(screen.getByTestId("research-consent-tavily").textContent).toBe(H.research.notAgreed);
    expect(button("research-turn-on").disabled).toBe(false);
  });

  it("sends a search key once and empties the field", async () => {
    let present = false;
    const keySet = vi.fn(() => {
      present = true;
      return Promise.resolve(null);
    });
    const view = await show(
      standInPending({
        research_source_options: vi.fn(() => Promise.resolve([option("built_in", { chosen: true }), option("keenable"), option("tavily", { key_present: present }), option("off")])),
        search_key_set: keySet,
      }),
      <ResearchSection />,
    );
    const field = screen.getByTestId<HTMLInputElement>("research-key-tavily-field");
    expect(field.type).toBe("password");
    fireEvent.change(field, { target: { value: KEY } });
    await click(button("research-key-tavily-save"));
    expect(keySet).toHaveBeenCalledWith("tavily", KEY);
    expect(field.value).toBe("");
    expect(screen.getByTestId("research-key-tavily-status").textContent).toBe(H.research.keySaved);
    expect(view.container.innerHTML).not.toContain(KEY);
  });

  it("says before anything is typed that a read-only key store keeps no key, and the way out", async () => {
    const keySet = vi.fn(() => Promise.resolve(null));
    const all = (change: Partial<SearchSourceOption>) =>
      (["built_in", "keenable", "tavily", "off"] as SearchSource[]).map((one) => option(one, { chosen: one === "built_in", ...change }));
    await show(
      standInPending({ research_source_options: vi.fn(() => Promise.resolve(all({ key_store: "environment" }))), search_key_set: keySet }),
      <ResearchSection />,
    );
    for (const [service, variable] of [
      ["keenable", "KEENABLE_API_KEY"],
      ["tavily", "TAVILY_API_KEY"],
    ] as const) {
      expect(screen.getByTestId<HTMLInputElement>(`research-key-${service}-field`).disabled).toBe(true);
      expect(button(`research-key-${service}-save`).disabled).toBe(true);
      const note = screen.getByTestId(`research-key-${service}-read-only`).textContent;
      expect(note).toBe(H.secretStore.readOnly(variable));
      expect(note).toContain("HOTEND_SECRETS=file");
      expect(screen.queryByTestId(`research-key-${service}-where`)).toBeNull();
    }
    expect(keySet).not.toHaveBeenCalled();
    // Choosing a source is not a secret and stays possible.
    expect(button("research-turn-on").disabled).toBe(false);
  });

  it("takes a key with the file store, says where it is kept, and names a key from the environment", async () => {
    let present = false;
    const keySet = vi.fn(() => {
      present = true;
      return Promise.resolve(null);
    });
    await show(
      standInPending({
        research_source_options: vi.fn(() =>
          Promise.resolve([
            option("built_in", { chosen: true, key_store: "file" }),
            option("keenable", { key_store: "file", key_present: true, key_from_environment: true }),
            option("tavily", { key_store: "file", key_present: present }),
            option("off", { key_store: "file" }),
          ]),
        ),
        search_key_set: keySet,
      }),
      <ResearchSection />,
    );
    expect(screen.queryByTestId("research-key-tavily-read-only")).toBeNull();
    expect(screen.getByTestId("research-key-tavily-where").textContent).toBe(`${H.research.keyStoredIn.file} ${H.secretStore.fileNote}`);
    // A key in the environment is used instead of a saved one, and is not removed from here.
    expect(screen.getByTestId("research-key-keenable-status").textContent).toBe(H.secretStore.fromEnvironment("KEENABLE_API_KEY"));
    expect(screen.queryByTestId("research-key-keenable-remove")).toBeNull();
    const field = screen.getByTestId<HTMLInputElement>("research-key-tavily-field");
    expect(field.disabled).toBe(false);
    fireEvent.change(field, { target: { value: KEY } });
    await click(button("research-key-tavily-save"));
    expect(keySet).toHaveBeenCalledWith("tavily", KEY);
    expect(field.value).toBe("");
    expect(screen.getByTestId("research-key-tavily-status").textContent).toBe(H.research.keySavedIn.file);
    expect(button("research-key-tavily-remove")).toBeTruthy();
  });

  it("empties the key field even when saving the key is refused", async () => {
    const keySet = vi.fn(() => Promise.reject(new Error("the key store is locked")));
    const view = await show(
      standInPending({
        research_source_options: vi.fn(() => Promise.resolve([option("built_in", { chosen: true }), option("keenable"), option("tavily"), option("off")])),
        search_key_set: keySet,
      }),
      <ResearchSection />,
    );
    const field = screen.getByTestId<HTMLInputElement>("research-key-tavily-field");
    fireEvent.change(field, { target: { value: KEY } });
    await click(button("research-key-tavily-save"));
    expect(keySet).toHaveBeenCalledWith("tavily", KEY);
    expect(field.value).toBe("");
    expect(view.container.innerHTML).not.toContain(KEY);
  });
});
