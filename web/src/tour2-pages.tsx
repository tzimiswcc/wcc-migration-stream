import {
  useEffect,
  useRef,
  useState,
  type ChangeEvent,
  type FormEvent,
  type ReactNode,
} from "react";
import {
  submitRevisedTourCommand,
  uploadSourcePreflight,
  useRead,
  type ReadState,
} from "./api";
import {
  ENGAGEMENT_COMMAND_SCHEMAS,
  REVISED_SOURCE_ACKNOWLEDGEMENT,
  SCHEMAS,
  ContractError,
  type ComparedFindingCategory,
  type EngagementReadinessPackageHistory,
  type RevisedTourAttempt,
  type SourcePreflight,
  type TourChain,
} from "./contracts";
import { AppLink, navigate, useLocation } from "./router";
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

const SALES_DOCUMENT_PACK_ID = "synthetic.sales-document.readiness";

function formatCount(value: number): string {
  return value.toLocaleString("en-US");
}

function formatBytes(value: number | null): string {
  if (value === null) return "Not available";
  if (value < 1024) return `${formatCount(value)} bytes`;
  return `${(value / 1024).toFixed(value >= 10_240 ? 1 : 2)} KB`;
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
    has(): boolean {
      return key.current !== null;
    },
  };
}

function safeError(error: unknown): ContractError {
  return error instanceof ContractError
    ? error
    : new ContractError(
        "UNEXPECTED_CLIENT_ERROR",
        "The bounded revised-validation action could not be completed safely.",
      );
}

function actionText(form: FormData, name: string): string {
  const value = form.get(name);
  return typeof value === "string" ? value.trim() : "";
}

function TourReadBoundary({
  state,
  retry,
  children,
}: {
  state: ReadState<TourChain>;
  retry: () => void;
  children: (chain: TourChain) => ReactNode;
}) {
  if (state.status === "loading") {
    return (
      <div className="state-panel" role="status" aria-live="polite">
        <span className="spinner" aria-hidden="true" />
        <div>
          <strong>Loading immutable source-revision evidence</strong>
          <p>
            The active validation remains unchanged while this read resolves.
          </p>
        </div>
      </div>
    );
  }
  if (state.status === "error") {
    return (
      <div className="state-panel state-panel--error" role="alert">
        <div>
          <p className="eyebrow">{state.error.code}</p>
          <strong>Source-revision evidence is unavailable</strong>
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
          Refresh the immutable source-revision evidence before retrying with
          the unchanged action identity.
        </p>
      ) : null}
    </div>
  );
}

function StartRevisedTour({
  chain,
  refresh,
}: {
  chain: TourChain;
  refresh: () => void;
}) {
  const identity = useActionIdentity("revised-tour-start");
  const [acknowledged, setAcknowledged] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ContractError | null>(null);

  async function submit(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    if (!acknowledged) return;
    const form = new FormData(event.currentTarget);
    setBusy(true);
    setError(null);
    try {
      await submitRevisedTourCommand({
        schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.startRevisedTour,
        idempotencyKey: identity.get(),
        expectedEngagementGeneration: chain.engagementGeneration,
        actor: actionText(form, "actor"),
        reason: actionText(form, "reason"),
        acknowledgement: REVISED_SOURCE_ACKNOWLEDGEMENT,
      });
      identity.complete();
      refresh();
    } catch (caught) {
      setError(safeError(caught));
      refresh();
    } finally {
      setBusy(false);
    }
  }

  return (
    <div id="start-source-refresh">
      <Card
        eyebrow="Same engagement · immutable prior revisions"
        title={`Start source revision ${chain.activeTour ? chain.activeTour.tourNumber + 1 : 2}`}
        className="tour-card tour-card--start"
      >
        <p>
          Open one bounded source-refresh attempt. The current revision remains
          active until the revised package, authority replay or new mapping,
          validation, and adjacent comparison all complete.
        </p>
        <form className="setup-form" onSubmit={(event) => void submit(event)}>
          <label>
            Operator attribution
            <input
              name="actor"
              required
              maxLength={256}
              autoComplete="off"
              placeholder="Named local operator"
            />
          </label>
          <label>
            Reason for revised validation
            <textarea
              name="reason"
              required
              maxLength={4096}
              placeholder="Describe why this revised local source is being accepted and compared."
            />
          </label>
          <label className="acknowledgment authority-acknowledgment">
            <input
              type="checkbox"
              checked={acknowledged}
              onChange={(event) => setAcknowledged(event.target.checked)}
            />
            <span>
              Start a local revised-source comparison only. This grants no
              correction, decision, reconciliation, result, publication, target,
              target-system or external-transmission authority.
            </span>
          </label>
          <ActionError error={error} />
          <button
            type="submit"
            className="button"
            disabled={!acknowledged || busy}
          >
            {busy ? "Opening source revision…" : "Start source refresh"}
          </button>
        </form>
      </Card>
    </div>
  );
}

function RevisedPreflightEvidence({
  preflight,
}: {
  preflight: SourcePreflight;
}) {
  if (preflight.state !== "READY" || !preflight.manifest) {
    return (
      <p className="notice" role="status">
        The revised package reservation is receiving local bytes.
      </p>
    );
  }
  return (
    <div className="preflight-evidence">
      <p className="notice notice--positive" role="status">
        <strong>Revised package passed the same local safety policy.</strong>
      </p>
      <DefinitionGrid
        items={[
          { label: "Package", value: preflight.sourceName },
          { label: "ZIP size", value: formatBytes(preflight.sourceSize) },
          {
            label: "Source SHA-256",
            value: <Hash>{preflight.sourceHash}</Hash>,
          },
          {
            label: "Manifest SHA-256",
            value: <Hash>{preflight.manifestHash}</Hash>,
          },
          {
            label: "CSV files",
            value: formatCount(preflight.manifest.csv_paths.length),
          },
        ]}
      />
      <ul className="manifest-list" aria-label="Revised CSV manifest">
        {preflight.manifest.csv_paths.map((path) => (
          <li key={path}>
            <Value>{path}</Value>
          </li>
        ))}
      </ul>
    </div>
  );
}

function RevisedSource({
  chain,
  attempt,
  refresh,
}: {
  chain: TourChain;
  attempt: RevisedTourAttempt;
  refresh: () => void;
}) {
  const reserveIdentity = useActionIdentity("revised-source-reserve");
  const acceptIdentity = useActionIdentity("revised-source-accept");
  const [file, setFile] = useState<File | null>(null);
  const [preflight, setPreflight] = useState<SourcePreflight | null>(null);
  const [acknowledged, setAcknowledged] = useState(false);
  const [busy, setBusy] = useState<"inspect" | "accept" | null>(null);
  const [error, setError] = useState<ContractError | null>(null);

  const resumableUpload =
    attempt.state === "PREFLIGHT_RECEIVING" && reserveIdentity.has();
  const canUpload = attempt.state === "STARTED" || resumableUpload;
  const ready =
    attempt.state === "PREFLIGHT_READY" &&
    attempt.preflightId !== null &&
    attempt.sourceHash !== null &&
    attempt.manifestHash !== null;

  async function inspect(): Promise<void> {
    if (!file) return;
    if (!file.name.toLowerCase().endsWith(".zip") || file.size === 0) {
      setError(
        new ContractError(
          "UNSUPPORTED_SOURCE",
          "Choose a non-empty ZIP containing the revised headered UTF-8 CSV files.",
        ),
      );
      return;
    }
    setBusy("inspect");
    setError(null);
    try {
      let preflightId = attempt.preflightId;
      if (attempt.state === "STARTED") {
        const reservation = await submitRevisedTourCommand({
          schemaVersion:
            ENGAGEMENT_COMMAND_SCHEMAS.reserveRevisedSourcePreflight,
          idempotencyKey: reserveIdentity.get(),
          expectedEngagementGeneration: chain.engagementGeneration,
          tourAttemptId: attempt.tourAttemptId,
          sourceName: file.name,
        });
        preflightId = reservation.preflight.preflightId;
      }
      if (!preflightId) {
        throw new ContractError(
          "MISSING_PREFLIGHT",
          "Refresh the source-revision evidence before resuming the revised ZIP upload.",
        );
      }
      const completed = await uploadSourcePreflight(
        preflightId,
        file,
        reserveIdentity.get(),
      );
      setPreflight(completed);
      reserveIdentity.complete();
      refresh();
    } catch (caught) {
      setError(safeError(caught));
      refresh();
    } finally {
      setBusy(null);
    }
  }

  async function accept(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    if (
      !acknowledged ||
      !attempt.preflightId ||
      !attempt.sourceHash ||
      !attempt.manifestHash
    ) {
      return;
    }
    const form = new FormData(event.currentTarget);
    setBusy("accept");
    setError(null);
    try {
      await submitRevisedTourCommand({
        schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.acceptRevisedSource,
        idempotencyKey: acceptIdentity.get(),
        expectedEngagementGeneration: chain.engagementGeneration,
        tourAttemptId: attempt.tourAttemptId,
        preflightId: attempt.preflightId,
        expectedSourceHash: attempt.sourceHash,
        expectedManifestHash: attempt.manifestHash,
        actor: actionText(form, "actor"),
        reason: actionText(form, "reason"),
        acknowledgement: REVISED_SOURCE_ACKNOWLEDGEMENT,
      });
      acceptIdentity.complete();
      refresh();
    } catch (caught) {
      setError(safeError(caught));
      refresh();
    } finally {
      setBusy(null);
    }
  }

  return (
    <Card
      eyebrow={`Revision ${attempt.tourNumber} · source intake`}
      title="Choose and accept the revised ZIP"
      className="tour-card"
    >
      <p>
        The same bounded ZIP safety and zero-retention profiling policy applies.
        The accepted package is isolated from every prior immutable revision.
      </p>
      {canUpload ? (
        <>
          <label className="file-picker">
            Revised source ZIP
            <input
              type="file"
              accept=".zip,application/zip"
              onChange={(event: ChangeEvent<HTMLInputElement>) => {
                setFile(event.target.files?.[0] ?? null);
                setPreflight(null);
                setError(null);
              }}
            />
          </label>
          {file ? (
            <p>
              <strong>{file.name}</strong> · {formatBytes(file.size)}
            </p>
          ) : null}
          <button
            type="button"
            className="button"
            disabled={!file || busy !== null}
            onClick={() => void inspect()}
          >
            {busy === "inspect"
              ? "Checking revised package…"
              : resumableUpload
                ? "Resume revised-package safety check"
                : "Run revised-package safety check"}
          </button>
        </>
      ) : null}
      {attempt.state === "PREFLIGHT_RECEIVING" &&
      !resumableUpload &&
      !preflight ? (
        <div className="notice notice--critical" role="alert">
          <strong>Refresh cannot invent the upload retry identity</strong>
          <p>
            The receiving reservation must finish in the browser action that
            opened it. No different ZIP or retry identity will be sent
            implicitly.
          </p>
        </div>
      ) : null}
      {preflight ? <RevisedPreflightEvidence preflight={preflight} /> : null}
      {ready ? (
        <>
          <DefinitionGrid
            items={[
              {
                label: "Preflight",
                value: <Value>{attempt.preflightId}</Value>,
              },
              {
                label: "Source SHA-256",
                value: <Hash>{attempt.sourceHash}</Hash>,
              },
              {
                label: "Manifest SHA-256",
                value: <Hash>{attempt.manifestHash}</Hash>,
              },
            ]}
          />
          <form className="setup-form" onSubmit={(event) => void accept(event)}>
            <label>
              Accepting operator
              <input
                name="actor"
                required
                maxLength={256}
                autoComplete="off"
                defaultValue={attempt.actor}
              />
            </label>
            <label>
              Acceptance reason
              <textarea
                name="reason"
                required
                maxLength={4096}
                defaultValue="Accept this exact revised local package for bounded compared validation."
              />
            </label>
            <label className="acknowledgment authority-acknowledgment">
              <input
                type="checkbox"
                checked={acknowledged}
                onChange={(event) => setAcknowledged(event.target.checked)}
              />
              <span>
                Accept this exact ZIP and manifest, profile it sample-free, and
                test structural compatibility without changing the active
                revision.
              </span>
            </label>
            <button
              type="submit"
              className="button"
              disabled={!acknowledged || busy !== null}
            >
              {busy === "accept"
                ? "Accepting and profiling…"
                : "Accept revised source locally"}
            </button>
          </form>
        </>
      ) : null}
      <ActionError error={error} />
    </Card>
  );
}

function ReuseAuthority({
  chain,
  attempt,
  refresh,
}: {
  chain: TourChain;
  attempt: RevisedTourAttempt;
  refresh: () => void;
}) {
  const identity = useActionIdentity("revised-validation-launch");
  const [acknowledged, setAcknowledged] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ContractError | null>(null);
  const baseline = chain.activeTour;

  async function launch(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    if (
      !acknowledged ||
      !attempt.reuseAuthorityId ||
      !attempt.reuseAuthorityHash
    ) {
      return;
    }
    const form = new FormData(event.currentTarget);
    setBusy(true);
    setError(null);
    try {
      await submitRevisedTourCommand({
        schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.launchRevisedValidation,
        idempotencyKey: identity.get(),
        expectedEngagementGeneration: chain.engagementGeneration,
        tourAttemptId: attempt.tourAttemptId,
        expectedReuseAuthorityId: attempt.reuseAuthorityId,
        expectedReuseAuthorityHash: attempt.reuseAuthorityHash,
        actor: actionText(form, "actor"),
        reason: actionText(form, "reason"),
        acknowledgement: REVISED_SOURCE_ACKNOWLEDGEMENT,
      });
      identity.complete();
      refresh();
    } catch (caught) {
      setError(safeError(caught));
      refresh();
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card
      eyebrow="Independent replay · exact structural match"
      title="Profile and mapping authority may be reused"
      className="tour-card"
    >
      <p className="notice notice--positive">
        <strong>Revised source profiled with zero retained samples.</strong>{" "}
        Exact file names, ordered headers, entity routing, profile approval, and
        mapping approval passed closed structural and integrity checks.
      </p>
      <DefinitionGrid
        items={[
          {
            label: "Revised profiling run",
            value: <Value>{attempt.profilingRunId}</Value>,
          },
          {
            label: "Structure hash",
            value: <Hash>{attempt.structureHash}</Hash>,
          },
          {
            label: "Reuse authority",
            value: <Value>{attempt.reuseAuthorityId}</Value>,
          },
          {
            label: "Reuse authority hash",
            value: <Hash>{attempt.reuseAuthorityHash}</Hash>,
          },
          {
            label: "Approved profile",
            value: baseline ? (
              <>
                <Value>
                  {baseline.profileId} · {baseline.profileVersion}
                </Value>
                <Hash>{baseline.profileHash}</Hash>
              </>
            ) : (
              "Unavailable"
            ),
          },
          {
            label: "Approved mapping",
            value: baseline ? (
              <>
                <Value>
                  {baseline.mappingId} · {baseline.mappingVersion}
                </Value>
                <Hash>{baseline.mappingHash}</Hash>
              </>
            ) : (
              "Unavailable"
            ),
          },
        ]}
      />
      <p className="supporting">
        This is exact authority reuse for local validation only. It is not a
        remapping shortcut or a compatibility claim beyond the declared
        structural contract.
      </p>
      <form className="setup-form" onSubmit={(event) => void launch(event)}>
        <label>
          Validation operator
          <input
            name="actor"
            required
            maxLength={256}
            autoComplete="off"
            defaultValue={attempt.actor}
          />
        </label>
        <label>
          Validation and comparison reason
          <textarea
            name="reason"
            required
            maxLength={4096}
            defaultValue={`Validate this exact revised package and compare immutable findings with Revision ${baseline?.tourNumber ?? attempt.tourNumber - 1}.`}
          />
        </label>
        <label className="acknowledgment authority-acknowledgment">
          <input
            type="checkbox"
            checked={acknowledged}
            onChange={(event) => setAcknowledged(event.target.checked)}
          />
          <span>
            Launch immutable local validation under this exact reuse authority.
            Advance the active revision only after complete validation and
            durable comparison registration.
          </span>
        </label>
        <ActionError error={error} />
        <button
          type="submit"
          className="button"
          disabled={!acknowledged || busy}
        >
          {busy
            ? "Validating and comparing…"
            : `Launch Revision ${attempt.tourNumber} validation`}
        </button>
      </form>
    </Card>
  );
}

const categoryExplanation: Record<ComparedFindingCategory, string> = {
  CLOSED: "Present in the predecessor revision and absent from its successor.",
  REMAINING:
    "Matched multiset occurrences of the same governed fingerprint; no row-to-row lineage is inferred.",
  NEW: "Absent from the predecessor revision and present in its successor.",
};

function ComparisonEvidence({
  chain,
  comparison = chain.latestComparison,
  entityCount,
}: {
  chain: TourChain;
  comparison?: TourChain["latestComparison"];
  entityCount: number | null;
}) {
  const [category, setCategory] = useState<ComparedFindingCategory | "ALL">(
    "ALL",
  );
  const current = comparison
    ? (chain.tours.find((tour) => tour.tourId === comparison.candidateTourId) ??
      null)
    : null;
  const baseline = comparison
    ? (chain.tours.find((tour) => tour.tourId === comparison.baselineTourId) ??
      null)
    : null;
  if (!comparison || !current) {
    return (
      <EmptyState title="No completed comparison">
        The current revision remains authoritative until revised validation and
        immutable adjacent-comparison registration both complete.
      </EmptyState>
    );
  }
  const findings =
    category === "ALL"
      ? comparison.findings
      : comparison.findings.filter((finding) => finding.category === category);

  return (
    <section className="tour-comparison" aria-labelledby="tour-comparison">
      <div className="section-heading">
        <div>
          <p className="eyebrow">Immutable compared validation</p>
          <h2 id="tour-comparison">
            Revision {baseline?.tourNumber ?? "?"} versus Revision{" "}
            {current.tourNumber}
          </h2>
          <p>
            Stable, value-suppressed finding identities classify what closed,
            remained, or appeared. Source rows remain drill-down evidence, not
            the comparison key.
          </p>
        </div>
        <Status value="COMPLETE" />
      </div>
      <div className="metric-grid tour-comparison__counts">
        {(
          [
            ["CLOSED", comparison.closedCount],
            ["REMAINING", comparison.remainingCount],
            ["NEW", comparison.newCount],
          ] as const
        ).map(([label, count]) => (
          <button
            key={label}
            type="button"
            className={`metric tour-category tour-category--${label.toLowerCase()}`}
            aria-pressed={category === label}
            onClick={() => setCategory(category === label ? "ALL" : label)}
          >
            <strong>{formatCount(count)}</strong>
            <span>{label}</span>
            <small>{categoryExplanation[label]}</small>
          </button>
        ))}
      </div>
      <Card title="Immutable comparison authority">
        <DefinitionGrid
          items={[
            {
              label: `Revision ${baseline?.tourNumber ?? "?"}`,
              value: (
                <>
                  <Value>{comparison.baselineTourId}</Value>
                  <Hash>{comparison.baselineResultHash}</Hash>
                </>
              ),
            },
            {
              label: `Revision ${current.tourNumber}`,
              value: (
                <>
                  <Value>{comparison.candidateTourId}</Value>
                  <Hash>{comparison.candidateResultHash}</Hash>
                </>
              ),
            },
            {
              label: `Revision ${baseline?.tourNumber ?? "?"} validation run`,
              value: <Value>{comparison.baselineValidationRunId}</Value>,
            },
            {
              label: `Revision ${current.tourNumber} validation run`,
              value: <Value>{comparison.candidateValidationRunId}</Value>,
            },
            {
              label: `Revision ${current.tourNumber} source`,
              value: <Hash>{current.sourceHash}</Hash>,
            },
            {
              label: `Revision ${current.tourNumber} manifest`,
              value: <Hash>{current.manifestHash}</Hash>,
            },
            {
              label: "Reused profile",
              value: (
                <>
                  <Value>
                    {current.profileId} · {current.profileVersion}
                  </Value>
                  <Hash>{current.profileHash}</Hash>
                </>
              ),
            },
            {
              label: "Reused mapping",
              value: (
                <>
                  <Value>
                    {current.mappingId} · {current.mappingVersion}
                  </Value>
                  <Hash>{current.mappingHash}</Hash>
                  <Value>{current.mappingAuthorityId}</Value>
                  <Hash>{current.mappingAuthorityHash}</Hash>
                </>
              ),
            },
            {
              label: "Comparison",
              value: <Value>{comparison.comparisonId}</Value>,
            },
            {
              label: "Comparison hash",
              value: <Hash>{comparison.comparisonHash}</Hash>,
            },
            {
              label: "Input hash",
              value: <Hash>{comparison.inputHash}</Hash>,
            },
            {
              label: `Revision ${current.tourNumber} evidence hash`,
              value: <Hash>{current.tourHash}</Hash>,
            },
            {
              label: "Algorithm",
              value: <Value>{comparison.algorithmVersion}</Value>,
            },
            {
              label: "Attribution",
              value: `${comparison.actor} · ${comparison.reason}`,
            },
          ]}
        />
      </Card>
      <div className="tour-filter">
        <label>
          Finding category
          <select
            value={category}
            onChange={(event) =>
              setCategory(event.target.value as ComparedFindingCategory | "ALL")
            }
          >
            <option value="ALL">All categories</option>
            <option value="CLOSED">Closed</option>
            <option value="REMAINING">Remaining</option>
            <option value="NEW">New</option>
          </select>
        </label>
        <span>{formatCount(findings.length)} value-suppressed findings</span>
      </div>
      {findings.length ? (
        <div className="tour-finding-list" aria-label="Compared findings">
          {findings.map((finding) => (
            <Card
              key={finding.findingId}
              eyebrow={`${finding.category} · ${finding.severity}`}
              title={finding.issueCode}
              className={`tour-finding tour-finding--${finding.category.toLowerCase()}`}
            >
              <p>{finding.displayMessage}</p>
              <DefinitionGrid
                items={[
                  {
                    label: "Canonical scope",
                    value: (
                      <Value>
                        {finding.entityId}.{finding.fieldId}
                      </Value>
                    ),
                  },
                  { label: "Rule", value: <Value>{finding.ruleId}</Value> },
                  ...(finding.relationshipId
                    ? [
                        {
                          label: "Relationship",
                          value: (
                            <Value>
                              {finding.relationshipId} ·{" "}
                              {finding.relationshipRole}
                            </Value>
                          ),
                        },
                      ]
                    : []),
                  {
                    label: "Finding identity",
                    value: <Hash>{finding.fingerprintHash}</Hash>,
                  },
                  {
                    label: "Identity basis",
                    value: (
                      <>
                        <Value>
                          {finding.identityBasis.replaceAll("_", " ")}
                        </Value>
                        <small>{finding.identityBasisExplanation}</small>
                      </>
                    ),
                  },
                  ...(finding.baselineIssueHashes.length
                    ? [
                        {
                          label: `Revision ${baseline?.tourNumber ?? "?"} issue hash`,
                          value: <Hash>{finding.baselineIssueHashes[0]}</Hash>,
                        },
                      ]
                    : []),
                  ...(finding.candidateIssueHashes.length
                    ? [
                        {
                          label: `Revision ${current.tourNumber} issue hash`,
                          value: <Hash>{finding.candidateIssueHashes[0]}</Hash>,
                        },
                      ]
                    : []),
                  {
                    label: "Occurrence",
                    value: formatCount(finding.occurrence),
                  },
                  {
                    label: "Category meaning",
                    value: categoryExplanation[finding.category],
                  },
                ]}
              />
            </Card>
          ))}
        </div>
      ) : (
        <EmptyState title={`No ${category.toLowerCase()} findings`}>
          The immutable comparison contains no findings in this category.
        </EmptyState>
      )}
      <div className="setup-boundary" role="note">
        <strong>
          {entityCount !== null && entityCount > 1
            ? "Multi-entity findings remain read-only"
            : "Comparison evidence remains immutable"}
        </strong>
        <span>
          {entityCount !== null && entityCount > 1
            ? "Multi-entity downstream authority is not implemented yet. Decisions, reconciliation, result approval, publication, external transmission, and target actions remain disabled."
            : "The comparison changes no finding, source, mapping, prior decision, or prior package. The active revision begins fresh downstream governance; no decision or result authority is carried forward silently."}
        </span>
      </div>
    </section>
  );
}

function RevisionHistory({ chain }: { chain: TourChain }) {
  return (
    <Card title="Immutable source revision history">
      {chain.tours.length ? (
        <TableShell label="Source revisions">
          <table>
            <thead>
              <tr>
                <th scope="col">Revision</th>
                <th scope="col">Source</th>
                <th scope="col">Validation run</th>
                <th scope="col">Result</th>
                <th scope="col">Completed</th>
              </tr>
            </thead>
            <tbody>
              {chain.tours.map((tour) => (
                <tr key={tour.tourId}>
                  <th scope="row">
                    Revision {tour.tourNumber}
                    {tour.tourId === chain.activeTour?.tourId ? (
                      <small>Current</small>
                    ) : null}
                  </th>
                  <td>
                    <Hash>{tour.sourceHash}</Hash>
                  </td>
                  <td>
                    <Value>{tour.validationRunId}</Value>
                  </td>
                  <td>
                    <Hash>{tour.validationResultHash}</Hash>
                  </td>
                  <td>{tour.completedAt}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </TableShell>
      ) : (
        <EmptyState title="No completed revision">
          Complete an immutable validation before accepting refreshed source
          evidence.
        </EmptyState>
      )}
      {chain.comparisons.length ? (
        <TableShell label="Adjacent source comparisons">
          <table>
            <thead>
              <tr>
                <th scope="col">Comparison</th>
                <th scope="col">Adjacent revisions</th>
                <th scope="col">Closed</th>
                <th scope="col">Remaining</th>
                <th scope="col">New</th>
                <th scope="col">Evidence hash</th>
              </tr>
            </thead>
            <tbody>
              {chain.comparisons.map((comparison) => {
                const predecessor = chain.tours.find(
                  (tour) => tour.tourId === comparison.baselineTourId,
                );
                const successor = chain.tours.find(
                  (tour) => tour.tourId === comparison.candidateTourId,
                );
                return (
                  <tr key={comparison.comparisonId}>
                    <th scope="row">
                      <AppLink
                        href={`/comparison?comparison_id=${encodeURIComponent(comparison.comparisonId)}`}
                      >
                        {comparison.comparisonId}
                      </AppLink>
                    </th>
                    <td>
                      Revision {predecessor?.tourNumber ?? "?"} → Revision{" "}
                      {successor?.tourNumber ?? "?"}
                    </td>
                    <td>{comparison.closedCount.toLocaleString("en-US")}</td>
                    <td>{comparison.remainingCount.toLocaleString("en-US")}</td>
                    <td>{comparison.newCount.toLocaleString("en-US")}</td>
                    <td>
                      <Hash>{comparison.comparisonHash}</Hash>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </TableShell>
      ) : null}
    </Card>
  );
}

function HistoricalReadinessPackages() {
  const read = useRead<EngagementReadinessPackageHistory>(
    "/api/v1/engagement/readiness-packages",
    SCHEMAS.engagementReadinessPackages,
  );
  return (
    <ReadBoundary state={read.state} retry={read.retry}>
      {(history) => (
        <Card title="Historical Readiness Packages">
          <p className="supporting">
            Prior packages remain immutable on their original revision. This
            read-only history grants no generation authority for the active
            revision.
          </p>
          {history.items.length ? (
            <TableShell label="Historical readiness package evidence">
              <table>
                <thead>
                  <tr>
                    <th scope="col">Package</th>
                    <th scope="col">Revision job</th>
                    <th scope="col">Attribution</th>
                    <th scope="col">Evidence hashes</th>
                    <th scope="col">Size</th>
                    <th scope="col">Completed</th>
                    <th scope="col">Action</th>
                  </tr>
                </thead>
                <tbody>
                  {history.items.map((item) => (
                    <tr key={item.publication_id}>
                      <th scope="row">
                        <Value>{item.publication_id}</Value>
                        <small>{item.status}</small>
                      </th>
                      <td>
                        <Value>{item.job_id}</Value>
                      </td>
                      <td>
                        {item.practitioner} · {item.publication_reason}
                      </td>
                      <td>
                        <Hash>{item.semantic_publication_hash}</Hash>
                        <Hash>{item.physical_archive_digest}</Hash>
                      </td>
                      <td>
                        {item.package_size_bytes.toLocaleString("en-US")} bytes
                      </td>
                      <td>{item.completed_at}</td>
                      <td>
                        <a
                          href={`/api/v1/engagement/readiness-packages/${encodeURIComponent(item.publication_id)}/download`}
                          download={`migration-stream-readiness-${item.job_id}-${item.publication_id}.zip`}
                        >
                          Download ZIP
                        </a>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </TableShell>
          ) : (
            <EmptyState title="No historical Readiness Packages">
              A package created for a completed single-entity revision will
              remain visible here after a later revision becomes active.
            </EmptyState>
          )}
          {history.truncated ? (
            <p className="supporting">
              Showing {history.returned_count.toLocaleString("en-US")} of{" "}
              {history.total_count.toLocaleString("en-US")} immutable packages.
            </p>
          ) : null}
        </Card>
      )}
    </ReadBoundary>
  );
}

function PendingTour({
  chain,
  refresh,
}: {
  chain: TourChain;
  refresh: () => void;
}) {
  const attempt = chain.pendingAttempt;
  if (!attempt) return null;
  if (
    attempt.state === "STARTED" ||
    attempt.state === "PREFLIGHT_RECEIVING" ||
    attempt.state === "PREFLIGHT_READY"
  ) {
    return <RevisedSource chain={chain} attempt={attempt} refresh={refresh} />;
  }
  if (attempt.state === "READY_TO_VALIDATE") {
    return <ReuseAuthority chain={chain} attempt={attempt} refresh={refresh} />;
  }
  if (attempt.state === "SOURCE_ACCEPTED" || attempt.state === "VALIDATING") {
    return (
      <Card title="Revised validation is running" className="tour-card">
        <p role="status">
          The predecessor revision remains active while the bounded revised run
          and comparison evidence complete.
        </p>
        <DefinitionGrid
          items={[
            {
              label: "Revision attempt",
              value: <Value>{attempt.tourAttemptId}</Value>,
            },
            {
              label: "Validation run",
              value: <Value>{attempt.validationRunId}</Value>,
            },
          ]}
        />
      </Card>
    );
  }
  const mappingRequired =
    attempt.state === "INCOMPATIBLE" &&
    attempt.errorCode === "MAPPING_REQUIRED";
  return (
    <div className="notice notice--critical" role="alert">
      <strong>
        {mappingRequired
          ? "A new governed mapping is required"
          : "Revised validation did not complete"}
      </strong>
      <p>
        {attempt.errorMessage ??
          "The bounded revised attempt stopped without changing the authoritative predecessor revision."}
      </p>
      {attempt.errorCode ? <small>{attempt.errorCode}</small> : null}
      {mappingRequired ? (
        <>
          <p>
            The workshop may save a revision-scoped draft against this immutable
            candidate source. Nothing is activated or approved automatically,
            and the predecessor remains authoritative.
          </p>
          <p>
            <AppLink className="button" href="/mapping">
              Open Mapping Workshop for Revision {attempt.tourNumber}
            </AppLink>
          </p>
        </>
      ) : null}
    </div>
  );
}

export function RevisedValidationPanel() {
  const read = useRead<TourChain>(
    "/api/v1/engagement/tours",
    SCHEMAS.tourChain,
  );
  const pendingState =
    read.state.status === "success"
      ? read.state.data.pendingAttempt?.state
      : null;

  useEffect(() => {
    if (pendingState !== "SOURCE_ACCEPTED" && pendingState !== "VALIDATING") {
      return;
    }
    const timer = window.setTimeout(read.retry, 1200);
    return () => window.clearTimeout(timer);
  }, [pendingState, read.retry]);

  return (
    <section className="setup-section" aria-labelledby="revised-validation">
      <div className="section-heading">
        <div>
          <p className="eyebrow">Repeatable source revisions</p>
          <h2 id="revised-validation">Accept a revised source safely</h2>
          <p>
            Reuse exact approved authority only after closed structural and
            integrity replay, then compare immutable findings.
          </p>
        </div>
        <Status
          value={
            pendingState ??
            (read.state.status === "success" &&
            read.state.data.latestComparison !== null
              ? "COMPLETE"
              : "AVAILABLE")
          }
        />
      </div>
      <TourReadBoundary {...read}>
        {(chain) => (
          <>
            <RevisionHistory chain={chain} />
            {chain.pendingAttempt ? (
              <PendingTour chain={chain} refresh={read.retry} />
            ) : (
              <StartRevisedTour chain={chain} refresh={read.retry} />
            )}
            {chain.latestComparison ? (
              <p>
                <AppLink className="button" href="/comparison">
                  Open compared validation
                </AppLink>
              </p>
            ) : null}
          </>
        )}
      </TourReadBoundary>
    </section>
  );
}

export function TourComparisonPage({
  entityCount = null,
}: {
  entityCount?: number | null;
}) {
  const location = useLocation();
  const read = useRead<TourChain>(
    "/api/v1/engagement/tours",
    SCHEMAS.tourChain,
  );
  return (
    <>
      <PageHeader
        eyebrow="Value-suppressed source-revision evidence"
        title="Source revisions and compared validation"
        description="Accept repeatable immutable source revisions and review durable adjacent CLOSED, REMAINING, and NEW finding classifications."
      />
      <TourReadBoundary {...read}>
        {(chain) => {
          const requested = location.searchParams.get("comparison_id");
          const requestedComparison =
            requested === null
              ? null
              : chain.comparisons.find(
                  (comparison) => comparison.comparisonId === requested,
                );
          const requestedComparisonMissing =
            requested !== null && requestedComparison === undefined;
          const selected =
            requested === null
              ? chain.latestComparison
              : (requestedComparison ?? null);
          return (
            <>
              <RevisionHistory chain={chain} />
              <HistoricalReadinessPackages />
              {chain.comparisons.length > 1 || requestedComparisonMissing ? (
                <label className="tour-filter">
                  Compared revisions
                  <select
                    value={selected?.comparisonId ?? ""}
                    onChange={(event) =>
                      navigate(
                        `/comparison?comparison_id=${encodeURIComponent(event.target.value)}`,
                      )
                    }
                  >
                    {requestedComparisonMissing ? (
                      <option value="" disabled>
                        Requested comparison unavailable
                      </option>
                    ) : null}
                    {chain.comparisons.map((comparison) => {
                      const predecessor = chain.tours.find(
                        (tour) => tour.tourId === comparison.baselineTourId,
                      );
                      const successor = chain.tours.find(
                        (tour) => tour.tourId === comparison.candidateTourId,
                      );
                      return (
                        <option
                          value={comparison.comparisonId}
                          key={comparison.comparisonId}
                        >
                          Revision {predecessor?.tourNumber ?? "?"} versus
                          Revision {successor?.tourNumber ?? "?"}
                        </option>
                      );
                    })}
                  </select>
                </label>
              ) : null}
              {requestedComparisonMissing ? (
                <EmptyState title="Comparison evidence unavailable">
                  The requested comparison identity does not belong to this
                  engagement revision chain. Select one of the exact governed
                  adjacent comparisons instead.
                </EmptyState>
              ) : selected ? (
                <ComparisonEvidence
                  chain={chain}
                  comparison={selected}
                  entityCount={entityCount}
                />
              ) : null}
              {chain.pendingAttempt ? (
                <PendingTour chain={chain} refresh={read.retry} />
              ) : (
                <StartRevisedTour chain={chain} refresh={read.retry} />
              )}
            </>
          );
        }}
      </TourReadBoundary>
    </>
  );
}

export { SALES_DOCUMENT_PACK_ID };
