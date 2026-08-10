import {
  useMemo,
  useRef,
  useState,
  type ChangeEvent,
  type FormEvent,
  type ReactNode,
} from "react";
import {
  apiQuery,
  submitDestinationCommand,
  uploadDestinationArtifact,
  useRead,
} from "./api";
import {
  DESTINATION_ACKNOWLEDGEMENTS,
  DESTINATION_COMMAND_SCHEMAS,
  SCHEMAS,
  ContractError,
  type DestinationCatalogueEntityPage,
  type DestinationCatalogueProperty,
  type DestinationCataloguePropertyPage,
  type DestinationCompatibilityFinding,
  type DestinationDraftEntity,
  type DestinationDraftField,
  type DestinationDraftRelationship,
  type DestinationDraftRule,
  type DestinationDraftSnapshot,
  type DestinationCarryForwardView,
  type DestinationRebaseView,
  type DestinationRuleKind,
  type DestinationWorkshop,
  type ManualEntityDefinition,
  type ManualPropertyDefinition,
} from "./contracts";
import {
  Card,
  DefinitionGrid,
  EmptyState,
  Hash,
  PageHeader,
  ReadBoundary,
  Status,
  TableShell,
  Value,
} from "./ui";

const DESTINATION_PATH = "/api/v1/engagement/destination";
const ENTITY_PAGE_SIZE = 50;
const MAX_RELATIONSHIP_KEY_PAIRS = 16;
const LOGICAL_TYPES = [
  "text",
  "identifier",
  "code",
  "date",
  "datetime",
  "integer",
  "decimal",
  "currency",
  "boolean",
] as const;
const EDM_TYPES = [
  "Edm.String",
  "Edm.Guid",
  "Edm.Boolean",
  "Edm.Int32",
  "Edm.Int64",
  "Edm.Decimal",
  "Edm.Date",
  "Edm.DateTime",
  "Edm.DateTimeOffset",
] as const;

function canonicalJson(value: unknown): string {
  if (value === null || typeof value !== "object") {
    return JSON.stringify(value) ?? "null";
  }
  if (Array.isArray(value)) {
    return `[${value.map((item) => canonicalJson(item)).join(",")}]`;
  }
  const record = value as Record<string, unknown>;
  return `{${Object.keys(record)
    .sort()
    .map((key) => `${JSON.stringify(key)}:${canonicalJson(record[key])}`)
    .join(",")}}`;
}

function entityRenderKey(
  entity: DestinationDraftEntity,
  index: number,
): string {
  return entity.technicalEntity ?? `manual-entity-${index}`;
}

function fieldRenderKey(field: DestinationDraftField, index: number): string {
  return (
    field.technicalProperty ??
    field.manualSupplement?.supplementId ??
    `manual-field-${index}`
  );
}

function idempotencyKey(scope: string): string {
  const suffix =
    typeof crypto.randomUUID === "function"
      ? crypto.randomUUID().replaceAll("-", "")
      : `${Date.now().toString(36)}${Math.random().toString(36).slice(2)}`;
  return `${scope}:${suffix}`.slice(0, 128);
}

function useActionIdentity(scope: string) {
  const identity = useRef<string | null>(null);
  return {
    get(): string {
      identity.current ??= idempotencyKey(scope);
      return identity.current;
    },
    complete(): void {
      identity.current = null;
    },
  };
}

function safeError(error: unknown): ContractError {
  return error instanceof ContractError
    ? error
    : new ContractError(
        "UNEXPECTED_CLIENT_ERROR",
        "The local destination action could not be completed safely.",
      );
}

function ActionError({ error }: { error: ContractError | null }) {
  if (!error) return null;
  return (
    <div className="notice notice--critical" role="alert">
      <strong>{error.code}</strong>
      <p>{error.message}</p>
      {error.code === "NETWORK_UNCERTAIN" ? (
        <p>
          Refresh the durable destination state before retrying with the same
          action identity.
        </p>
      ) : null}
    </div>
  );
}

function formText(form: FormData, name: string): string {
  const value = form.get(name);
  return typeof value === "string" ? value.trim() : "";
}

function nullableText(form: FormData, name: string): string | null {
  return formText(form, name) || null;
}

function bytes(value: number): string {
  if (value < 1024) return `${value.toLocaleString("en-US")} bytes`;
  const units = ["KB", "MB", "GB"];
  let amount = value / 1024;
  let unit = 0;
  while (amount >= 1024 && unit < units.length - 1) {
    amount /= 1024;
    unit += 1;
  }
  return `${amount.toFixed(amount >= 10 ? 1 : 2)} ${units[unit]}`;
}

function canonicalId(value: string, fallback: string): string {
  const candidate = value
    .replace(/[^A-Za-z0-9._:-]+/g, "_")
    .replace(/^[_:.-]+|[_:.-]+$/g, "");
  if (!candidate) return fallback;
  return `${candidate[0]?.toLowerCase() ?? ""}${candidate.slice(1)}`.slice(
    0,
    128,
  );
}

function availableCanonicalId(
  preferred: string,
  existing: ReadonlyArray<{ fieldId: string }>,
): string {
  for (let sequence = 1; sequence <= existing.length + 1; sequence += 1) {
    const suffix = sequence === 1 ? "" : `_${sequence}`;
    const candidate = `${preferred.slice(0, 128 - suffix.length)}${suffix}`;
    if (!existing.some((field) => field.fieldId === candidate)) {
      return candidate;
    }
  }
  // The bounded draft contains fewer fields than the number of candidates
  // tested above, so this branch is unreachable unless that bound is broken.
  throw new Error("No collision-safe canonical field ID is available.");
}

function suggestedLogicalType(edmType: string): string {
  if (edmType === "Edm.Boolean") return "boolean";
  if (["Edm.Byte", "Edm.Int16", "Edm.Int32", "Edm.Int64"].includes(edmType))
    return "integer";
  if (["Edm.Decimal", "Edm.Double", "Edm.Single"].includes(edmType))
    return "decimal";
  if (edmType === "Edm.Date") return "date";
  if (["Edm.DateTime", "Edm.DateTimeOffset"].includes(edmType))
    return "datetime";
  if (edmType === "Edm.Guid") return "identifier";
  return "text";
}

function DestinationBoundary({
  children,
}: {
  children: (workshop: DestinationWorkshop, refresh: () => void) => ReactNode;
}) {
  const read = useRead<DestinationWorkshop>(
    DESTINATION_PATH,
    SCHEMAS.destinationWorkshop,
  );
  return (
    <ReadBoundary {...read}>
      {(workshop) => children(workshop, read.retry)}
    </ReadBoundary>
  );
}

function EdmxImport({
  workshop,
  refresh,
}: {
  workshop: DestinationWorkshop;
  refresh: () => void;
}) {
  const identity = useActionIdentity("destination-edmx-import");
  const formRef = useRef<HTMLFormElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [file, setFile] = useState<File | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ContractError | null>(null);

  async function submit(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    if (!file) return;
    if (
      file.size === 0 ||
      file.size > workshop.limits.maximumEdmxBytes ||
      !file.name.toLowerCase().endsWith(".edmx")
    ) {
      setError(
        new ContractError(
          "INVALID_DESTINATION_ARTIFACT",
          `Choose one non-empty .edmx file no larger than ${bytes(workshop.limits.maximumEdmxBytes)}.`,
        ),
      );
      return;
    }
    const form = new FormData(event.currentTarget);
    setBusy(true);
    setError(null);
    try {
      const key = identity.get();
      const reservation = await submitDestinationCommand({
        schemaVersion: DESTINATION_COMMAND_SCHEMAS.reserveEdmx,
        expectedEngagementGeneration: workshop.engagementGeneration,
        expectedWorkspaceId: workshop.workspaceId,
        expectedWorkspaceDescriptorHash: workshop.workspaceDescriptorHash,
        sourceName: file.name,
        artifactLabel: formText(form, "artifactLabel"),
        system: nullableText(form, "system"),
        clientOrTenant: nullableText(form, "clientOrTenant"),
        release: nullableText(form, "release"),
        idempotencyKey: key,
      });
      await uploadDestinationArtifact(reservation.attemptId, file, key);
      identity.complete();
      formRef.current?.reset();
      if (fileInputRef.current) fileInputRef.current.value = "";
      setFile(null);
      refresh();
    } catch (caught) {
      setError(safeError(caught));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card title="Import one offline EDMX" className="destination-origin-card">
      <p>
        The browser sends only the selected bytes and useful provenance. It
        never supplies or learns an arbitrary server filesystem path.
      </p>
      <form
        ref={formRef}
        className="setup-form"
        onSubmit={(event) => void submit(event)}
      >
        <label className="file-picker">
          EDMX file
          <input
            ref={fileInputRef}
            type="file"
            accept=".edmx,application/xml,text/xml"
            onChange={(event: ChangeEvent<HTMLInputElement>) => {
              setFile(event.target.files?.[0] ?? null);
              setError(null);
            }}
          />
        </label>
        <label>
          Artifact label
          <input
            name="artifactLabel"
            required
            maxLength={256}
            autoComplete="off"
            placeholder="Destination metadata extract"
          />
        </label>
        <div className="destination-form-grid">
          <label>
            System (optional)
            <input name="system" maxLength={512} autoComplete="off" />
          </label>
          <label>
            Client or tenant (optional)
            <input name="clientOrTenant" maxLength={512} autoComplete="off" />
          </label>
          <label>
            Release (optional)
            <input name="release" maxLength={512} autoComplete="off" />
          </label>
        </div>
        <p className="supporting">
          Optional provenance is bounded to 512 UTF-8 bytes per field and is
          recorded as context, not target compatibility.
        </p>
        {file ? (
          <p>
            <strong>{file.name}</strong> · {bytes(file.size)}
          </p>
        ) : null}
        <ActionError error={error} />
        <button type="submit" className="button" disabled={!file || busy}>
          {busy ? "Preserving and importing locally…" : "Import offline EDMX"}
        </button>
      </form>
    </Card>
  );
}

function blankManualProperty(): ManualPropertyDefinition {
  return {
    name: "",
    edmType: "Edm.String",
    nullable: true,
    maxLength: null,
    keyCandidate: false,
  };
}

function blankManualEntity(): ManualEntityDefinition {
  return { namespace: "", name: "", properties: [blankManualProperty()] };
}

function ManualCatalogue({
  workshop,
  refresh,
}: {
  workshop: DestinationWorkshop;
  refresh: () => void;
}) {
  const identity = useActionIdentity("destination-manual-catalogue");
  const [entities, setEntities] = useState<ManualEntityDefinition[]>([
    blankManualEntity(),
  ]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ContractError | null>(null);
  const totalProperties = entities.reduce(
    (count, entity) => count + entity.properties.length,
    0,
  );

  function updateEntity(
    entityIndex: number,
    update: (entity: ManualEntityDefinition) => ManualEntityDefinition,
  ) {
    setEntities((current) =>
      current.map((entity, index) =>
        index === entityIndex ? update(entity) : entity,
      ),
    );
  }

  function updateProperty(
    entityIndex: number,
    propertyIndex: number,
    update: (property: ManualPropertyDefinition) => ManualPropertyDefinition,
  ) {
    updateEntity(entityIndex, (entity) => ({
      ...entity,
      properties: entity.properties.map((property, index) =>
        index === propertyIndex ? update(property) : property,
      ),
    }));
  }

  async function submit(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    setBusy(true);
    setError(null);
    try {
      await submitDestinationCommand({
        schemaVersion: DESTINATION_COMMAND_SCHEMAS.createManualCatalogue,
        expectedEngagementGeneration: workshop.engagementGeneration,
        artifactLabel: formText(form, "artifactLabel"),
        actor: formText(form, "actor"),
        reason: formText(form, "reason"),
        entities,
        idempotencyKey: identity.get(),
      });
      identity.complete();
      refresh();
    } catch (caught) {
      setError(safeError(caught));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card title="Create a manual technical catalogue">
      <p>
        This bounded structured definition uses the same workshop. It is
        explicitly manual technical provenance, not arbitrary profile JSON and
        not target compatibility.
      </p>
      <form className="setup-form" onSubmit={(event) => void submit(event)}>
        <label>
          Catalogue label
          <input name="artifactLabel" required maxLength={256} />
        </label>
        {entities.map((entity, entityIndex) => (
          <fieldset
            className="destination-inline-card"
            key={`manual-entity-${entityIndex}`}
          >
            <legend>Manual entity {entityIndex + 1}</legend>
            <label>
              Namespace
              <input
                aria-label={`Manual entity ${entityIndex + 1} namespace`}
                required
                maxLength={256}
                value={entity.namespace}
                onChange={(event) =>
                  updateEntity(entityIndex, (current) => ({
                    ...current,
                    namespace: event.target.value,
                  }))
                }
              />
            </label>
            <label>
              Technical entity name
              <input
                aria-label={`Manual entity ${entityIndex + 1} technical entity name`}
                required
                maxLength={256}
                value={entity.name}
                onChange={(event) =>
                  updateEntity(entityIndex, (current) => ({
                    ...current,
                    name: event.target.value,
                  }))
                }
              />
            </label>
            {entity.properties.map((property, propertyIndex) => (
              <fieldset
                className="destination-inline-card"
                key={`manual-property-${entityIndex}-${propertyIndex}`}
              >
                <legend>
                  Property {propertyIndex + 1} · manual entity {entityIndex + 1}
                </legend>
                <label>
                  Property name
                  <input
                    aria-label={`Manual entity ${entityIndex + 1} property ${propertyIndex + 1} name`}
                    required
                    maxLength={256}
                    value={property.name}
                    onChange={(event) =>
                      updateProperty(entityIndex, propertyIndex, (current) => ({
                        ...current,
                        name: event.target.value,
                      }))
                    }
                  />
                </label>
                <label>
                  Technical type
                  <select
                    aria-label={`Manual entity ${entityIndex + 1} property ${propertyIndex + 1} technical type`}
                    value={property.edmType}
                    onChange={(event) =>
                      updateProperty(entityIndex, propertyIndex, (current) => ({
                        ...current,
                        edmType: event.target.value,
                      }))
                    }
                  >
                    {EDM_TYPES.map((type) => (
                      <option key={type} value={type}>
                        {type}
                      </option>
                    ))}
                  </select>
                </label>
                <label>
                  Maximum length (optional)
                  <input
                    aria-label={`Manual entity ${entityIndex + 1} property ${propertyIndex + 1} maximum length`}
                    type="number"
                    min={1}
                    max={1_000_000}
                    value={property.maxLength ?? ""}
                    onChange={(event) =>
                      updateProperty(entityIndex, propertyIndex, (current) => ({
                        ...current,
                        maxLength: event.target.value
                          ? Number(event.target.value)
                          : null,
                      }))
                    }
                  />
                </label>
                <label>
                  <input
                    aria-label={`Manual entity ${entityIndex + 1} property ${propertyIndex + 1} nullable technical candidate`}
                    type="checkbox"
                    checked={property.nullable}
                    onChange={(event) =>
                      updateProperty(entityIndex, propertyIndex, (current) => ({
                        ...current,
                        nullable: event.target.checked,
                      }))
                    }
                  />
                  Nullable technical candidate
                </label>
                <label>
                  <input
                    aria-label={`Manual entity ${entityIndex + 1} property ${propertyIndex + 1} technical key candidate`}
                    type="checkbox"
                    checked={property.keyCandidate}
                    onChange={(event) =>
                      updateProperty(entityIndex, propertyIndex, (current) => ({
                        ...current,
                        keyCandidate: event.target.checked,
                      }))
                    }
                  />
                  Technical key candidate
                </label>
                <button
                  aria-label={`Remove property ${propertyIndex + 1} from manual entity ${entityIndex + 1}`}
                  type="button"
                  className="button button--secondary"
                  disabled={entity.properties.length === 1}
                  onClick={() =>
                    updateEntity(entityIndex, (current) => ({
                      ...current,
                      properties: current.properties.filter(
                        (_, index) => index !== propertyIndex,
                      ),
                    }))
                  }
                >
                  Remove property
                </button>
              </fieldset>
            ))}
            <div className="destination-actions">
              <button
                aria-label={`Add property to manual entity ${entityIndex + 1}`}
                type="button"
                className="button button--secondary"
                disabled={totalProperties >= workshop.limits.maximumFields}
                onClick={() =>
                  updateEntity(entityIndex, (current) => ({
                    ...current,
                    properties: [...current.properties, blankManualProperty()],
                  }))
                }
              >
                Add property
              </button>
              <button
                aria-label={`Remove manual entity ${entityIndex + 1}`}
                type="button"
                className="button button--secondary"
                disabled={entities.length === 1}
                onClick={() =>
                  setEntities((current) =>
                    current.filter((_, index) => index !== entityIndex),
                  )
                }
              >
                Remove entity
              </button>
            </div>
          </fieldset>
        ))}
        <button
          type="button"
          className="button button--secondary"
          disabled={entities.length >= workshop.limits.maximumEntities}
          onClick={() =>
            setEntities((current) => [...current, blankManualEntity()])
          }
        >
          Add direct-child candidate
        </button>
        <label>
          Author
          <input name="actor" required maxLength={256} autoComplete="name" />
        </label>
        <label>
          Reason for manual technical definition
          <textarea name="reason" required maxLength={4096} rows={3} />
        </label>
        <ActionError error={error} />
        <button type="submit" className="button" disabled={busy}>
          {busy
            ? "Preserving manual catalogue…"
            : "Create manual technical catalogue"}
        </button>
      </form>
    </Card>
  );
}

function TechnicalEvidence({ workshop }: { workshop: DestinationWorkshop }) {
  const evidence = workshop.latestArtifact;
  if (!evidence) return null;
  return (
    <section
      className="destination-section"
      aria-labelledby="technical-evidence"
    >
      <div className="section-heading">
        <div>
          <p className="eyebrow">Immutable technical evidence</p>
          <h2 id="technical-evidence">Destination catalogue authority</h2>
          <p>
            These mechanically extracted or explicitly manual facts remain
            GENERATED_DRAFT technical evidence. They do not establish reviewed
            business meaning or target compatibility.
          </p>
        </div>
        <Status value={evidence.origin} />
      </div>
      <div className="dashboard-grid">
        <Card title={evidence.artifactLabel}>
          <DefinitionGrid
            items={[
              {
                label: "Catalogue authority",
                value: <Value>{evidence.catalogueAuthorityId}</Value>,
              },
              {
                label: "Catalogue hash",
                value: <Hash>{evidence.catalogueHash}</Hash>,
              },
              {
                label: "Catalogue payload SHA-256",
                value: <Hash>{evidence.cataloguePayloadSha256}</Hash>,
              },
              {
                label: "Catalogue file SHA-256",
                value: evidence.catalogueFileSha256 ? (
                  <Hash>{evidence.catalogueFileSha256}</Hash>
                ) : (
                  "Manual catalogue · no EDMX file"
                ),
              },
              {
                label: "Created",
                value: <Value>{evidence.createdAt}</Value>,
              },
            ]}
          />
        </Card>
        <Card title="Snapshot and parser evidence">
          <DefinitionGrid
            items={[
              {
                label: "Snapshot",
                value: evidence.snapshotId ? (
                  <>
                    <Value>{evidence.snapshotId}</Value>
                    {evidence.snapshotSha256 ? (
                      <Hash>{evidence.snapshotSha256}</Hash>
                    ) : null}
                  </>
                ) : (
                  "Manual origin"
                ),
              },
              {
                label: "Snapshot size",
                value:
                  evidence.snapshotBytes === null
                    ? "Not applicable"
                    : bytes(evidence.snapshotBytes),
              },
              {
                label: "Parser",
                value:
                  evidence.parserName && evidence.parserVersion
                    ? `${evidence.parserName} · ${evidence.parserVersion}`
                    : "Manual catalogue",
              },
              {
                label: "OData generation",
                value: evidence.odataGeneration ?? "Not applicable",
              },
              {
                label: "Entities / properties / navigations",
                value: `${evidence.entityCount} / ${evidence.propertyCount} / ${evidence.navigationCount}`,
              },
              {
                label: "Namespaces",
                value:
                  evidence.namespaces.length === 0 ? (
                    "None declared"
                  ) : (
                    <>
                      <Value>{evidence.namespaces.join(", ")}</Value>
                      <span className="muted">
                        {evidence.namespaceCount} exact{" "}
                        {evidence.namespaceCount === 1
                          ? "namespace"
                          : "namespaces"}
                      </span>
                    </>
                  ),
              },
            ]}
          />
        </Card>
        <Card title="Recorded technical provenance">
          {evidence.origin === "EDMX" ? (
            <DefinitionGrid
              items={[
                {
                  label: "System",
                  value: evidence.system ?? "Not provided",
                },
                {
                  label: "Client or tenant",
                  value: evidence.clientOrTenant ?? "Not provided",
                },
                {
                  label: "Release",
                  value: evidence.release ?? "Not provided",
                },
              ]}
            />
          ) : (
            <DefinitionGrid
              items={[
                {
                  label: "Manual definition author",
                  value: evidence.manualActor ?? "Unavailable",
                },
                {
                  label: "Manual definition reason",
                  value: evidence.manualReason ?? "Unavailable",
                },
              ]}
            />
          )}
          <p className="supporting">
            This is immutable origin context, not reviewed business meaning or
            target compatibility.
          </p>
        </Card>
      </div>
    </section>
  );
}

function CompatibilityReport({
  workshop,
  snapshot,
  setSnapshot,
}: {
  workshop: DestinationWorkshop;
  snapshot: DestinationDraftSnapshot | null;
  setSnapshot: React.Dispatch<
    React.SetStateAction<DestinationDraftSnapshot | null>
  >;
}) {
  const report = workshop.latestArtifact?.compatibility;
  if (!report) return null;
  const reportMatchesDraft =
    workshop.draft !== null &&
    workshop.latestArtifact !== null &&
    workshop.draft.catalogueAuthorityId ===
      workshop.latestArtifact.catalogueAuthorityId &&
    workshop.draft.catalogueHash === workshop.latestArtifact.catalogueHash;
  const editable =
    workshop.draft?.state === "EDITABLE" &&
    snapshot !== null &&
    reportMatchesDraft;

  function acknowledge(
    finding: DestinationCompatibilityFinding,
    event: FormEvent<HTMLFormElement>,
  ) {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    setSnapshot((current) =>
      current
        ? {
            ...current,
            compatibilityAcknowledgements: [
              ...current.compatibilityAcknowledgements.filter(
                (item) => item.findingHash !== finding.findingHash,
              ),
              {
                findingHash: finding.findingHash,
                actor: formText(form, "actor"),
                reason: formText(form, "reason"),
              },
            ],
          }
        : current,
    );
  }

  return (
    <section
      className="destination-section"
      aria-labelledby="compatibility-report"
    >
      <div className="section-heading">
        <div>
          <p className="eyebrow">Bounded completeness report</p>
          <h2 id="compatibility-report">Technical extraction boundaries</h2>
          <p>
            Unsupported constructs are inventoried without turning annotations
            or parser guesses into business authority.
          </p>
        </div>
      </div>
      <div className="dashboard-grid">
        <Card title="Safely extracted facts">
          <strong className="destination-metric">
            {report.safelyExtractedCount.toLocaleString("en-US")}
          </strong>
          <p>Bounded technical facts were extracted mechanically.</p>
        </Card>
        <Card title="Unsupported technical inventory">
          <strong className="destination-metric">
            {report.unsupportedInventoryCount.toLocaleString("en-US")}
          </strong>
          <p>
            The offline importer inventories these constructs without
            interpreting them as business authority.
          </p>
        </Card>
        {report.scopedCompletenessHash ? (
          <Card title="Outside the selected scope">
            <strong className="destination-metric">
              {report.unsupportedIrrelevantCount.toLocaleString("en-US")}
            </strong>
            <p>
              These findings do not touch the exact technical entities,
              properties or relationships selected in this draft.
            </p>
          </Card>
        ) : null}
        <Card title="Unresolved in selected scope">
          <strong className="destination-metric">
            {report.unresolvedSelectedScopeCount.toLocaleString("en-US")}
          </strong>
          <p>
            Selected-scope ambiguity cannot be acknowledged away and blocks
            profile freeze.
          </p>
        </Card>
      </div>
      {workshop.draft?.state === "EDITABLE" && !reportMatchesDraft ? (
        <p className="notice" role="status">
          This report belongs to the newer immutable catalogue. Rebase the
          durable draft before recording acknowledgements against its exact
          findings.
        </p>
      ) : null}
      {report.findings.length ? (
        <div className="destination-card-list">
          {report.findings.map((finding) => {
            const acknowledgement =
              snapshot?.compatibilityAcknowledgements.find(
                (item) => item.findingHash === finding.findingHash,
              );
            return (
              <Card
                key={finding.findingHash}
                title={`${finding.code} · ${finding.count}`}
              >
                <DefinitionGrid
                  items={[
                    { label: "Class", value: finding.classification },
                    {
                      label: "Technical location",
                      value: <Value>{finding.technicalLocation}</Value>,
                    },
                    {
                      label: "Finding hash",
                      value: <Hash>{finding.findingHash}</Hash>,
                    },
                    {
                      label: "Selected-scope blocker",
                      value: finding.blocking ? "Yes" : "No",
                    },
                  ]}
                />
                {acknowledgement ? (
                  <p className="notice notice--positive">
                    Acknowledged in this working draft by{" "}
                    {acknowledgement.actor}.
                  </p>
                ) : !finding.blocking &&
                  editable &&
                  (finding.classification === "NON_STRUCTURAL_WARNING" ||
                    finding.classification === "SELECTED_SCOPE_WARNING") ? (
                  <form
                    className="destination-form-grid"
                    onSubmit={(event) => acknowledge(finding, event)}
                  >
                    <label>
                      Acknowledging practitioner
                      <input name="actor" required maxLength={256} />
                    </label>
                    <label>
                      Reason
                      <textarea
                        name="reason"
                        required
                        maxLength={4096}
                        rows={2}
                      />
                    </label>
                    <button type="submit" className="button button--secondary">
                      Add bounded acknowledgement
                    </button>
                  </form>
                ) : null}
              </Card>
            );
          })}
        </div>
      ) : (
        <EmptyState>No unsupported construct was reported.</EmptyState>
      )}
      <p className="supporting">
        Inventory report algorithm <Value>{report.algorithm}</Value> ·{" "}
        <Hash>{report.reportHash}</Hash>
        {report.scopedCompletenessHash ? (
          <>
            {" "}
            · Scoped completeness <Hash>{report.scopedCompletenessHash}</Hash>
          </>
        ) : null}
      </p>
    </section>
  );
}

function catalogueEntitiesPath(
  workshop: DestinationWorkshop,
  catalogueAuthorityId: string,
  query: string,
  cursor: string | null,
): string {
  return apiQuery("/api/v1/engagement/destination/catalogue/entities", {
    catalogueAuthorityId,
    pageSize: Math.min(ENTITY_PAGE_SIZE, workshop.limits.maximumPageSize),
    query,
    cursor,
  });
}

function StartDraft({
  workshop,
  refresh,
}: {
  workshop: DestinationWorkshop;
  refresh: () => void;
}) {
  const evidence = workshop.latestArtifact!;
  const identity = useActionIdentity("destination-draft-start");
  const [query, setQuery] = useState("");
  const [appliedQuery, setAppliedQuery] = useState("");
  const [cursor, setCursor] = useState<string | null>(null);
  const [root, setRoot] = useState("");
  const [children, setChildren] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ContractError | null>(null);
  const entities = useRead<DestinationCatalogueEntityPage>(
    catalogueEntitiesPath(
      workshop,
      evidence.catalogueAuthorityId,
      appliedQuery,
      cursor,
    ),
    SCHEMAS.destinationCatalogueEntities,
  );

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!root) return;
    const form = new FormData(event.currentTarget);
    setBusy(true);
    setError(null);
    try {
      await submitDestinationCommand({
        schemaVersion: DESTINATION_COMMAND_SCHEMAS.startDraft,
        expectedEngagementGeneration: workshop.engagementGeneration,
        catalogueAuthorityId: evidence.catalogueAuthorityId,
        expectedCatalogueHash: evidence.catalogueHash,
        rootTechnicalEntity: root,
        childTechnicalEntities: children,
        profileId: formText(form, "profileId"),
        title: formText(form, "title"),
        owner: formText(form, "owner"),
        actor: formText(form, "actor"),
        reason: formText(form, "reason"),
        idempotencyKey: identity.get(),
      });
      identity.complete();
      refresh();
    } catch (caught) {
      setError(safeError(caught));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="destination-section" aria-labelledby="start-draft">
      <div className="section-heading">
        <div>
          <p className="eyebrow">Explicit bounded scope</p>
          <h2 id="start-draft">Start a destination profile draft</h2>
          <p>
            Select one root and optional direct children. Technical keys and
            non-null properties will be pre-activated as NEEDS_REVIEW
            suggestions, never as reviewed business truth.
          </p>
        </div>
      </div>
      <form
        className="destination-toolbar"
        onSubmit={(event) => {
          event.preventDefault();
          setCursor(null);
          setAppliedQuery(query.trim());
        }}
      >
        <label>
          Search technical entities
          <input
            value={query}
            maxLength={256}
            onChange={(event) => setQuery(event.target.value)}
          />
        </label>
        <button type="submit" className="button button--secondary">
          Apply entity search
        </button>
      </form>
      <ReadBoundary {...entities}>
        {(page) =>
          page.items.length ? (
            <>
              <TableShell label="Bounded destination entity catalogue">
                <table>
                  <thead>
                    <tr>
                      <th scope="col">Technical entity</th>
                      <th scope="col">Technical counts</th>
                      <th scope="col">Draft role</th>
                    </tr>
                  </thead>
                  <tbody>
                    {page.items.map((entity) => (
                      <tr key={entity.technicalEntity}>
                        <th scope="row">
                          {entity.name}
                          <small>{entity.technicalEntity}</small>
                        </th>
                        <td>
                          {entity.keyProperties.length} keys ·{" "}
                          {entity.propertyCount} properties ·{" "}
                          {entity.navigationCount} navigations
                        </td>
                        <td className="destination-actions">
                          <label>
                            <input
                              aria-label={`${entity.technicalEntity} as root`}
                              type="radio"
                              name="rootTechnicalEntity"
                              checked={root === entity.technicalEntity}
                              onChange={() => {
                                setRoot(entity.technicalEntity);
                                setChildren((current) =>
                                  current.filter(
                                    (item) => item !== entity.technicalEntity,
                                  ),
                                );
                              }}
                            />
                            Root
                          </label>
                          <label>
                            <input
                              aria-label={`${entity.technicalEntity} as direct child`}
                              type="checkbox"
                              checked={children.includes(
                                entity.technicalEntity,
                              )}
                              disabled={
                                root === entity.technicalEntity ||
                                (!children.includes(entity.technicalEntity) &&
                                  children.length >=
                                    workshop.limits.maximumEntities - 1)
                              }
                              onChange={(event) =>
                                setChildren((current) =>
                                  event.target.checked
                                    ? [...current, entity.technicalEntity]
                                    : current.filter(
                                        (item) =>
                                          item !== entity.technicalEntity,
                                      ),
                                )
                              }
                            />
                            Direct child
                          </label>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </TableShell>
              <div className="pagination">
                <button
                  type="button"
                  className="button button--secondary"
                  disabled={cursor === null}
                  onClick={() => setCursor(null)}
                >
                  First entity page
                </button>
                <button
                  type="button"
                  className="button button--secondary"
                  disabled={page.nextCursor === null}
                  onClick={() => setCursor(page.nextCursor)}
                >
                  Next entity page
                </button>
              </div>
            </>
          ) : (
            <EmptyState>
              No technical entity matches this bounded page.
            </EmptyState>
          )
        }
      </ReadBoundary>
      <Card title="Draft identity, scope and attribution">
        <form className="setup-form" onSubmit={(event) => void submit(event)}>
          <div className="destination-form-grid">
            <label>
              Profile ID
              <input
                name="profileId"
                required
                maxLength={128}
                pattern="[a-z0-9][a-z0-9.-]*"
              />
            </label>
            <label>
              Profile title
              <input name="title" required maxLength={512} />
            </label>
            <label>
              Profile owner
              <input name="owner" required maxLength={256} />
            </label>
            <label>
              Draft author
              <input name="actor" required maxLength={256} />
            </label>
          </div>
          <label>
            Reason for selected technical scope
            <textarea name="reason" required maxLength={4096} rows={3} />
          </label>
          <DefinitionGrid
            items={[
              {
                label: "Selected root",
                value: root ? <Value>{root}</Value> : "Choose one root",
              },
              {
                label: "Selected direct children",
                value: children.length
                  ? children.map((child) => <Value key={child}>{child}</Value>)
                  : "None",
              },
            ]}
          />
          <ActionError error={error} />
          <button type="submit" className="button" disabled={!root || busy}>
            {busy ? "Starting exact draft…" : "Start governed profile draft"}
          </button>
        </form>
      </Card>
    </section>
  );
}

function parseMaximumLength(value: string | null): number | null {
  if (value === null || !/^[1-9][0-9]*$/.test(value)) return null;
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) ? parsed : null;
}

function PropertyBrowser({
  workshop,
  snapshot,
  entity,
  setSnapshot,
}: {
  workshop: DestinationWorkshop;
  snapshot: DestinationDraftSnapshot;
  entity: DestinationDraftEntity;
  setSnapshot: React.Dispatch<
    React.SetStateAction<DestinationDraftSnapshot | null>
  >;
}) {
  const draft = workshop.draft!;
  const technicalEntity = entity.technicalEntity!;
  const [query, setQuery] = useState("");
  const [appliedQuery, setAppliedQuery] = useState("");
  const [state, setState] = useState("");
  const [cursor, setCursor] = useState<string | null>(null);
  const [manualLinks, setManualLinks] = useState<Record<string, string>>({});
  const path = apiQuery("/api/v1/engagement/destination/catalogue/properties", {
    catalogueAuthorityId: draft.catalogueAuthorityId,
    technicalEntity,
    query: appliedQuery,
    state,
    pageSize: Math.min(ENTITY_PAGE_SIZE, workshop.limits.maximumPageSize),
    cursor,
  });
  const read = useRead<DestinationCataloguePropertyPage>(
    path,
    SCHEMAS.destinationCatalogueProperties,
  );

  function updateEntity(
    update: (current: DestinationDraftEntity) => DestinationDraftEntity,
  ) {
    setSnapshot((current) =>
      current
        ? {
            ...current,
            entities: current.entities.map((candidate) =>
              candidate.entityId === entity.entityId
                ? update(candidate)
                : candidate,
            ),
          }
        : current,
    );
  }

  function activate(property: DestinationCatalogueProperty) {
    updateEntity((current) => {
      if (
        current.fields.some(
          (field) => field.technicalProperty === property.technicalProperty,
        )
      ) {
        return current;
      }
      const field: DestinationDraftField = property.field ?? {
        fieldId: availableCanonicalId(
          canonicalId(property.name, "field"),
          current.fields,
        ),
        label: property.name,
        logicalType: suggestedLogicalType(property.edmType),
        businessRequired: false,
        sensitive: false,
        key: property.keyCandidate,
        state: "NEEDS_REVIEW",
        technicalProperty: property.technicalProperty,
        manualSupplement: null,
        technicalChangeReview: null,
        confirmMaxLength: false,
        maxLength: parseMaximumLength(property.maxLength),
        diffState: null,
      };
      if (
        current.fields.some((candidate) => candidate.fieldId === field.fieldId)
      ) {
        return current;
      }
      return {
        ...current,
        fields: [...current.fields, field].sort((left, right) =>
          left.fieldId.localeCompare(right.fieldId),
        ),
      };
    });
  }

  function deactivate(property: DestinationCatalogueProperty) {
    setSnapshot((current) => {
      if (!current) return current;
      const field = current.entities
        .find((candidate) => candidate.entityId === entity.entityId)
        ?.fields.find(
          (candidate) =>
            candidate.technicalProperty === property.technicalProperty,
        );
      if (!field) return current;
      return {
        ...current,
        entities: current.entities.map((candidate) =>
          candidate.entityId === entity.entityId
            ? {
                ...candidate,
                fields: candidate.fields.filter(
                  (candidateField) => candidateField.fieldId !== field.fieldId,
                ),
              }
            : candidate,
        ),
        ...fieldDependenciesRemoved(current, entity.entityId, field.fieldId),
      };
    });
  }

  function linkManual(
    property: DestinationCatalogueProperty,
    manual: DestinationDraftField,
  ) {
    updateEntity((current) => ({
      ...current,
      fields: current.fields.map((field) =>
        field.fieldId === manual.fieldId
          ? {
              ...field,
              technicalProperty: property.technicalProperty,
              state: "NEEDS_REVIEW",
              diffState: null,
              technicalChangeReview: null,
              maxLength:
                field.maxLength ?? parseMaximumLength(property.maxLength),
            }
          : field,
      ),
    }));
  }

  function activateTechnicalCandidates(
    properties: DestinationCatalogueProperty[],
  ) {
    for (const property of properties) {
      if (property.keyCandidate || property.requiredCandidate) {
        activate(property);
      }
    }
  }

  return (
    <Card
      title={`Technical properties · ${entity.label}`}
      className="destination-property-browser"
    >
      <p>
        Only an explicit activation enters the destination profile. Exact-name
        links to a manual field also require an explicit practitioner action.
      </p>
      <form
        className="destination-toolbar"
        onSubmit={(event) => {
          event.preventDefault();
          setCursor(null);
          setAppliedQuery(query.trim());
        }}
      >
        <label>
          Search this entity
          <input
            value={query}
            maxLength={256}
            onChange={(event) => setQuery(event.target.value)}
          />
        </label>
        <label>
          Activation state
          <select
            value={state}
            onChange={(event) => {
              setCursor(null);
              setState(event.target.value);
            }}
          >
            <option value="">All states</option>
            <option value="AVAILABLE">Available</option>
            <option value="NEEDS_REVIEW">Needs review</option>
            <option value="ACTIVE">Active</option>
          </select>
        </label>
        <button type="submit" className="button button--secondary">
          Apply field filters
        </button>
      </form>
      <ReadBoundary {...read}>
        {(page) =>
          page.items.length ? (
            <>
              <button
                type="button"
                className="button button--secondary"
                onClick={() => activateTechnicalCandidates(page.items)}
              >
                Activate key/required candidates on this page
              </button>
              <TableShell label={`${entity.label} bounded property catalogue`}>
                <table>
                  <thead>
                    <tr>
                      <th scope="col">Technical property</th>
                      <th scope="col">Facets</th>
                      <th scope="col">Draft state</th>
                      <th scope="col">Action</th>
                    </tr>
                  </thead>
                  <tbody>
                    {page.items.map((property) => {
                      const active = entity.fields.find(
                        (field) =>
                          field.technicalProperty ===
                          property.technicalProperty,
                      );
                      const relationshipDependencies = active
                        ? dependentRelationshipIds(
                            snapshot,
                            entity.entityId,
                            active.fieldId,
                          )
                        : [];
                      const manualFields = entity.fields.filter(
                        (field) =>
                          field.manualSupplement !== null &&
                          field.technicalProperty === null,
                      );
                      const selectedManual =
                        manualFields.find(
                          (field) =>
                            field.fieldId ===
                            manualLinks[property.technicalProperty],
                        ) ?? null;
                      return (
                        <tr key={property.technicalProperty}>
                          <th scope="row">
                            {property.name}
                            <small>{property.technicalProperty}</small>
                          </th>
                          <td>
                            {property.edmType}
                            <small>
                              {property.keyCandidate ? "Key candidate · " : ""}
                              {property.requiredCandidate
                                ? "Required candidate · "
                                : ""}
                              nullable{" "}
                              {property.nullable === null
                                ? "unspecified"
                                : property.nullable
                                  ? "yes"
                                  : "no"}
                              {property.maxLength
                                ? ` · max ${property.maxLength}`
                                : ""}
                            </small>
                          </td>
                          <td>
                            <Status value={active?.state ?? property.state} />
                          </td>
                          <td className="destination-actions">
                            {active ? (
                              <button
                                aria-label={`Deactivate ${property.technicalProperty}`}
                                type="button"
                                className="button button--secondary"
                                disabled={relationshipDependencies.length > 0}
                                onClick={() => deactivate(property)}
                              >
                                Deactivate
                              </button>
                            ) : (
                              <button
                                aria-label={`Activate ${property.technicalProperty} for review`}
                                type="button"
                                className="button button--secondary"
                                onClick={() => activate(property)}
                              >
                                Activate for review
                              </button>
                            )}
                            {relationshipDependencies.length > 0 ? (
                              <small>
                                Remove or remap relationship{" "}
                                {relationshipDependencies.join(", ")} before
                                deactivating this field.
                              </small>
                            ) : null}
                            {!active && manualFields.length > 0 ? (
                              <>
                                <label>
                                  Manual field to link
                                  <select
                                    aria-label={`Manual field to link to ${property.technicalProperty}`}
                                    value={
                                      manualLinks[property.technicalProperty] ??
                                      ""
                                    }
                                    onChange={(event) =>
                                      setManualLinks((current) => ({
                                        ...current,
                                        [property.technicalProperty]:
                                          event.target.value,
                                      }))
                                    }
                                  >
                                    <option value="">
                                      Choose one explicit manual field
                                    </option>
                                    {manualFields.map((field) => {
                                      const nameMatch =
                                        field.fieldId.toLowerCase() ===
                                          property.name.toLowerCase() ||
                                        field.label.toLowerCase() ===
                                          property.name.toLowerCase();
                                      return (
                                        <option
                                          key={
                                            field.manualSupplement
                                              ?.supplementId ?? field.fieldId
                                          }
                                          value={field.fieldId}
                                        >
                                          {field.fieldId}
                                          {nameMatch
                                            ? " · potential name match"
                                            : ""}
                                        </option>
                                      );
                                    })}
                                  </select>
                                </label>
                                <button
                                  aria-label={`Link ${property.technicalProperty} to selected manual field`}
                                  type="button"
                                  className="button button--secondary"
                                  disabled={selectedManual === null}
                                  onClick={() => {
                                    if (selectedManual) {
                                      linkManual(property, selectedManual);
                                      setManualLinks((current) => {
                                        const next = { ...current };
                                        delete next[property.technicalProperty];
                                        return next;
                                      });
                                    }
                                  }}
                                >
                                  Link selected manual field
                                </button>
                              </>
                            ) : null}
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </TableShell>
              <div className="pagination">
                <button
                  type="button"
                  className="button button--secondary"
                  disabled={cursor === null}
                  onClick={() => setCursor(null)}
                >
                  First field page
                </button>
                <button
                  type="button"
                  className="button button--secondary"
                  disabled={page.nextCursor === null}
                  onClick={() => setCursor(page.nextCursor)}
                >
                  Next field page
                </button>
              </div>
            </>
          ) : (
            <EmptyState>
              No technical property matches this bounded page.
            </EmptyState>
          )
        }
      </ReadBoundary>
    </Card>
  );
}

function CatalogueBrowser({
  workshop,
  snapshot,
  setSnapshot,
}: {
  workshop: DestinationWorkshop;
  snapshot: DestinationDraftSnapshot;
  setSnapshot: React.Dispatch<
    React.SetStateAction<DestinationDraftSnapshot | null>
  >;
}) {
  const entityOptions = snapshot.entities.map((entity, index) => ({
    entity,
    renderKey: entityRenderKey(entity, index),
  }));
  const [selectedEntityKey, setSelectedEntityKey] = useState(
    entityOptions[0]?.renderKey ?? "",
  );
  const selectedEntity =
    entityOptions.find((item) => item.renderKey === selectedEntityKey)
      ?.entity ??
    entityOptions[0]?.entity ??
    null;
  if (!selectedEntity) return null;
  return (
    <section
      className="destination-section"
      aria-labelledby="catalogue-browser"
    >
      <div className="section-heading">
        <div>
          <p className="eyebrow">Large-catalogue activation</p>
          <h2 id="catalogue-browser">Activate deliberate destination fields</h2>
          <p>
            Browse one selected root or direct child and one bounded property
            page at a time. AVAILABLE properties remain outside the profile.
          </p>
        </div>
      </div>
      <div className="destination-actions">
        {entityOptions.map(({ entity, renderKey }) => (
          <button
            key={renderKey}
            type="button"
            className={
              renderKey ===
              entityRenderKey(
                selectedEntity,
                snapshot.entities.indexOf(selectedEntity),
              )
                ? "button"
                : "button button--secondary"
            }
            onClick={() => setSelectedEntityKey(renderKey)}
          >
            {entity.label} · {entity.root ? "root" : "direct child"}
          </button>
        ))}
      </div>
      {selectedEntity.technicalEntity ? (
        <PropertyBrowser
          key={selectedEntity.technicalEntity}
          workshop={workshop}
          snapshot={snapshot}
          entity={selectedEntity}
          setSnapshot={setSnapshot}
        />
      ) : (
        <Card title={`${selectedEntity.label} is manual`}>
          <p>
            This entity has no EDMX technical identity. Add explicitly
            attributable manual supplemental fields in the draft editor.
          </p>
        </Card>
      )}
    </section>
  );
}

function updateEntityIdentity(
  snapshot: DestinationDraftSnapshot,
  priorId: string,
  nextId: string,
): DestinationDraftSnapshot {
  if (
    (nextId !== "" && !/^[A-Za-z0-9][A-Za-z0-9_:-]*$/.test(nextId)) ||
    snapshot.entities.some(
      (entity) => entity.entityId !== priorId && entity.entityId === nextId,
    )
  ) {
    return snapshot;
  }
  return {
    ...snapshot,
    entities: snapshot.entities.map((entity) =>
      entity.entityId === priorId
        ? {
            ...entity,
            entityId: nextId,
          }
        : entity,
    ),
    relationships: snapshot.relationships.map((relationship) => ({
      ...relationship,
      fromEntityId:
        relationship.fromEntityId === priorId
          ? nextId
          : relationship.fromEntityId,
      toEntityId:
        relationship.toEntityId === priorId ? nextId : relationship.toEntityId,
    })),
    rules: snapshot.rules.map((rule) => ({
      ...rule,
      target: rule.target.startsWith(`${priorId}.`)
        ? `${nextId}.${rule.target.slice(priorId.length + 1)}`
        : rule.target,
    })),
  };
}

function fieldDependenciesRemoved(
  snapshot: DestinationDraftSnapshot,
  entityId: string,
  fieldId: string,
): Pick<DestinationDraftSnapshot, "rules" | "relationships"> {
  return {
    rules: snapshot.rules.filter((rule) => {
      if (rule.target === `${entityId}.${fieldId}`) return false;
      if (
        rule.kind === "unique" &&
        typeof rule.parameters === "object" &&
        rule.parameters !== null &&
        !Array.isArray(rule.parameters) &&
        Array.isArray(rule.parameters.scopeFields)
      ) {
        const targetEntity = rule.target.split(".", 1)[0];
        return !(
          targetEntity === entityId &&
          rule.parameters.scopeFields.includes(fieldId)
        );
      }
      return true;
    }),
    relationships: snapshot.relationships.filter(
      (relationship) =>
        !relationship.keyPairs.some(
          (pair) =>
            (relationship.fromEntityId === entityId &&
              pair.fromFieldId === fieldId) ||
            (relationship.toEntityId === entityId &&
              pair.toFieldId === fieldId),
        ),
    ),
  };
}

function dependentRelationshipIds(
  snapshot: DestinationDraftSnapshot,
  entityId: string,
  fieldId: string,
): string[] {
  return snapshot.relationships
    .filter((relationship) =>
      relationship.keyPairs.some(
        (pair) =>
          (relationship.fromEntityId === entityId &&
            pair.fromFieldId === fieldId) ||
          (relationship.toEntityId === entityId && pair.toFieldId === fieldId),
      ),
    )
    .map((relationship) => relationship.relationshipId);
}

function DraftFieldEditor({
  snapshot,
  entity,
  editable,
  setSnapshot,
}: {
  snapshot: DestinationDraftSnapshot;
  entity: DestinationDraftEntity;
  editable: boolean;
  setSnapshot: React.Dispatch<
    React.SetStateAction<DestinationDraftSnapshot | null>
  >;
}) {
  function updateField(
    priorId: string,
    update: (field: DestinationDraftField) => DestinationDraftField,
  ) {
    setSnapshot((current) => {
      if (!current) return current;
      const previous = entity.fields.find((field) => field.fieldId === priorId);
      if (!previous) return current;
      const next = update(previous);
      const target = `${entity.entityId}.${priorId}`;
      return {
        ...current,
        entities: current.entities.map((candidate) =>
          candidate.entityId === entity.entityId
            ? {
                ...candidate,
                fields: candidate.fields.map((field) =>
                  field.fieldId === priorId ? next : field,
                ),
              }
            : candidate,
        ),
        rules: current.rules.filter(
          (rule) =>
            rule.target !== target ||
            ((rule.kind !== "required" || next.businessRequired) &&
              (rule.kind !== "max_length" || next.confirmMaxLength)),
        ),
        relationships: current.relationships.map((relationship) => {
          const usesFromField =
            relationship.fromEntityId === entity.entityId &&
            relationship.keyPairs.some((pair) => pair.fromFieldId === priorId);
          const usesToField =
            relationship.toEntityId === entity.entityId &&
            relationship.keyPairs.some((pair) => pair.toFieldId === priorId);
          const relationshipStillValid =
            next.state === "ACTIVE" &&
            next.businessRequired &&
            (!usesFromField || next.key);
          return (usesFromField || usesToField) && !relationshipStillValid
            ? { ...relationship, confirmed: false }
            : relationship;
        }),
      };
    });
  }

  function removeField(fieldId: string) {
    setSnapshot((current) => {
      if (!current) return current;
      return {
        ...current,
        entities: current.entities.map((candidate) =>
          candidate.entityId === entity.entityId
            ? {
                ...candidate,
                fields: candidate.fields.filter(
                  (field) => field.fieldId !== fieldId,
                ),
              }
            : candidate,
        ),
        ...fieldDependenciesRemoved(current, entity.entityId, fieldId),
      };
    });
  }

  function renameField(priorId: string, nextId: string) {
    setSnapshot((current) =>
      current
        ? current.entities
            .find((candidate) => candidate.entityId === entity.entityId)
            ?.fields.some(
              (field) => field.fieldId !== priorId && field.fieldId === nextId,
            )
          ? current
          : {
              ...current,
              entities: current.entities.map((candidate) =>
                candidate.entityId === entity.entityId
                  ? {
                      ...candidate,
                      fields: candidate.fields.map((field) =>
                        field.fieldId === priorId
                          ? { ...field, fieldId: nextId }
                          : field,
                      ),
                    }
                  : candidate,
              ),
              rules: current.rules.map((rule) => ({
                ...rule,
                target:
                  rule.target === `${entity.entityId}.${priorId}`
                    ? `${entity.entityId}.${nextId}`
                    : rule.target,
                parameters:
                  rule.kind === "unique" &&
                  rule.target.startsWith(`${entity.entityId}.`) &&
                  typeof rule.parameters === "object" &&
                  rule.parameters !== null &&
                  !Array.isArray(rule.parameters) &&
                  Array.isArray(rule.parameters.scopeFields)
                    ? {
                        ...rule.parameters,
                        scopeFields: rule.parameters.scopeFields.map((value) =>
                          value === priorId ? nextId : value,
                        ),
                      }
                    : rule.parameters,
              })),
              relationships: current.relationships.map((relationship) => ({
                ...relationship,
                keyPairs: relationship.keyPairs.map((pair) => ({
                  fromFieldId:
                    relationship.fromEntityId === entity.entityId &&
                    pair.fromFieldId === priorId
                      ? nextId
                      : pair.fromFieldId,
                  toFieldId:
                    relationship.toEntityId === entity.entityId &&
                    pair.toFieldId === priorId
                      ? nextId
                      : pair.toFieldId,
                })),
              })),
            }
        : current,
    );
  }

  function convertToManual(
    field: DestinationDraftField,
    event: FormEvent<HTMLFormElement>,
  ) {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    updateField(field.fieldId, (current) => ({
      ...current,
      technicalProperty: null,
      manualSupplement: {
        supplementId: formText(form, "supplementId"),
        actor: formText(form, "actor"),
        reason: formText(form, "reason"),
      },
      technicalChangeReview: null,
      state: "NEEDS_REVIEW",
      diffState: null,
    }));
  }

  function acceptTechnicalChange(
    field: DestinationDraftField,
    event: FormEvent<HTMLFormElement>,
  ) {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    updateField(field.fieldId, (current) => ({
      ...current,
      state: "ACTIVE",
      technicalChangeReview: {
        decision: "ACCEPT_REVISED_TECHNICAL_EVIDENCE",
        actor: formText(form, "actor"),
        reason: formText(form, "reason"),
      },
    }));
  }

  return (
    <Card title={`${entity.label} fields`}>
      {entity.fields.length ? (
        <TableShell label={`${entity.label} destination field decisions`}>
          <table className="destination-field-editor">
            <thead>
              <tr>
                <th scope="col">Canonical field</th>
                <th scope="col">Meaning</th>
                <th scope="col">Business decisions</th>
                <th scope="col">Evidence and action</th>
              </tr>
            </thead>
            <tbody>
              {entity.fields.map((field, fieldIndex) => (
                <tr key={fieldRenderKey(field, fieldIndex)}>
                  <th scope="row">
                    <label>
                      Field ID
                      <input
                        aria-label={`Canonical field ID for ${entity.entityId}.${field.fieldId}`}
                        value={field.fieldId}
                        disabled={!editable}
                        maxLength={128}
                        onChange={(event) =>
                          renameField(field.fieldId, event.target.value)
                        }
                      />
                    </label>
                    <small>{entity.entityId}</small>
                  </th>
                  <td>
                    <label>
                      Label
                      <input
                        aria-label={`Field label for ${entity.entityId}.${field.fieldId}`}
                        value={field.label}
                        disabled={!editable}
                        maxLength={512}
                        onChange={(event) =>
                          updateField(field.fieldId, (current) => ({
                            ...current,
                            label: event.target.value,
                          }))
                        }
                      />
                    </label>
                    <label>
                      Logical type
                      <select
                        aria-label={`Logical type for ${entity.entityId}.${field.fieldId}`}
                        value={field.logicalType}
                        disabled={!editable}
                        onChange={(event) =>
                          updateField(field.fieldId, (current) => ({
                            ...current,
                            logicalType: event.target.value,
                          }))
                        }
                      >
                        {LOGICAL_TYPES.map((logicalType) => (
                          <option key={logicalType} value={logicalType}>
                            {logicalType}
                          </option>
                        ))}
                      </select>
                    </label>
                  </td>
                  <td>
                    <label>
                      <input
                        aria-label={`Business required for ${entity.entityId}.${field.fieldId}`}
                        type="checkbox"
                        checked={field.businessRequired}
                        disabled={!editable}
                        onChange={(event) =>
                          updateField(field.fieldId, (current) => ({
                            ...current,
                            businessRequired: event.target.checked,
                          }))
                        }
                      />
                      Business required
                    </label>
                    <label>
                      <input
                        aria-label={`Canonical key for ${entity.entityId}.${field.fieldId}`}
                        type="checkbox"
                        checked={field.key}
                        disabled={!editable}
                        onChange={(event) =>
                          updateField(field.fieldId, (current) => ({
                            ...current,
                            key: event.target.checked,
                          }))
                        }
                      />
                      Canonical key
                    </label>
                    <label>
                      <input
                        aria-label={`Sensitive for ${entity.entityId}.${field.fieldId}`}
                        type="checkbox"
                        checked={field.sensitive}
                        disabled={!editable}
                        onChange={(event) =>
                          updateField(field.fieldId, (current) => ({
                            ...current,
                            sensitive: event.target.checked,
                          }))
                        }
                      />
                      Sensitive
                    </label>
                    {field.maxLength !== null ? (
                      <label>
                        <input
                          aria-label={`Enforce reviewed maximum length for ${entity.entityId}.${field.fieldId}`}
                          type="checkbox"
                          checked={field.confirmMaxLength}
                          disabled={!editable}
                          onChange={(event) =>
                            updateField(field.fieldId, (current) => ({
                              ...current,
                              confirmMaxLength: event.target.checked,
                            }))
                          }
                        />
                        Enforce reviewed max length {field.maxLength}
                      </label>
                    ) : null}
                  </td>
                  <td>
                    <Status value={field.state} />
                    {field.diffState ? (
                      <Status value={field.diffState} />
                    ) : null}
                    <small>
                      {field.technicalProperty ?? "No technical property link"}
                    </small>
                    {field.manualSupplement ? (
                      <small>
                        Manual supplement {field.manualSupplement.supplementId}{" "}
                        by {field.manualSupplement.actor}
                        {field.technicalProperty
                          ? " · explicitly linked to technical evidence"
                          : ""}
                      </small>
                    ) : null}
                    {field.technicalChangeReview ? (
                      <small>
                        Revised technical evidence accepted by{" "}
                        {field.technicalChangeReview.actor}
                      </small>
                    ) : null}
                    <div className="destination-actions">
                      {field.state === "NEEDS_REVIEW" ? (
                        <button
                          type="button"
                          className="button button--secondary"
                          aria-label={`Confirm field ${entity.entityId}.${field.fieldId}`}
                          disabled={
                            !editable ||
                            (field.key && !field.businessRequired) ||
                            ["REMOVED", "TECHNICALLY_CHANGED"].includes(
                              field.diffState ?? "",
                            )
                          }
                          onClick={() =>
                            updateField(field.fieldId, (current) => ({
                              ...current,
                              state: "ACTIVE",
                            }))
                          }
                        >
                          Confirm field
                        </button>
                      ) : (
                        <button
                          type="button"
                          className="button button--secondary"
                          aria-label={`Return ${entity.entityId}.${field.fieldId} to review`}
                          disabled={!editable}
                          onClick={() =>
                            updateField(field.fieldId, (current) => ({
                              ...current,
                              state: "NEEDS_REVIEW",
                              technicalChangeReview: null,
                            }))
                          }
                        >
                          Return to review
                        </button>
                      )}
                      <button
                        type="button"
                        className="button button--secondary"
                        aria-label={`Deactivate ${entity.entityId}.${field.fieldId}`}
                        disabled={
                          !editable ||
                          entity.fields.length === 1 ||
                          dependentRelationshipIds(
                            snapshot,
                            entity.entityId,
                            field.fieldId,
                          ).length > 0
                        }
                        onClick={() => removeField(field.fieldId)}
                      >
                        Deactivate
                      </button>
                    </div>
                    {dependentRelationshipIds(
                      snapshot,
                      entity.entityId,
                      field.fieldId,
                    ).length > 0 ? (
                      <small>
                        Remove or remap relationship{" "}
                        {dependentRelationshipIds(
                          snapshot,
                          entity.entityId,
                          field.fieldId,
                        ).join(", ")}{" "}
                        before deactivating this field.
                      </small>
                    ) : null}
                    {editable &&
                    field.technicalProperty &&
                    field.diffState === "TECHNICALLY_CHANGED" &&
                    field.technicalChangeReview === null ? (
                      <form
                        className="setup-form"
                        onSubmit={(event) =>
                          acceptTechnicalChange(field, event)
                        }
                      >
                        <p>
                          The technical signature changed. Ordinary confirmation
                          cannot resolve it; deliberately accept the revised
                          evidence or use one of the alternatives below.
                        </p>
                        <label>
                          Technical-change reviewer
                          <input name="actor" required maxLength={256} />
                        </label>
                        <label>
                          Technical-change reason
                          <textarea
                            name="reason"
                            required
                            maxLength={4096}
                            rows={2}
                          />
                        </label>
                        <button
                          type="submit"
                          className="button button--secondary"
                        >
                          Accept revised technical evidence
                        </button>
                      </form>
                    ) : null}
                    {editable &&
                    field.technicalProperty &&
                    ["REMOVED", "TECHNICALLY_CHANGED"].includes(
                      field.diffState ?? "",
                    ) ? (
                      <form
                        className="setup-form"
                        onSubmit={(event) => convertToManual(field, event)}
                      >
                        <label>
                          Manual supplement ID
                          <input
                            name="supplementId"
                            required
                            maxLength={128}
                            defaultValue={`${entity.entityId}.${field.fieldId}.manual`}
                          />
                        </label>
                        <label>
                          Actor
                          <input name="actor" required maxLength={256} />
                        </label>
                        <label>
                          Reason
                          <textarea
                            name="reason"
                            required
                            maxLength={4096}
                            rows={2}
                          />
                        </label>
                        <button
                          type="submit"
                          className="button button--secondary"
                        >
                          Convert to explicit manual supplement
                        </button>
                      </form>
                    ) : null}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </TableShell>
      ) : (
        <EmptyState>This selected entity has no activated field.</EmptyState>
      )}
    </Card>
  );
}

function ManualSupplementEditor({
  workshop,
  snapshot,
  setSnapshot,
}: {
  workshop: DestinationWorkshop;
  snapshot: DestinationDraftSnapshot;
  setSnapshot: React.Dispatch<
    React.SetStateAction<DestinationDraftSnapshot | null>
  >;
}) {
  const [error, setError] = useState("");

  function add(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const entityId = formText(form, "entityId");
    const field: DestinationDraftField = {
      fieldId: formText(form, "fieldId"),
      label: formText(form, "label"),
      logicalType: formText(form, "logicalType"),
      businessRequired: false,
      sensitive: false,
      key: false,
      state: "NEEDS_REVIEW",
      technicalProperty: null,
      manualSupplement: {
        supplementId: formText(form, "supplementId"),
        actor: formText(form, "actor"),
        reason: formText(form, "reason"),
      },
      technicalChangeReview: null,
      confirmMaxLength: false,
      maxLength: null,
      diffState: null,
    };
    const targetEntity = snapshot.entities.find(
      (entity) => entity.entityId === entityId,
    );
    if (!targetEntity) {
      setError("Choose one exact selected entity.");
      return;
    }
    if (
      targetEntity.fields.some(
        (candidate) => candidate.fieldId === field.fieldId,
      )
    ) {
      setError(
        `Canonical field ${entityId}.${field.fieldId} already exists. Choose a new field ID; this action never replaces prior work.`,
      );
      return;
    }
    if (
      snapshot.entities.some((entity) =>
        entity.fields.some(
          (candidate) =>
            candidate.manualSupplement?.supplementId ===
            field.manualSupplement?.supplementId,
        ),
      )
    ) {
      setError(
        "That manual supplement ID already exists. Choose a new immutable supplement identity.",
      );
      return;
    }
    const totalFields = snapshot.entities.reduce(
      (count, entity) => count + entity.fields.length,
      0,
    );
    if (totalFields >= workshop.limits.maximumFields) {
      setError("The bounded destination field limit has been reached.");
      return;
    }
    setSnapshot((current) =>
      current
        ? {
            ...current,
            entities: current.entities.map((entity) =>
              entity.entityId === entityId
                ? {
                    ...entity,
                    fields: [...entity.fields, field],
                  }
                : entity,
            ),
          }
        : current,
    );
    setError("");
    event.currentTarget.reset();
  }
  return (
    <Card title="Add a manual supplemental field">
      <p>
        The field is visibly not EDMX-proven and carries its own actor, reason
        and immutable supplement identity.
      </p>
      <form className="destination-form-grid" onSubmit={add}>
        <label>
          Entity
          <select name="entityId" required>
            <option value="">Choose selected entity</option>
            {snapshot.entities.map((entity) => (
              <option key={entity.entityId} value={entity.entityId}>
                {entity.label}
              </option>
            ))}
          </select>
        </label>
        <label>
          Supplement ID
          <input
            name="supplementId"
            required
            maxLength={128}
            pattern="[A-Za-z0-9][A-Za-z0-9._:-]*"
          />
        </label>
        <label>
          Canonical field ID
          <input
            name="fieldId"
            required
            maxLength={128}
            pattern="[A-Za-z0-9][A-Za-z0-9._:-]*"
          />
        </label>
        <label>
          Label
          <input name="label" required maxLength={512} />
        </label>
        <label>
          Logical type
          <select name="logicalType" defaultValue="text">
            {LOGICAL_TYPES.map((logicalType) => (
              <option key={logicalType} value={logicalType}>
                {logicalType}
              </option>
            ))}
          </select>
        </label>
        <label>
          Actor
          <input name="actor" required maxLength={256} />
        </label>
        <label>
          Reason
          <textarea name="reason" required maxLength={4096} rows={2} />
        </label>
        {error ? (
          <p className="notice notice--critical" role="alert">
            {error}
          </p>
        ) : null}
        <button type="submit" className="button button--secondary">
          Add manual field for review
        </button>
      </form>
    </Card>
  );
}

function ruleParameters(
  kind: DestinationRuleKind,
  raw: string,
  targetField: string,
  form: FormData,
): DestinationDraftRule["parameters"] | null {
  switch (kind) {
    case "required":
      return {};
    case "max_length": {
      const maximum = Number(raw);
      return Number.isInteger(maximum) && maximum > 0 && maximum <= 1_000_000
        ? { maximum }
        : null;
    }
    case "allowed_values": {
      const values = [
        ...new Set(raw.split(",").map((item) => item.trim())),
      ].filter(Boolean);
      return values.length ? { values } : null;
    }
    case "pattern":
      return raw ? { pattern: raw } : null;
    case "unique": {
      const scopeFields = [
        ...new Set(raw.split(",").map((item) => item.trim())),
      ].filter(Boolean);
      return scopeFields.includes(targetField) ? { scopeFields } : null;
    }
    case "conditional_required": {
      const whenField = formText(form, "whenField");
      const equals = formText(form, "equals");
      return whenField && equals ? { whenField, equals } : null;
    }
    case "reference_lookup": {
      const tableId = formText(form, "tableId");
      const tableVersion = formText(form, "tableVersion");
      return tableId && tableVersion ? { tableId, tableVersion } : null;
    }
  }
}

function RuleEditor({
  workshop,
  snapshot,
  setSnapshot,
}: {
  workshop: DestinationWorkshop;
  snapshot: DestinationDraftSnapshot;
  setSnapshot: React.Dispatch<
    React.SetStateAction<DestinationDraftSnapshot | null>
  >;
}) {
  const [error, setError] = useState("");
  const [selectedTarget, setSelectedTarget] = useState("");
  const [selectedKind, setSelectedKind] =
    useState<DestinationRuleKind>("required");
  const [formGeneration, setFormGeneration] = useState(0);
  const targets = snapshot.entities.flatMap((entity) =>
    entity.fields.map((field) => ({
      target: `${entity.entityId}.${field.fieldId}`,
      entity,
      field,
    })),
  );
  const selectedTargetEntity =
    targets.find((candidate) => candidate.target === selectedTarget)?.entity
      .entityId ?? null;
  const sameEntityConditionTargets =
    selectedTargetEntity === null
      ? []
      : targets.filter(
          (candidate) => candidate.entity.entityId === selectedTargetEntity,
        );

  function add(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const target = formText(form, "target");
    const selected = targets.find((candidate) => candidate.target === target);
    const kind = formText(form, "kind") as DestinationRuleKind;
    if (!selected) {
      setError("Choose one exact entity.field target.");
      return;
    }
    if (kind === "required" && !selected.field.businessRequired) {
      setError(
        "A required rule may be added only after the practitioner marks the field business-required.",
      );
      return;
    }
    if (kind === "max_length" && !selected.field.confirmMaxLength) {
      setError(
        "A maximum-length rule may be added only after the practitioner confirms that technical limit for enforcement.",
      );
      return;
    }
    const parameters = ruleParameters(
      kind,
      formText(form, "parameters"),
      selected.field.fieldId,
      form,
    );
    if (parameters === null) {
      setError(
        kind === "unique"
          ? "Unique scope must be a comma-separated field list that includes the target field."
          : "The bounded rule parameters are incomplete.",
      );
      return;
    }
    const rule: DestinationDraftRule = {
      ruleId: formText(form, "ruleId"),
      severity: formText(form, "severity"),
      kind,
      target,
      message: formText(form, "message"),
      parameters,
    };
    event.currentTarget.reset();
    setSelectedTarget("");
    setSelectedKind("required");
    setFormGeneration((current) => current + 1);
    setSnapshot((current) =>
      current
        ? {
            ...current,
            rules: [
              ...current.rules.filter(
                (candidate) => candidate.ruleId !== rule.ruleId,
              ),
              rule,
            ],
          }
        : current,
    );
    setError("");
  }

  return (
    <Card title="Bounded validation rules">
      <p>
        Logical types are profile meaning, not value validation. Add only the
        currently supported explicit rules; required and max-length claims
        require the corresponding field decisions first.
      </p>
      {snapshot.rules.length ? (
        <ul className="rule-list">
          {snapshot.rules.map((rule) => (
            <li key={rule.ruleId}>
              <strong>
                {rule.ruleId} · {rule.kind}
              </strong>
              <span>
                {rule.target} · {rule.message}
              </span>
              <button
                type="button"
                className="button button--secondary"
                onClick={() =>
                  setSnapshot((current) =>
                    current
                      ? {
                          ...current,
                          rules: current.rules.filter(
                            (candidate) => candidate.ruleId !== rule.ruleId,
                          ),
                        }
                      : current,
                  )
                }
              >
                Remove rule
              </button>
            </li>
          ))}
        </ul>
      ) : (
        <p>No explicit validation rule is in this draft.</p>
      )}
      <form
        key={formGeneration}
        className="destination-form-grid"
        onSubmit={add}
      >
        <label>
          Rule ID
          <input
            name="ruleId"
            required
            maxLength={128}
            pattern="[A-Za-z0-9][A-Za-z0-9._:-]*"
          />
        </label>
        <label>
          Target field
          <select
            name="target"
            required
            value={selectedTarget}
            onChange={(event) => setSelectedTarget(event.target.value)}
          >
            <option value="">Choose entity.field</option>
            {targets.map(({ target }) => (
              <option key={target} value={target}>
                {target}
              </option>
            ))}
          </select>
        </label>
        <label>
          Rule kind
          <select
            name="kind"
            required
            value={selectedKind}
            onChange={(event) =>
              setSelectedKind(event.target.value as DestinationRuleKind)
            }
          >
            <option value="required">Required</option>
            <option value="max_length">Maximum length</option>
            <option value="unique">Unique</option>
            <option value="allowed_values">Allowed values</option>
            <option value="pattern">Pattern</option>
            <option value="conditional_required">Conditionally required</option>
            <option value="reference_lookup">Reference lookup</option>
          </select>
        </label>
        <label>
          Severity
          <select name="severity" required defaultValue="error">
            <option value="error">Error</option>
            <option value="warning">Warning</option>
            <option value="information">Information</option>
          </select>
        </label>
        {selectedKind === "conditional_required" ? (
          <>
            <label>
              Condition field
              <select name="whenField" required>
                <option value="">Choose a field from the same entity</option>
                {sameEntityConditionTargets.map(({ target }) => (
                  <option key={target} value={target}>
                    {target}
                  </option>
                ))}
              </select>
            </label>
            <label>
              Condition equals
              <input name="equals" required maxLength={4096} />
            </label>
          </>
        ) : selectedKind === "reference_lookup" ? (
          <>
            <label>
              Required reference table ID
              <input
                name="tableId"
                required
                maxLength={128}
                pattern="[a-z0-9][a-z0-9.-]*"
              />
            </label>
            <label>
              Required reference table version
              <input
                name="tableVersion"
                required
                maxLength={32}
                pattern="\d+\.\d+\.\d+"
              />
            </label>
          </>
        ) : (
          <label>
            Parameters
            <input
              name="parameters"
              maxLength={4096}
              placeholder="Maximum, values, pattern, or unique scope fields"
            />
          </label>
        )}
        <label>
          Message
          <input name="message" required maxLength={1024} />
        </label>
        {error ? (
          <p className="notice notice--critical" role="alert">
            {error}
          </p>
        ) : null}
        <button
          type="submit"
          className="button button--secondary"
          disabled={snapshot.rules.length >= workshop.limits.maximumRules}
        >
          Add bounded rule
        </button>
      </form>
    </Card>
  );
}

function RelationshipEditor({
  workshop,
  snapshot,
  setSnapshot,
}: {
  workshop: DestinationWorkshop;
  snapshot: DestinationDraftSnapshot;
  setSnapshot: React.Dispatch<
    React.SetStateAction<DestinationDraftSnapshot | null>
  >;
}) {
  const root = snapshot.entities.find((entity) => entity.root) ?? null;
  const children = snapshot.entities.filter((entity) => !entity.root);
  const childOptions = children.map((entity, index) => ({
    entity,
    renderKey: entityRenderKey(entity, index),
  }));
  const [selectedChildKey, setSelectedChildKey] = useState(
    childOptions[0]?.renderKey ?? "",
  );
  const selectedChild =
    childOptions.find((item) => item.renderKey === selectedChildKey)?.entity ??
    childOptions[0]?.entity ??
    null;
  const [keyPairs, setKeyPairs] = useState<
    DestinationDraftRelationship["keyPairs"]
  >([{ fromFieldId: "", toFieldId: "" }]);

  function updateKeyPair(
    index: number,
    side: "fromFieldId" | "toFieldId",
    value: string,
  ) {
    setKeyPairs((current) =>
      current.map((pair, pairIndex) =>
        pairIndex === index ? { ...pair, [side]: value } : pair,
      ),
    );
  }

  function resetKeyPairs() {
    setKeyPairs([{ fromFieldId: "", toFieldId: "" }]);
  }

  const rootRelationshipFields =
    root?.fields.filter(
      (field) =>
        field.key && field.businessRequired && field.state === "ACTIVE",
    ) ?? [];
  const childRelationshipFields =
    selectedChild?.fields.filter(
      (field) => field.businessRequired && field.state === "ACTIVE",
    ) ?? [];

  const relationshipLimitReached =
    snapshot.relationships.length >= workshop.limits.maximumRelationships;

  function appendKeyPair() {
    setKeyPairs((current) =>
      current.length >= MAX_RELATIONSHIP_KEY_PAIRS
        ? current
        : [...current, { fromFieldId: "", toFieldId: "" }],
    );
  }

  function removeKeyPair(index: number) {
    setKeyPairs((current) =>
      current.length === 1
        ? current
        : current.filter((_, pairIndex) => pairIndex !== index),
    );
  }

  function add(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!root || !selectedChild) return;
    const form = new FormData(event.currentTarget);
    const relationship: DestinationDraftRelationship = {
      relationshipId: formText(form, "relationshipId"),
      fromEntityId: root.entityId,
      toEntityId: selectedChild.entityId,
      cardinality: formText(form, "cardinality") as "0..*" | "1..*",
      technicalNavigation: nullableText(form, "technicalNavigation"),
      keyPairs,
      confirmed: form.get("confirmed") === "on",
    };
    setSnapshot((current) =>
      current
        ? {
            ...current,
            relationships: [
              ...current.relationships.filter(
                (candidate) =>
                  candidate.relationshipId !== relationship.relationshipId,
              ),
              relationship,
            ],
          }
        : current,
    );
    event.currentTarget.reset();
    resetKeyPairs();
  }

  return (
    <Card title="Explicit root-to-child relationships">
      <p>
        Relationships never come from shared names. Confirm the exact canonical
        key pairs and bounded cardinality; an optional technical navigation is
        evidence, not the decision itself.
      </p>
      {snapshot.relationships.length ? (
        <ul className="rule-list">
          {snapshot.relationships.map((relationship) => (
            <li key={relationship.relationshipId}>
              <strong>
                {relationship.relationshipId} · {relationship.cardinality}
              </strong>
              <span>
                {relationship.keyPairs.map((pair, index) => (
                  <span
                    key={`${index}\u0000${pair.fromFieldId}\u0000${pair.toFieldId}`}
                  >
                    {index > 0 ? " · " : ""}
                    {relationship.fromEntityId}.{pair.fromFieldId} ={" "}
                    {relationship.toEntityId}.{pair.toFieldId}
                  </span>
                ))}
              </span>
              <small>
                {relationship.confirmed
                  ? "Practitioner confirmed"
                  : "Needs explicit confirmation"}
              </small>
              <button
                type="button"
                className="button button--secondary"
                onClick={() =>
                  setSnapshot((current) =>
                    current
                      ? {
                          ...current,
                          relationships: current.relationships.filter(
                            (candidate) =>
                              candidate.relationshipId !==
                              relationship.relationshipId,
                          ),
                        }
                      : current,
                  )
                }
              >
                Remove relationship
              </button>
            </li>
          ))}
        </ul>
      ) : (
        <p>No direct-child relationship is in this draft.</p>
      )}
      <form className="destination-form-grid" onSubmit={add}>
        <label>
          Relationship ID
          <input
            name="relationshipId"
            required
            maxLength={128}
            pattern="[A-Za-z0-9][A-Za-z0-9._:-]*"
          />
        </label>
        <p>
          Root entity <Value>{root?.entityId ?? "Choose a root first"}</Value>
        </p>
        <label>
          Direct child
          <select
            required
            value={selectedChildKey}
            onChange={(event) => {
              setSelectedChildKey(event.target.value);
              resetKeyPairs();
            }}
          >
            <option value="">Choose direct child</option>
            {childOptions.map(({ entity, renderKey }) => (
              <option key={renderKey} value={renderKey}>
                {entity.entityId}
              </option>
            ))}
          </select>
        </label>
        <fieldset className="destination-inline-card">
          <legend>Confirmed relationship key pairs</legend>
          {keyPairs.map((pair, index) => (
            <fieldset
              className="destination-inline-card"
              key={`relationship-key-pair-${index}`}
            >
              <legend>Key pair {index + 1}</legend>
              <label>
                Root canonical key · pair {index + 1}
                <select
                  required
                  value={pair.fromFieldId}
                  onChange={(event) =>
                    updateKeyPair(index, "fromFieldId", event.target.value)
                  }
                >
                  <option value="">Choose active required root key</option>
                  {rootRelationshipFields.map((field) => (
                    <option key={field.fieldId} value={field.fieldId}>
                      {root?.entityId}.{field.fieldId}
                    </option>
                  ))}
                </select>
              </label>
              <label>
                Child relationship field · pair {index + 1}
                <select
                  required
                  value={pair.toFieldId}
                  onChange={(event) =>
                    updateKeyPair(index, "toFieldId", event.target.value)
                  }
                >
                  <option value="">
                    Choose active business-required child field
                  </option>
                  {childRelationshipFields.map((field) => (
                    <option key={field.fieldId} value={field.fieldId}>
                      {selectedChild?.entityId}.{field.fieldId}
                      {field.key ? " · canonical key" : " · relationship field"}
                    </option>
                  ))}
                </select>
              </label>
              <button
                type="button"
                className="button button--secondary"
                disabled={keyPairs.length === 1}
                onClick={() => removeKeyPair(index)}
              >
                Remove key pair {index + 1}
              </button>
            </fieldset>
          ))}
          <button
            type="button"
            className="button button--secondary"
            disabled={keyPairs.length >= MAX_RELATIONSHIP_KEY_PAIRS}
            onClick={appendKeyPair}
          >
            Add relationship key pair
          </button>
        </fieldset>
        <label>
          Cardinality
          <select name="cardinality" defaultValue="0..*" required>
            <option value="0..*">0..*</option>
            <option value="1..*">1..*</option>
          </select>
        </label>
        <label>
          Technical navigation (optional)
          <input name="technicalNavigation" maxLength={512} />
        </label>
        <label>
          <input name="confirmed" type="checkbox" required />I confirm this
          exact key-pair set and cardinality.
        </label>
        <button
          type="submit"
          className="button button--secondary"
          disabled={
            !root ||
            selectedChild === null ||
            relationshipLimitReached ||
            rootRelationshipFields.length === 0 ||
            childRelationshipFields.length === 0
          }
        >
          Add confirmed relationship
        </button>
      </form>
    </Card>
  );
}

function DraftEditor({
  workshop,
  snapshot,
  snapshotMatchesDurableDraft,
  setSnapshot,
  refresh,
}: {
  workshop: DestinationWorkshop;
  snapshot: DestinationDraftSnapshot;
  snapshotMatchesDurableDraft: boolean;
  setSnapshot: React.Dispatch<
    React.SetStateAction<DestinationDraftSnapshot | null>
  >;
  refresh: () => void;
}) {
  const draft = workshop.draft!;
  const identity = useActionIdentity("destination-draft-save");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ContractError | null>(null);
  const editable = draft.state === "EDITABLE";

  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    setBusy(true);
    setError(null);
    try {
      await submitDestinationCommand({
        schemaVersion: DESTINATION_COMMAND_SCHEMAS.saveDraft,
        expectedEngagementGeneration: workshop.engagementGeneration,
        draftId: draft.draftId,
        expectedDraftGeneration: draft.generation,
        expectedDraftHash: draft.draftHash,
        snapshot,
        actor: formText(form, "actor"),
        reason: formText(form, "reason"),
        idempotencyKey: identity.get(),
      });
      identity.complete();
      refresh();
    } catch (caught) {
      setError(safeError(caught));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="destination-section" aria-labelledby="profile-draft">
      <div className="section-heading">
        <div>
          <p className="eyebrow">Operator-governed meaning</p>
          <h2 id="profile-draft">Destination profile working draft</h2>
          <p>
            Canonical names, business requiredness, sensitivity, keys,
            relationships and rules become reviewed meaning only through this
            append-only draft chain.
          </p>
        </div>
        <Status value={draft.state} />
      </div>
      <Card title="Profile identity and owner">
        <div className="destination-form-grid">
          <label>
            Profile ID
            <input
              value={snapshot.profileId}
              disabled={!editable}
              maxLength={128}
              onChange={(event) =>
                setSnapshot((current) =>
                  current
                    ? { ...current, profileId: event.target.value }
                    : current,
                )
              }
            />
          </label>
          <label>
            Title
            <input
              value={snapshot.title}
              disabled={!editable}
              maxLength={512}
              onChange={(event) =>
                setSnapshot((current) =>
                  current ? { ...current, title: event.target.value } : current,
                )
              }
            />
          </label>
          <label>
            Owner
            <input
              value={snapshot.owner}
              disabled={!editable}
              maxLength={256}
              onChange={(event) =>
                setSnapshot((current) =>
                  current ? { ...current, owner: event.target.value } : current,
                )
              }
            />
          </label>
        </div>
      </Card>
      <Card title="Canonical entities">
        <div className="destination-card-list">
          {snapshot.entities.map((entity, entityIndex) => (
            <div
              className="destination-inline-card"
              key={entityRenderKey(entity, entityIndex)}
            >
              <label>
                Canonical entity ID
                <input
                  aria-label={`Canonical entity ID for ${entity.label}`}
                  value={entity.entityId}
                  disabled={!editable}
                  maxLength={128}
                  pattern="[A-Za-z0-9][A-Za-z0-9_:-]*"
                  onChange={(event) =>
                    setSnapshot((current) =>
                      current
                        ? updateEntityIdentity(
                            current,
                            entity.entityId,
                            event.target.value,
                          )
                        : current,
                    )
                  }
                />
              </label>
              <label>
                Label
                <input
                  aria-label={`Canonical entity label for ${entity.entityId}`}
                  value={entity.label}
                  disabled={!editable}
                  maxLength={512}
                  onChange={(event) =>
                    setSnapshot((current) =>
                      current
                        ? {
                            ...current,
                            entities: current.entities.map((candidate) =>
                              candidate.entityId === entity.entityId
                                ? {
                                    ...candidate,
                                    label: event.target.value,
                                  }
                                : candidate,
                            ),
                          }
                        : current,
                    )
                  }
                />
              </label>
              <Status value={entity.root ? "ROOT" : "DIRECT CHILD"} />
              <small>
                {entity.technicalEntity ?? "Manual technical entity"}
              </small>
            </div>
          ))}
        </div>
      </Card>
      {snapshot.entities.map((entity, entityIndex) => (
        <DraftFieldEditor
          key={entityRenderKey(entity, entityIndex)}
          snapshot={snapshot}
          entity={entity}
          editable={editable}
          setSnapshot={setSnapshot}
        />
      ))}
      {editable ? (
        <>
          <ManualSupplementEditor
            workshop={workshop}
            snapshot={snapshot}
            setSnapshot={setSnapshot}
          />
          <RuleEditor
            workshop={workshop}
            snapshot={snapshot}
            setSnapshot={setSnapshot}
          />
          <RelationshipEditor
            workshop={workshop}
            snapshot={snapshot}
            setSnapshot={setSnapshot}
          />
        </>
      ) : null}
      <Card title="Durable draft generation">
        {!snapshotMatchesDurableDraft ? (
          <div className="notice notice--critical" role="status">
            <strong>Unsaved local draft changes</strong>
            <p>
              The fields shown above are not yet bound to the durable generation
              and hash below. Save this working draft before any freeze, rebase,
              approval, or mapping-authority transition.
            </p>
          </div>
        ) : (
          <p className="notice notice--positive" role="status">
            The displayed draft exactly matches this durable generation and
            hash.
          </p>
        )}
        <DefinitionGrid
          items={[
            { label: "Draft", value: <Value>{draft.draftId}</Value> },
            { label: "Generation", value: draft.generation },
            { label: "Draft hash", value: <Hash>{draft.draftHash}</Hash> },
            {
              label: "Catalogue",
              value: (
                <>
                  <Value>{draft.catalogueAuthorityId}</Value>
                  <Hash>{draft.catalogueHash}</Hash>
                </>
              ),
            },
            { label: "Updated", value: <Value>{draft.updatedAt}</Value> },
          ]}
        />
        {editable ? (
          <form className="setup-form" onSubmit={(event) => void save(event)}>
            <label>
              Revision author
              <input name="actor" required maxLength={256} />
            </label>
            <label>
              Revision reason
              <textarea name="reason" required maxLength={4096} rows={3} />
            </label>
            <ActionError error={error} />
            <button
              type="submit"
              className="button"
              disabled={busy || snapshotMatchesDurableDraft}
            >
              {busy ? "Saving exact draft revision…" : "Save working draft"}
            </button>
          </form>
        ) : null}
      </Card>
    </section>
  );
}

function RebasePanel({
  workshop,
  snapshotMatchesDurableDraft,
  refresh,
}: {
  workshop: DestinationWorkshop;
  snapshotMatchesDurableDraft: boolean;
  refresh: () => void;
}) {
  const identity = useActionIdentity("destination-draft-rebase");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ContractError | null>(null);
  const [result, setResult] = useState<DestinationRebaseView | null>(null);
  const draft = workshop.draft;
  const target = workshop.latestArtifact;
  if (
    !draft ||
    !target ||
    draft.catalogueAuthorityId === target.catalogueAuthorityId
  ) {
    return null;
  }
  const activeDraft = draft;
  const activeTarget = target;

  async function rebase(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    setBusy(true);
    setError(null);
    try {
      const response = await submitDestinationCommand({
        schemaVersion: DESTINATION_COMMAND_SCHEMAS.rebaseDraft,
        expectedEngagementGeneration: workshop.engagementGeneration,
        draftId: activeDraft.draftId,
        expectedDraftGeneration: activeDraft.generation,
        expectedDraftHash: activeDraft.draftHash,
        targetCatalogueAuthorityId: activeTarget.catalogueAuthorityId,
        expectedTargetCatalogueHash: activeTarget.catalogueHash,
        actor: formText(form, "actor"),
        reason: formText(form, "reason"),
        idempotencyKey: identity.get(),
      });
      identity.complete();
      setResult(response);
    } catch (caught) {
      setError(safeError(caught));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card title="Rebase onto the latest immutable catalogue">
      <DefinitionGrid
        items={[
          {
            label: "Current draft catalogue",
            value: (
              <>
                <Value>{activeDraft.catalogueAuthorityId}</Value>
                <Hash>{activeDraft.catalogueHash}</Hash>
              </>
            ),
          },
          {
            label: "New catalogue",
            value: (
              <>
                <Value>{activeTarget.catalogueAuthorityId}</Value>
                <Hash>{activeTarget.catalogueHash}</Hash>
              </>
            ),
          },
        ]}
      />
      <p>
        Only exact fully-qualified technical identities carry automatically.
        Added key/required candidates enter NEEDS_REVIEW; removed, changed and
        potential manual matches remain unresolved until the practitioner acts.
        The prior snapshot, draft and approved profile remain immutable.
      </p>
      {!snapshotMatchesDurableDraft ? (
        <p className="notice notice--critical" role="status">
          Save or discard the visible local changes before rebasing. The rebase
          command is bound to the last durable draft hash, never unsaved screen
          state.
        </p>
      ) : null}
      {result ? (
        <div className="notice notice--positive" role="status">
          <strong>New editable draft created</strong>
          <p>
            Rebase <Value>{result.rebaseId}</Value> ·{" "}
            <Hash>{result.diffHash}</Hash>
          </p>
          <ul>
            {Object.entries(result.counts).map(([state, count]) => (
              <li key={state}>
                {state}: {count}
              </li>
            ))}
          </ul>
          <p>
            New draft <Value>{result.draft.draftId}</Value> ·{" "}
            <Hash>{result.draft.draftHash}</Hash>
          </p>
          <button type="button" className="button" onClick={refresh}>
            Open rebased working draft
          </button>
        </div>
      ) : (
        <form className="setup-form" onSubmit={(event) => void rebase(event)}>
          <label>
            Rebase operator
            <input name="actor" required maxLength={256} />
          </label>
          <label>
            Rebase reason
            <textarea name="reason" required maxLength={4096} rows={3} />
          </label>
          <ActionError error={error} />
          <button
            type="submit"
            className="button"
            disabled={busy || !snapshotMatchesDurableDraft}
          >
            {busy
              ? "Creating deterministic rebase…"
              : "Rebase destination draft onto this snapshot"}
          </button>
        </form>
      )}
    </Card>
  );
}

function explainCarryForwardRelationship(
  reason: DestinationCarryForwardView["unresolvedRelationships"][number]["reason"],
): string {
  switch (reason) {
    case "RELATIONSHIP_KEY_BINDING_UNRESOLVED":
      return "One or more relationship key fields do not have an exact compatible direct binding.";
    case "RELATIONSHIP_CHANGED":
      return "The relationship exists in both profiles, but its exact entities, cardinality or key pairs changed.";
    case "RELATIONSHIP_NOT_PRESENT":
      return "The target relationship was not present in the prior approved mapping.";
  }
}

function MappingCarryForward({ workshop }: { workshop: DestinationWorkshop }) {
  const identity = useActionIdentity("destination-mapping-carry-forward");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ContractError | null>(null);
  const [result, setResult] = useState<DestinationCarryForwardView | null>(
    null,
  );
  const active = workshop.activeDestination;
  const candidate = workshop.reviewedCandidate;
  const eligibleSource = workshop.eligibleCarryForwardSource;
  if (
    !active ||
    active.profileAuthority !== "LOCAL_MAPPING_VALIDATION" ||
    !candidate ||
    candidate.profile.authority !== "LOCAL_MAPPING_VALIDATION" ||
    (candidate.profile.profile_id === active.profileId &&
      candidate.profile.profile_version === active.profileVersion &&
      candidate.profile.profile_hash === active.profileHash)
  )
    return null;
  const approvedCandidate = candidate;

  if (!eligibleSource) {
    return (
      <Card title="Mapping carry-forward unavailable">
        <p>
          The approved candidate does not replace the active profile. This
          engagement has no independently verified active mapping approval and
          validated source context that can be carried forward. Keep using the
          existing authority or prepare a new mapping through its ordinary
          governed path.
        </p>
      </Card>
    );
  }
  const verifiedSource = eligibleSource;

  async function carry(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    setBusy(true);
    setError(null);
    try {
      const response = await submitDestinationCommand({
        schemaVersion: DESTINATION_COMMAND_SCHEMAS.carryForwardMapping,
        expectedEngagementGeneration: workshop.engagementGeneration,
        sourceMappingId: verifiedSource.mappingId,
        sourceMappingVersion: verifiedSource.mappingVersion,
        sourceMappingHash: verifiedSource.mappingHash,
        targetProfileId: approvedCandidate.profile.profile_id,
        targetProfileVersion: approvedCandidate.profile.profile_version,
        targetProfileHash: approvedCandidate.profile.profile_hash,
        actor: formText(form, "actor"),
        reason: formText(form, "reason"),
        idempotencyKey: identity.get(),
      });
      identity.complete();
      setResult(response);
    } catch (caught) {
      setError(safeError(caught));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card title="Optional explicit mapping carry-forward">
      <p>
        The active profile and mapping remain unchanged until this explicit
        action succeeds. The server independently verified the exact prior
        mapping approval and validated source context shown below. The result is
        a new editable draft; unresolved fields and relationships still require
        ordinary review, freeze and approval.
      </p>
      <DefinitionGrid
        items={[
          {
            label: "Current active profile",
            value: `${active.profileId} · ${active.profileVersion}`,
          },
          {
            label: "Approved candidate",
            value: `${candidate.profile.profile_id} · ${candidate.profile.profile_version}`,
          },
          {
            label: "Candidate profile hash",
            value: <Hash>{candidate.profile.profile_hash}</Hash>,
          },
          {
            label: "Eligible mapping",
            value: `${eligibleSource.mappingId} · ${eligibleSource.mappingVersion}`,
          },
          {
            label: "Eligible mapping hash",
            value: <Hash>{eligibleSource.mappingHash}</Hash>,
          },
          {
            label: "Mapping approval",
            value: (
              <>
                <Value>{eligibleSource.mappingApprovalId}</Value>
                <Hash>{eligibleSource.mappingApprovalHash}</Hash>
              </>
            ),
          },
          {
            label: "Active source for new draft",
            value: (
              <>
                <Value>{eligibleSource.activeJobId}</Value>
                <Hash>{eligibleSource.activeSourceHash}</Hash>
              </>
            ),
          },
          {
            label: "Mapping-validated source",
            value: (
              <>
                <Value>{eligibleSource.validatedJobId}</Value>
                <Hash>{eligibleSource.validatedSourceHash}</Hash>
              </>
            ),
          },
        ]}
      />
      {result ? (
        <div className="notice notice--positive" role="status">
          <strong>Editable carry-forward draft created</strong>
          <p>
            {result.clonedBindingCount} exact compatible bindings cloned ·{" "}
            {result.unresolvedTargetCount} target slots unresolved ·{" "}
            {result.unresolvedRelationshipCount} relationships unresolved
          </p>
          {result.unresolvedRelationships.length > 0 ? (
            <ul>
              {result.unresolvedRelationships.map((relationship) => (
                <li key={relationship.relationshipId}>
                  <Value>{relationship.relationshipId}</Value>:{" "}
                  {explainCarryForwardRelationship(relationship.reason)}{" "}
                  <Status value={relationship.reason} />
                </li>
              ))}
            </ul>
          ) : null}
          <p>
            <Value>{result.carryForwardId}</Value> ·{" "}
            <Hash>{result.resultHash}</Hash>
          </p>
        </div>
      ) : (
        <form className="setup-form" onSubmit={(event) => void carry(event)}>
          <div className="destination-form-grid">
            <label>
              Operator
              <input name="actor" required maxLength={256} />
            </label>
          </div>
          <label>
            Carry-forward reason
            <textarea name="reason" required maxLength={4096} rows={3} />
          </label>
          <ActionError error={error} />
          <button type="submit" className="button" disabled={busy}>
            {busy
              ? "Comparing exact profile slots…"
              : "Create compatible mapping draft"}
          </button>
        </form>
      )}
    </Card>
  );
}

function FreezeAndApproval({
  workshop,
  snapshotMatchesDurableDraft,
  refresh,
}: {
  workshop: DestinationWorkshop;
  snapshotMatchesDurableDraft: boolean;
  refresh: () => void;
}) {
  const freezeIdentity = useActionIdentity("destination-draft-freeze");
  const approveIdentity = useActionIdentity("destination-profile-approve");
  const [busy, setBusy] = useState<"freeze" | "approve" | null>(null);
  const [error, setError] = useState<ContractError | null>(null);
  const draft = workshop.draft;
  const candidate = workshop.reviewedCandidate;
  const active = workshop.activeDestination;
  if (!draft) return null;
  const activeDraft = draft;
  const candidateMatchesFrozenDraft =
    candidate !== null &&
    draft.state === "FROZEN" &&
    candidate.draftId === draft.draftId &&
    candidate.draftGeneration === draft.generation &&
    candidate.draftHash === draft.draftHash &&
    candidate.profile.profile_id === draft.frozenProfileId &&
    candidate.profile.profile_version === draft.frozenProfileVersion &&
    candidate.profile.profile_hash === draft.frozenProfileHash;

  async function freeze(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    setBusy("freeze");
    setError(null);
    try {
      await submitDestinationCommand({
        schemaVersion: DESTINATION_COMMAND_SCHEMAS.freezeDraft,
        expectedEngagementGeneration: workshop.engagementGeneration,
        draftId: activeDraft.draftId,
        expectedDraftGeneration: activeDraft.generation,
        expectedDraftHash: activeDraft.draftHash,
        profileVersion: formText(form, "profileVersion"),
        acknowledgement: DESTINATION_ACKNOWLEDGEMENTS.freeze,
        actor: formText(form, "actor"),
        reason: formText(form, "reason"),
        idempotencyKey: freezeIdentity.get(),
      });
      freezeIdentity.complete();
      refresh();
    } catch (caught) {
      setError(safeError(caught));
    } finally {
      setBusy(null);
    }
  }

  async function approve(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!candidate || !candidateMatchesFrozenDraft) return;
    const form = new FormData(event.currentTarget);
    setBusy("approve");
    setError(null);
    try {
      await submitDestinationCommand({
        schemaVersion: DESTINATION_COMMAND_SCHEMAS.approveProfile,
        expectedEngagementGeneration: workshop.engagementGeneration,
        profileId: candidate.profile.profile_id,
        profileVersion: candidate.profile.profile_version,
        profileHash: candidate.profile.profile_hash,
        originBindingHash: candidate.originBindingHash,
        actor: formText(form, "actor"),
        reason: formText(form, "reason"),
        authorityAcknowledgement:
          DESTINATION_ACKNOWLEDGEMENTS.localMappingValidation,
        idempotencyKey: approveIdentity.get(),
      });
      approveIdentity.complete();
      refresh();
    } catch (caught) {
      setError(safeError(caught));
    } finally {
      setBusy(null);
    }
  }

  return (
    <section className="destination-section" aria-labelledby="freeze-approval">
      <div className="section-heading">
        <div>
          <p className="eyebrow">Two separate authority steps</p>
          <h2 id="freeze-approval">Freeze, then approve the exact hash</h2>
          <p>
            Freezing registers immutable REVIEWED_DRAFT bytes and their exact
            technical/manual origin. A separate attributable approval grants
            only local mapping and validation authority.
          </p>
        </div>
      </div>
      {draft.freezeBlockers.length ? (
        <div className="notice notice--critical" role="status">
          <strong>Profile freeze is blocked</strong>
          <ul>
            {draft.freezeBlockers.map((blocker) => (
              <li key={blocker}>{blocker}</li>
            ))}
          </ul>
        </div>
      ) : null}
      {draft.state === "EDITABLE" ? (
        <Card title="Submit this exact working generation">
          {!snapshotMatchesDurableDraft ? (
            <p className="notice notice--critical" role="status">
              Freeze is unavailable until the displayed draft is saved and
              matches the durable generation and hash.
            </p>
          ) : null}
          <form className="setup-form" onSubmit={(event) => void freeze(event)}>
            <label>
              New profile version
              <input
                name="profileVersion"
                required
                maxLength={32}
                pattern="[0-9]+\.[0-9]+\.[0-9]+"
              />
            </label>
            <label>
              Reviewer
              <input name="actor" required maxLength={256} />
            </label>
            <label>
              Review reason
              <textarea name="reason" required maxLength={4096} rows={3} />
            </label>
            <label className="acknowledgment">
              <input type="checkbox" required />
              <span>
                I reviewed this exact draft generation, its technical origin,
                manual supplements, relationships, rules and acknowledged
                warnings. Freeze it as REVIEWED_DRAFT only.
              </span>
            </label>
            <ActionError error={error} />
            <button
              type="submit"
              className="button"
              disabled={
                busy !== null ||
                draft.freezeBlockers.length > 0 ||
                !snapshotMatchesDurableDraft
              }
            >
              {busy === "freeze"
                ? "Freezing exact profile bytes…"
                : "Submit Reviewed Draft"}
            </button>
          </form>
        </Card>
      ) : null}
      {candidate && candidateMatchesFrozenDraft ? (
        <Card title="Immutable reviewed destination profile">
          <DefinitionGrid
            items={[
              {
                label: "Profile",
                value: `${candidate.profile.profile_id} · ${candidate.profile.profile_version}`,
              },
              {
                label: "Exact profile hash",
                value: <Hash>{candidate.profile.profile_hash}</Hash>,
              },
              {
                label: "Exact origin binding",
                value: (
                  <>
                    <Value>{candidate.originBindingId}</Value>
                    <Hash>{candidate.originBindingHash}</Hash>
                  </>
                ),
              },
              {
                label: "Narrow authority",
                value: <Status value={candidate.profile.authority} />,
              },
            ]}
          />
        </Card>
      ) : null}
      {candidate &&
      candidate.profile.authority === "NONE" &&
      candidateMatchesFrozenDraft ? (
        <Card title="Approve this exact profile and origin hash">
          <form
            className="setup-form"
            onSubmit={(event) => void approve(event)}
          >
            <label>
              Approver
              <input name="actor" required maxLength={256} />
            </label>
            <label>
              Approval reason
              <textarea name="reason" required maxLength={4096} rows={3} />
            </label>
            <label className="acknowledgment">
              <input type="checkbox" required />
              <span>
                I approve this exact REVIEWED_DRAFT profile and origin binding
                for local mapping and validation only. This is not target
                compatibility, load authority or publication approval.
              </span>
            </label>
            <ActionError error={error} />
            <button type="submit" className="button" disabled={busy !== null}>
              {busy === "approve"
                ? "Recording exact-hash approval…"
                : "Approve exact profile hash"}
            </button>
          </form>
        </Card>
      ) : null}
      {candidate?.profile.authority === "LOCAL_MAPPING_VALIDATION" ? (
        <div className="notice notice--positive" role="status">
          <strong>
            {active?.profileId === candidate.profile.profile_id &&
            active.profileVersion === candidate.profile.profile_version &&
            active.profileHash === candidate.profile.profile_hash
              ? "Destination authority approved"
              : "Revised destination candidate approved"}
          </strong>
          <p>
            Approval {candidate.profile.approval?.approval_id ?? "recorded"}{" "}
            grants only local mapping and validation authority for the exact
            candidate profile and origin hashes.
          </p>
          {candidate.profile.approval?.approval_hash ? (
            <Hash>{candidate.profile.approval.approval_hash}</Hash>
          ) : null}
          {active &&
          (active.profileId !== candidate.profile.profile_id ||
            active.profileVersion !== candidate.profile.profile_version ||
            active.profileHash !== candidate.profile.profile_hash) ? (
            <p>
              Current active authority remains <Value>{active.profileId}</Value>{" "}
              · <Value>{active.profileVersion}</Value>. The approved candidate
              does not replace it unless the operator explicitly creates and
              reviews a compatible mapping draft.
            </p>
          ) : null}
        </div>
      ) : null}
      <MappingCarryForward workshop={workshop} />
    </section>
  );
}

function Workshop({
  workshop,
  refresh,
}: {
  workshop: DestinationWorkshop;
  refresh: () => void;
}) {
  const [snapshot, setSnapshot] = useState<DestinationDraftSnapshot | null>(
    workshop.draft?.snapshot ?? null,
  );
  const evidence = workshop.latestArtifact;
  const draft = workshop.draft;
  const canStartDraft = evidence !== null && draft === null;
  const snapshotMatchesDurableDraft = useMemo(
    () =>
      draft === null ||
      snapshot === null ||
      canonicalJson(snapshot) === canonicalJson(draft.snapshot),
    [draft, snapshot],
  );
  const activeProfileLabel =
    workshop.activeDestination?.profileAuthority === "LOCAL_MAPPING_VALIDATION"
      ? "Approved profile"
      : workshop.activeDestination
        ? "Reviewed profile awaiting approval"
        : "Destination profile";

  return (
    <>
      <Card title="Current destination authority">
        <DefinitionGrid
          items={[
            {
              label: "Workspace",
              value: (
                <>
                  <Value>{workshop.workspaceId}</Value>
                  <Hash>{workshop.workspaceDescriptorHash}</Hash>
                </>
              ),
            },
            {
              label: "Engagement",
              value: (
                <>
                  <Value>{workshop.engagementId}</Value> · generation{" "}
                  {workshop.engagementGeneration}
                </>
              ),
            },
            {
              label: "Destination origin",
              value: workshop.destinationOrigin ?? "Not approved",
            },
            {
              label: activeProfileLabel,
              value: workshop.activeDestination
                ? `${workshop.activeDestination.profileId} · ${workshop.activeDestination.profileVersion}`
                : "None",
            },
          ]}
        />
      </Card>
      <section
        className="destination-section"
        aria-labelledby="destination-origin"
      >
        <div className="section-heading">
          <div>
            <p className="eyebrow">One common authority contract</p>
            <h2 id="destination-origin">Add immutable destination evidence</h2>
            <p>
              Import an offline EDMX or create a bounded manual technical
              catalogue. Either creates technical evidence only. Existing
              reviewed controlled packs remain compatible through the
              established proof-pack path.
            </p>
          </div>
        </div>
        <div className="destination-origin-grid">
          <EdmxImport workshop={workshop} refresh={refresh} />
          <ManualCatalogue workshop={workshop} refresh={refresh} />
        </div>
      </section>
      <TechnicalEvidence workshop={workshop} />
      <CompatibilityReport
        workshop={workshop}
        snapshot={snapshot}
        setSnapshot={setSnapshot}
      />
      {canStartDraft ? (
        <StartDraft workshop={workshop} refresh={refresh} />
      ) : null}
      {draft && snapshot ? (
        <>
          {draft.state === "EDITABLE" ? (
            <CatalogueBrowser
              workshop={workshop}
              snapshot={snapshot}
              setSnapshot={setSnapshot}
            />
          ) : null}
          <RebasePanel
            workshop={workshop}
            snapshotMatchesDurableDraft={snapshotMatchesDurableDraft}
            refresh={refresh}
          />
          <DraftEditor
            workshop={workshop}
            snapshot={snapshot}
            snapshotMatchesDurableDraft={snapshotMatchesDurableDraft}
            setSnapshot={setSnapshot}
            refresh={refresh}
          />
          <FreezeAndApproval
            workshop={workshop}
            snapshotMatchesDurableDraft={snapshotMatchesDurableDraft}
            refresh={refresh}
          />
        </>
      ) : workshop.destinationOrigin === "CONTROLLED_PACK" ? (
        <Card title="Controlled reviewed pack">
          <p>
            This engagement uses the existing controlled-pack review and
            exact-hash approval flow. Continue through the established{" "}
            <a href="/setup#object-pack">controlled-pack section</a>.
          </p>
        </Card>
      ) : null}
    </>
  );
}

export function DestinationPage() {
  return (
    <>
      <PageHeader
        eyebrow="Destination contract workshop"
        title="Define the destination contract"
        description="Preserve immutable technical evidence, activate only deliberate scope, govern business meaning in a durable draft, freeze REVIEWED_DRAFT bytes, then approve their exact hash separately."
      />
      <div className="setup-boundary" role="note">
        <strong>Local authority boundary</strong>
        <span>
          Offline EDMX and manual catalogue facts remain technical evidence
          until a practitioner freezes and separately approves an exact profile.
          Nothing here claims compatibility with or connects to an external
          target system, writes to a target, publishes, or transmits data
          externally.
        </span>
      </div>
      <DestinationBoundary>
        {(workshop, refresh) => (
          <Workshop
            key={
              workshop.draft
                ? `${workshop.draft.draftId}:${workshop.draft.generation}:${workshop.draft.draftHash}:${workshop.draft.state}`
                : "destination-no-draft"
            }
            workshop={workshop}
            refresh={refresh}
          />
        )}
      </DestinationBoundary>
    </>
  );
}
