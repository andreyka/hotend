// The commands of the core that the screens for snapshots, a real printer's status over the
// local network and where web research comes from use, gathered in one client so that a test
// can stand in for them (tests/fake-pending.ts).
//
// The shell carries all of them: their types are the binding's (`wire.rs` mirrors
// `hotend_core::Snapshot`, `SnapshotExport`, `PrinterLanSetup`, `LanPrinterStatus`,
// `SearchSourceOption`) and each method passes the call to the client. A window whose shell
// answers `NOT_AVAILABLE` (or a client-less test) gets the screens' plain disabled state.
import type {
  ApiError,
  CertificateConfirmation,
  LanPrinterStatus,
  PrinterLanSetup,
  ResearchConsentAnswer,
  SearchService,
  SearchSource,
  SearchSourceOption,
  Settings,
  ShellEvent,
  Snapshot,
  SnapshotExport,
  Version,
} from "./bindings";
import type { CoreClient, Unsubscribe } from "./client";

// ---- snapshots (carried by the shell) ---------------------------------------------------

export type { MissingPrinted, PrintedFile, Snapshot, SnapshotExport, SnapshotFile } from "./bindings";

// ---- a real printer's status, read-only (carried by the shell) ------------------------

export type {
  AccessCodeSource,
  CertificateConfirmation,
  CertificatePin,
  LanPrinterStatus,
  LanTrust,
  NozzleReading,
  PrinterLanAddress,
  PrinterLanSetup,
  SecretStoreKind,
  TemperatureReading,
} from "./bindings";

/** The event the shell streams for `CoreEvent::Printer` (channel `printer`). */
export type PrinterShellEvent = Extract<ShellEvent, { channel: "printer" }>;

// ---- where web research comes from (carried by the shell) -----------------------------

export type { ResearchConsentAnswer, SearchService, SearchSource, SearchSourceOption } from "./bindings";

// ---- the commands ---------------------------------------------------------------------

export interface PendingCommands {
  /** False without a client: every command then answers `NOT_AVAILABLE`. */
  readonly available: boolean;

  snapshots_list(projectId: string): Promise<Snapshot[]>;
  snapshot_create(projectId: string, name: string, version: number | null, note: string): Promise<Snapshot>;
  snapshot_delete(projectId: string, name: string): Promise<Snapshot>;
  snapshot_restore(projectId: string, name: string): Promise<Version>;
  /** `folder` must come from `folder_pick` with the title "Save snapshot files", and no other. */
  snapshot_export(projectId: string, name: string, folder: string): Promise<SnapshotExport>;

  printer_lan_get(): Promise<PrinterLanSetup>;
  printer_lan_set(ip: string, serial: string): Promise<PrinterLanSetup>;
  printer_lan_clear(): Promise<null>;
  /** Write-only: no command returns the code. */
  printer_lan_key_set(code: string): Promise<null>;
  printer_lan_key_delete(): Promise<null>;
  /**
   * Probes the printer's certificate and shows the shell's own native dialog with its
   * fingerprint; the shell pins it only when the person confirms there. The window never
   * sends a fingerprint.
   */
  printer_certificate_check(): Promise<CertificateConfirmation>;
  printer_certificate_forget(): Promise<null>;
  printer_watch_start(): Promise<null>;
  printer_watch_stop(): Promise<boolean>;
  printer_watch_latest(): Promise<LanPrinterStatus | null>;
  /** The printer's reports, from the event stream (channel `printer`). */
  printer_events(handler: (status: LanPrinterStatus) => void): Promise<Unsubscribe>;

  research_source_options(): Promise<SearchSourceOption[]>;
  research_source_set(source: SearchSource): Promise<Settings>;
  /**
   * Shows the shell's own native dialog that explains where the queries go for `source`;
   * the shell records the agreement only when the person agrees there.
   */
  research_consent_ask(source: SearchSource): Promise<ResearchConsentAnswer>;
  /** Withdraws any agreement. Takes nothing away that the person did not give. */
  research_consent_withdraw(): Promise<null>;
  /** Write-only: no command returns the key. */
  search_key_set(service: SearchService, key: string): Promise<null>;
  search_key_delete(service: SearchService): Promise<null>;
  search_key_status(service: SearchService): Promise<boolean>;
}

/** What every pending command answers until the shell carries it. */
export const NOT_AVAILABLE: ApiError = {
  kind: "not_configured",
  message: "Not available in this window yet.",
  next_step: "This part of Hotend works on the command line for now.",
};

export function isNotAvailable(error: unknown): boolean {
  return (
    typeof error === "object" &&
    error !== null &&
    (error as ApiError).kind === NOT_AVAILABLE.kind &&
    (error as ApiError).message === NOT_AVAILABLE.message
  );
}

/** True for a printer's report on the event stream. */
export function isPrinterEvent(event: ShellEvent): event is PrinterShellEvent {
  return event.channel === "printer";
}

/**
 * The commands for `client`: the shell's own. Without a client (a test that only wants the
 * names) every command answers `NOT_AVAILABLE`.
 */
export function pendingCommands(client: CoreClient | null): PendingCommands {
  /** The client's command, or a refusal without a client. */
  function carried<A extends unknown[], R>(call: (core: CoreClient, ...args: A) => Promise<R>): (...args: A) => Promise<R> {
    return (...args) => (client ? call(client, ...args) : Promise.reject(NOT_AVAILABLE));
  }
  return {
    available: client !== null,
    snapshots_list: carried((core, projectId: string) => core.snapshots_list(projectId)),
    snapshot_create: carried((core, projectId: string, name: string, version: number | null, note: string) =>
      core.snapshot_create(projectId, name, version, note),
    ),
    snapshot_delete: carried((core, projectId: string, name: string) => core.snapshot_delete(projectId, name)),
    snapshot_restore: carried((core, projectId: string, name: string) => core.snapshot_restore(projectId, name)),
    snapshot_export: carried((core, projectId: string, name: string, folder: string) =>
      core.snapshot_export(projectId, name, folder),
    ),
    printer_lan_get: carried((core) => core.printer_lan_get()),
    printer_lan_set: carried((core, ip: string, serial: string) => core.printer_lan_set(ip, serial)),
    printer_lan_clear: carried((core) => core.printer_lan_clear()),
    printer_lan_key_set: carried((core, code: string) => core.printer_lan_key_set(code)),
    printer_lan_key_delete: carried((core) => core.printer_lan_key_delete()),
    printer_certificate_check: carried((core) => core.printer_certificate_check()),
    printer_certificate_forget: carried((core) => core.printer_certificate_forget()),
    printer_watch_start: carried((core) => core.printer_watch_start()),
    printer_watch_stop: carried((core) => core.printer_watch_stop()),
    printer_watch_latest: carried((core) => core.printer_watch_latest()),
    printer_events: carried((core, handler: (status: LanPrinterStatus) => void) =>
      core.subscribe((event) => {
        if (isPrinterEvent(event)) handler(event.status);
      }),
    ),
    research_source_options: carried((core) => core.research_source_options()),
    research_source_set: carried((core, source: SearchSource) => core.research_source_set(source)),
    research_consent_ask: carried((core, source: SearchSource) => core.research_consent_ask(source)),
    research_consent_withdraw: carried((core) => core.research_consent_withdraw()),
    search_key_set: carried((core, service: SearchService, key: string) => core.search_key_set(service, key)),
    search_key_delete: carried((core, service: SearchService) => core.search_key_delete(service)),
    search_key_status: carried((core, service: SearchService) => core.search_key_status(service)),
  };
}
