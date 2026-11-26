// Settings sections whose commands come through @/lib/core/pending: the printer on the
// local network (Settings, Printer) and where web research comes from (Settings, AI model).
// In a window whose shell does not carry them they show their rows switched off and say so. What only a person may decide, trusting the printer's certificate and agreeing
// to send searches somewhere, is asked in the shell's own native dialog: the window has a
// button that asks for that dialog, and never a switch that decides it.
import { useCallback, useEffect, useState } from "react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { RadioGroup, RadioRow } from "@/components/ui/radio-group";
import { toApiError, type ApiError } from "@/lib/core";
import {
  isNotAvailable,
  type PrinterLanSetup,
  type SearchService,
  type SearchSource,
  type SearchSourceOption,
} from "@/lib/core/pending";
import { cn } from "@/lib/utils";
import { ApiErrorCard, StatusIcon } from "@/hotend/bits";

import { usePending } from "./core-context";
import { Row } from "./sections";
import { H } from "./strings";

const CARD = "rounded-card border border-app-line bg-app-raised p-4";

function Sub({ children }: { children: string }) {
  return <h2 className="t-label pt-6 pb-2 text-app-text-2">{children}</h2>;
}

function NotAvailable({ text, testId }: { text: string; testId: string }) {
  return (
    <p className="t-ui flex gap-2 text-app-text-2" data-testid={testId}>
      <StatusIcon kind="note" className="mt-px" />
      <span className="min-w-0 flex-1">{text}</span>
    </p>
  );
}

type Loaded<T> = { state: "loading" } | { state: "unavailable" } | { state: "ready"; value: T } | { state: "failed"; error: ApiError };

/** A pending command's answer, or that the shell does not carry it yet, or its error. */
async function loadedFrom<T>(answer: Promise<T>): Promise<Loaded<T>> {
  try {
    return { state: "ready", value: await answer };
  } catch (thrown) {
    return isNotAvailable(thrown) ? { state: "unavailable" } : { state: "failed", error: toApiError(thrown) };
  }
}

/** Runs one command at a time and keeps its error. */
function useAction() {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const run = useCallback(async (work: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try {
      await work();
    } catch (thrown) {
      setError(toApiError(thrown));
    } finally {
      setBusy(false);
    }
  }, []);
  return { busy, error, run };
}

/**
 * A write-only secret: the field sends what is typed and is emptied the moment it is sent,
 * whether the save then succeeds or is refused (the refusal is shown, and the secret is
 * typed again). Nothing that was typed or stored stays in the page.
 *
 * When the key store keeps nothing (`HOTEND_SECRETS=env`), `readOnly` is the reason and
 * the way out: it is shown above the field, which is switched off, before anything is typed.
 */
function SecretField({
  label,
  placeholder,
  saveLabel,
  disabled,
  readOnly = null,
  testId,
  onSave,
}: {
  label: string;
  placeholder: string;
  saveLabel: string;
  disabled: boolean;
  readOnly?: string | null;
  testId: string;
  onSave(secret: string): Promise<boolean>;
}) {
  const [secret, setSecret] = useState("");
  const [saving, setSaving] = useState(false);
  if (readOnly !== null) {
    return (
      <div className="space-y-1.5">
        <NotAvailable text={readOnly} testId={`${testId}-read-only`} />
        <div className="flex items-center gap-2">
          <Input type="password" value="" disabled readOnly aria-label={label} placeholder={placeholder} data-testid={`${testId}-field`} />
          <Button type="button" variant="outline" disabled data-testid={`${testId}-save`}>
            {saveLabel}
          </Button>
        </div>
      </div>
    );
  }
  return (
    <form
      className="flex items-center gap-2"
      onSubmit={(event) => {
        event.preventDefault();
        const text = secret.trim();
        if (text === "" || saving || disabled) return;
        setSaving(true);
        setSecret("");
        void onSave(text)
          .catch(() => false)
          .finally(() => {
            setSaving(false);
          });
      }}
    >
      <Input
        type="password"
        autoComplete="off"
        spellCheck={false}
        value={secret}
        disabled={disabled}
        readOnly={saving}
        aria-label={label}
        placeholder={placeholder}
        data-testid={`${testId}-field`}
        onChange={(event) => {
          setSecret(event.target.value);
        }}
      />
      <Button type="submit" variant="outline" disabled={disabled || saving || secret.trim() === ""} data-testid={`${testId}-save`}>
        {saveLabel}
      </Button>
    </form>
  );
}

// ---- the printer on the local network ---------------------------------------------------

function accessCodeText(source: PrinterLanSetup["access_code"]): string {
  switch (source) {
    case "environment":
      return H.printerLan.codeEnv;
    case "secret_store":
      return H.printerLan.codeSet;
    case "missing":
      return H.printerLan.codeMissing;
    case "unreadable":
      return H.printerLan.codeUnreadable;
  }
}

/** Settings, Printer: the printer whose status Hotend reads on the local network. */
export function PrinterLanSection() {
  const pending = usePending();
  const [loaded, setLoaded] = useState<Loaded<PrinterLanSetup>>({ state: "loading" });
  const [ip, setIp] = useState("");
  const [serial, setSerial] = useState("");
  const [declined, setDeclined] = useState(false);
  const { busy, error, run } = useAction();

  const apply = useCallback((next: Loaded<PrinterLanSetup>) => {
    setLoaded(next);
    if (next.state !== "ready") return;
    setIp(next.value.address?.ip ?? "");
    setSerial(next.value.address?.serial ?? "");
  }, []);
  const show = (setup: PrinterLanSetup) => {
    apply({ state: "ready", value: setup });
  };
  const fetchSetup = useCallback(() => loadedFrom(pending.printer_lan_get()), [pending]);
  const read = async () => {
    apply(await fetchSetup());
  };

  useEffect(() => {
    let gone = false;
    void fetchSetup().then((next) => {
      if (!gone) apply(next);
    });
    return () => {
      gone = true;
    };
  }, [fetchSetup, apply]);

  const unavailable = loaded.state !== "ready";
  const setup = loaded.state === "ready" ? loaded.value : null;
  const off = unavailable || busy;
  // Where a typed code would be kept; with the environment store, nowhere.
  const codeStore = setup?.access_code_store ?? null;
  const codeReadOnly = codeStore === "environment" ? H.secretStore.readOnly(H.printerLan.codeVariable) : null;

  return (
    <>
      <Sub>{H.printerLan.title}</Sub>
      <div className={cn(CARD, "space-y-3")} data-testid="printer-lan" data-state={loaded.state}>
        <p className="t-ui max-w-[68ch] text-app-text-2">{H.printerLan.why}</p>
        {loaded.state === "unavailable" ? <NotAvailable text={H.printerLan.notAvailable} testId="printer-lan-unavailable" /> : null}
        {loaded.state === "failed" ? <ApiErrorCard error={loaded.error} /> : null}

        <form
          className="space-y-2"
          onSubmit={(event) => {
            event.preventDefault();
            void run(async () => {
              show(await pending.printer_lan_set(ip.trim(), serial.trim()));
            });
          }}
        >
          <Row label={H.printerLan.ip}>
            <Input
              value={ip}
              disabled={off}
              aria-label={H.printerLan.ip}
              placeholder={H.printerLan.ipPlaceholder}
              inputMode="decimal"
              spellCheck={false}
              data-testid="printer-lan-ip"
              onChange={(event) => {
                setIp(event.target.value);
              }}
            />
          </Row>
          <Row label={H.printerLan.serial}>
            <Input
              value={serial}
              disabled={off}
              aria-label={H.printerLan.serial}
              placeholder={H.printerLan.serialPlaceholder}
              spellCheck={false}
              data-testid="printer-lan-serial"
              onChange={(event) => {
                setSerial(event.target.value);
              }}
            />
          </Row>
          <Row label="">
            <Button type="submit" variant="outline" size="sm" disabled={off || ip.trim() === "" || serial.trim() === ""} data-testid="printer-lan-save">
              {busy ? H.printerLan.saving : H.printerLan.save}
            </Button>
          </Row>
        </form>

        <Row label={H.printerLan.accessCode} top>
          <div className="space-y-1.5">
            <p className="t-ui min-h-7 py-[5px]" data-testid="printer-lan-code">
              {setup ? accessCodeText(setup.access_code) : H.printerLan.codeMissing}
            </p>
            <SecretField
              label={H.printerLan.accessCode}
              placeholder={H.printerLan.codePlaceholder}
              saveLabel={H.printerLan.codeSave}
              disabled={off}
              readOnly={codeReadOnly}
              testId="printer-lan-code"
              onSave={async (code) => {
                let stored = false;
                await run(async () => {
                  await pending.printer_lan_key_set(code);
                  stored = true;
                  await read();
                });
                return stored;
              }}
            />
            {codeReadOnly === null ? (
              <p className="t-caption text-app-text-2" data-testid="printer-lan-code-where">
                {codeStore ? H.printerLan.codeStoredIn[codeStore] : H.printerLan.codeStored}
                {codeStore === "file" ? ` ${H.secretStore.fileNote}` : ""}
              </p>
            ) : null}
            {setup?.access_code === "secret_store" ? (
              <Button
                variant="ghost"
                size="sm"
                className="-ml-2"
                disabled={off}
                data-testid="printer-lan-code-remove"
                onClick={() => {
                  void run(async () => {
                    await pending.printer_lan_key_delete();
                    await read();
                  });
                }}
              >
                {H.printerLan.codeRemove}
              </Button>
            ) : null}
          </div>
        </Row>

        <Row label={H.printerLan.certificate} top>
          <div className="space-y-1.5">
            <p className="t-ui min-h-7 py-[5px]" data-testid="printer-lan-trust">
              {setup ? (H.printerLan.trust[setup.trust] ?? setup.trust) : H.printerLan.trust.unconfirmed}
            </p>
            {setup?.pin ? <p className="t-mono text-app-text-2">{H.printerLan.pinned(setup.pin.sha256)}</p> : null}
            <div className="flex flex-wrap gap-2">
              <Button
                variant="outline"
                size="sm"
                disabled={off || setup?.address == null || setup.trust === "ca_file"}
                data-testid="printer-lan-confirm"
                onClick={() => {
                  setDeclined(false);
                  void run(async () => {
                    const answer = await pending.printer_certificate_check();
                    show(answer.setup);
                    setDeclined(!answer.confirmed);
                  });
                }}
              >
                {H.printerLan.confirm}
              </Button>
              {setup?.pin ? (
                <Button
                  variant="ghost"
                  size="sm"
                  disabled={off}
                  data-testid="printer-lan-forget-certificate"
                  onClick={() => {
                    void run(async () => {
                      await pending.printer_certificate_forget();
                      await read();
                    });
                  }}
                >
                  {H.printerLan.forgetCertificate}
                </Button>
              ) : null}
            </div>
            <p className="t-caption max-w-[60ch] text-app-text-2">{H.printerLan.confirmNote}</p>
            {declined ? (
              <p className="t-ui text-app-text-2" role="status" data-testid="printer-lan-declined">
                {H.printerLan.notConfirmed}
              </p>
            ) : null}
          </div>
        </Row>

        {setup?.watching ? <p className="t-ui text-app-text-2">{H.printerLan.watching}</p> : null}
        {setup?.address ? (
          <div className="space-y-1 border-t border-app-line pt-3">
            <Button
              variant="outline"
              size="sm"
              disabled={off}
              data-testid="printer-lan-forget"
              onClick={() => {
                void run(async () => {
                  await pending.printer_lan_clear();
                  await read();
                });
              }}
            >
              {H.printerLan.forget}
            </Button>
            <p className="t-caption text-app-text-2">{H.printerLan.forgetNote}</p>
          </div>
        ) : loaded.state === "ready" ? (
          <p className="t-ui text-app-text-2" data-testid="printer-lan-none">
            {H.printerLan.notSet}
          </p>
        ) : null}
        {error ? <ApiErrorCard error={error} /> : null}
      </div>
    </>
  );
}

// ---- where web research comes from ------------------------------------------------------

const SOURCES: SearchSource[] = ["built_in", "keenable", "tavily", "off"];
const SERVICES: { service: SearchService; label: string; variable: string }[] = [
  { service: "keenable", label: "Keenable", variable: "KEENABLE_API_KEY" },
  { service: "tavily", label: "Tavily", variable: "TAVILY_API_KEY" },
];

/** One search service's key: whether it is stored, and a write-only field. */
function ServiceKey({
  service,
  label,
  variable,
  option,
  disabled,
  onChanged,
  run,
}: {
  service: SearchService;
  label: string;
  variable: string;
  option: SearchSourceOption | null;
  disabled: boolean;
  onChanged(): Promise<void>;
  run(work: () => Promise<void>): Promise<void>;
}) {
  const pending = usePending();
  const present = option?.key_present ?? false;
  const fromEnvironment = present && (option?.key_from_environment ?? false);
  const store = option?.key_store ?? null;
  // With the environment store nothing typed here could be kept: said before typing.
  const readOnly = store === "environment" ? H.secretStore.readOnly(variable) : null;
  const saved = fromEnvironment
    ? H.secretStore.fromEnvironment(variable)
    : store
      ? H.research.keySavedIn[store]
      : H.research.keySaved;
  return (
    <Row label={H.research.key(label)} top>
      <div className="space-y-1.5" data-testid={`research-key-${service}`}>
        <div className="flex flex-wrap items-center gap-2">
          <p className="t-ui flex min-h-7 flex-1 items-center gap-2" data-testid={`research-key-${service}-status`}>
            {present ? <StatusIcon kind="ok" /> : null}
            {present ? saved : option?.needs_key ? H.research.keyNeeded : H.research.keyMissing}
          </p>
          {present && !fromEnvironment && readOnly === null ? (
            <Button
              variant="outline"
              size="sm"
              disabled={disabled}
              data-testid={`research-key-${service}-remove`}
              onClick={() => {
                void run(async () => {
                  await pending.search_key_delete(service);
                  await onChanged();
                });
              }}
            >
              {H.research.keyRemove}
            </Button>
          ) : null}
        </div>
        <SecretField
          label={H.research.key(label)}
          placeholder={H.research.keyPlaceholder}
          saveLabel={H.research.keySave}
          disabled={disabled}
          readOnly={readOnly}
          testId={`research-key-${service}`}
          onSave={async (key) => {
            let stored = false;
            await run(async () => {
              await pending.search_key_set(service, key);
              stored = true;
              await onChanged();
            });
            return stored;
          }}
        />
        {readOnly === null ? (
          <p className="t-caption text-app-text-2" data-testid={`research-key-${service}-where`}>
            {store ? H.research.keyStoredIn[store] : H.research.keyStored}
            {store === "file" ? ` ${H.secretStore.fileNote}` : ""}
          </p>
        ) : null}
      </div>
    </Row>
  );
}

/** Settings, AI model: who runs the web searches, their keys, and the person's agreement. */
export function ResearchSection() {
  const pending = usePending();
  const [loaded, setLoaded] = useState<Loaded<SearchSourceOption[]>>({ state: "loading" });
  const [declined, setDeclined] = useState(false);
  const { busy, error, run } = useAction();

  const fetchOptions = useCallback(() => loadedFrom(pending.research_source_options()), [pending]);
  const read = async () => {
    setLoaded(await fetchOptions());
  };

  useEffect(() => {
    let gone = false;
    void fetchOptions().then((next) => {
      if (!gone) setLoaded(next);
    });
    return () => {
      gone = true;
    };
  }, [fetchOptions]);

  const options = loaded.state === "ready" ? loaded.value : [];
  const optionOf = (source: SearchSource) => options.find((option) => option.source === source) ?? null;
  const chosen = options.find((option) => option.chosen) ?? null;
  const off = loaded.state !== "ready" || busy;

  return (
    <>
      <Sub>{H.research.title}</Sub>
      <div className={cn(CARD, "space-y-3")} data-testid="research" data-state={loaded.state}>
        <p className="t-ui max-w-[68ch] text-app-text-2">{H.research.why}</p>
        {loaded.state === "unavailable" ? <NotAvailable text={H.research.notAvailable} testId="research-unavailable" /> : null}
        {loaded.state === "failed" ? <ApiErrorCard error={loaded.error} /> : null}

        <Row label={H.research.source} top>
          <RadioGroup
            aria-label={H.research.source}
            value={chosen?.source ?? null}
            disabled={off}
            className="-mx-2"
            onValueChange={(value) => {
              const source = SOURCES.find((one) => one === value);
              if (source === undefined || source === chosen?.source) return;
              setDeclined(false);
              void run(async () => {
                await pending.research_source_set(source);
                await read();
              });
            }}
          >
            {SOURCES.map((source) => {
              const option = optionOf(source);
              const usable = option?.available ?? false;
              return (
                <RadioRow key={source} value={source} disabled={off || !usable}>
                  <span className="block" data-testid={`research-source-${source}`}>
                    <span className={cn("flex flex-wrap items-center gap-2", !usable && "text-app-text-2")}>
                      {H.research.sources[source] ?? source}
                      {option?.suggested ? <Badge tone="accent">{H.research.suggested}</Badge> : null}
                    </span>
                    {option && !option.available && option.reason ? (
                      <span className="block text-app-text-2">{H.research.unavailable(option.reason)}</span>
                    ) : null}
                    {option?.host ? <span className="block text-app-text-2">{H.research.host(option.host)}</span> : null}
                    {option?.keyless && !option.key_present ? <span className="block text-app-text-2">{H.research.keyless}</span> : null}
                    {option && source !== "off" ? (
                      <span className="block text-app-text-2" data-testid={`research-consent-${source}`}>
                        {option.agreed ? H.research.agreed : option.chosen ? H.research.notAgreed : ""}
                      </span>
                    ) : null}
                  </span>
                </RadioRow>
              );
            })}
          </RadioGroup>
        </Row>

        {/* The agreement for the chosen source: asked in the shell's own dialog, never here. */}
        <Row label="" top>
          <div className="space-y-1.5">
            {chosen?.source === "off" ? (
              <p className="t-ui text-app-text-2">{H.research.offNote}</p>
            ) : chosen?.agreed ? (
              <Button
                variant="outline"
                size="sm"
                disabled={off}
                data-testid="research-turn-off"
                onClick={() => {
                  void run(async () => {
                    await pending.research_consent_withdraw();
                    await read();
                  });
                }}
              >
                {H.research.turnOff}
              </Button>
            ) : (
              <>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={off || chosen === null || !chosen.available}
                  data-testid="research-turn-on"
                  onClick={() => {
                    if (chosen === null) return;
                    setDeclined(false);
                    void run(async () => {
                      const answer = await pending.research_consent_ask(chosen.source);
                      setLoaded({ state: "ready", value: answer.options });
                      setDeclined(!answer.agreed);
                    });
                  }}
                >
                  {H.research.turnOn}
                </Button>
                <p className="t-caption max-w-[60ch] text-app-text-2">{H.research.turnOnNote}</p>
              </>
            )}
            {declined ? (
              <p className="t-ui text-app-text-2" role="status" data-testid="research-declined">
                {H.research.notAgreedNow}
              </p>
            ) : null}
          </div>
        </Row>

        {SERVICES.map(({ service, label, variable }) => (
          <ServiceKey
            key={service}
            service={service}
            label={label}
            variable={variable}
            option={optionOf(service)}
            disabled={off}
            onChanged={read}
            run={run}
          />
        ))}
        {error ? <ApiErrorCard error={error} /> : null}
      </div>
    </>
  );
}
