import {
  useEffect,
  useRef,
  useState,
  type ChangeEvent,
  type FormEvent,
  type ReactNode,
} from "react";
import {
  reserveSourcePreflight,
  submitEngagementStateCommand,
  uploadSourcePreflight,
  useRead,
  type ReadState,
} from "./api";
import {
  ENGAGEMENT_COMMAND_SCHEMAS,
  SCHEMAS,
  ContractError,
  type EngagementContext,
  type EngagementDetail,
  type EngagementState,
  type MappingBindingCommand,
  type MappingContractCoverage,
  type MappingCoveragePage,
  type MappingDraftSnapshot,
  type MappingOperation,
  type MappingWorkspace,
  type ObjectPack,
  type ObjectPackList,
  type ObjectWorkspaceStatus,
  type ProfilingSummaryPage,
  type SourcePreflight,
  type SourceProfileRules,
  type ValidationSummary,
} from "./contracts";
import { ActiveObjectWorkspace } from "./object-workspace";
import { AppLink } from "./router";
import {
  Card,
  DefinitionGrid,
  EmptyState,
  Hash,
  PageHeader,
  Status,
  TableShell,
  Value,
} from "./ui";
import { RevisedValidationPanel, SALES_DOCUMENT_PACK_ID } from "./tour2-pages";

function number(value: number | null): string {
  return value === null ? "Not available" : value.toLocaleString("en-US");
}

function bytes(value: number | null): string {
  if (value === null) return "Not available";
  if (value < 1024) return `${number(value)} bytes`;
  const units = ["KB", "MB", "GB"];
  let amount = value / 1024;
  let unit = 0;
  while (amount >= 1024 && unit < units.length - 1) {
    amount /= 1024;
    unit += 1;
  }
  return `${amount.toFixed(amount >= 10 ? 1 : 2)} ${units[unit]}`;
}

function safeError(error: unknown): ContractError {
  return error instanceof ContractError
    ? error
    : new ContractError(
        "UNEXPECTED_CLIENT_ERROR",
        "The local action could not be completed safely.",
      );
}

function formText(form: FormData, name: string): string {
  const value = form.get(name);
  return typeof value === "string" ? value.trim() : "";
}

function idempotencyKey(scope: string): string {
  const suffix =
    typeof crypto.randomUUID === "function"
      ? crypto.randomUUID().replaceAll("-", "")
      : `${Date.now().toString(36)}${Math.random().toString(36).slice(2)}`;
  return `${scope}:${suffix}`.slice(0, 128);
}

function useActionIdentity(scope: string) {
  const key = useRef<string | null>(null);
  return {
    get(): string {
      key.current ??= idempotencyKey(scope);
      return key.current;
    },
    complete(): void {
      key.current = null;
    },
  };
}

function SetupBoundary<T>({
  state,
  retry,
  children,
  label = "Loading local engagement",
}: {
  state: ReadState<T>;
  retry: () => void;
  children: (value: T) => ReactNode;
  label?: string;
}) {
  if (state.status === "loading") {
    return (
      <div className="state-panel" role="status" aria-live="polite">
        <span className="spinner" aria-hidden="true" />
        <div>
          <strong>{label}</strong>
          <p>Waiting for bounded local evidence.</p>
        </div>
      </div>
    );
  }
  if (state.status === "error") {
    return (
      <div className="state-panel state-panel--error" role="alert">
        <div>
          <p className="eyebrow">{state.error.code}</p>
          <strong>Local setup evidence is unavailable</strong>
          <p>{state.error.message}</p>
          <button type="button" className="button" onClick={retry}>
            Retry safe read
          </button>
        </div>
      </div>
    );
  }
  return <>{children(state.data)}</>;
}

function ActionError({ error }: { error: ContractError | null }) {
  if (!error) return null;
  return (
    <div className="notice notice--critical" role="alert">
      <strong>{error.code}</strong>
      <p>{error.message}</p>
      {error.code === "NETWORK_UNCERTAIN" ? (
        <p>
          Refresh durable engagement state before retrying. An uncertain
          response is never treated as success.
        </p>
      ) : null}
    </div>
  );
}

function SetupProgress({ state }: { state: EngagementState }) {
  const steps = [
    ["Engagement opened", true],
    ["Source accepted", state.active.jobId !== null],
    [
      "Source profiled without stored sample values",
      state.active.profilingRunId !== null,
    ],
    ["Profile approved locally", state.profileStage === "APPROVED"],
    ["Mapping validated", state.mappingStage === "VALIDATED"],
    ["Validation complete", state.validationStage === "COMPLETE"],
  ] as const;
  const firstIncomplete = steps.findIndex(([, complete]) => !complete);
  return (
    <nav className="setup-progress" aria-label="Engagement setup progress">
      <ol>
        {steps.map(([label, complete], index) => (
          <li
            key={label}
            className={
              complete
                ? "setup-progress__complete"
                : index === firstIncomplete
                  ? "setup-progress__active"
                  : ""
            }
          >
            <span aria-hidden="true">{index + 1}</span>
            <strong>{label}</strong>
            <small>
              {complete
                ? "Complete"
                : index === firstIncomplete
                  ? "Current"
                  : "Later"}
            </small>
          </li>
        ))}
      </ol>
    </nav>
  );
}

function CreateEngagement({ onCreated }: { onCreated: () => void }) {
  const identity = useActionIdentity("create-engagement");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ContractError | null>(null);

  async function submit(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    setBusy(true);
    setError(null);
    try {
      await submitEngagementStateCommand("/api/v1/engagement", {
        schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.create,
        displayName: formText(form, "displayName"),
        idempotencyKey: identity.get(),
      });
      identity.complete();
      onCreated();
    } catch (caught) {
      setError(safeError(caught));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card title="Start a local engagement" className="setup-focus-card">
      <p>
        Name this piece of work commercially. This process owns one isolated
        engagement and reopens the same durable evidence on return.
      </p>
      <form className="setup-form" onSubmit={(event) => void submit(event)}>
        <label>
          Engagement name
          <input
            name="displayName"
            required
            maxLength={256}
            autoComplete="off"
            placeholder="Local migration readiness"
          />
        </label>
        <div className="authority-acknowledgment">
          <strong>Local-only workspace</strong>
          <p>
            Creation sends nothing externally and grants no target-system,
            result-approval, or publication authority.
          </p>
        </div>
        <ActionError error={error} />
        <button type="submit" className="button" disabled={busy}>
          {busy ? "Opening engagement…" : "Create local engagement"}
        </button>
      </form>
    </Card>
  );
}

function activePreflight(detail: EngagementDetail): SourcePreflight | null {
  const active = detail.state.active.preflightId;
  if (active) {
    const match = detail.preflights.find(
      (preflight) => preflight.preflightId === active,
    );
    if (match) return match;
  }
  return detail.preflights.at(-1) ?? null;
}

function PreflightEvidence({ preflight }: { preflight: SourcePreflight }) {
  if (preflight.state === "REJECTED" || preflight.state === "INTERRUPTED") {
    return (
      <div className="notice notice--critical" role="alert">
        <strong>Package needs attention</strong>
        <p>{preflight.errorMessage}</p>
        <small>{preflight.errorCode}</small>
      </div>
    );
  }
  if (preflight.state !== "READY" || !preflight.manifest) {
    return (
      <div className="notice" role="status">
        The source reservation is receiving bytes. If the upload was
        interrupted, choose the ZIP again after refreshing local state.
      </div>
    );
  }
  return (
    <div className="preflight-evidence">
      <div className="notice notice--positive" role="status">
        <strong>Package passed the local safety check.</strong>
        <p>
          Acceptance remains a separate local action. This manifest establishes
          no business meaning or target compatibility.
        </p>
      </div>
      <DefinitionGrid
        items={[
          { label: "Package", value: preflight.sourceName },
          { label: "ZIP size", value: bytes(preflight.sourceSize) },
          { label: "SHA-256", value: <Hash>{preflight.sourceHash}</Hash> },
          {
            label: "Archive entries",
            value: number(preflight.manifest.entries.length),
          },
          {
            label: "CSV files",
            value: number(preflight.manifest.csv_paths.length),
          },
          {
            label: "Expanded size",
            value: bytes(preflight.manifest.total_expanded_bytes),
          },
          {
            label: "Highest expanded ratio",
            value: `${preflight.manifest.maximum_observed_compression_ratio.toFixed(2)}×`,
          },
          {
            label: "Manifest hash",
            value: <Hash>{preflight.manifestHash}</Hash>,
          },
        ]}
      />
      <h3>CSV manifest</h3>
      <ul className="manifest-list">
        {preflight.manifest.csv_paths.map((path) => (
          <li key={path}>
            <Value>{path}</Value>
          </li>
        ))}
      </ul>
    </div>
  );
}

function SourceSetup({
  detail,
  refresh,
}: {
  detail: EngagementDetail;
  refresh: () => void;
}) {
  const uploadIdentity = useActionIdentity("source-preflight");
  const acceptIdentity = useActionIdentity("source-accept");
  const [file, setFile] = useState<File | null>(null);
  const [preflight, setPreflight] = useState<SourcePreflight | null>(
    activePreflight(detail),
  );
  const [acknowledged, setAcknowledged] = useState(false);
  const [busy, setBusy] = useState<"inspect" | "accept" | null>(null);
  const [error, setError] = useState<ContractError | null>(null);

  async function inspect(): Promise<void> {
    if (!file) return;
    if (!file.name.toLowerCase().endsWith(".zip") || file.size === 0) {
      setError(
        new ContractError(
          "UNSUPPORTED_SOURCE",
          "Choose a non-empty ZIP containing headered UTF-8 CSV files.",
        ),
      );
      return;
    }
    setBusy("inspect");
    setError(null);
    try {
      const key = uploadIdentity.get();
      const reservation = await reserveSourcePreflight({
        schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.reserveSourcePreflight,
        sourceName: file.name,
        idempotencyKey: key,
      });
      if (reservation.preflight.state !== "RECEIVING") {
        throw new ContractError(
          "INVALID_PREFLIGHT_STATE",
          "The server did not return a receiving upload reservation.",
        );
      }
      const completed = await uploadSourcePreflight(
        reservation.preflight.preflightId,
        file,
        key,
      );
      setPreflight(completed);
      uploadIdentity.complete();
      refresh();
    } catch (caught) {
      setError(safeError(caught));
    } finally {
      setBusy(null);
    }
  }

  async function accept(): Promise<void> {
    if (
      !acknowledged ||
      preflight?.state !== "READY" ||
      !preflight.sourceHash ||
      !preflight.manifestHash
    ) {
      return;
    }
    setBusy("accept");
    setError(null);
    try {
      await submitEngagementStateCommand(
        `/api/v1/engagement/source-preflights/${encodeURIComponent(preflight.preflightId)}/accept`,
        {
          schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.acceptSource,
          preflightId: preflight.preflightId,
          expectedSourceHash: preflight.sourceHash,
          expectedManifestHash: preflight.manifestHash,
          idempotencyKey: acceptIdentity.get(),
          acknowledgement: "migration-stream/source-acceptance/v1",
        },
      );
      acceptIdentity.complete();
      refresh();
    } catch (caught) {
      setError(safeError(caught));
    } finally {
      setBusy(null);
    }
  }

  return (
    <Card title="Choose and accept a source ZIP" className="setup-focus-card">
      <p>
        The server applies its fixed intake policy before creating an immutable
        local source copy. Browser file selection never supplies a filesystem
        path.
      </p>
      <label className="file-picker">
        Source ZIP
        <input
          type="file"
          accept=".zip,application/zip"
          onChange={(event: ChangeEvent<HTMLInputElement>) => {
            setFile(event.target.files?.[0] ?? null);
            setPreflight(null);
            setAcknowledged(false);
            setError(null);
          }}
        />
      </label>
      {file ? (
        <p>
          <strong>{file.name}</strong> · {bytes(file.size)}
        </p>
      ) : null}
      <button
        type="button"
        className="button"
        disabled={!file || busy !== null}
        onClick={() => void inspect()}
      >
        {busy === "inspect"
          ? "Running local safety check…"
          : "Run local safety check"}
      </button>
      {preflight ? <PreflightEvidence preflight={preflight} /> : null}
      {preflight?.state === "READY" ? (
        <div className="acceptance-panel">
          <label className="acknowledgment">
            <input
              type="checkbox"
              checked={acknowledged}
              onChange={(event) => setAcknowledged(event.target.checked)}
            />
            <span>
              Accept this exact manifest as an immutable local source and run
              bounded textual profiling with no stored sample values. Nothing is
              transmitted externally, and no target compatibility or load
              authority is established.
            </span>
          </label>
          <button
            type="button"
            className="button"
            disabled={!acknowledged || busy !== null}
            onClick={() => void accept()}
          >
            {busy === "accept"
              ? "Accepting and profiling locally…"
              : "Accept source locally"}
          </button>
        </div>
      ) : null}
      <ActionError error={error} />
    </Card>
  );
}

function ProfilingEvidence() {
  const read = useRead<ProfilingSummaryPage>(
    "/api/v1/profiling?include_samples=false&page_size=200",
    SCHEMAS.profiling,
  );
  return (
    <SetupBoundary {...read} label="Loading sample-free profiling">
      {(response) => {
        const unsafe =
          response.include_samples ||
          response.items.some(
            (item) =>
              item.sample_count !== 0 ||
              (item.samples !== undefined && item.samples.length > 0),
          );
        if (unsafe) {
          return (
            <div className="notice notice--critical" role="alert">
              <strong>Profiling evidence was withheld</strong>
              <p>
                Setup accepts only evidence with no stored or returned source
                value samples.
              </p>
            </div>
          );
        }
        return (
          <section className="setup-section" aria-labelledby="profiled-source">
            <div className="section-heading">
              <div>
                <p className="eyebrow">Descriptive evidence only</p>
                <h2 id="profiled-source">Source profiled locally</h2>
                <p>
                  Textual shape is visible without values. It infers no business
                  semantics and makes no compatibility claim.
                </p>
              </div>
              <Status value="SAMPLE-FREE" />
            </div>
            <div className="profiling-grid">
              {response.items.map((item) => (
                <Card
                  key={`${item.path}:${item.column_ordinal}`}
                  eyebrow={`${item.path} · column ${item.column_ordinal}`}
                  title={item.column_name}
                >
                  <DefinitionGrid
                    items={[
                      { label: "Empty", value: number(item.empty_count) },
                      {
                        label: "Non-empty",
                        value: number(item.nonempty_count),
                      },
                      {
                        label: "Decoded length",
                        value: `${number(item.min_decoded_length)} – ${number(item.max_decoded_length)}`,
                      },
                      {
                        label: "Oversized",
                        value: number(item.oversized_count),
                      },
                      {
                        label: "Distinct status",
                        value: <Status value={item.distinct_status} />,
                      },
                      {
                        label: "Distinct count",
                        value: number(item.distinct_count),
                      },
                    ]}
                  />
                </Card>
              ))}
            </div>
            <p className="supporting">
              Value samples are not stored for this engagement.
            </p>
          </section>
        );
      }}
    </SetupBoundary>
  );
}

function ObjectPackSelection({
  state,
  packs,
  refresh,
}: {
  state: EngagementState;
  packs: ObjectPackList;
  refresh: () => void;
}) {
  const identity = useActionIdentity("select-object-pack");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ContractError | null>(null);
  const [changingSelection, setChangingSelection] = useState(false);
  const selectedPack = state.active.objectPackId
    ? (packs.items.find(
        (pack) => pack.objectPackId === state.active.objectPackId,
      ) ?? null)
    : null;
  const selectionCanChange =
    selectedPack !== null &&
    state.profileStage === "REVIEWED" &&
    state.mappingStage === "NOT_PREPARED" &&
    state.validationStage === "NOT_LAUNCHED";
  const showChoices = selectedPack === null || changingSelection;

  async function select(pack: ObjectPack): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      await submitEngagementStateCommand(
        "/api/v1/engagement/object-packs/select",
        {
          schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.selectObjectPack,
          objectPackId: pack.objectPackId,
          idempotencyKey: identity.get(),
        },
      );
      identity.complete();
      setChangingSelection(false);
      refresh();
    } catch (caught) {
      setError(safeError(caught));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="setup-section" aria-labelledby="object-pack">
      <div className="section-heading">
        <div>
          <p className="eyebrow">Controlled proof material</p>
          <h2 id="object-pack">Review the synthetic object pack</h2>
          <p>
            This reviewed proof pack is not raw EDMX and is not automatically
            approved.
          </p>
        </div>
      </div>
      {selectedPack && !showChoices ? (
        <Card title={`Selected proof pack · ${selectedPack.title}`}>
          <p>{selectedPack.description}</p>
          <p className="supporting">
            Confirm the pack title before approving its profile. The selection
            can be corrected here only while the profile remains under review.
          </p>
          {selectionCanChange ? (
            <button
              type="button"
              className="button button--secondary"
              aria-label={`Change proof pack from ${selectedPack.title}`}
              disabled={busy}
              onClick={() => setChangingSelection(true)}
            >
              Choose a different proof pack
            </button>
          ) : null}
        </Card>
      ) : null}
      {showChoices ? (
        <>
          {selectedPack ? (
            <div className="notice" role="status">
              <strong>Current selection: {selectedPack.title}</strong>
              <p>
                Choose another reviewed pack below, or keep this exact
                selection. Changing it resets the active profile choice before
                approval.
              </p>
              <button
                type="button"
                className="button button--secondary"
                aria-label={`Keep selected proof pack: ${selectedPack.title}`}
                disabled={busy}
                onClick={() => setChangingSelection(false)}
              >
                Keep this proof pack
              </button>
            </div>
          ) : null}
          {packs.items
            .filter((pack) => pack.objectPackId !== selectedPack?.objectPackId)
            .map((pack) => (
              <Card key={pack.objectPackId} title={pack.title}>
                <p>{pack.description}</p>
                <DefinitionGrid
                  items={[
                    { label: "Scope", value: pack.scope },
                    { label: "Root object", value: pack.rootEntityId },
                    { label: "Version", value: pack.version },
                    {
                      label: "Mapping control",
                      value:
                        pack.mappingMode === "OPERATOR_GOVERNED"
                          ? "Operator-governed workspace"
                          : "Reviewed controlled suggestions",
                    },
                    {
                      label: "Mapping suggestions",
                      value: number(pack.mappingSuggestions.length),
                    },
                    {
                      label: "Embedded reference tables",
                      value: number(pack.referenceTables.length),
                    },
                  ]}
                />
                <h3>Limitations</h3>
                <ul className="rule-list">
                  {pack.limitations.map((limitation) => (
                    <li key={limitation}>{limitation}</li>
                  ))}
                </ul>
                <button
                  type="button"
                  className="button"
                  aria-label={`Use this proof pack: ${pack.title}`}
                  disabled={busy}
                  onClick={() => void select(pack)}
                >
                  {busy ? `Registering ${pack.title}…` : "Use this proof pack"}
                </button>
              </Card>
            ))}
        </>
      ) : null}
      <ActionError error={error} />
    </section>
  );
}

function ProfileReview({
  state,
  evidence,
  refresh,
}: {
  state: EngagementState;
  evidence: SourceProfileRules;
  refresh: () => void;
}) {
  const identity = useActionIdentity("approve-profile");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ContractError | null>(null);

  async function approve(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    setBusy(true);
    setError(null);
    try {
      await submitEngagementStateCommand(
        "/api/v1/engagement/object-packs/approve",
        {
          schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.approveObjectPackProfile,
          actor: formText(form, "actor"),
          reason: formText(form, "reason"),
          authorityAcknowledgement:
            "migration-stream/local-mapping-validation-authority/v1",
          idempotencyKey: identity.get(),
        },
      );
      identity.complete();
      refresh();
    } catch (caught) {
      setError(safeError(caught));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="setup-section" aria-labelledby="profile-review">
      <div className="section-heading">
        <div>
          <p className="eyebrow">Human authority required</p>
          <h2 id="profile-review">Review the canonical proof profile</h2>
          <p>
            Registration preserves REVIEWED_DRAFT. Only an attributable action
            below may grant local mapping and validation authority.
          </p>
        </div>
        <Status value={state.profileStage} />
      </div>
      <TableShell label="Canonical proof fields">
        <table>
          <thead>
            <tr>
              <th scope="col">Canonical field</th>
              <th scope="col">Logical type</th>
              <th scope="col">Required</th>
              <th scope="col">Sensitive</th>
            </tr>
          </thead>
          <tbody>
            {evidence.profileFields.map((field) => (
              <tr key={`${field.entityId}:${field.fieldId}`}>
                <th scope="row">
                  {field.label}
                  <small>
                    {field.entityId}.{field.fieldId}
                  </small>
                </th>
                <td>{field.logicalType}</td>
                <td>{field.required ? "Yes" : "No"}</td>
                <td>{field.sensitive ? "Yes" : "No"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </TableShell>
      {evidence.profileFields.some(
        (field) =>
          field.entityId === "SalesDocumentItem" &&
          field.fieldId === "quantity",
      ) ? (
        <p className="supporting">
          Quantity is evaluated as positive decimal text in the reviewed format
          during validation. Its logical type does not claim typed numeric or
          arithmetic validation.
        </p>
      ) : null}
      <Card title="Immutable rules">
        <ul className="rule-list">
          {evidence.rules.map((rule) => (
            <li key={rule.ruleId}>
              <strong>{rule.message}</strong>
              <small>
                {rule.severity} · {rule.kind} · {rule.target}
              </small>
            </li>
          ))}
        </ul>
      </Card>
      {state.profileStage !== "APPROVED" ? (
        <Card title="Approve for this local proof">
          <form
            className="setup-form"
            onSubmit={(event) => void approve(event)}
          >
            <label>
              Approver
              <input
                name="actor"
                required
                maxLength={256}
                autoComplete="name"
              />
            </label>
            <label>
              Approval reason
              <textarea name="reason" required maxLength={4096} rows={3} />
            </label>
            <fieldset className="acknowledgment-fieldset">
              <legend>Explicit authority boundary</legend>
              <label className="acknowledgment">
                <input type="checkbox" required />
                <span>
                  I reviewed this exact REVIEWED_DRAFT profile and its immutable
                  rules.
                </span>
              </label>
              <label className="acknowledgment">
                <input type="checkbox" required />
                <span>
                  This approval grants local mapping and validation authority
                  only. It creates no target-compatibility or load claim.
                </span>
              </label>
            </fieldset>
            <ActionError error={error} />
            <button type="submit" className="button" disabled={busy}>
              {busy
                ? "Recording attributable approval…"
                : "Approve for local mapping and validation"}
            </button>
          </form>
        </Card>
      ) : (
        <div className="notice notice--positive" role="status">
          <strong>Local profile authority recorded</strong>
          <p>
            The exact profile hash may now govern mapping and validation only.
          </p>
        </div>
      )}
    </section>
  );
}

function blankOperatorDraft(workspace: MappingWorkspace): MappingDraftSnapshot {
  return {
    sourceEntityAssignments: [],
    bindings: workspace.entities.flatMap((entity, entityIndex) =>
      entity.fields.map((field, fieldIndex): MappingBindingCommand => ({
        bindingId: `operator.binding.${entityIndex}.${fieldIndex}`,
        targetEntityId: entity.entityId,
        targetFieldId: field.fieldId,
        kind: "direct",
      })),
    ),
    relationshipBindings: [],
    referenceTables: workspace.controlledReferenceTables.length
      ? structuredClone(workspace.controlledReferenceTables)
      : workspace.referenceTableObligations.map((obligation) => ({
          entityId: obligation.entityId,
          tableId: obligation.tableId,
          version: obligation.tableVersion,
          values: {},
        })),
  };
}

interface ReferenceTableEditorRow {
  rowId: string;
  source: string;
  target: string;
}

interface ReferenceTableEditorState {
  editorId: string;
  entityId: string;
  tableId: string;
  version: string;
  rows: ReferenceTableEditorRow[];
}

function referenceTableEditorState(
  snapshot: MappingDraftSnapshot,
): ReferenceTableEditorState[] {
  return snapshot.referenceTables.map((table, tableIndex) => ({
    editorId: `table-${tableIndex}-${table.entityId ?? "legacy"}-${table.tableId}`,
    entityId: table.entityId ?? "",
    tableId: table.tableId,
    version: table.version,
    rows: Object.entries(table.values).map(([source, target], rowIndex) => ({
      rowId: `row-${tableIndex}-${rowIndex}`,
      source,
      target,
    })),
  }));
}

function referenceTablesFromEditors(
  editors: ReferenceTableEditorState[],
): MappingDraftSnapshot["referenceTables"] {
  return editors.map((editor) => ({
    entityId: editor.entityId,
    tableId: editor.tableId,
    version: editor.version,
    values: Object.fromEntries(
      editor.rows.map((row) => [row.source, row.target]),
    ),
  }));
}

function unboundOperatorBinding(
  workspace: MappingWorkspace,
  snapshot: MappingDraftSnapshot,
  entityId: string,
  fieldId: string,
): MappingBindingCommand {
  const entityIndex = workspace.entities.findIndex(
    (entity) => entity.entityId === entityId,
  );
  const fieldIndex =
    entityIndex < 0
      ? -1
      : (workspace.entities[entityIndex]?.fields.findIndex(
          (field) => field.fieldId === fieldId,
        ) ?? -1);
  if (entityIndex < 0 || fieldIndex < 0) {
    throw new Error("The mapping target slot is outside the active profile.");
  }
  const base = `operator.binding.${entityIndex}.${fieldIndex}`;
  const used = new Set(snapshot.bindings.map((binding) => binding.bindingId));
  let bindingId = base;
  for (let suffix = 2; used.has(bindingId); suffix += 1) {
    bindingId = `${base}.${suffix}`;
  }
  return {
    bindingId,
    targetEntityId: entityId,
    targetFieldId: fieldId,
    kind: "direct",
  };
}

function isOperatorMappingId(value: string): boolean {
  return (
    value.length > 0 &&
    value.length <= 128 &&
    /^[a-z0-9][a-z0-9.-]*$/.test(value)
  );
}

function isOperatorMappingVersion(value: string): boolean {
  return value.length <= 32 && /^\d+\.\d+\.\d+$/.test(value);
}

function snapshotWithRelationshipConfirmation(
  snapshot: MappingDraftSnapshot,
  relationshipConfirmed: boolean,
  relationships: MappingWorkspace["relationships"],
): MappingDraftSnapshot {
  return {
    ...snapshot,
    relationshipBindings: relationshipConfirmed
      ? relationships.map((relationship) => ({
          ...relationship,
          keyPairs: relationship.keyPairs.map((pair) => ({ ...pair })),
        }))
      : [],
  };
}

function canonicalDraftJson(snapshot: MappingDraftSnapshot): string {
  function canonicalValue(value: unknown): unknown {
    if (Array.isArray(value)) {
      return value.map(canonicalValue);
    }
    if (value !== null && typeof value === "object") {
      const record = value as Record<string, unknown>;
      return Object.fromEntries(
        Object.keys(record)
          .sort()
          .flatMap((key) =>
            record[key] === undefined
              ? []
              : [[key, canonicalValue(record[key])]],
          ),
      );
    }
    return value;
  }

  return JSON.stringify(canonicalValue(snapshot));
}

function withoutBindingSource(
  binding: MappingBindingCommand,
): MappingBindingCommand {
  const result = { ...binding };
  delete result.sourceFile;
  delete result.sourceColumn;
  delete result.sourceColumnOrdinal;
  return result;
}

function withoutBindingOperationDetails(
  binding: MappingBindingCommand,
): MappingBindingCommand {
  const result = { ...binding };
  delete result.constant;
  delete result.lookupTableId;
  return result;
}

function isRelationshipKey(
  workspace: MappingWorkspace,
  entityId: string,
  fieldId: string,
): boolean {
  return workspace.relationships.some((relationship) =>
    relationship.keyPairs.some(
      (pair) =>
        (relationship.fromEntityId === entityId &&
          pair.fromFieldId === fieldId) ||
        (relationship.toEntityId === entityId && pair.toFieldId === fieldId),
    ),
  );
}

function mappingOperationLabel(operation: MappingOperation): string {
  return operation === "constant"
    ? "fixed value (no source)"
    : operation.replaceAll("_", " ");
}

function OperatorMappingWorkspace({
  state,
  refresh,
}: {
  state: EngagementState;
  refresh: () => void;
}) {
  const read = useRead<MappingWorkspace>(
    "/api/v1/engagement/mapping-workspace",
    SCHEMAS.mappingWorkspace,
  );
  return (
    <SetupBoundary {...read} label="Loading durable Mapping Workspace">
      {(workspace) => (
        <OperatorMappingWorkspaceEditor
          key={`${workspace.engagementGeneration}:${workspace.draft?.draftGeneration ?? 0}:${workspace.draft?.state ?? "NEW"}`}
          state={state}
          workspace={workspace}
          refresh={() => {
            read.retry();
            refresh();
          }}
        />
      )}
    </SetupBoundary>
  );
}

function OperatorMappingWorkspaceEditor({
  state,
  workspace,
  refresh,
}: {
  state: EngagementState;
  workspace: MappingWorkspace;
  refresh: () => void;
}) {
  const saveIdentity = useActionIdentity("save-mapping-draft");
  const submitIdentity = useActionIdentity("submit-mapping-draft");
  const approveIdentity = useActionIdentity("approve-mapping-version");
  const [snapshot, setSnapshot] = useState<MappingDraftSnapshot>(
    workspace.draft?.snapshot ?? blankOperatorDraft(workspace),
  );
  const [referenceTableEditors, setReferenceTableEditors] = useState<
    ReferenceTableEditorState[]
  >(() =>
    referenceTableEditorState(
      workspace.draft?.snapshot ?? blankOperatorDraft(workspace),
    ),
  );
  const [relationshipConfirmed, setRelationshipConfirmed] = useState(
    () =>
      workspace.relationships.length === 0 ||
      workspace.relationships.every((relationship) =>
        (workspace.draft?.snapshot.relationshipBindings ?? []).some(
          (binding) => binding.relationshipId === relationship.relationshipId,
        ),
      ),
  );
  const [approvalActor, setApprovalActor] = useState("");
  const [approvalReason, setApprovalReason] = useState("");
  const [approvalAcknowledged, setApprovalAcknowledged] = useState(false);
  const [mappingId, setMappingId] = useState("");
  const [mappingVersion, setMappingVersion] = useState(
    workspace.profileVersion,
  );
  const [busy, setBusy] = useState<"save" | "submit" | "approve" | null>(null);
  const [error, setError] = useState<ContractError | null>(null);
  const editable =
    workspace.draft?.state !== "SUBMITTED" &&
    workspace.draft?.state !== "APPROVED";
  const submissionAvailable =
    workspace.revisionDraftScope?.submissionAvailable ?? true;
  const rootEntityId = workspace.entities.find(
    (entity) => entity.root,
  )?.entityId;
  const derivedReferenceTables = referenceTablesFromEditors(
    referenceTableEditors,
  );
  const salesRouteTable =
    state.active.destinationOrigin === "CONTROLLED_PACK" &&
    state.active.objectPackId === SALES_DOCUMENT_PACK_ID
      ? derivedReferenceTables.find(
          (table) =>
            table.entityId === rootEntityId &&
            table.tableId === "sales-document-route-map",
        )
      : undefined;
  const visibleSnapshot = snapshotWithRelationshipConfirmation(
    { ...snapshot, referenceTables: derivedReferenceTables },
    relationshipConfirmed,
    workspace.relationships,
  );
  const draftHasUnsavedChanges =
    workspace.draft?.state === "EDITABLE" &&
    canonicalDraftJson(visibleSnapshot) !==
      canonicalDraftJson(workspace.draft.snapshot);
  const invalidLookupBindings = visibleSnapshot.bindings.filter(
    (binding) =>
      binding.kind === "lookup" &&
      !derivedReferenceTables.some(
        (table) =>
          table.entityId === binding.targetEntityId &&
          table.tableId === binding.lookupTableId,
      ),
  );
  const referenceTableBlockers = (() => {
    if (workspace.destinationOrigin === "CONTROLLED_PACK") return [];
    const blockers: string[] = [];
    const identities = new Set<string>();
    let entries = 0;
    let referenceBytes = 0;
    for (const table of referenceTableEditors) {
      const identity = `${table.entityId}\u0000${table.tableId}`;
      if (
        !table.entityId ||
        !table.tableId ||
        !/^[a-z0-9][a-z0-9.-]*$/.test(table.tableId) ||
        !/^\d+\.\d+\.\d+$/.test(table.version) ||
        identities.has(identity)
      ) {
        blockers.push(
          "Every reference table needs one unique entity, lowercase table ID, and major.minor.patch version.",
        );
      }
      identities.add(identity);
      if (table.rows.length === 0) {
        blockers.push(`${table.tableId || "Reference table"} has no entries.`);
      }
      const sources = new Set<string>();
      for (const row of table.rows) {
        entries += 1;
        referenceBytes +=
          new TextEncoder().encode(row.source).byteLength +
          new TextEncoder().encode(row.target).byteLength;
        if (
          !row.source ||
          !row.target ||
          new TextEncoder().encode(row.source).byteLength >
            workspace.limits.maximumReferenceKeyBytes ||
          new TextEncoder().encode(row.target).byteLength >
            workspace.limits.maximumReferenceValueBytes ||
          sources.has(row.source)
        ) {
          blockers.push(
            `${table.tableId || "Reference table"} contains a blank, duplicate, or overlong exact entry.`,
          );
        }
        sources.add(row.source);
      }
    }
    for (const obligation of workspace.referenceTableObligations) {
      if (
        !referenceTableEditors.some(
          (table) =>
            table.entityId === obligation.entityId &&
            table.tableId === obligation.tableId &&
            table.version === obligation.tableVersion,
        )
      ) {
        blockers.push(
          `${obligation.entityId}.${obligation.tableId}/${obligation.tableVersion} is required by the approved destination profile.`,
        );
      }
    }
    if (
      referenceTableEditors.length > workspace.limits.maximumReferenceTables ||
      entries > workspace.limits.maximumReferenceEntries ||
      referenceBytes > workspace.limits.maximumReferenceBytes
    ) {
      blockers.push(
        "Reference tables exceed the active count, entry, or byte bound.",
      );
    }
    return [...new Set(blockers)];
  })();

  function assignedFile(entityId: string): string {
    return (
      snapshot.sourceEntityAssignments.find(
        (assignment) => assignment.entityId === entityId,
      )?.sourceFile ?? ""
    );
  }

  function bindingIsResolved(binding: MappingBindingCommand): boolean {
    if (binding.kind === "constant") {
      return binding.constant !== undefined;
    }
    const routedFile = assignedFile(binding.targetEntityId);
    const source = workspace.sourceFiles
      .find((file) => file.sourceFile === routedFile)
      ?.columns.some(
        (column) =>
          binding.sourceFile === routedFile &&
          column.sourceColumn === binding.sourceColumn &&
          column.sourceColumnOrdinal === binding.sourceColumnOrdinal,
      );
    if (!source) return false;
    return (
      binding.kind !== "lookup" ||
      derivedReferenceTables.some(
        (table) =>
          table.entityId === binding.targetEntityId &&
          table.tableId === binding.lookupTableId,
      )
    );
  }

  const unresolvedTargets = workspace.entities.flatMap((entity) =>
    entity.fields.flatMap((field) => {
      const binding = visibleSnapshot.bindings.find(
        (candidate) =>
          candidate.targetEntityId === entity.entityId &&
          candidate.targetFieldId === field.fieldId,
      );
      return binding && bindingIsResolved(binding)
        ? []
        : [`${entity.entityId}.${field.fieldId}`];
    }),
  );
  const mappingIdentityReady =
    isOperatorMappingId(mappingId) && isOperatorMappingVersion(mappingVersion);

  function assignFile(sourceFile: string, entityId: string): void {
    setSnapshot((current) => {
      const withoutFile = current.sourceEntityAssignments.filter(
        (assignment) =>
          assignment.sourceFile !== sourceFile &&
          assignment.entityId !== entityId,
      );
      const assignments = entityId
        ? [...withoutFile, { sourceFile, entityId }]
        : withoutFile;
      const bindings = current.bindings.map((binding) => {
        if (
          binding.targetEntityId !== entityId &&
          binding.sourceFile !== sourceFile
        ) {
          return binding;
        }
        return withoutBindingSource(binding);
      });
      return { ...current, sourceEntityAssignments: assignments, bindings };
    });
  }

  function updateBinding(
    entityId: string,
    fieldId: string,
    update: (binding: MappingBindingCommand) => MappingBindingCommand,
  ): void {
    setSnapshot((current) => {
      const existing = current.bindings.find(
        (binding) =>
          binding.targetEntityId === entityId &&
          binding.targetFieldId === fieldId,
      );
      if (!existing) {
        return {
          ...current,
          bindings: [
            ...current.bindings,
            update(
              unboundOperatorBinding(workspace, current, entityId, fieldId),
            ),
          ],
        };
      }
      return {
        ...current,
        bindings: current.bindings.map((binding) =>
          binding.targetEntityId === entityId &&
          binding.targetFieldId === fieldId
            ? update(binding)
            : binding,
        ),
      };
    });
  }

  function selectSource(
    entityId: string,
    fieldId: string,
    value: string,
  ): void {
    const [fileIndexText, columnIndexText] = value.split(":");
    const file = workspace.sourceFiles[Number(fileIndexText)];
    const column = file?.columns[Number(columnIndexText)];
    updateBinding(entityId, fieldId, (binding) => {
      if (!file || !column) {
        return withoutBindingSource(binding);
      }
      return {
        ...binding,
        sourceFile: file.sourceFile,
        sourceColumn: column.sourceColumn,
        sourceColumnOrdinal: column.sourceColumnOrdinal,
      };
    });
  }

  function selectOperation(
    entityId: string,
    fieldId: string,
    kind: MappingOperation,
  ): void {
    updateBinding(entityId, fieldId, (binding) => {
      const base = withoutBindingOperationDetails(binding);
      if (kind === "constant") {
        const withoutSource = withoutBindingSource(base);
        return { ...withoutSource, kind, constant: "" };
      }
      if (kind === "lookup") {
        return { ...base, kind };
      }
      return { ...base, kind };
    });
  }

  function updateLookup(
    entityId: string | undefined,
    tableId: string,
    source: string,
    target: string,
  ): void {
    setReferenceTableEditors((current) =>
      current.map((table) =>
        table.entityId === entityId && table.tableId === tableId
          ? {
              ...table,
              rows: table.rows.map((row) =>
                row.source === source ? { ...row, target } : row,
              ),
            }
          : table,
      ),
    );
  }

  function addReferenceTable(event: FormEvent<HTMLFormElement>): void {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const entityId = formText(form, "referenceEntityId");
    const tableId = formText(form, "referenceTableId");
    const version = formText(form, "referenceTableVersion");
    if (
      !entityId ||
      !tableId ||
      !version ||
      referenceTableEditors.length >= workspace.limits.maximumReferenceTables
    ) {
      return;
    }
    setReferenceTableEditors((current) => [
      ...current,
      {
        editorId: `table-${Date.now()}-${current.length}`,
        entityId,
        tableId,
        version,
        rows: [],
      },
    ]);
    event.currentTarget.reset();
  }

  function addReferenceRow(editorId: string): void {
    setReferenceTableEditors((current) =>
      current.map((table) =>
        table.editorId === editorId
          ? {
              ...table,
              rows: [
                ...table.rows,
                {
                  rowId: `row-${Date.now()}-${table.rows.length}`,
                  source: "",
                  target: "",
                },
              ],
            }
          : table,
      ),
    );
  }

  function updateReferenceRow(
    editorId: string,
    rowId: string,
    field: "source" | "target",
    value: string,
  ): void {
    setReferenceTableEditors((current) =>
      current.map((table) =>
        table.editorId === editorId
          ? {
              ...table,
              rows: table.rows.map((row) =>
                row.rowId === rowId ? { ...row, [field]: value } : row,
              ),
            }
          : table,
      ),
    );
  }

  function removeReferenceRow(editorId: string, rowId: string): void {
    setReferenceTableEditors((current) =>
      current.map((table) =>
        table.editorId === editorId
          ? {
              ...table,
              rows: table.rows.filter((row) => row.rowId !== rowId),
            }
          : table,
      ),
    );
  }

  async function saveDraft(): Promise<void> {
    setBusy("save");
    setError(null);
    try {
      await submitEngagementStateCommand(
        "/api/v1/engagement/mapping-drafts/save",
        {
          schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.saveMappingDraft,
          idempotencyKey: saveIdentity.get(),
          expectedEngagementGeneration: workspace.engagementGeneration,
          expectedDraftId: workspace.draft?.draftId ?? null,
          expectedDraftGeneration: workspace.draft?.draftGeneration ?? null,
          expectedDraftHash: workspace.draft?.draftHash ?? null,
          snapshot: visibleSnapshot,
        },
      );
      saveIdentity.complete();
      refresh();
    } catch (caught) {
      setError(safeError(caught));
    } finally {
      setBusy(null);
    }
  }

  async function submitDraft(): Promise<void> {
    const draft = workspace.draft;
    if (
      !draft ||
      draft.state !== "EDITABLE" ||
      draftHasUnsavedChanges ||
      invalidLookupBindings.length > 0 ||
      referenceTableBlockers.length > 0 ||
      unresolvedTargets.length > 0 ||
      !mappingIdentityReady
    ) {
      return;
    }
    setBusy("submit");
    setError(null);
    try {
      await submitEngagementStateCommand(
        "/api/v1/engagement/mapping-drafts/submit",
        {
          schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.submitMappingDraft,
          idempotencyKey: submitIdentity.get(),
          expectedEngagementGeneration: workspace.engagementGeneration,
          draftId: draft.draftId,
          expectedDraftGeneration: draft.draftGeneration,
          expectedDraftHash: draft.draftHash,
          mappingId,
          version: mappingVersion,
          acknowledgement:
            "migration-stream/operator-governed-mapping-reviewed-and-frozen/v1",
        },
      );
      submitIdentity.complete();
      refresh();
    } catch (caught) {
      setError(safeError(caught));
    } finally {
      setBusy(null);
    }
  }

  async function approveMapping(): Promise<void> {
    const draft = workspace.draft;
    if (
      !draft ||
      draft.state !== "SUBMITTED" ||
      !draft.submittedMappingId ||
      !draft.submittedMappingVersion ||
      !draft.submittedMappingHash ||
      !approvalAcknowledged
    ) {
      return;
    }
    setBusy("approve");
    setError(null);
    try {
      await submitEngagementStateCommand(
        "/api/v1/engagement/mappings/approve",
        {
          schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.approveMappingVersion,
          idempotencyKey: approveIdentity.get(),
          expectedEngagementGeneration: workspace.engagementGeneration,
          draftId: draft.draftId,
          expectedDraftGeneration: draft.draftGeneration,
          expectedDraftHash: draft.draftHash,
          expectedMappingId: draft.submittedMappingId,
          expectedMappingVersion: draft.submittedMappingVersion,
          expectedMappingHash: draft.submittedMappingHash,
          actor: approvalActor.trim(),
          reason: approvalReason.trim(),
          acknowledgement: "migration-stream/local-mapping-version-approval/v1",
        },
      );
      approveIdentity.complete();
      refresh();
    } catch (caught) {
      setError(safeError(caught));
    } finally {
      setBusy(null);
    }
  }

  return (
    <section className="setup-section" aria-labelledby="mapping-workspace">
      <div className="section-heading">
        <div>
          <p className="eyebrow">Operator-governed · entity-qualified</p>
          <h2 id="mapping-workspace">Mapping Workspace</h2>
          <p>
            Assign each accepted file to one entity, then bind canonical slots
            from accepted file and column identities. One accepted source column
            may feed more than one destination field. Each binding still creates
            one destination-field value and does not fan out rows. No sample
            values, joins, expressions, formulas, scripts, fuzzy matches, or
            operation chains are available.
          </p>
        </div>
        <Status value={workspace.draft?.state ?? "NEW DRAFT"} />
      </div>

      {workspace.revisionDraftScope ? (
        <div className="notice notice--attention" role="status">
          <strong>
            Revision {workspace.revisionDraftScope.tourNumber} mapping review
          </strong>
          <p>
            This draft is bound to profiling run {workspace.profilingRunId}. You
            may save an exact mapping draft for the structurally changed source,
            but it cannot be frozen or activated from this pending revision.
          </p>
          <p>{workspace.revisionDraftScope.unavailableReason}</p>
        </div>
      ) : null}

      <Card title="File to entity routing">
        <div className="dashboard-grid">
          {workspace.sourceFiles.map((file) => {
            const current =
              snapshot.sourceEntityAssignments.find(
                (assignment) => assignment.sourceFile === file.sourceFile,
              )?.entityId ?? "";
            return (
              <label key={file.sourceFile}>
                {file.sourceFile}
                <select
                  aria-label={`Assign ${file.sourceFile} to entity`}
                  value={current}
                  disabled={!editable}
                  onChange={(event) =>
                    assignFile(file.sourceFile, event.target.value)
                  }
                >
                  <option value="">Choose destination entity</option>
                  {workspace.entities.map((entity) => (
                    <option key={entity.entityId} value={entity.entityId}>
                      {entity.root ? "Root entity" : "Direct child entity"} ·{" "}
                      {entity.entityId}
                    </option>
                  ))}
                </select>
              </label>
            );
          })}
        </div>
      </Card>

      {workspace.entities.map((entity) => {
        const routedFile = assignedFile(entity.entityId);
        const sourceFileIndex = workspace.sourceFiles.findIndex(
          (file) => file.sourceFile === routedFile,
        );
        const columns =
          sourceFileIndex >= 0
            ? (workspace.sourceFiles[sourceFileIndex]?.columns ?? [])
            : [];
        return (
          <Card
            key={entity.entityId}
            eyebrow={
              entity.root
                ? "Root destination entity"
                : "Direct child destination entity"
            }
            title={entity.entityId}
          >
            <TableShell label={`${entity.entityId} canonical slot bindings`}>
              <table className="mapping-editor">
                <thead>
                  <tr>
                    <th scope="col">Canonical slot</th>
                    <th scope="col">Accepted source column</th>
                    <th scope="col">One bounded operation</th>
                  </tr>
                </thead>
                <tbody>
                  {entity.fields.map((field) => {
                    const retainedBinding = snapshot.bindings.find(
                      (candidate) =>
                        candidate.targetEntityId === entity.entityId &&
                        candidate.targetFieldId === field.fieldId,
                    );
                    const binding =
                      retainedBinding ??
                      unboundOperatorBinding(
                        workspace,
                        snapshot,
                        entity.entityId,
                        field.fieldId,
                      );
                    const resolved = bindingIsResolved(binding);
                    const selectedColumn =
                      binding.sourceFile === routedFile
                        ? columns.findIndex(
                            (column) =>
                              column.sourceColumn === binding.sourceColumn &&
                              column.sourceColumnOrdinal ===
                                binding.sourceColumnOrdinal,
                          )
                        : -1;
                    const keyField = isRelationshipKey(
                      workspace,
                      entity.entityId,
                      field.fieldId,
                    );
                    return (
                      <tr key={`${entity.entityId}:${field.fieldId}`}>
                        <th scope="row">
                          {field.label}
                          <small>
                            {entity.entityId}.{field.fieldId}
                            {field.required ? " · required" : ""}
                            {entity.key.includes(field.fieldId) ? " · key" : ""}
                            {!resolved ? " · unresolved" : ""}
                          </small>
                        </th>
                        <td>
                          {binding.kind === "constant" ? (
                            <input
                              aria-label={`Fixed value for ${entity.entityId}.${field.fieldId}`}
                              value={binding.constant ?? ""}
                              disabled={!editable}
                              onChange={(event) =>
                                updateBinding(
                                  entity.entityId,
                                  field.fieldId,
                                  (current) => ({
                                    ...current,
                                    constant: event.target.value,
                                  }),
                                )
                              }
                            />
                          ) : (
                            <select
                              aria-label={`Source for ${entity.entityId}.${field.fieldId}`}
                              value={
                                selectedColumn >= 0
                                  ? `${sourceFileIndex}:${selectedColumn}`
                                  : ""
                              }
                              disabled={!editable || !routedFile}
                              onChange={(event) =>
                                selectSource(
                                  entity.entityId,
                                  field.fieldId,
                                  event.target.value,
                                )
                              }
                            >
                              <option value="">Choose accepted column</option>
                              {columns.map((column, index) => (
                                <option
                                  key={`${column.sourceColumnOrdinal}:${column.sourceColumn}`}
                                  value={`${sourceFileIndex}:${index}`}
                                >
                                  {column.sourceColumn}
                                </option>
                              ))}
                            </select>
                          )}
                        </td>
                        <td>
                          <select
                            aria-label={`Operation for ${entity.entityId}.${field.fieldId}`}
                            value={binding.kind}
                            disabled={!editable}
                            onChange={(event) =>
                              selectOperation(
                                entity.entityId,
                                field.fieldId,
                                event.target.value as MappingOperation,
                              )
                            }
                          >
                            {(keyField
                              ? (["direct"] as MappingOperation[])
                              : workspace.allowedOperations
                            ).map((operation) => (
                              <option key={operation} value={operation}>
                                {mappingOperationLabel(operation)}
                              </option>
                            ))}
                          </select>
                          {binding.kind === "lookup" ? (
                            <select
                              aria-label={`Lookup for ${entity.entityId}.${field.fieldId}`}
                              aria-invalid={invalidLookupBindings.includes(
                                binding,
                              )}
                              required
                              value={
                                invalidLookupBindings.includes(binding)
                                  ? ""
                                  : (binding.lookupTableId ?? "")
                              }
                              disabled={!editable}
                              onChange={(event) =>
                                updateBinding(
                                  entity.entityId,
                                  field.fieldId,
                                  (current) => {
                                    const next =
                                      withoutBindingOperationDetails(current);
                                    return event.target.value
                                      ? {
                                          ...next,
                                          lookupTableId: event.target.value,
                                        }
                                      : next;
                                  },
                                )
                              }
                            >
                              <option value="">
                                Choose exact reference table
                              </option>
                              {derivedReferenceTables
                                .filter(
                                  (table) => table.entityId === entity.entityId,
                                )
                                .map((table) => (
                                  <option
                                    key={table.tableId}
                                    value={table.tableId}
                                  >
                                    {table.tableId} · {table.version}
                                  </option>
                                ))}
                            </select>
                          ) : null}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </TableShell>
          </Card>
        );
      })}

      {workspace.destinationOrigin === "WORKSPACE_PROFILE" ? (
        <Card title="Exact local reference tables">
          <p>
            Author small entity-scoped exact-match tables inside this CAS draft.
            A transform lookup maps a source key to one canonical value; a
            reference-domain rule separately checks that a projected value is
            present among the exact table keys. Table entries remain on this
            narrow editing surface and are omitted from broad coverage pages.
          </p>
          {referenceTableEditors.map((table) => {
            const required = workspace.referenceTableObligations.some(
              (obligation) =>
                obligation.entityId === table.entityId &&
                obligation.tableId === table.tableId &&
                obligation.tableVersion === table.version,
            );
            return (
              <section
                className="reference-table-editor"
                key={table.editorId}
                aria-label={`${table.tableId || "New reference table"} ${table.version || ""}`}
              >
                <div className="section-heading">
                  <div>
                    <strong>
                      {table.entityId}.{table.tableId || "Unidentified table"} ·{" "}
                      {table.version || "No version"}
                    </strong>
                    <p>
                      {required
                        ? "Required by the exact approved destination profile."
                        : "Available only to explicit lookup bindings in this mapping."}
                    </p>
                  </div>
                  {!required ? (
                    <button
                      type="button"
                      className="button button--secondary"
                      disabled={!editable}
                      onClick={() =>
                        setReferenceTableEditors((current) =>
                          current.filter(
                            (candidate) =>
                              candidate.editorId !== table.editorId,
                          ),
                        )
                      }
                    >
                      Remove table
                    </button>
                  ) : null}
                </div>
                {table.rows.length ? (
                  <TableShell label={`${table.tableId} exact entries`}>
                    <table>
                      <thead>
                        <tr>
                          <th scope="col">Exact key</th>
                          <th scope="col">Exact mapped/domain value</th>
                          <th scope="col">Action</th>
                        </tr>
                      </thead>
                      <tbody>
                        {table.rows.map((row) => (
                          <tr key={row.rowId}>
                            <td>
                              <input
                                aria-label={`Exact key in ${table.tableId}`}
                                value={row.source}
                                maxLength={
                                  workspace.limits.maximumReferenceKeyBytes
                                }
                                disabled={!editable}
                                onChange={(event) =>
                                  updateReferenceRow(
                                    table.editorId,
                                    row.rowId,
                                    "source",
                                    event.target.value,
                                  )
                                }
                              />
                            </td>
                            <td>
                              <input
                                aria-label={`Exact value in ${table.tableId}`}
                                value={row.target}
                                maxLength={
                                  workspace.limits.maximumReferenceValueBytes
                                }
                                disabled={!editable}
                                onChange={(event) =>
                                  updateReferenceRow(
                                    table.editorId,
                                    row.rowId,
                                    "target",
                                    event.target.value,
                                  )
                                }
                              />
                            </td>
                            <td>
                              <button
                                type="button"
                                className="button button--secondary"
                                disabled={!editable}
                                onClick={() =>
                                  removeReferenceRow(table.editorId, row.rowId)
                                }
                              >
                                Remove entry
                              </button>
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </TableShell>
                ) : (
                  <p className="notice notice--attention">
                    Add at least one exact key/value entry before saving.
                  </p>
                )}
                <button
                  type="button"
                  className="button button--secondary"
                  disabled={
                    !editable ||
                    table.rows.length >=
                      workspace.limits.maximumReferenceEntries
                  }
                  onClick={() => addReferenceRow(table.editorId)}
                >
                  Add exact entry
                </button>
              </section>
            );
          })}
          {editable ? (
            <form
              className="destination-form-grid"
              onSubmit={addReferenceTable}
            >
              <label>
                Entity
                <select name="referenceEntityId" required>
                  <option value="">Choose destination entity</option>
                  {workspace.entities.map((entity) => (
                    <option key={entity.entityId} value={entity.entityId}>
                      {entity.entityId}
                    </option>
                  ))}
                </select>
              </label>
              <label>
                Table ID
                <input
                  name="referenceTableId"
                  required
                  maxLength={128}
                  pattern="[a-z0-9][a-z0-9.-]*"
                  placeholder="bounded-code-map"
                />
              </label>
              <label>
                Version
                <input
                  name="referenceTableVersion"
                  required
                  maxLength={32}
                  pattern="[0-9]+\.[0-9]+\.[0-9]+"
                  placeholder="1.0.0"
                />
              </label>
              <button
                type="submit"
                className="button button--secondary"
                disabled={
                  referenceTableEditors.length >=
                  workspace.limits.maximumReferenceTables
                }
              >
                Add exact table
              </button>
            </form>
          ) : null}
          {referenceTableBlockers.length ? (
            <div className="notice notice--critical" role="status">
              <strong>Reference-table contract is incomplete</strong>
              <ul>
                {referenceTableBlockers.map((blocker) => (
                  <li key={blocker}>{blocker}</li>
                ))}
              </ul>
            </div>
          ) : null}
        </Card>
      ) : null}

      <div className="dashboard-grid">
        {salesRouteTable ? (
          <Card title="Govern route-code reference data">
            <p>
              N and R are fixed pack-configured source keys. They were not
              discovered from, sampled from, or inferred from received data.
            </p>
            <div key={`${salesRouteTable.entityId}:${salesRouteTable.tableId}`}>
              {Object.entries(salesRouteTable.values).map(
                ([source, target]) => (
                  <label key={source}>
                    Pack-configured source key {source}
                    <select
                      aria-label={`Route code ${source}`}
                      value={target}
                      disabled={!editable}
                      onChange={(event) =>
                        updateLookup(
                          salesRouteTable.entityId,
                          salesRouteTable.tableId,
                          source,
                          event.target.value,
                        )
                      }
                    >
                      <option value="STANDARD">STANDARD</option>
                      <option value="RETURN">RETURN</option>
                    </select>
                  </label>
                ),
              )}
              <small>
                Keys are fixed to the reviewed synthetic table; targets stay
                inside the controlled STANDARD / RETURN domain.
              </small>
            </div>
          </Card>
        ) : null}
        <Card title="Relationship key confirmation">
          {workspace.relationships.length === 0 ? (
            <p className="notice notice--positive">
              No relationships are declared by this profile. Relationship-key
              coverage is complete for this root-only contract.
            </p>
          ) : (
            <>
              {workspace.relationships.map((relationship) => (
                <div key={relationship.relationshipId}>
                  <strong>{relationship.relationshipId}</strong>
                  <p>
                    {relationship.fromEntityId}.
                    {relationship.keyPairs[0]?.fromFieldId} ={" "}
                    {relationship.toEntityId}.
                    {relationship.keyPairs[0]?.toFieldId}
                  </p>
                  <p>
                    Cardinality {relationship.cardinality}; validation enforces
                    parent existence
                    {relationship.cardinality === "1..*"
                      ? " and at least one child."
                      : "."}
                  </p>
                </div>
              ))}
              <label className="acknowledgment">
                <input
                  type="checkbox"
                  checked={relationshipConfirmed}
                  disabled={!editable}
                  onChange={(event) =>
                    setRelationshipConfirmed(event.target.checked)
                  }
                />
                <span>
                  I confirm the exact profile-declared parent key pair. No
                  name-based or inferred join is used.
                </span>
              </label>
            </>
          )}
        </Card>
      </div>

      <MappingContractCoveragePanel
        unsavedChanges={
          workspace.draft?.state === "EDITABLE" && draftHasUnsavedChanges
        }
      />

      <Card title="Draft attribution and durability">
        <label>
          Mapping author
          <input
            value={snapshot.author ?? ""}
            disabled={!editable}
            maxLength={256}
            onChange={(event) =>
              setSnapshot((current) => ({
                ...current,
                author: event.target.value,
              }))
            }
          />
        </label>
        {workspace.draft ? (
          <DefinitionGrid
            items={[
              {
                label: "Draft generation",
                value: number(workspace.draft.draftGeneration),
              },
              {
                label: "Draft hash",
                value: <Hash>{workspace.draft.draftHash}</Hash>,
              },
              {
                label: "State",
                value: <Status value={workspace.draft.state} />,
              },
            ]}
          />
        ) : (
          <p>No durable draft has been saved yet.</p>
        )}
        {unresolvedTargets.length ? (
          <div className="notice notice--attention" role="status">
            <strong>Resolve every active destination slot</strong>
            <p>
              Bind and durably save{" "}
              {unresolvedTargets.map((target) => target).join(", ")} before
              submitting this mapping.
            </p>
          </div>
        ) : null}
        {invalidLookupBindings.length ? (
          <div className="notice notice--critical" role="status">
            <strong>Choose an exact controlled lookup table</strong>
            <p>
              A lookup table is required for{" "}
              {invalidLookupBindings
                .map(
                  (binding) =>
                    `${binding.targetEntityId}.${binding.targetFieldId}`,
                )
                .join(", ")}
              . Missing or unknown table identities cannot be saved or frozen.
            </p>
          </div>
        ) : null}
        {workspace.draft?.state === "EDITABLE" ? (
          <>
            <div className="destination-form-grid">
              <label>
                New mapping identity
                <input
                  value={mappingId}
                  required
                  maxLength={128}
                  pattern="[a-z0-9][a-z0-9.-]*"
                  placeholder="workspace.mapping-contract"
                  autoComplete="off"
                  aria-invalid={
                    mappingId.length > 0 && !isOperatorMappingId(mappingId)
                  }
                  onChange={(event) => setMappingId(event.target.value)}
                />
              </label>
              <label>
                New mapping version
                <input
                  value={mappingVersion}
                  required
                  maxLength={32}
                  pattern="[0-9]+\.[0-9]+\.[0-9]+"
                  autoComplete="off"
                  aria-invalid={!isOperatorMappingVersion(mappingVersion)}
                  onChange={(event) => setMappingVersion(event.target.value)}
                />
              </label>
            </div>
            <p className="supporting">
              Enter a new, unused mapping identity. The version defaults to the
              active destination profile version and remains editable. Existing
              immutable mapping versions are never overwritten; the server
              rejects a collision.
            </p>
          </>
        ) : null}
        {editable ? (
          <button
            type="button"
            className="button"
            disabled={
              busy !== null ||
              invalidLookupBindings.length > 0 ||
              referenceTableBlockers.length > 0 ||
              unresolvedTargets.length > 0
            }
            onClick={() => void saveDraft()}
          >
            {busy === "save" ? "Saving exact revision…" : "Save durable draft"}
          </button>
        ) : null}
        {workspace.draft?.state === "EDITABLE" ? (
          <>
            {draftHasUnsavedChanges ? (
              <p role="status">
                Save the visible mapping changes as a new durable generation
                before submitting.
              </p>
            ) : null}
            <button
              type="button"
              className="button button--secondary"
              disabled={
                busy !== null ||
                draftHasUnsavedChanges ||
                invalidLookupBindings.length > 0 ||
                referenceTableBlockers.length > 0 ||
                unresolvedTargets.length > 0 ||
                !mappingIdentityReady ||
                !submissionAvailable
              }
              onClick={() => void submitDraft()}
            >
              {busy === "submit"
                ? "Freezing and checking mapping…"
                : "Submit and freeze this generation"}
            </button>
            {!submissionAvailable ? (
              <p className="supporting">
                {workspace.revisionDraftScope?.unavailableReason}
              </p>
            ) : null}
          </>
        ) : null}
      </Card>

      {workspace.draft?.submittedMappingHash ? (
        <>
          <Card title="Immutable submitted mapping">
            <DefinitionGrid
              items={[
                {
                  label: "Mapping",
                  value: `${workspace.draft.submittedMappingId} · ${workspace.draft.submittedMappingVersion}`,
                },
                {
                  label: "Exact mapping hash",
                  value: <Hash>{workspace.draft.submittedMappingHash}</Hash>,
                },
              ]}
            />
          </Card>
          <MappingCoverage />
        </>
      ) : null}

      {workspace.draft?.state === "SUBMITTED" ? (
        <Card title="Approve this exact mapping hash">
          <label>
            Mapping approver
            <input
              value={approvalActor}
              maxLength={256}
              onChange={(event) => setApprovalActor(event.target.value)}
            />
          </label>
          <label>
            Approval reason
            <textarea
              value={approvalReason}
              maxLength={4096}
              rows={3}
              onChange={(event) => setApprovalReason(event.target.value)}
            />
          </label>
          <label className="acknowledgment">
            <input
              type="checkbox"
              checked={approvalAcknowledged}
              onChange={(event) =>
                setApprovalAcknowledged(event.target.checked)
              }
            />
            <span>
              I reviewed and approve the exact immutable mapping hash for local
              validation only. This grants no target or publication authority.
            </span>
          </label>
          <button
            type="button"
            className="button"
            disabled={
              busy !== null ||
              !approvalAcknowledged ||
              !approvalActor.trim() ||
              !approvalReason.trim()
            }
            onClick={() => void approveMapping()}
          >
            {busy === "approve"
              ? "Recording exact-hash approval…"
              : "Approve exact mapping hash"}
          </button>
        </Card>
      ) : null}

      {workspace.draft?.state === "APPROVED" ? (
        <div className="notice notice--positive" role="status">
          <strong>Exact mapping hash approved</strong>
          <p>
            The frozen entity-qualified mapping may now govern local validation.
          </p>
        </div>
      ) : null}
      <ActionError error={error} />
      {state.mappingStage === "VALIDATED" ? (
        <p className="supporting">
          Mapping authority is active for local validation only.
        </p>
      ) : null}
    </section>
  );
}

function MappingSetup({
  state,
  pack,
  evidence,
  refresh,
}: {
  state: EngagementState;
  pack: ObjectPack;
  evidence: SourceProfileRules;
  refresh: () => void;
}) {
  const identity = useActionIdentity("prepare-mapping");
  const [selected, setSelected] = useState(
    () =>
      new Set(
        pack.mappingSuggestions.map((suggestion) => suggestion.suggestionId),
      ),
  );
  const [author, setAuthor] = useState("");
  const [acknowledged, setAcknowledged] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ContractError | null>(null);

  function toggle(suggestionId: string, checked: boolean): void {
    setSelected((current) => {
      const next = new Set(current);
      if (checked) next.add(suggestionId);
      else next.delete(suggestionId);
      return next;
    });
  }

  async function prepare(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    setBusy(true);
    setError(null);
    try {
      if (!acknowledged) {
        throw new ContractError(
          "MAPPING_ACKNOWLEDGMENT_REQUIRED",
          "Review the controlled suggestions before preparing mapping evidence.",
        );
      }
      const selectedTargets = new Set(
        pack.mappingSuggestions
          .filter((suggestion) => selected.has(suggestion.suggestionId))
          .map((suggestion) => suggestion.targetFieldId),
      );
      const missingRequired = evidence.profileFields.filter(
        (field) => field.required && !selectedTargets.has(field.fieldId),
      );
      if (missingRequired.length) {
        throw new ContractError(
          "REQUIRED_FIELD_UNMAPPED",
          `Required fields need controlled suggestions: ${missingRequired.map((field) => field.label).join(", ")}.`,
        );
      }
      await submitEngagementStateCommand(
        "/api/v1/engagement/mappings/prepare",
        {
          schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.prepareMapping,
          idempotencyKey: identity.get(),
          mappingId: "engagement.mapping",
          version: "1.0.0",
          selectedSuggestionIds: pack.mappingSuggestions
            .filter((suggestion) => selected.has(suggestion.suggestionId))
            .map((suggestion) => suggestion.suggestionId),
          author: author.trim() || null,
          reviewer: null,
          acknowledgement:
            "migration-stream/local-assisted-mapping-reviewed-suggestions-no-target-compatibility/v1",
        },
      );
      identity.complete();
      refresh();
    } catch (caught) {
      setError(safeError(caught));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="setup-section" aria-labelledby="mapping">
      <div className="section-heading">
        <div>
          <p className="eyebrow">Operator-governed assisted mapping</p>
          <h2 id="mapping">Review controlled mapping suggestions</h2>
          <p>
            The browser may select only suggestion identities from the reviewed
            pack. The server resolves exact bindings and embedded reference
            tables, then validates them against the profiled source.
          </p>
        </div>
        <Status value={state.mappingStage} />
      </div>
      <form className="setup-form" onSubmit={(event) => void prepare(event)}>
        <TableShell label="Controlled field mapping suggestions">
          <table className="mapping-editor">
            <thead>
              <tr>
                <th scope="col">Use</th>
                <th scope="col">Canonical field</th>
                <th scope="col">Operation</th>
                <th scope="col">Controlled source/default</th>
              </tr>
            </thead>
            <tbody>
              {pack.mappingSuggestions.map((suggestion) => {
                const field = evidence.profileFields.find(
                  (candidate) => candidate.fieldId === suggestion.targetFieldId,
                );
                const sourceMatches =
                  suggestion.kind === "constant" ||
                  evidence.sourceColumns.some(
                    (column) =>
                      column.sourceFile === suggestion.sourceFile &&
                      column.sourceColumn === suggestion.sourceColumn &&
                      column.sourceColumnOrdinal ===
                        suggestion.sourceColumnOrdinal,
                  );
                return (
                  <tr key={suggestion.suggestionId}>
                    <td>
                      <input
                        type="checkbox"
                        aria-label={`Use suggestion for ${field?.label ?? suggestion.targetFieldId}`}
                        checked={selected.has(suggestion.suggestionId)}
                        disabled={!sourceMatches}
                        onChange={(event) =>
                          toggle(suggestion.suggestionId, event.target.checked)
                        }
                      />
                    </td>
                    <th scope="row">
                      {field?.label ?? suggestion.targetFieldId}
                      <small>
                        {suggestion.suggestionId}
                        {field?.required ? " · required" : ""}
                      </small>
                    </th>
                    <td>{suggestion.kind.replaceAll("_", " ")}</td>
                    <td>
                      {suggestion.kind === "constant"
                        ? `Fixed pack default: ${suggestion.constant ?? ""}`
                        : `${suggestion.sourceColumn ?? "Missing"} (${suggestion.sourceFile ?? "No source"})`}
                      {suggestion.lookupTableId ? (
                        <small>
                          Reference table {suggestion.lookupTableId}
                        </small>
                      ) : null}
                      {!sourceMatches ? (
                        <small className="critical-text">
                          Exact profiled source identity is absent
                        </small>
                      ) : null}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </TableShell>
        <div className="dashboard-grid">
          <Card title="Mapping attribution">
            <label>
              Mapping author
              <input
                value={author}
                maxLength={256}
                onChange={(event) => setAuthor(event.target.value)}
              />
            </label>
          </Card>
          <Card title="Embedded controlled reference data">
            {pack.referenceTables.length ? (
              <ul className="rule-list">
                {pack.referenceTables.map((table) => (
                  <li key={table.tableId}>
                    <strong>{table.tableId}</strong>
                    <small>
                      Version {table.version} ·{" "}
                      {Object.keys(table.values).length} bounded entries
                    </small>
                  </li>
                ))}
              </ul>
            ) : (
              <p>No reference table is embedded in this pack.</p>
            )}
            <p className="supporting">
              These values are server-owned proof material; the browser cannot
              replace or extend them.
            </p>
          </Card>
        </div>
        <label className="acknowledgment authority-acknowledgment">
          <input
            type="checkbox"
            checked={acknowledged}
            onChange={(event) => setAcknowledged(event.target.checked)}
          />
          <span>
            I reviewed the exact controlled suggestions and embedded reference
            data. This creates immutable local mapping evidence only.
          </span>
        </label>
        <ActionError error={error} />
        <button type="submit" className="button" disabled={busy}>
          {busy ? "Checking controlled mapping…" : "Check and prepare mapping"}
        </button>
      </form>
      {state.mappingStage === "VALIDATED" ? <MappingCoverage /> : null}
    </section>
  );
}

function MappingContractCoveragePanel({
  unsavedChanges,
}: {
  unsavedChanges: boolean;
}) {
  const read = useRead<MappingContractCoverage>(
    "/api/v1/engagement/mapping-contract-coverage",
    SCHEMAS.mappingContractCoverage,
  );
  return (
    <SetupBoundary {...read} label="Loading mapping contract coverage">
      {(coverage) => (
        <Card title="Value-free contract coverage" className="card--wide">
          <p>
            This server-derived view is bound to the exact workspace, accepted
            source, approved destination profile and durable draft or mapping.
            It contains file/column identities and counts, never received
            values, fixed values, conditional comparators, or reference-table
            entries.
          </p>
          {unsavedChanges ? (
            <div className="notice notice--attention" role="status">
              Coverage describes the last durable draft generation. Save the
              visible edits to derive a new exact coverage hash.
            </div>
          ) : null}
          <DefinitionGrid
            items={[
              {
                label: "Overall contract",
                value: <Status value={coverage.overallStatus} />,
              },
              {
                label: "Coverage hash",
                value: <Hash>{coverage.coverageHash}</Hash>,
              },
              {
                label: "Profile approval",
                value: (
                  <>
                    <Value>{coverage.authority.profileApprovalId}</Value>
                    <Hash>{coverage.authority.profileApprovalHash}</Hash>
                  </>
                ),
              },
              {
                label: "Durable draft",
                value: coverage.authority.draftId ? (
                  <>
                    <Value>
                      {coverage.authority.draftId} · generation{" "}
                      {number(coverage.authority.draftGeneration)}
                    </Value>
                    <Hash>{coverage.authority.draftHash}</Hash>
                  </>
                ) : (
                  "Not saved"
                ),
              },
              {
                label: "Exact mapping approval",
                value: coverage.authority.mappingApprovalId ? (
                  <>
                    <Value>{coverage.authority.mappingApprovalId}</Value>
                    <Hash>{coverage.authority.mappingApprovalHash}</Hash>
                  </>
                ) : (
                  "Not yet approved"
                ),
              },
            ]}
          />
          <div className="metric-grid">
            <div className="metric">
              <strong>{number(coverage.summary.complete)}</strong>
              <span>Complete</span>
            </div>
            <div className="metric">
              <strong>{number(coverage.summary.unresolved)}</strong>
              <span>Unresolved</span>
            </div>
            <div className="metric">
              <strong>{number(coverage.summary.warnings)}</strong>
              <span>Warnings</span>
            </div>
            <div className="metric">
              <strong>{number(coverage.summary.blockers)}</strong>
              <span>Blockers</span>
            </div>
          </div>
          <TableShell label="Active destination field coverage">
            <table>
              <thead>
                <tr>
                  <th scope="col">Destination field</th>
                  <th scope="col">Binding</th>
                  <th scope="col">Rule obligations</th>
                  <th scope="col">Status</th>
                </tr>
              </thead>
              <tbody>
                {coverage.fields.map((field) => (
                  <tr key={`${field.entityId}:${field.fieldId}`}>
                    <th scope="row">
                      {field.entityId}.{field.fieldId}
                    </th>
                    <td>
                      {field.operation === "constant"
                        ? "Fixed value configured (value withheld)"
                        : field.operation
                          ? `${field.operation.replaceAll("_", " ")} · ${
                              field.sourceFile ?? "No file"
                            } · ${field.sourceColumn ?? "No column"}`
                          : "No durable binding"}
                      {field.lookupTableId
                        ? ` · ${field.lookupTableId}/${field.lookupTableVersion ?? "unknown"}`
                        : ""}
                    </td>
                    <td>
                      {field.ruleIds.length
                        ? field.ruleIds
                            .map(
                              (ruleId, index) =>
                                `${ruleId} (${field.ruleKinds[index] ?? "rule"})`,
                            )
                            .join(", ")
                        : "None"}
                    </td>
                    <td>
                      <Status value={field.status} />
                      {field.reasons.map((reason) => (
                        <small key={reason}>{reason}</small>
                      ))}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </TableShell>
          {coverage.sourceColumns.length ? (
            <TableShell label="Accepted source-column coverage">
              <table>
                <thead>
                  <tr>
                    <th scope="col">Accepted source column</th>
                    <th scope="col">Ordinal</th>
                    <th scope="col">Status</th>
                  </tr>
                </thead>
                <tbody>
                  {coverage.sourceColumns.map((source) => (
                    <tr
                      key={`${source.sourceFile}:${source.sourceColumnOrdinal}`}
                    >
                      <th scope="row">
                        {source.sourceFile} · {source.sourceColumn}
                      </th>
                      <td>{number(source.sourceColumnOrdinal)}</td>
                      <td>
                        <Status value={source.status} />
                        <small>{source.reason}</small>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </TableShell>
          ) : null}
          {coverage.rules.length ? (
            <TableShell label="Validation-rule coverage">
              <table>
                <thead>
                  <tr>
                    <th scope="col">Rule obligation</th>
                    <th scope="col">Target field</th>
                    <th scope="col">Reference table</th>
                    <th scope="col">Status</th>
                  </tr>
                </thead>
                <tbody>
                  {coverage.rules.map((rule) => (
                    <tr key={rule.ruleId}>
                      <th scope="row">
                        {rule.ruleId}
                        <small>{rule.kind}</small>
                      </th>
                      <td>
                        {rule.entityId}.{rule.fieldId}
                      </td>
                      <td>
                        {rule.referenceTableId
                          ? `${rule.referenceTableId}/${rule.referenceTableVersion ?? "unknown"}`
                          : "None"}
                      </td>
                      <td>
                        <Status value={rule.status} />
                        {rule.reasons.map((reason) => (
                          <small key={reason}>{reason}</small>
                        ))}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </TableShell>
          ) : null}
          {coverage.tables.length ? (
            <TableShell label="Exact reference-table obligations">
              <table>
                <thead>
                  <tr>
                    <th scope="col">Entity and table</th>
                    <th scope="col">Version</th>
                    <th scope="col">Entries</th>
                    <th scope="col">Role</th>
                    <th scope="col">Status</th>
                  </tr>
                </thead>
                <tbody>
                  {coverage.tables.map((table) => (
                    <tr
                      key={`${table.entityId}:${table.tableId}:${table.tableVersion}`}
                    >
                      <th scope="row">
                        {table.entityId}.{table.tableId}
                      </th>
                      <td>{table.tableVersion}</td>
                      <td>{number(table.entryCount)}</td>
                      <td>{table.roles.join(", ")}</td>
                      <td>
                        <Status value={table.status} />
                        {table.reasons.map((reason) => (
                          <small key={reason}>{reason}</small>
                        ))}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </TableShell>
          ) : null}
          {coverage.relationships.length ? (
            <ul className="evidence-list">
              {coverage.relationships.map((relationship) => (
                <li key={relationship.relationshipId}>
                  <strong>{relationship.relationshipId}</strong>
                  <span>
                    {relationship.fromEntityId} → {relationship.toEntityId} ·{" "}
                    {relationship.cardinality} · {relationship.keyPairs.length}{" "}
                    explicit key pair(s) · {relationship.status}
                  </span>
                  {relationship.reasons.map((reason) => (
                    <small key={reason}>{reason}</small>
                  ))}
                </li>
              ))}
            </ul>
          ) : (
            <p className="notice notice--positive">
              Root-only destination contract: no relationship acknowledgement is
              required.
            </p>
          )}
        </Card>
      )}
    </SetupBoundary>
  );
}

function MappingCoverage() {
  const read = useRead<MappingCoveragePage>(
    "/api/v1/engagement/mapping-coverage?page_size=200",
    SCHEMAS.mapping,
  );
  return (
    <SetupBoundary {...read} label="Loading mapping coverage">
      {(response) => {
        const errors = response.items.filter((item) => item.issue_code);
        return (
          <Card title="Mapping coverage">
            <div className="metric-grid">
              <div className="metric">
                <strong>
                  {
                    response.items.filter((item) => item.kind === "MAPPED")
                      .length
                  }
                </strong>
                <span>Mapped fields</span>
              </div>
              <div className="metric">
                <strong>{errors.length}</strong>
                <span>Structural findings</span>
              </div>
            </div>
            {errors.length ? (
              <ul className="mapping-errors">
                {errors.map((item, index) => (
                  <li key={`${item.issue_code}:${item.field_id}:${index}`}>
                    <strong>{item.issue_code}</strong>
                    <span>{item.detail}</span>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="notice notice--positive">
                Structural mapping coverage reports no blocking binding or
                relationship-key findings.
              </p>
            )}
            <p className="supporting">
              This evidence checks mapping structure only. Received-value checks
              run when validation is launched.
            </p>
            <p className="supporting">
              Evidence <Hash>{response.canonical_hash}</Hash>
            </p>
          </Card>
        );
      }}
    </SetupBoundary>
  );
}

function ValidationStep({
  state,
  evidence,
  refresh,
}: {
  state: EngagementState;
  evidence: SourceProfileRules;
  refresh: () => void;
}) {
  const identity = useActionIdentity("launch-validation");
  const [acknowledged, setAcknowledged] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ContractError | null>(null);

  async function launch(): Promise<void> {
    const active = state.active;
    if (
      !acknowledged ||
      !active.jobId ||
      !active.mappingId ||
      !active.mappingVersion ||
      !active.mappingHash
    ) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await submitEngagementStateCommand(
        "/api/v1/engagement/validation/launch",
        {
          schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.launchValidation,
          idempotencyKey: identity.get(),
          expectedEngagementGeneration: state.generation,
          expectedJobId: active.jobId,
          expectedMappingId: active.mappingId,
          expectedMappingVersion: active.mappingVersion,
          expectedMappingHash: active.mappingHash,
        },
      );
      identity.complete();
      refresh();
    } catch (caught) {
      setError(safeError(caught));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="setup-section" aria-labelledby="validation">
      <div className="section-heading">
        <div>
          <p className="eyebrow">Immutable rules · fixed server policy</p>
          <h2 id="validation">Review and launch validation</h2>
          <p>
            Rules cannot be edited here. The server binds its fixed policy to
            the exact source, approved profile, and validated mapping.
          </p>
        </div>
        <Status value={state.validationStage} />
      </div>
      <Card title="Immutable rule review">
        <ul className="rule-list">
          {evidence.rules.map((rule) => (
            <li key={rule.ruleId}>
              <strong>{rule.message}</strong>
              <small>
                {rule.severity} · {rule.kind} · {rule.target}
              </small>
            </li>
          ))}
        </ul>
      </Card>
      {state.validationStage === "NOT_LAUNCHED" ? (
        <div className="validation-launch">
          <label className="acknowledgment authority-acknowledgment">
            <input
              type="checkbox"
              checked={acknowledged}
              onChange={(event) => setAcknowledged(event.target.checked)}
            />
            <span>
              I reviewed the immutable rules and accept the fixed local
              execution policy. This does not approve a migration result or
              authorize a target load.
            </span>
          </label>
          <ActionError error={error} />
          <button
            type="button"
            className="button"
            disabled={!acknowledged || busy}
            onClick={() => void launch()}
          >
            {busy ? "Launching validation…" : "Launch local validation"}
          </button>
        </div>
      ) : (
        <ValidationEvidence />
      )}
    </section>
  );
}

function ValidationEvidence() {
  const read = useRead<ValidationSummary>(
    "/api/v1/validation",
    SCHEMAS.validation,
  );
  return (
    <SetupBoundary {...read} label="Loading validation evidence">
      {(response) => (
        <Card title="First-issues handoff" className="issues-handoff">
          <DefinitionGrid
            items={[
              {
                label: "Validation state",
                value: <Status value={response.data.state} />,
              },
              {
                label: "Rows processed",
                value: number(response.data.rows_processed),
              },
              {
                label: "Issues produced",
                value: number(response.data.issue_count),
              },
              {
                label: "Completed",
                value: response.data.timestamps.completed_at ?? "Not complete",
              },
            ]}
          />
          {response.data.state === "COMPLETE" ? (
            <>
              <p>
                {response.data.issue_count === 0
                  ? "This run produced no issues. That is not a readiness or target-compatibility claim."
                  : "Validation evidence is complete. Continue to the existing issue workbench."}
              </p>
              {response.data.issue_count === 0 ? (
                <a className="button" href="/validation">
                  Review completed validation
                </a>
              ) : (
                <a className="button" href="/issues">
                  Review first issues
                </a>
              )}
              <p>
                <AppLink
                  className="button button--secondary"
                  href="/comparison#start-source-refresh"
                >
                  Start source refresh
                </AppLink>
              </p>
            </>
          ) : (
            <p role="status">
              Validation is not terminal; durable state will be refreshed.
            </p>
          )}
        </Card>
      )}
    </SetupBoundary>
  );
}

function GovernanceFlow({
  detail,
  refresh,
}: {
  detail: EngagementDetail;
  refresh: () => void;
}) {
  if (detail.state.active.destinationOrigin === "WORKSPACE_PROFILE") {
    return (
      <WorkspaceProfileGovernance state={detail.state} refresh={refresh} />
    );
  }
  return <ControlledPackGovernance detail={detail} refresh={refresh} />;
}

function ControlledPackGovernance({
  detail,
  refresh,
}: {
  detail: EngagementDetail;
  refresh: () => void;
}) {
  const packRead = useRead<ObjectPackList>(
    "/api/v1/engagement/object-packs",
    SCHEMAS.objectPacks,
  );
  return (
    <SetupBoundary {...packRead} label="Loading controlled proof pack">
      {(packs) => {
        const selected = detail.state.active.objectPackId
          ? packs.items.find(
              (pack) => pack.objectPackId === detail.state.active.objectPackId,
            )
          : null;
        return (
          <>
            <ObjectPackSelection
              state={detail.state}
              packs={packs}
              refresh={refresh}
            />
            {selected ? (
              <GovernanceEvidence
                state={detail.state}
                pack={selected}
                refresh={refresh}
              />
            ) : null}
          </>
        );
      }}
    </SetupBoundary>
  );
}

function WorkspaceProfileGovernance({
  state,
  refresh,
}: {
  state: EngagementState;
  refresh: () => void;
}) {
  if (state.profileStage !== "APPROVED") {
    return (
      <Card title="Workspace destination profile awaits approval">
        <p>
          The reviewed workspace profile is frozen, but it has no local mapping
          and validation authority until its exact hash is separately approved.
        </p>
        <a className="button" href="/destination">
          Review destination authority
        </a>
      </Card>
    );
  }
  return <WorkspaceProfileGovernanceEvidence state={state} refresh={refresh} />;
}

function WorkspaceProfileGovernanceEvidence({
  state,
  refresh,
}: {
  state: EngagementState;
  refresh: () => void;
}) {
  const read = useRead<SourceProfileRules>(
    "/api/v1/engagement/source-profile-rules",
    SCHEMAS.sourceProfileRules,
  );
  return (
    <SetupBoundary {...read} label="Loading workspace profile fields and rules">
      {(evidence) => (
        <>
          <Card title="Approved workspace destination profile">
            <p>
              This exact workspace-reviewed profile has local mapping and
              validation authority. It contains no embedded source-mapping
              answers.
            </p>
            <DefinitionGrid
              items={[
                {
                  label: "Profile",
                  value: state.active.profileId ?? "Unavailable",
                },
                {
                  label: "Version",
                  value: state.active.profileVersion ?? "Unavailable",
                },
                {
                  label: "Profile hash",
                  value: <Hash>{state.active.profileHash}</Hash>,
                },
                {
                  label: "Origin authority",
                  value: state.active.destinationAuthorityId ?? "Unavailable",
                },
              ]}
            />
            <a className="button button--secondary" href="/destination">
              Review destination evidence
            </a>
          </Card>
          <OperatorMappingWorkspace state={state} refresh={refresh} />
          {state.mappingStage === "VALIDATED" ? (
            <ValidationStep
              state={state}
              evidence={evidence}
              refresh={refresh}
            />
          ) : null}
        </>
      )}
    </SetupBoundary>
  );
}

type MappingPageMode =
  "OPERATOR_GOVERNED" | "CONTROLLED_SUGGESTIONS" | "UNAVAILABLE";

function MappingContractPageIntro({ mode }: { mode: MappingPageMode }) {
  const operatorGoverned = mode === "OPERATOR_GOVERNED";
  const controlledSuggestions = mode === "CONTROLLED_SUGGESTIONS";
  return (
    <>
      <PageHeader
        eyebrow={
          operatorGoverned
            ? "Mapping contract workshop"
            : "Controlled mapping review"
        }
        title={
          operatorGoverned
            ? "Build the mapping contract"
            : controlledSuggestions
              ? "Review controlled mapping preparation"
              : "Review mapping authority"
        }
        description={
          operatorGoverned
            ? "Route each accepted file to one destination entity, bind every active field with one bounded operation, then freeze and separately approve the exact contract."
            : controlledSuggestions
              ? "Review the approved pack's server-owned suggestions, choose their exact identities, and prepare the bounded mapping evidence."
              : "The exact active destination and mapping authority could not be resolved."
        }
      />
      <div className="setup-boundary" role="note">
        <strong>
          {operatorGoverned
            ? "Sample-free contract boundary"
            : controlledSuggestions
              ? "Controlled mapping boundary"
              : "Unavailable mapping boundary"}
        </strong>
        <span>
          {operatorGoverned
            ? "For received source evidence, the workshop uses accepted file and column identities only. Operator-authored fixed values and bounded exact tables stay inside the governed mapping draft. It does not expose received values, add joins or expressions, shape records, authorize a target write, or approve validation results."
            : controlledSuggestions
              ? "The browser reviews only server-owned suggestion identities from the approved pack. The server resolves and validates exact bindings without exposing received values or granting target, publication, or result-approval authority."
              : "No mapping workflow is exposed until the exact active destination and mapping authority resolves."}
        </span>
      </div>
    </>
  );
}

function ControlledMappingContractStage({
  detail,
  refresh,
}: {
  detail: EngagementDetail;
  refresh: () => void;
}) {
  const packRead = useRead<ObjectPackList>(
    "/api/v1/engagement/object-packs",
    SCHEMAS.objectPacks,
  );
  const evidenceRead = useRead<SourceProfileRules>(
    "/api/v1/engagement/source-profile-rules",
    SCHEMAS.sourceProfileRules,
  );
  return (
    <SetupBoundary {...packRead} label="Loading controlled mapping authority">
      {(packs) => (
        <SetupBoundary
          {...evidenceRead}
          label="Loading canonical fields and rules"
        >
          {(evidence) => {
            const selected = packs.items.find(
              (pack) =>
                pack.objectPackId === detail.state.active.objectPackId &&
                pack.version === detail.state.active.profileVersion,
            );
            if (!selected) {
              return (
                <>
                  <MappingContractPageIntro mode="UNAVAILABLE" />
                  <EmptyState title="Exact mapping authority is unavailable">
                    Return to <AppLink href="/setup">setup</AppLink> and review
                    the selected destination authority. No mapping draft was
                    opened.
                  </EmptyState>
                </>
              );
            }
            return (
              <>
                <MappingContractPageIntro mode={selected.mappingMode} />
                {selected.mappingMode === "OPERATOR_GOVERNED" ? (
                  <OperatorMappingWorkspace
                    state={detail.state}
                    refresh={refresh}
                  />
                ) : (
                  <MappingSetup
                    state={detail.state}
                    pack={selected}
                    evidence={evidence}
                    refresh={refresh}
                  />
                )}
              </>
            );
          }}
        </SetupBoundary>
      )}
    </SetupBoundary>
  );
}

function MappingContractStage({
  detail,
  refresh,
}: {
  detail: EngagementDetail;
  refresh: () => void;
}) {
  return detail.state.active.destinationOrigin === "WORKSPACE_PROFILE" ? (
    <>
      <MappingContractPageIntro mode="OPERATOR_GOVERNED" />
      <Card title="Approved workspace destination profile">
        <p>
          This exact workspace-reviewed profile has local mapping and validation
          authority. It contains no embedded source-mapping answers.
        </p>
        <DefinitionGrid
          items={[
            {
              label: "Profile",
              value: detail.state.active.profileId ?? "Unavailable",
            },
            {
              label: "Version",
              value: detail.state.active.profileVersion ?? "Unavailable",
            },
            {
              label: "Profile hash",
              value: <Hash>{detail.state.active.profileHash}</Hash>,
            },
            {
              label: "Origin authority",
              value:
                detail.state.active.destinationAuthorityId ?? "Unavailable",
            },
          ]}
        />
        <a className="button button--secondary" href="/destination">
          Review destination evidence
        </a>
      </Card>
      <OperatorMappingWorkspace state={detail.state} refresh={refresh} />
    </>
  ) : (
    <ControlledMappingContractStage detail={detail} refresh={refresh} />
  );
}

export function EngagementMappingContractPage() {
  const read = useRead<EngagementDetail>(
    "/api/v1/engagement",
    SCHEMAS.engagementDetail,
  );
  return (
    <SetupBoundary {...read} label="Loading exact mapping authority">
      {(detail) => (
        <MappingContractStage detail={detail} refresh={read.retry} />
      )}
    </SetupBoundary>
  );
}

export function EngagementValidationPage() {
  const detailRead = useRead<EngagementDetail>(
    "/api/v1/engagement",
    SCHEMAS.engagementDetail,
  );
  const evidenceRead = useRead<SourceProfileRules>(
    "/api/v1/engagement/source-profile-rules",
    SCHEMAS.sourceProfileRules,
  );
  return (
    <>
      <PageHeader
        eyebrow="Validation"
        title="Validate the approved mapping"
        description="Review the exact profile rules and mapping authority, then launch one immutable local validation run."
      />
      <SetupBoundary {...detailRead} label="Loading validation authority">
        {(detail) => (
          <SetupBoundary
            {...evidenceRead}
            label="Loading immutable validation rules"
          >
            {(evidence) => (
              <ValidationStep
                state={detail.state}
                evidence={evidence}
                refresh={detailRead.retry}
              />
            )}
          </SetupBoundary>
        )}
      </SetupBoundary>
    </>
  );
}

function GovernanceEvidence({
  state,
  pack,
  refresh,
}: {
  state: EngagementState;
  pack: ObjectPack;
  refresh: () => void;
}) {
  const read = useRead<SourceProfileRules>(
    "/api/v1/engagement/source-profile-rules",
    SCHEMAS.sourceProfileRules,
  );
  return (
    <SetupBoundary {...read} label="Loading canonical fields and rules">
      {(evidence) => (
        <>
          <ProfileReview state={state} evidence={evidence} refresh={refresh} />
          {state.profileStage === "APPROVED" ? (
            pack.mappingMode === "OPERATOR_GOVERNED" ? (
              <OperatorMappingWorkspace state={state} refresh={refresh} />
            ) : (
              <MappingSetup
                state={state}
                pack={pack}
                evidence={evidence}
                refresh={refresh}
              />
            )
          ) : null}
          {state.mappingStage === "VALIDATED" ? (
            <ValidationStep
              state={state}
              evidence={evidence}
              refresh={refresh}
            />
          ) : null}
        </>
      )}
    </SetupBoundary>
  );
}

function ExistingEngagement() {
  const read = useRead<EngagementDetail>(
    "/api/v1/engagement",
    SCHEMAS.engagementDetail,
  );
  const phase =
    read.state.status === "success" ? read.state.data.state.phase : "";
  const shouldPoll =
    phase === "PREFLIGHT_RECEIVING" ||
    phase === "INGESTING" ||
    phase === "VALIDATING";

  useEffect(() => {
    if (!shouldPoll) return;
    const timer = window.setTimeout(read.retry, 1200);
    return () => window.clearTimeout(timer);
  }, [phase, read.retry, shouldPoll]);

  return (
    <SetupBoundary {...read}>
      {(detail) => {
        const state = detail.state;
        const accepted = state.active.jobId !== null;
        const profiled = state.active.profilingRunId !== null;
        const preflight = activePreflight(detail);
        return (
          <>
            <SetupProgress state={state} />
            <Card title={state.displayName} className="engagement-summary">
              <DefinitionGrid
                items={[
                  {
                    label: "Commercial stage",
                    value: <Status value={state.phase} />,
                  },
                  { label: "Updated", value: state.updatedAt },
                  {
                    label: "Persistence",
                    value: "Durable in this local process",
                  },
                ]}
              />
              <button
                type="button"
                className="button button--secondary"
                onClick={read.retry}
              >
                Refresh local state
              </button>
            </Card>
            {!accepted ? (
              <SourceSetup detail={detail} refresh={read.retry} />
            ) : preflight ? (
              <Card title="Accepted local source">
                <DefinitionGrid
                  items={[
                    { label: "Package", value: preflight.sourceName },
                    { label: "Bytes", value: bytes(preflight.sourceSize) },
                    {
                      label: "SHA-256",
                      value: <Hash>{preflight.sourceHash}</Hash>,
                    },
                    {
                      label: "Manifest hash",
                      value: <Hash>{preflight.manifestHash}</Hash>,
                    },
                  ]}
                />
                <p className="supporting">
                  No filesystem path is exposed and no external transmission
                  occurred.
                </p>
              </Card>
            ) : null}
            {accepted && !profiled ? (
              <Card title="Preparing source evidence">
                <p role="status">
                  Local intake and value-redacted textual profiling are in
                  progress. No invented percentage is shown.
                </p>
              </Card>
            ) : null}
            {profiled ? <ProfilingEvidence /> : null}
            {profiled ? (
              <GovernanceFlow detail={detail} refresh={read.retry} />
            ) : null}
            {state.phase === "FIRST_ISSUES_READY" &&
            state.validationStage === "COMPLETE" &&
            state.active.validationRunId !== null ? (
              <RevisedValidationPanel />
            ) : null}
          </>
        );
      }}
    </SetupBoundary>
  );
}

export function EngagementSetupPage({
  context,
  workspace,
}: {
  context: EngagementContext;
  workspace: ObjectWorkspaceStatus;
}) {
  const [created, setCreated] = useState(context.engagement !== null);
  return (
    <>
      <PageHeader
        eyebrow="Fresh project to first issues"
        title="Local migration readiness proof"
        description="Create or reopen one engagement, accept source evidence, review authority, prepare a bounded mapping, and reach the first validation issues."
      />
      <div className="setup-boundary" role="note">
        <strong>Local proof boundary</strong>
        <span>
          No external transmission. No automatic approval. No inferred business
          semantics. No target-system compatibility, migration-result, or load
          claim.
        </span>
      </div>
      <ActiveObjectWorkspace status={workspace}>
        {created ? (
          <ExistingEngagement />
        ) : (
          <CreateEngagement onCreated={() => setCreated(true)} />
        )}
      </ActiveObjectWorkspace>
    </>
  );
}

export function EngagementSetupUnavailable() {
  return (
    <>
      <PageHeader
        eyebrow="Local engagement"
        title="Setup route not found"
        description="This process exposes only the bounded setup flow and its existing issue-workbench handoff."
      />
      <EmptyState title="Unsupported local route">
        Return to <AppLink href="/setup">engagement setup</AppLink>.
      </EmptyState>
    </>
  );
}
