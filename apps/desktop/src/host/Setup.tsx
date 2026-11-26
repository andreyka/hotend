// Setup (DESIGN_SPEC 4.1): one page, three sections, any order, no step counter. The
// health check starts by itself when the page opens.
import { useEffect, useState } from "react";
import { useNavigate } from "react-router";

import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import type { ProviderSettings } from "@/lib/core";
import { ApiErrorCard } from "@/hotend/bits";
import { useHotend } from "@/hotend/context";
import { EXAMPLE_CODE } from "@/hotend/example";
import { S } from "@/hotend/strings";

import { BrandMark } from "./BrandMark";
import { useCore, useHost, useHostStore } from "./core-context";
import { failedRequired } from "./health";
import { AiSection, Card, firstModel, healthCounts, HealthRows, MaterialSelect, PrinterIntro, Row } from "./sections";
import { H } from "./strings";

export function Setup() {
  const navigate = useNavigate();
  const client = useCore();
  const store = useHostStore();
  const settings = useHost((state) => state.settings);
  const doctor = useHost((state) => state.doctor);
  const doctorBusy = useHost((state) => state.doctorBusy);
  const openExample = useHotend((state) => state.openExample);
  const hotendError = useHotend((state) => state.error);

  const [provider, setProvider] = useState<ProviderSettings | null>(null);
  const [stored, setStored] = useState<boolean | null>(null);
  const [busy, setBusy] = useState<"start" | "example" | null>(null);

  useEffect(() => {
    void store.getState().checkComputer();
  }, [store]);

  // The settings' provider when one is chosen; otherwise the first, with its first model.
  const chosen = provider ?? settings?.provider ?? { kind: "anthropic" as const, model: firstModel("anthropic") };
  const failed = failedRequired(doctor);
  const hasModel = chosen.model.trim() !== "";
  const hasKey = chosen.kind === "ollama" || stored === true;
  const ready = failed === null && hasModel && hasKey && doctor !== null && !doctorBusy;
  const missing = failed ? null : !hasModel ? H.setup.missingProvider : !hasKey ? H.setup.missingKey : null;

  const start = async () => {
    if (!ready || busy !== null) return;
    setBusy("start");
    const saved = await store.getState().change({ provider: { kind: chosen.kind, model: chosen.model.trim() } });
    setBusy(null);
    if (saved) void navigate("/hotend");
  };
  const example = async () => {
    if (failed || busy !== null) return;
    setBusy("example");
    const project = await openExample(S.home.exampleName, EXAMPLE_CODE, S.home.exampleNote);
    setBusy(null);
    if (project) void navigate(`/hotend/p/${project.id}/design`);
  };

  return (
    <div className="app-scroll h-full overflow-y-auto" data-testid="setup">
      <div className="mx-auto w-full max-w-[720px] space-y-6 px-6 pt-10 pb-12">
        <header className="space-y-1.5">
          <h1 className="t-display flex items-center gap-3">
            <BrandMark size={28} className="shrink-0" decorative />
            {H.setup.title}
          </h1>
          <p className="t-body text-app-text-2">{H.setup.line}</p>
        </header>

        <Card title={H.setup.ai}>
          <p className="t-ui text-app-text-2">{H.ai.intro}</p>
          <AiSection value={chosen} onChange={setProvider} onStored={setStored} scripted={client.transport === "bridge"} />
        </Card>

        <Card title={H.setup.printer}>
          <PrinterIntro />
          <Row label={H.printer.material}>
            <MaterialSelect />
          </Row>
        </Card>

        <Card
          title={H.setup.computer}
          aside={doctor && !doctorBusy ? <span className="t-caption num text-app-text-2">{healthCounts(doctor)}</span> : null}
        >
          <HealthRows report={doctor} busy={doctorBusy} />
        </Card>

        <div className="space-y-2">
          <div className="flex flex-wrap items-center justify-between gap-4">
            <Button disabled={!ready || busy !== null} onClick={() => void start()} data-testid="setup-start">
              {busy === "start" ? <Spinner /> : null}
              {H.setup.start}
            </Button>
            <Button variant="link" className="text-app-accent" disabled={failed !== null || busy !== null} onClick={() => void example()} data-testid="setup-example">
              {busy === "example" ? <Spinner /> : null}
              {busy === "example" ? S.home.exampleBuilding : H.setup.example}
            </Button>
          </div>
          {failed ? (
            <div role="alert" className="t-ui space-y-1" data-testid="setup-fails-closed">
              <p className="text-app-text">{H.setup.failsClosed}</p>
              {failed.fix ? <p className="text-app-text-2">{failed.fix}</p> : null}
            </div>
          ) : missing && doctor !== null ? (
            <p className="t-ui text-app-text-2" data-testid="setup-missing">
              {missing}
            </p>
          ) : null}
          {hotendError ? <ApiErrorCard error={hotendError} /> : null}
        </div>
      </div>
    </div>
  );
}
