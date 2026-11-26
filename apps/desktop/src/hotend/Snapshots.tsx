// Snapshots beside the version rail: a name the maker gives a version, so that it can be
// exported or gone back to later. A snapshot changes no version; going back makes its
// version the current one again and keeps the later ones; deleting removes the name only.
// The commands come through @/lib/core/pending, which passes them to the shell; a window
// whose shell does not carry them says so and has its buttons off.
import { useCallback, useEffect, useState } from "react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { useCore, usePending } from "@/host/core-context";
import { toApiError, type ApiError } from "@/lib/core";
import { isNotAvailable, type Snapshot, type SnapshotExport } from "@/lib/core/pending";
import { cn } from "@/lib/utils";

import { ApiErrorCard, PathText, StatusIcon } from "./bits";
import { useHotend, useNow } from "./context";
import { ago } from "./format";
import { checkSnapshotName, exportSources, MAX_SNAPSHOT_NAME, type NameProblem } from "./snapshots";
import { S } from "./strings";

function problemText(problem: NameProblem): string {
  switch (problem) {
    case "empty":
      return S.snapshots.invalid.empty;
    case "long":
      return S.snapshots.invalid.long(MAX_SNAPSHOT_NAME);
    case "plain":
      return S.snapshots.invalid.plain;
    case "taken":
      return S.snapshots.invalid.taken;
  }
}

/** The name of a new snapshot of the current version. Plain text, checked as the core checks it. */
function NameForm({
  current,
  existing,
  onSave,
  onCancel,
}: {
  current: number;
  existing: readonly Snapshot[];
  onSave(name: string): Promise<boolean>;
  onCancel(): void;
}) {
  const [name, setName] = useState("");
  const [tried, setTried] = useState(false);
  const [saving, setSaving] = useState(false);
  const checked = checkSnapshotName(name, existing);
  // An empty field is not an error until the maker tries to save it.
  const shown = checked.problem === null || (checked.problem === "empty" && !tried) ? null : checked.problem;
  return (
    <form
      className="space-y-1.5"
      data-testid="snapshot-form"
      onSubmit={(event) => {
        event.preventDefault();
        setTried(true);
        if (checked.problem !== null || saving) return;
        setSaving(true);
        void onSave(checked.name).finally(() => {
          setSaving(false);
        });
      }}
    >
      <p className="t-ui-strong">{S.snapshots.takeTitle(current)}</p>
      <Input
        autoFocus
        value={name}
        readOnly={saving}
        aria-label={S.snapshots.name}
        aria-invalid={shown !== null || undefined}
        placeholder={S.snapshots.namePlaceholder}
        data-testid="snapshot-name"
        onChange={(event) => {
          setName(event.target.value);
        }}
      />
      <p className={cn("t-caption", shown ? "text-app-fail" : "text-app-text-2")} data-testid="snapshot-name-note">
        {shown ? problemText(shown) : S.snapshots.nameHint(MAX_SNAPSHOT_NAME)}
      </p>
      <div className="flex gap-2">
        <Button type="submit" size="sm" disabled={saving || shown !== null} data-testid="snapshot-save">
          {saving ? S.snapshots.saving : S.snapshots.save}
        </Button>
        <Button type="button" variant="ghost" size="sm" onClick={onCancel}>
          {S.snapshots.cancel}
        </Button>
      </div>
    </form>
  );
}

function ExportResult({ result }: { result: SnapshotExport }) {
  const { copied, exported } = exportSources(result);
  return (
    <div className="space-y-1" data-testid="snapshot-export-result" role="status">
      <p className="t-ui flex gap-2">
        <StatusIcon kind="ok" className="mt-px" />
        <span className="min-w-0 flex-1">{S.snapshots.exported(result.snapshot.name)}</span>
      </p>
      <PathText path={result.folder} />
      <p className="t-caption text-app-text-2" data-testid="snapshot-export-sources">
        {S.snapshots.files(result.files.length)} · {S.snapshots.sources(copied, exported)}
      </p>
      <ul className="t-caption space-y-0.5 text-app-text-2">
        {result.files.map((file) => (
          <li key={file.path} data-testid="snapshot-export-file">
            {S.snapshots.fileKind[file.kind] ?? file.kind} · {S.snapshots.fileSource[file.source] ?? file.source}
          </li>
        ))}
      </ul>
      {result.printed_missing ? (
        <p className="t-ui flex gap-2 text-app-text" data-testid="snapshot-printed-missing">
          <StatusIcon kind="note" className="mt-px" />
          <span className="min-w-0 flex-1">{S.snapshots.printedMissing}</span>
        </p>
      ) : null}
    </div>
  );
}

function Row({
  snapshot,
  current,
  busy,
  now,
  onExport,
  onRestore,
  onDelete,
}: {
  snapshot: Snapshot;
  current: number | null;
  busy: boolean;
  now: number;
  onExport(): void;
  onRestore(): void;
  onDelete(): void;
}) {
  const [asking, setAsking] = useState(false);
  const isCurrent = snapshot.version === current;
  return (
    <li className="space-y-1.5 py-2" data-testid="snapshot-row" data-name={snapshot.name}>
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
        <span className="t-ui-strong min-w-0 truncate" title={snapshot.name}>
          {snapshot.name}
        </span>
        <span className="t-ui num text-app-text-2">{S.snapshots.version(snapshot.version)}</span>
        <span className="t-caption text-app-text-2">{ago(snapshot.created_at_ms, now)}</span>
        {snapshot.printed_job_id !== null ? (
          <Badge tone="ok" title={S.snapshots.printedHint}>
            <span data-testid="snapshot-printed">{S.snapshots.printed}</span>
          </Badge>
        ) : null}
        {isCurrent ? <Badge>{S.snapshots.current}</Badge> : null}
      </div>
      {asking ? (
        <div className="flex flex-wrap items-center gap-2" role="group" aria-label={S.snapshots.removeSure}>
          <p className="t-caption w-full text-app-text-2">{S.snapshots.removeHint}</p>
          <Button
            variant="destructive"
            size="sm"
            disabled={busy}
            data-testid="snapshot-delete-sure"
            onClick={() => {
              setAsking(false);
              onDelete();
            }}
          >
            {S.snapshots.removeSure}
          </Button>
          <Button
            variant="ghost"
            size="sm"
            onClick={() => {
              setAsking(false);
            }}
          >
            {S.snapshots.keep}
          </Button>
        </div>
      ) : (
        <div className="flex flex-wrap gap-2">
          <Button variant="outline" size="sm" disabled={busy} onClick={onExport} data-testid="snapshot-export">
            {S.snapshots.exportButton}
          </Button>
          {isCurrent ? null : (
            <Button
              variant="outline"
              size="sm"
              disabled={busy}
              title={S.snapshots.restoreHint(snapshot.version)}
              onClick={onRestore}
              data-testid="snapshot-restore"
            >
              {S.snapshots.restore}
            </Button>
          )}
          <Button
            variant="ghost"
            size="sm"
            disabled={busy}
            onClick={() => {
              setAsking(true);
            }}
            data-testid="snapshot-delete"
          >
            {S.snapshots.remove}
          </Button>
        </div>
      )}
    </li>
  );
}

type Listing = { state: "loading" } | { state: "unavailable" } | { state: "ready"; snapshots: Snapshot[] } | { state: "failed"; error: ApiError };

/**
 * The snapshots of a project and what can be done with them. `onRestored` is told after
 * a snapshot was gone back to, so that the window reads the project again.
 */
export function SnapshotsPanel({
  projectId,
  current,
  onRestored,
  onCount,
}: {
  projectId: string;
  current: number | null;
  onRestored(): Promise<void> | void;
  onCount?: (count: number | null) => void;
}) {
  const client = useCore();
  const pending = usePending();
  const now = useNow(false);
  const [listing, setListing] = useState<Listing>({ state: "loading" });
  const [naming, setNaming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [exported, setExported] = useState<SnapshotExport | null>(null);

  // Newest first.
  const fetchListing = useCallback(async (): Promise<Listing> => {
    try {
      const snapshots = await pending.snapshots_list(projectId);
      return { state: "ready", snapshots: [...snapshots].reverse() };
    } catch (thrown) {
      return isNotAvailable(thrown) ? { state: "unavailable" } : { state: "failed", error: toApiError(thrown) };
    }
  }, [pending, projectId]);
  const load = async () => {
    setListing(await fetchListing());
  };

  useEffect(() => {
    let gone = false;
    void fetchListing().then((next) => {
      if (!gone) setListing(next);
    });
    return () => {
      gone = true;
    };
  }, [fetchListing]);

  const count = listing.state === "ready" ? listing.snapshots.length : null;
  useEffect(() => {
    onCount?.(count);
  }, [count, onCount]);

  const act = async (work: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try {
      await work();
    } catch (thrown) {
      setError(toApiError(thrown));
    } finally {
      setBusy(false);
    }
  };

  const unavailable = listing.state === "unavailable";
  const snapshots = listing.state === "ready" ? listing.snapshots : [];

  return (
    <div className="w-[340px] max-w-full space-y-2" data-testid="snapshots" data-state={listing.state}>
      <div className="flex items-center justify-between gap-2">
        <p className="t-label text-app-text-2">{S.snapshots.label}</p>
        {naming ? null : (
          <Button
            variant="outline"
            size="sm"
            disabled={unavailable || listing.state === "loading" || current === null || busy}
            onClick={() => {
              setNaming(true);
              setExported(null);
            }}
            data-testid="snapshot-take"
          >
            {S.snapshots.take}
          </Button>
        )}
      </div>
      {unavailable ? (
        <p className="t-ui flex gap-2 text-app-text-2" data-testid="snapshots-unavailable">
          <StatusIcon kind="note" className="mt-px" />
          <span className="min-w-0 flex-1">{S.snapshots.notAvailable}</span>
        </p>
      ) : null}
      {listing.state === "failed" ? <ApiErrorCard error={listing.error} /> : null}
      {naming && current !== null ? (
        <NameForm
          current={current}
          existing={snapshots}
          onCancel={() => {
            setNaming(false);
          }}
          onSave={async (name) => {
            setError(null);
            try {
              await pending.snapshot_create(projectId, name, current, "");
              setNaming(false);
              await load();
              return true;
            } catch (thrown) {
              setError(toApiError(thrown));
              return false;
            }
          }}
        />
      ) : null}
      {listing.state === "ready" && snapshots.length === 0 && !naming ? (
        <p className="t-ui text-app-text-2" data-testid="snapshots-empty">
          {S.snapshots.none}
        </p>
      ) : null}
      {snapshots.length > 0 ? (
        <ul className="divide-y divide-app-line" data-testid="snapshot-list">
          {snapshots.map((snapshot) => (
            <Row
              key={snapshot.id}
              snapshot={snapshot}
              current={current}
              busy={busy}
              now={now}
              onExport={() => {
                void act(async () => {
                  setExported(null);
                  const folder = await client.folder_pick(S.snapshots.exportTitle);
                  if (folder === null) return;
                  setExported(await pending.snapshot_export(projectId, snapshot.name, folder));
                });
              }}
              onRestore={() => {
                void act(async () => {
                  await pending.snapshot_restore(projectId, snapshot.name);
                  await onRestored();
                });
              }}
              onDelete={() => {
                void act(async () => {
                  await pending.snapshot_delete(projectId, snapshot.name);
                  await load();
                });
              }}
            />
          ))}
        </ul>
      ) : null}
      {exported ? <ExportResult result={exported} /> : null}
      {error ? <ApiErrorCard error={error} /> : null}
    </div>
  );
}

/** The rail's button: how many snapshots there are, and the list in a popover. */
export function SnapshotsButton({ projectId }: { projectId: string }) {
  const current = useHotend((state) => state.project?.project.current_version ?? null);
  const reloadProject = useHotend((state) => state.reloadProject);
  const [count, setCount] = useState<number | null>(null);
  return (
    <Popover>
      <PopoverTrigger
        className="t-caption num inline-flex h-6 shrink-0 items-center rounded-control px-2 text-app-text-2 hover:bg-app-hover hover:text-app-text"
        data-testid="snapshots-open"
      >
        {S.snapshots.open(count ?? 0)}
      </PopoverTrigger>
      <PopoverContent side="top" align="end">
        <SnapshotsPanel projectId={projectId} current={current} onRestored={reloadProject} onCount={setCount} />
      </PopoverContent>
    </Popover>
  );
}
