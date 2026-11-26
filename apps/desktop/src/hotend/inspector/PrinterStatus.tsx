// A handed-off print as the printer itself reports it over the local network. Read-only:
// Hotend listens to the printer and sends it nothing. A report counts for this job only
// when the core matched it to the job (`status.job_id`); any other report is said to be
// the printer's, not this job's. The commands come through @/lib/core/pending; in a window
// whose shell does not carry them, the line Hotend always said stays.
import { useCallback, useEffect, useState } from "react";
import { Link } from "react-router";

import { Button } from "@/components/ui/button";
import { usePending } from "@/host/core-context";
import { toApiError, type ApiError, type PrintJob } from "@/lib/core";
import { isNotAvailable, type LanPrinterStatus, type PrinterLanSetup } from "@/lib/core/pending";

import { ApiErrorCard, SectionLabel, StatusIcon } from "../bits";
import { duration, sentence } from "../format";
import { S } from "../strings";

const F = S.print.followed;
const FACTS = "grid grid-cols-[96px_minmax(0,1fr)] gap-x-3 gap-y-1.5";

function Fact({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <>
      <dt className="t-ui text-app-text-2">{label}</dt>
      <dd className="t-ui num min-w-0 text-app-text">{children}</dd>
    </>
  );
}

/** What the printer reported for this job: phase, layer, progress, time left, temperatures, problems. */
export function FollowedFacts({ status }: { status: LanPrinterStatus }) {
  const { state } = status;
  const many = status.nozzles.length > 1;
  return (
    <div className="space-y-2" data-testid="followed-facts">
      <dl className={FACTS}>
        <Fact label={F.phase}>{state.headline || state.phase}</Fact>
        {state.layer !== null && state.total_layers !== null ? (
          <Fact label={F.layer}>
            <span data-testid="followed-layer">{F.layerOf(state.layer, state.total_layers)}</span>
          </Fact>
        ) : null}
        {state.progress_percent !== null ? (
          <Fact label={F.progress}>
            <span data-testid="followed-percent">{`${state.progress_percent} %`}</span>
          </Fact>
        ) : null}
        {state.remaining_minutes !== null ? (
          <Fact label={F.left}>
            <span data-testid="followed-left">{duration(state.remaining_minutes * 60)}</span>
          </Fact>
        ) : null}
        {status.nozzles.map((nozzle) => (
          <Fact key={nozzle.id} label={F.nozzle(nozzle.id, many)}>
            {F.temperature(nozzle.current_c, nozzle.target_c)}
          </Fact>
        ))}
        <Fact label={F.bed}>{F.temperature(status.bed.current_c, status.bed.target_c)}</Fact>
        {status.chamber.current_c !== null ? <Fact label={F.chamber}>{F.temperature(status.chamber.current_c, status.chamber.target_c)}</Fact> : null}
      </dl>
      {state.problems.length > 0 ? (
        <div className="space-y-1" data-testid="followed-problems">
          <p className="t-ui text-app-text-2">{F.problems}</p>
          {state.problems.map((problem) => (
            <div key={problem.raw_code} className="flex gap-2">
              <StatusIcon kind={problem.blocking ? "problem" : "note"} className="mt-px" />
              <div className="min-w-0 flex-1">
                <p className="t-ui">{sentence(problem.meaning ?? problem.message)}</p>
                <p className="t-mono text-app-text-2">{problem.raw_code}</p>
              </div>
            </div>
          ))}
        </div>
      ) : null}
      {status.ended !== null ? (
        <p className="t-ui text-app-text-2">{F.ended(status.ended)}</p>
      ) : !status.connected ? (
        <p className="t-ui text-app-text-2">{F.disconnected}</p>
      ) : null}
      <p className="t-caption text-app-text-2">{F.fromPrinter}</p>
    </div>
  );
}

type Setup = { state: "loading" } | { state: "unavailable" } | { state: "ready"; setup: PrinterLanSetup } | { state: "failed"; error: ApiError };

/** The printer status of a handed-off job, in the Print stage's inspector. */
export function FollowedStatus({ job }: { job: PrintJob }) {
  const pending = usePending();
  const [setup, setSetup] = useState<Setup>({ state: "loading" });
  const [latest, setLatest] = useState<LanPrinterStatus | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  const [starting, setStarting] = useState(false);

  const fetchSetup = useCallback(async (): Promise<{ setup: Setup; latest: LanPrinterStatus | null }> => {
    try {
      const [found, last] = await Promise.all([pending.printer_lan_get(), pending.printer_watch_latest()]);
      return { setup: { state: "ready", setup: found }, latest: last };
    } catch (thrown) {
      return { setup: isNotAvailable(thrown) ? { state: "unavailable" } : { state: "failed", error: toApiError(thrown) }, latest: null };
    }
  }, [pending]);
  const apply = useCallback((next: { setup: Setup; latest: LanPrinterStatus | null }) => {
    setSetup(next.setup);
    if (next.latest) setLatest(next.latest);
  }, []);
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

  // The printer's reports, as they come.
  useEffect(() => {
    let stop: (() => void) | null = null;
    let left = false;
    pending
      .printer_events((status) => {
        if (!left) setLatest(status);
      })
      .then(
        (unsubscribe) => {
          if (left) unsubscribe();
          else stop = unsubscribe;
        },
        () => undefined,
      );
    return () => {
      left = true;
      stop?.();
    };
  }, [pending]);

  if (setup.state === "loading") return null;
  if (setup.state === "unavailable") {
    return (
      <div className="space-y-0.5" data-testid="followed" data-state="unavailable">
        <p className="t-ui text-app-text-2">{S.print.cannotFollow}</p>
        <p className="t-caption text-app-text-2">{F.notAvailable}</p>
      </div>
    );
  }
  if (setup.state === "failed") return <ApiErrorCard error={setup.error} />;

  const { setup: lan } = setup;
  if (lan.address === null) {
    return (
      <div className="space-y-0.5" data-testid="followed" data-state="not-set-up">
        <p className="t-ui text-app-text-2">{F.notSetUp}</p>
        <Link
          to="/settings/printer"
          className="t-ui inline-flex min-h-7 items-center rounded-tag text-app-accent underline-offset-2 hover:underline"
          data-testid="followed-settings"
        >
          {F.setUp}
        </Link>
      </div>
    );
  }

  const mine = latest !== null && latest.job_id === job.id;
  const state = mine ? "following" : latest !== null ? "other-job" : lan.watching ? "waiting" : "not-following";
  return (
    <section className="space-y-2" aria-labelledby="followed-title" data-testid="followed" data-state={state}>
      <SectionLabel id="followed-title">{F.title}</SectionLabel>
      {mine ? (
        <FollowedFacts status={latest} />
      ) : latest !== null ? (
        <div className="space-y-0.5">
          <p className="t-ui">{F.notThisJob}</p>
          <p className="t-ui text-app-text-2">{F.notThisJobDetail(latest.matched_name ?? latest.state.job_name)}</p>
        </div>
      ) : lan.watching ? (
        <p className="t-ui text-app-text-2">{F.waiting}</p>
      ) : (
        <p className="t-ui text-app-text-2">{F.notFollowing}</p>
      )}
      {lan.watching || latest?.connected ? null : (
        <Button
          variant="outline"
          size="sm"
          disabled={starting}
          data-testid="followed-start"
          onClick={() => {
            setStarting(true);
            setError(null);
            pending
              .printer_watch_start()
              .then(read)
              .catch((thrown: unknown) => {
                setError(toApiError(thrown));
              })
              .finally(() => {
                setStarting(false);
              });
          }}
        >
          {F.follow}
        </Button>
      )}
      {error ? <ApiErrorCard error={error} /> : null}
    </section>
  );
}
