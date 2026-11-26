// The sections Setup and Settings share (DESIGN_SPEC 4.1, 4.8): the AI, the printer and
// material, and the health of this computer.
import { ChevronDown, ExternalLink } from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";
import { Link } from "react-router";

import { Button } from "@/components/ui/button";
import { Collapsible, CollapsiblePanel, CollapsibleTrigger } from "@/components/ui/collapsible";
import { Input } from "@/components/ui/input";
import { Menu, MenuContent, MenuItem, MenuTrigger } from "@/components/ui/menu";
import { Segmented } from "@/components/ui/segmented";
import { Select } from "@/components/ui/select";
import { Spinner } from "@/components/ui/spinner";
import type { DoctorReport, Effort, LinkId, ProviderKind, ProviderSettings } from "@/lib/core";
import { cn } from "@/lib/utils";
import { ApiErrorCard, SimTag, StatusIcon, type StatusKind } from "@/hotend/bits";

import { useCore, useHost, useHostStore } from "./core-context";
import providerData from "./data/providers.json";
import { healthRows, keysInFile, noKeychain, type HealthStatus } from "./health";
import { H } from "./strings";

const KINDS: ProviderKind[] = ["anthropic", "openai", "google", "openrouter", "ollama"];
const PROVIDERS = providerData.providers as Record<ProviderKind, { models: string[]; link: string }>;
const EFFORT_MODELS = providerData.anthropic_effort as Record<string, string[]>;
const EFFORTS: Effort[] = ["low", "medium", "high", "xhigh", "max"];

/** What the settings say about the effort level for this provider and model: whether the
 * core sends it (the same table as the core's egress). */
export function effortNote(provider: ProviderSettings | null, effort: Effort): string {
  if (provider === null || provider.kind !== "anthropic") return H.ai.thinkingNotForProvider;
  const levels = EFFORT_MODELS[provider.model.trim()];
  if (levels === undefined) return H.ai.thinkingNotForModel;
  if (!levels.includes(effort)) return H.ai.thinkingLevelNotForModel;
  return H.ai.thinkingSent;
}

/** The effort level, next to the model. */
export function EffortRow({
  provider,
  value,
  onChange,
}: {
  provider: ProviderSettings | null;
  value: Effort;
  onChange(next: Effort): void;
}) {
  return (
    <>
      <Row label={H.ai.thinking}>
        <Select
          label={H.ai.thinking}
          value={value}
          className="w-[240px]"
          options={EFFORTS.map((level) => ({ value: level, label: H.ai.thinkingLevels[level] }))}
          onChange={(level) => {
            const next = EFFORTS.find((entry) => entry === level);
            if (next !== undefined && next !== value) onChange(next);
          }}
        />
      </Row>
      <p className="t-caption max-w-[68ch] text-app-text-2" data-testid="effort-note">
        {effortNote(provider, value)}
      </p>
    </>
  );
}

/** The first suggestion of a provider, or nothing for Ollama. */
export function firstModel(kind: ProviderKind): string {
  return PROVIDERS[kind].models[0] ?? "";
}

/** A row of a settings form: a label column of 96 px, then the control. */
export function Row({ label, children, top = false }: { label: ReactNode; children: ReactNode; top?: boolean }) {
  return (
    <div className={cn("grid grid-cols-[96px_minmax(0,1fr)] gap-x-4", top ? "items-start" : "items-center")}>
      <div className={cn("t-ui text-app-text-2", top && "pt-[5px]")}>{label}</div>
      <div className="min-w-0">{children}</div>
    </div>
  );
}

/** The 11 px uppercase label over a card of Setup. */
export function Card({ title, aside, children, className }: { title?: string; aside?: ReactNode; children: ReactNode; className?: string }) {
  return (
    <section className={className}>
      {title ? (
        <div className="flex items-end justify-between gap-3 pb-2">
          <h2 className="t-label text-app-text-2">{title}</h2>
          {aside}
        </div>
      ) : null}
      <div className="space-y-3 rounded-card border border-app-line bg-app-raised p-4">{children}</div>
    </section>
  );
}

function KeyRow({ kind, onStored }: { kind: ProviderKind; onStored?: (stored: boolean | null) => void }) {
  const client = useCore();
  const store = useHostStore();
  const doctor = useHost((state) => state.doctor);
  const checking = useHost((state) => state.doctorBusy);
  const hostError = useHost((state) => state.error);
  const [stored, setStored] = useState<boolean | null>(null);
  const [key, setKey] = useState("");
  const [saving, setSaving] = useState(false);
  const [replacing, setReplacing] = useState(false);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    let left = false;
    client.provider_key_status(kind).then(
      (found) => {
        if (!left) setStored(found);
      },
      () => {
        if (!left) setStored(false);
      },
    );
    return () => {
      left = true;
    };
  }, [client, kind]);

  useEffect(() => {
    onStored?.(stored);
  }, [stored, onStored]);

  // Whether this computer has a keychain is in the health report. The row asks for it
  // itself (Settings can open before anything else did) and shows no field until the
  // answer, or the failure to get one, is in.
  const [asked, setAsked] = useState(false);
  useEffect(() => {
    let left = false;
    void store
      .getState()
      .quickCheck()
      .finally(() => {
        if (!left) setAsked(true);
      });
    return () => {
      left = true;
    };
  }, [store]);

  const keychainMissing = noKeychain(doctor);
  const inFile = keysInFile(doctor);
  const variable = H.keyVariables[kind];
  const lines = `HOTEND_SECRETS=env\n${variable}=<your key>`;

  const save = async () => {
    if (key.trim() === "" || saving) return;
    setSaving(true);
    const saved = await store.getState().saveKey(kind, key.trim());
    setSaving(false);
    if (saved) {
      // The key leaves the page the moment it is stored.
      setKey("");
      setReplacing(false);
      setStored(true);
    }
  };

  if (stored === null || (doctor === null && (!asked || checking))) return <div className="h-7" data-testid="key-waiting" />;

  // A key in the provider's variable is used instead of a stored one: no Replace or Remove,
  // which would change only the stored copy.
  const fromEnvironment = doctor?.keys_from_environment.includes(kind) ?? false;
  if (stored && (keychainMissing || fromEnvironment)) {
    return (
      <p className="t-ui py-[5px]" data-testid="key-env">
        {inFile ? H.ai.usingEnvOverFile(variable) : H.ai.usingEnv(variable)}
      </p>
    );
  }
  if (!stored && keychainMissing) {
    return (
      <div className="space-y-2" data-testid="key-no-keychain">
        <p className="t-ui flex gap-2">
          <StatusIcon kind="note" className="mt-px" />
          <span>{H.ai.noKeychain}</span>
        </p>
        <pre className="t-mono rounded-control bg-app-subtle px-3 py-2 text-app-text">{lines}</pre>
        <Button
          variant="outline"
          size="sm"
          onClick={() => {
            void navigator.clipboard.writeText(lines).then(
              () => {
                setCopied(true);
                setTimeout(() => {
                  setCopied(false);
                }, 2000);
              },
              () => undefined,
            );
          }}
        >
          {copied ? H.ai.copied : H.ai.copyLines}
        </Button>
        <p className="t-caption max-w-[60ch] text-app-text-2">{H.ai.orFile}</p>
      </div>
    );
  }
  if (stored && !replacing) {
    return (
      <div className="space-y-1" data-testid="key-saved">
        <div className="flex flex-wrap items-center gap-2">
          <p className="t-ui flex min-h-7 flex-1 items-center gap-2">
            <StatusIcon kind="ok" />
            {inFile ? H.ai.savedFile : H.ai.saved}
          </p>
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              setReplacing(true);
            }}
          >
            {H.ai.replace}
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              void store
                .getState()
                .removeKey(kind)
                .then(() => client.provider_key_status(kind))
                .then(setStored, () => undefined);
            }}
          >
            {H.ai.remove}
          </Button>
        </div>
        <p className="t-caption text-app-text-2">{H.ai.checked}</p>
      </div>
    );
  }
  return (
    <div className="space-y-1.5">
      <form
        className="flex items-center gap-2"
        onSubmit={(event) => {
          event.preventDefault();
          void save();
        }}
      >
        <Input
          type="password"
          autoComplete="off"
          spellCheck={false}
          value={key}
          readOnly={saving}
          aria-label={H.ai.key}
          placeholder={H.ai.keyPlaceholder}
          data-testid="key-field"
          onChange={(event) => {
            setKey(event.target.value);
          }}
        />
        <Button type="submit" variant="outline" disabled={key.trim() === "" || saving} data-testid="key-save">
          {saving ? H.ai.saving : H.ai.save}
        </Button>
        {replacing ? (
          <Button
            type="button"
            variant="ghost"
            onClick={() => {
              setReplacing(false);
              setKey("");
            }}
          >
            {H.ai.cancel}
          </Button>
        ) : null}
      </form>
      <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-1">
        <p className="t-caption text-app-text-2">{inFile ? `${H.ai.storedFile} ${H.secretStore.fileNote}` : H.ai.stored}</p>
        <Button
          variant="link"
          size="sm"
          className="h-auto p-0 text-app-accent"
          onClick={() => {
            void client.open_link(PROVIDERS[kind].link as LinkId).catch(() => undefined);
          }}
        >
          {H.ai.whereKey}
          <ExternalLink className="size-3.5" strokeWidth={1.5} aria-hidden />
        </Button>
      </div>
      {hostError ? <ApiErrorCard error={hostError} /> : null}
    </div>
  );
}

/** Provider, model and key. The key field is write-only: a stored key is never shown. */
export function AiSection({
  value,
  onChange,
  scripted,
  onStored,
}: {
  value: ProviderSettings;
  onChange(next: ProviderSettings): void;
  /** Told whether a key for the chosen provider is stored; null while that is read. */
  onStored?: (stored: boolean | null) => void;
  /** The bridge's written conversation stands in for a model. */
  scripted: boolean;
}) {
  // The field follows the stored model until somebody types in it.
  const [model, setModel] = useState(value.model);
  const [followed, setFollowed] = useState(value.model);
  if (followed !== value.model) {
    setFollowed(value.model);
    setModel(value.model);
  }
  const suggestions = PROVIDERS[value.kind].models;
  const commit = (text: string) => {
    const next = text.trim();
    if (next !== value.model) onChange({ kind: value.kind, model: next });
  };

  return (
    <div className="space-y-3" data-testid="ai-section">
      <Row label={H.ai.provider}>
        <Segmented<ProviderKind>
          label={H.ai.provider}
          value={value.kind}
          onChange={(kind) => {
            if (kind !== value.kind) onChange({ kind, model: firstModel(kind) });
          }}
          segments={KINDS.map((kind) => ({ value: kind, label: H.providers[kind] }))}
        />
      </Row>
      <Row label={H.ai.model}>
        <div className="relative">
          <Input
            value={model}
            spellCheck={false}
            autoComplete="off"
            maxLength={200}
            aria-label={H.ai.model}
            placeholder={value.kind === "ollama" ? H.ai.ollamaPlaceholder : H.ai.modelPlaceholder}
            data-testid="model-field"
            className={cn("t-mono", suggestions.length > 0 && "pr-9")}
            onChange={(event) => {
              setModel(event.target.value);
            }}
            onBlur={() => {
              commit(model);
            }}
            onKeyDown={(event) => {
              if (event.key === "Enter") commit(model);
            }}
          />
          {suggestions.length > 0 ? (
            <Menu>
              <MenuTrigger
                render={
                  <button
                    type="button"
                    aria-label={H.ai.suggestions}
                    className="absolute top-1/2 right-1 grid size-6 -translate-y-1/2 place-items-center rounded-tag text-app-text-2 hover:bg-app-hover hover:text-app-text"
                  >
                    <ChevronDown className="size-4" strokeWidth={1.5} />
                  </button>
                }
              />
              <MenuContent align="end" className="min-w-[260px]">
                {suggestions.map((suggestion) => (
                  <MenuItem
                    key={suggestion}
                    className="t-mono"
                    onClick={() => {
                      setModel(suggestion);
                      commit(suggestion);
                    }}
                  >
                    {suggestion}
                  </MenuItem>
                ))}
                <p className="t-caption max-w-[300px] px-2 pt-1.5 pb-1 text-app-text-2">{H.ai.listFrom(providerData.taken, providerData.source)}</p>
              </MenuContent>
            </Menu>
          ) : null}
        </div>
      </Row>
      {value.kind === "ollama" ? (
        <Row label="">
          <p className="t-ui text-app-text-2">{scripted && value.model === "scripted" ? H.ai.scripted : H.ai.ollama}</p>
        </Row>
      ) : (
        <Row label={H.ai.key} top>
          <KeyRow key={value.kind} kind={value.kind} onStored={onStored} />
        </Row>
      )}
    </div>
  );
}

/** The material menu: the three materials the build has slicer settings for. */
export function MaterialSelect() {
  const material = useHost((state) => state.settings?.material ?? null);
  const change = useHost((state) => state.change);
  return (
    <Select
      label={H.printer.material}
      value={material?.id ?? ""}
      className="w-[240px]"
      options={H.materials.map((entry) => ({ value: entry.id, label: entry.label }))}
      onChange={(id) => {
        const next = H.materials.find((entry) => entry.id === id);
        if (next) void change({ material: { id: next.id, family: next.family, density_g_cm3: next.density_g_cm3 } });
      }}
    />
  );
}

/** The build plate on the printer. Nothing is chosen until the person chooses. */
export function PlateSelect() {
  const plate = useHost((state) => state.settings?.plate_type ?? null);
  const change = useHost((state) => state.change);
  const known = H.plates.some((entry) => entry.id === plate);
  return (
    <Select
      label={H.printer.plate}
      value={plate ?? ""}
      className="w-[240px]"
      options={[
        ...(plate === null ? [{ value: "", label: H.printer.plateNone }] : []),
        // A plate set elsewhere (the command line) that this list does not name.
        ...(plate !== null && !known ? [{ value: plate, label: plate }] : []),
        ...H.plates.map((entry) => ({ value: entry.id, label: entry.label })),
      ]}
      onChange={(id) => {
        if (id !== "" && id !== plate) void change({ plate_type: id });
      }}
    />
  );
}

export function PrinterIntro() {
  return (
    <>
      <div className="flex items-center justify-between gap-3">
        <p className="t-heading">{H.printer.card}</p>
        <SimTag>{H.printer.tag}</SimTag>
      </div>
      <p className="t-ui max-w-[68ch] text-app-text-2">{H.printer.setup}</p>
    </>
  );
}

const ROW_ICON: Record<HealthStatus, StatusKind> = { ok: "ok", warn: "note", fail: "problem" };

/** Counts for the caption of the section: "2 ready · 2 notes". */
export function healthCounts(report: DoctorReport): string {
  const rows = healthRows(report);
  const of = (status: HealthStatus) => rows.filter((row) => row.status === status).length;
  return H.setup.counts(of("ok"), of("warn"), of("fail"));
}

export function HealthRows({ report, busy }: { report: DoctorReport | null; busy: boolean }) {
  if (!report || busy) {
    return (
      <ul className="space-y-2" data-testid="health-checking" aria-busy>
        {Object.values(H.health.rows).map((label, index) => (
          <li key={label} className="grid grid-cols-[16px_minmax(0,220px)_minmax(0,1fr)] items-start gap-x-3">
            <Spinner className="mt-px" />
            <span className="t-ui">{label}</span>
            <span className="t-ui text-app-text-2">{index === 0 ? H.health.checking : ""}</span>
          </li>
        ))}
      </ul>
    );
  }
  return (
    <div className="space-y-3" data-testid="health-rows">
      <ul className="space-y-2">
        {healthRows(report).map((row) => (
          <li key={row.id} className="grid grid-cols-[16px_minmax(0,220px)_minmax(0,1fr)] items-start gap-x-3" data-status={row.status}>
            <StatusIcon kind={ROW_ICON[row.status]} label={H.health.status[row.status]} className="mt-px" />
            <span className="t-ui">{row.label}</span>
            <span className="t-ui min-w-0 text-app-text-2">
              <span className="block break-words">{row.text}</span>
              {row.fix ? <span className="block break-words text-app-text">{row.fix}</span> : null}
              {row.link ? (
                <Link
                  to={row.link.to}
                  className="inline-flex min-h-7 items-center rounded-tag text-app-accent underline-offset-2 hover:underline"
                  data-testid={`health-link-${row.id}`}
                >
                  {row.link.label}
                </Link>
              ) : null}
            </span>
          </li>
        ))}
      </ul>
      <Collapsible>
        <CollapsibleTrigger className="t-ui group flex h-7 items-center gap-1 rounded-control text-app-text-2 hover:text-app-text">
          <ChevronDown className="size-3.5 -rotate-90 group-data-[panel-open]:rotate-0" strokeWidth={1.5} aria-hidden />
          {H.health.details}
        </CollapsibleTrigger>
        <CollapsiblePanel>
          <ul className="space-y-2 pt-2" data-testid="health-details">
            {report.checks.map((check) => (
              <li key={check.id} className="grid grid-cols-[16px_minmax(0,1fr)] gap-x-3">
                <StatusIcon
                  kind={check.status === "fail" ? "problem" : check.status === "warn" ? "note" : check.status === "skipped" ? "skipped" : "ok"}
                  label={H.health.status[check.status]}
                  className="mt-px"
                />
                <div className="min-w-0">
                  <p className="t-ui">
                    {check.label.trim()}
                    {check.required ? <span className="text-app-text-2"> · {H.health.required}</span> : null}
                  </p>
                  <p className="t-caption break-words text-app-text-2">{check.detail}</p>
                  {check.fix ? <p className="t-caption break-words text-app-text">{check.fix}</p> : null}
                </div>
              </li>
            ))}
          </ul>
        </CollapsiblePanel>
      </Collapsible>
    </div>
  );
}
