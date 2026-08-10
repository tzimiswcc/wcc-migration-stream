import { useRef, useState, type FormEvent } from "react";
import { submitRemediationPackCommand, useRead } from "./api";
import {
  ENGAGEMENT_COMMAND_SCHEMAS,
  REMEDIATION_LOCAL_EXPORT_ACKNOWLEDGEMENT,
  REMEDIATION_OFFENDING_VALUES_ACKNOWLEDGEMENT,
  SCHEMAS,
  ContractError,
  type RemediationKeyColumn,
  type RemediationPackHistory,
  type RemediationPackOptions,
  type RemediationPackView,
} from "./contracts";
import {
  Card,
  DefinitionGrid,
  EmptyState,
  Hash,
  ReadBoundary,
  Status,
  TableShell,
  Value,
} from "./ui";

function actionIdentity(): string {
  return `remediation-pack:${crypto.randomUUID()}`;
}

function selectedIdentity(column: RemediationKeyColumn): string {
  return `${column.entityId}\u0000${column.sourceFile}\u0000${column.sourceColumnOrdinal}\u0000${column.sourceColumn}`;
}

function downloadPath(packId: string): string {
  return `/api/v1/engagement/remediation-packs/${encodeURIComponent(packId)}/download`;
}

function safeError(value: unknown): ContractError {
  return value instanceof ContractError
    ? value
    : new ContractError(
        "REMEDIATION_PACK_FAILED",
        "The local remediation pack could not be completed safely.",
      );
}

function PackEvidence({ pack }: { pack: RemediationPackView }) {
  return (
    <Card title={`Remediation pack ${pack.packId}`}>
      <DefinitionGrid
        items={[
          { label: "State", value: <Status value={pack.state} /> },
          {
            label: "Validation run",
            value: <Value>{pack.validationRunId}</Value>,
          },
          { label: "Issues", value: pack.issueCount.toLocaleString("en-US") },
          {
            label: "Selected key columns",
            value: pack.selectedKeyColumns
              .map(
                (column) =>
                  `${column.entityId} · ${column.sourceFile} · ${column.sourceColumn}`,
              )
              .join(", "),
          },
          {
            label: "Offending values",
            value: pack.includeOffendingValues
              ? "Explicitly authorized where exact source-column lineage applies"
              : "Not included",
          },
          {
            label: "Actor and reason",
            value: `${pack.actor} · ${pack.reason}`,
          },
          {
            label: "Membership hash",
            value: <Hash>{pack.membershipHash}</Hash>,
          },
          { label: "Authority hash", value: <Hash>{pack.authorityHash}</Hash> },
          {
            label: "Archive SHA-256",
            value: <Hash>{pack.archiveSha256}</Hash>,
          },
          {
            label: "Archive",
            value: `${pack.memberCount.toLocaleString("en-US")} members · ${pack.archiveSizeBytes.toLocaleString("en-US")} bytes`,
          },
          { label: "Completed", value: pack.completedAt },
        ]}
      />
      <a
        className="button"
        href={downloadPath(pack.packId)}
        download={`migration-stream-remediation-${pack.packId}.zip`}
      >
        Download remediation pack
      </a>
    </Card>
  );
}

function RemediationHistory({ history }: { history: RemediationPackHistory }) {
  return (
    <Card title="Local remediation pack history">
      <p className="supporting">
        These data-bearing local exports are evidence for their original
        immutable validation run. They are not Readiness Packages and are not
        transmitted externally by Migration Stream.
      </p>
      {history.truncated ? (
        <p className="notice notice--attention" role="status">
          Showing the newest {history.returnedCount.toLocaleString("en-US")} of{" "}
          {history.totalCount.toLocaleString("en-US")} immutable packs. Older
          packs remain governed evidence but are outside this bounded view.
        </p>
      ) : null}
      {history.items.length === 0 ? (
        <EmptyState title="No remediation packs generated">
          Completed local remediation exports for this engagement will appear
          here.
        </EmptyState>
      ) : (
        <TableShell label="Local remediation pack history">
          <table>
            <thead>
              <tr>
                <th scope="col">Pack</th>
                <th scope="col">Validation</th>
                <th scope="col">Selected key columns</th>
                <th scope="col">Offending values</th>
                <th scope="col">Issues</th>
                <th scope="col">Archive SHA-256</th>
                <th scope="col">Action</th>
              </tr>
            </thead>
            <tbody>
              {history.items.map((pack) => (
                <tr key={pack.packId}>
                  <th scope="row">
                    <Value>{pack.packId}</Value>
                    <small>{pack.completedAt}</small>
                  </th>
                  <td>
                    <Value>{pack.validationRunId}</Value>
                  </td>
                  <td>
                    {pack.selectedKeyColumns
                      .map(
                        (column) => `${column.entityId}.${column.sourceColumn}`,
                      )
                      .join(", ")}
                  </td>
                  <td>
                    {pack.includeOffendingValues ? "Authorized" : "Excluded"}
                  </td>
                  <td>{pack.issueCount.toLocaleString("en-US")}</td>
                  <td>
                    <Hash>{pack.archiveSha256}</Hash>
                  </td>
                  <td>
                    <a
                      href={downloadPath(pack.packId)}
                      download={`migration-stream-remediation-${pack.packId}.zip`}
                    >
                      Download ZIP
                    </a>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </TableShell>
      )}
    </Card>
  );
}

function RemediationPackForm({
  options,
  onCreated,
}: {
  options: RemediationPackOptions;
  onCreated: (pack: RemediationPackView) => void;
}) {
  const identity = useRef(actionIdentity());
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [actor, setActor] = useState("");
  const [reason, setReason] = useState("");
  const [localExportAcknowledged, setLocalExportAcknowledged] = useState(false);
  const [includeOffendingValues, setIncludeOffendingValues] = useState(false);
  const [offendingValuesAcknowledged, setOffendingValuesAcknowledged] =
    useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ContractError | null>(null);

  const selectedColumns = options.files.flatMap((file) =>
    file.columns
      .map((column) => ({
        entityId: file.entityId,
        sourceFile: file.sourceFile,
        sourceColumnOrdinal: column.sourceColumnOrdinal,
        sourceColumn: column.sourceColumn,
      }))
      .filter((column) => selected.has(selectedIdentity(column))),
  );
  const everyFileHasKey = options.files.every((file) =>
    file.columns.some((column) =>
      selected.has(
        selectedIdentity({
          entityId: file.entityId,
          sourceFile: file.sourceFile,
          sourceColumnOrdinal: column.sourceColumnOrdinal,
          sourceColumn: column.sourceColumn,
        }),
      ),
    ),
  );
  const ready =
    everyFileHasKey &&
    actor.trim().length > 0 &&
    reason.trim().length > 0 &&
    localExportAcknowledged &&
    (!includeOffendingValues || offendingValuesAcknowledged) &&
    !busy;

  function toggle(column: RemediationKeyColumn, checked: boolean): void {
    const key = selectedIdentity(column);
    setSelected((current) => {
      const next = new Set(current);
      if (checked) next.add(key);
      else next.delete(key);
      return next;
    });
    identity.current = actionIdentity();
    setError(null);
  }

  async function submit(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    if (!ready) return;
    setBusy(true);
    setError(null);
    try {
      const result = await submitRemediationPackCommand({
        schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.createRemediationPack,
        expectedEngagementGeneration: options.engagementGeneration,
        expectedValidationRunId: options.validationRunId,
        expectedOptionsHash: options.optionsHash,
        selectedKeyColumns: selectedColumns,
        actor: actor.trim(),
        reason: reason.trim(),
        localDataExportAcknowledgement:
          REMEDIATION_LOCAL_EXPORT_ACKNOWLEDGEMENT,
        includeOffendingValues,
        offendingValuesAcknowledgement: includeOffendingValues
          ? REMEDIATION_OFFENDING_VALUES_ACKNOWLEDGEMENT
          : null,
        idempotencyKey: identity.current,
      });
      onCreated(result.pack);
      identity.current = actionIdentity();
    } catch (caught) {
      setError(safeError(caught));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card title="Create a local remediation pack">
      <p>
        Confirm the source business-key columns needed to identify affected
        records. Suggested columns start unchecked. The server exports complete
        bounded issue membership for this exact validation, not only the visible
        Issues page.
      </p>
      <DefinitionGrid
        items={[
          {
            label: "Validation run",
            value: <Value>{options.validationRunId}</Value>,
          },
          {
            label: "Issues",
            value: options.issueCount.toLocaleString("en-US"),
          },
          { label: "Options hash", value: <Hash>{options.optionsHash}</Hash> },
        ]}
      />
      <form className="setup-form" onSubmit={(event) => void submit(event)}>
        {options.files.map((file) => (
          <fieldset key={`${file.entityId}:${file.sourceFile}`}>
            <legend>
              {file.entityId} · {file.sourceFile}
            </legend>
            <p className="supporting">
              Select at least one exact accepted source column for this routed
              file and entity.
            </p>
            <div className="mapping-checklist">
              {file.columns.map((column) => {
                const keyColumn = {
                  entityId: file.entityId,
                  sourceFile: file.sourceFile,
                  sourceColumnOrdinal: column.sourceColumnOrdinal,
                  sourceColumn: column.sourceColumn,
                };
                return (
                  <label
                    className="acknowledgment"
                    key={`${column.sourceColumnOrdinal}:${column.sourceColumn}`}
                  >
                    <input
                      type="checkbox"
                      checked={selected.has(selectedIdentity(keyColumn))}
                      onChange={(event) =>
                        toggle(keyColumn, event.target.checked)
                      }
                    />
                    <span>
                      {column.sourceColumn}
                      <small>
                        Column {column.sourceColumnOrdinal}
                        {column.suggested
                          ? " · suggested from approved key mapping"
                          : ""}
                      </small>
                    </span>
                  </label>
                );
              })}
            </div>
          </fieldset>
        ))}
        <label>
          Exporting practitioner
          <input
            required
            maxLength={256}
            value={actor}
            onChange={(event) => {
              setActor(event.target.value);
              identity.current = actionIdentity();
            }}
          />
        </label>
        <label>
          Local remediation reason
          <textarea
            required
            maxLength={4096}
            rows={3}
            value={reason}
            onChange={(event) => {
              setReason(event.target.value);
              identity.current = actionIdentity();
            }}
          />
        </label>
        <label className="acknowledgment authority-acknowledgment">
          <input
            type="checkbox"
            checked={localExportAcknowledged}
            onChange={(event) => {
              setLocalExportAcknowledged(event.target.checked);
              identity.current = actionIdentity();
            }}
          />
          <span>
            I authorize this local data-bearing export of only the confirmed
            business-key columns. Migration Stream does not transmit it
            externally; it is not target-ready evidence or a Readiness Package.
          </span>
        </label>
        <label className="acknowledgment">
          <input
            type="checkbox"
            checked={includeOffendingValues}
            onChange={(event) => {
              setIncludeOffendingValues(event.target.checked);
              setOffendingValuesAcknowledged(false);
              identity.current = actionIdentity();
            }}
          />
          <span>
            Include exact offending source values where one source-column
            lineage applies
            <small>Off by default. No other source values are included.</small>
          </span>
        </label>
        {includeOffendingValues ? (
          <label className="acknowledgment authority-acknowledgment">
            <input
              type="checkbox"
              checked={offendingValuesAcknowledged}
              onChange={(event) => {
                setOffendingValuesAcknowledged(event.target.checked);
                identity.current = actionIdentity();
              }}
            />
            <span>
              I separately authorize exact offending source values only where
              issue lineage identifies one source column. Otherwise the pack
              must record that no single offending value applies.
            </span>
          </label>
        ) : null}
        {!everyFileHasKey ? (
          <p className="notice notice--attention" role="status">
            Confirm at least one source business-key column for every routed
            file and entity.
          </p>
        ) : null}
        {error ? (
          <div className="notice notice--critical" role="alert">
            <strong>{error.code}</strong>
            <p>{error.message}</p>
          </div>
        ) : null}
        <button className="button" type="submit" disabled={!ready}>
          {busy ? "Creating governed local pack…" : "Create remediation pack"}
        </button>
      </form>
    </Card>
  );
}

export function RemediationPackPanel() {
  const optionsRead = useRead<RemediationPackOptions>(
    "/api/v1/engagement/remediation-packs/options",
    SCHEMAS.remediationPackOptions,
  );
  const historyRead = useRead<RemediationPackHistory>(
    "/api/v1/engagement/remediation-packs",
    SCHEMAS.remediationPackHistory,
  );
  const [created, setCreated] = useState<RemediationPackView | null>(null);

  return (
    <section className="setup-section" aria-labelledby="remediation-packs">
      <div className="section-heading">
        <div>
          <p className="eyebrow">Governed local data export</p>
          <h2 id="remediation-packs">Remediation packs</h2>
          <p>
            Export complete value-suppressed issue evidence with only explicitly
            confirmed source keys and, separately, explicitly authorized
            offending values.
          </p>
        </div>
      </div>
      <div className="setup-boundary" role="note">
        <strong>Not a Readiness Package</strong>
        <span>
          A remediation pack is a data-bearing local working export. Creating or
          downloading it performs no source writeback, transmission, target
          action, or readiness approval.
        </span>
      </div>
      <ReadBoundary {...optionsRead}>
        {(options) => (
          <RemediationPackForm
            key={options.optionsHash}
            options={options}
            onCreated={(pack) => {
              setCreated(pack);
              historyRead.retry();
            }}
          />
        )}
      </ReadBoundary>
      {created ? <PackEvidence pack={created} /> : null}
      <ReadBoundary {...historyRead}>
        {(history) => <RemediationHistory history={history} />}
      </ReadBoundary>
    </section>
  );
}
