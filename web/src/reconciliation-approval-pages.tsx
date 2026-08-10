import { type FormEvent, useState } from "react";
import { apiQuery, submitResultCommand, useRead } from "./api";
import {
  RESULT_COMMAND_SCHEMAS,
  SCHEMAS,
  ContractError,
  type CreateResultCandidateCommand,
  type ResultActionHistory,
  type ResultApprovalEligibility,
  type ResultCandidateList,
  type ResultCandidateSummary,
  type ResultCommand,
  type ResultCommandResult,
  type PageInfo,
  type ResultReconciliationRowDetail,
  type ResultReconciliationRowList,
  type ResultReconciliationSnapshot,
  type ResultReconciliationSnapshotList,
  type ResultReconciliationSnapshotSummary,
} from "./contracts";
import { AppLink, navigate } from "./router";
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

const AUTHORITY_BOUNDARY =
  "I understand this is local Migration Stream readiness evidence only. It is not publication approval or publication, target-load authorization, target acceptance or compatibility, an authenticated signature, or production approval.";

type RunnerState =
  | { status: "idle" }
  | { status: "submitting"; submission: ResultSubmission }
  | {
      status: "result";
      submission: ResultSubmission;
      result: ResultCommandResult;
    }
  | {
      status: "error";
      submission: ResultSubmission;
      error: ContractError;
    };

interface ResultSubmission {
  path: string;
  command: ResultCommand;
  idempotencyKey: string;
}

function commandIdentity(): string {
  return `migration-result-${crypto.randomUUID()}`;
}

function useCommandRunner(onCommitted: () => void) {
  const [state, setState] = useState<RunnerState>({ status: "idle" });

  async function execute(submission: ResultSubmission): Promise<void> {
    setState({ status: "submitting", submission });
    try {
      const result = await submitResultCommand(
        submission.path,
        submission.command,
        submission.idempotencyKey,
      );
      setState({ status: "result", submission, result });
    } catch (error) {
      const safeError =
        error instanceof ContractError
          ? error
          : new ContractError(
              "UNEXPECTED_CLIENT_ERROR",
              "The migration-result command could not be displayed safely.",
            );
      setState({
        status: "error",
        submission,
        error: safeError,
      });
    }
  }

  function submit(path: string, command: ResultCommand): void {
    void execute({ path, command, idempotencyKey: commandIdentity() });
  }

  function retry(): void {
    if (state.status === "error") void execute(state.submission);
  }

  return {
    state,
    submit,
    retry,
    reset: () => {
      if (state.status === "result") onCommitted();
      setState({ status: "idle" });
    },
  };
}

function safeId(value: string): string {
  return encodeURIComponent(value);
}

function pageSize(location: URL): number {
  const parsed = Number(location.searchParams.get("page_size") ?? "50");
  return Number.isInteger(parsed) && parsed >= 1 && parsed <= 200 ? parsed : 50;
}

function formText(form: FormData, key: string): string {
  const value = form.get(key);
  return typeof value === "string" ? value : "";
}

function selectedHref(
  location: URL,
  changes: Record<string, string | null>,
): string {
  const query = new URLSearchParams(location.searchParams);
  Object.entries(changes).forEach(([key, value]) => {
    if (value === null) query.delete(key);
    else query.set(key, value);
  });
  const encoded = query.toString();
  return encoded ? `/result-approval?${encoded}` : "/result-approval";
}

function ResultPagination({
  label,
  page,
  firstHref,
  nextHref,
}: {
  label: string;
  page: PageInfo;
  firstHref: string | null;
  nextHref: string | null;
}) {
  return (
    <nav className="pagination" aria-label={label}>
      {firstHref ? (
        <AppLink className="button button--secondary" href={firstHref}>
          First page
        </AppLink>
      ) : (
        <span
          className="button button--secondary"
          aria-disabled="true"
          role="link"
        >
          First page
        </span>
      )}
      <span aria-live="polite">
        Returned {page.returned} · page size {page.page_size}
      </span>
      <button
        type="button"
        className="button button--secondary"
        disabled={!nextHref}
        onClick={() => nextHref && navigate(nextHref)}
      >
        Next page
      </button>
    </nav>
  );
}

function ResultCommandFeedback({
  state,
  retry,
  reset,
}: {
  state: RunnerState;
  retry: () => void;
  reset: () => void;
}) {
  if (state.status === "idle") return null;
  if (state.status === "submitting") {
    return (
      <div className="notice" role="status" aria-live="polite">
        Committing immutable local evidence. No authority is displayed until the
        server returns a validated durable result.
      </div>
    );
  }
  if (state.status === "error") {
    return (
      <div className="notice notice--critical" role="alert">
        <strong>{state.error.code}</strong>
        <p>{state.error.message}</p>
        <div className="filter-actions">
          {state.error.retryable ? (
            <button className="button" type="button" onClick={retry}>
              Retry exact command identity
            </button>
          ) : null}
          <button
            className="button button--secondary"
            type="button"
            onClick={reset}
          >
            Return to current evidence
          </button>
        </div>
      </div>
    );
  }
  const result = state.result;
  const resultIdentity =
    result.schema_version === SCHEMAS.resultReconciliationCommand
      ? result.data.snapshot_id
      : result.schema_version === SCHEMAS.resultCandidateCommand
        ? result.data.candidate_id
        : result.data.action_id;
  return (
    <div className="notice" role="status" aria-live="polite">
      <strong>Durable local evidence recorded</strong>
      <p>
        Server evidence {resultIdentity}. This result is not publication,
        target-system authority, target acceptance, or an authenticated
        signature.
      </p>
      <button
        className="button button--secondary"
        type="button"
        onClick={reset}
      >
        Return to current evidence
      </button>
    </div>
  );
}

export function ResultApprovalCapabilityUnavailablePage() {
  return (
    <>
      <PageHeader
        eyebrow="Capability unavailable"
        title="Migration-result approval is disabled"
        description="Only the explicit v3 governed capability can expose local reconciliation and migration-result actions. Read-only and issue-decision modes do not imply this authority."
      />
      <Card title="No result authority inferred">
        <p>
          Loopback operation is not authentication. This browser cannot enable
          approval, publication, or target-system authority itself.
        </p>
        <AppLink href="/reconciliation">Return to Reconciliation</AppLink>
      </Card>
    </>
  );
}

function SnapshotHistory({
  response,
  selectedId,
  location,
}: {
  response: ResultReconciliationSnapshotList;
  selectedId: string | null;
  location: URL;
}) {
  if (!response.items.length) {
    return (
      <EmptyState>No immutable reconciliation snapshot exists.</EmptyState>
    );
  }
  return (
    <>
      <TableShell label="Immutable reconciliation snapshot history">
        <table>
          <thead>
            <tr>
              <th scope="col">Snapshot</th>
              <th scope="col">State</th>
              <th scope="col">Current</th>
              <th scope="col">Accepted rows</th>
              <th scope="col">Blocked rows</th>
              <th scope="col">Completed</th>
              <th scope="col">Inspect</th>
            </tr>
          </thead>
          <tbody>
            {response.items.map((item) => {
              const blocked = item.counts
                .filter((count) => count.category.startsWith("BLOCKED_"))
                .reduce((total, count) => total + count.row_count, 0);
              return (
                <tr key={item.snapshot_id}>
                  <th scope="row">
                    <Value>{item.snapshot_id}</Value>
                    <small>
                      <Hash>{item.snapshot_hash}</Hash>
                    </small>
                  </th>
                  <td>
                    <Status value={item.state} />
                  </td>
                  <td>
                    <Status value={item.current ? "CURRENT" : "STALE"} />
                  </td>
                  <td>{item.accepted_row_count}</td>
                  <td>{blocked}</td>
                  <td>{item.completed_at ?? "Not complete"}</td>
                  <td>
                    <AppLink
                      href={selectedHref(location, {
                        snapshot_id: item.snapshot_id,
                        row_cursor: null,
                        detail_source_file: null,
                        detail_source_ordinal: null,
                      })}
                      current={item.snapshot_id === selectedId}
                    >
                      Inspect snapshot
                    </AppLink>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </TableShell>
      <ResultPagination
        label="Snapshot history pagination"
        page={response.page}
        firstHref={
          location.searchParams.has("snapshot_cursor")
            ? selectedHref(location, { snapshot_cursor: null })
            : null
        }
        nextHref={
          response.page.next_cursor
            ? selectedHref(location, {
                snapshot_cursor: response.page.next_cursor,
              })
            : null
        }
      />
    </>
  );
}

function CategoryClosure({
  snapshot,
}: {
  snapshot: ResultReconciliationSnapshotSummary;
}) {
  const total = snapshot.counts.reduce(
    (sum, count) => sum + count.row_count,
    0,
  );
  const blocked = snapshot.counts
    .filter((count) => count.category.startsWith("BLOCKED_"))
    .reduce((sum, count) => sum + count.row_count, 0);
  return (
    <Card title="Eight-category closure" className="card--wide">
      <p className="supporting">
        These are governed row outcomes, not a claim that every included row is
        issue-free. <Value>INCLUDED_VALID</Value> means no unresolved
        policy-blocking issue; warnings or information findings may remain.
      </p>
      <div className="category-count-grid" aria-label="Closed row categories">
        {snapshot.counts.map((count) => (
          <div className="category-count" key={count.category}>
            <strong>{count.row_count}</strong>
            <span>{count.category}</span>
          </div>
        ))}
      </div>
      <DefinitionGrid
        items={[
          {
            label: "Exact closure",
            value: (
              <Status
                value={
                  total === snapshot.accepted_row_count ? "CLOSED" : "OPEN"
                }
              />
            ),
          },
          { label: "Classified rows", value: total },
          { label: "Accepted source rows", value: snapshot.accepted_row_count },
          { label: "Blocked rows", value: blocked },
        ]}
      />
    </Card>
  );
}

function Codes({ values, empty }: { values: string[]; empty: string }) {
  return values.length ? (
    <ul className="evidence-code-list">
      {values.map((value) => (
        <li key={value}>
          <Value>{value}</Value>
        </li>
      ))}
    </ul>
  ) : (
    <p className="muted">{empty}</p>
  );
}

function ExactReconciliationRow({
  snapshot,
  sourceFile,
  sourceOrdinal,
  location,
}: {
  snapshot: ResultReconciliationSnapshotSummary;
  sourceFile: string;
  sourceOrdinal: number;
  location: URL;
}) {
  const read = useRead<ResultReconciliationRowDetail>(
    apiQuery(
      `/api/v1/result-reconciliation/snapshots/${safeId(snapshot.snapshot_id)}/rows/${sourceOrdinal}`,
      { source_file: sourceFile },
    ),
    SCHEMAS.resultReconciliationRow,
  );
  return (
    <Card title="Exact row lineage" className="card--wide">
      <p className="supporting">
        This exact read exposes immutable identities, reason codes, and lineage
        references only. It does not expose received or effective values.
      </p>
      <ReadBoundary {...read}>
        {(response) => (
          <>
            <DefinitionGrid
              items={[
                {
                  label: "Source file",
                  value: <Value>{response.data.source_file}</Value>,
                },
                {
                  label: "Source ordinal",
                  value: response.data.source_ordinal,
                },
                {
                  label: "Lines",
                  value: `${response.data.line_start}–${response.data.line_end}`,
                },
                {
                  label: "Entity",
                  value: <Value>{response.data.entity_id}</Value>,
                },
                {
                  label: "Category",
                  value: <Status value={response.data.category} />,
                },
                {
                  label: "Row identity hash",
                  value: <Hash>{response.data.row_identity_hash}</Hash>,
                },
                {
                  label: "Classification hash",
                  value: <Hash>{response.data.classification_hash}</Hash>,
                },
              ]}
            />
            <h3>Reason chain</h3>
            <Codes
              values={response.data.explanation_codes}
              empty="No additional explanation code is required."
            />
            <TableShell label="Exact immutable lineage references">
              <table>
                <thead>
                  <tr>
                    <th scope="col">Kind</th>
                    <th scope="col">Identity</th>
                    <th scope="col">Hash</th>
                  </tr>
                </thead>
                <tbody>
                  {response.data.lineage_references.map((reference) => (
                    <tr
                      key={`${reference.reference_kind}\u0000${reference.reference_id}`}
                    >
                      <td>
                        <Status value={reference.reference_kind} />
                      </td>
                      <td>
                        <Value>{reference.reference_id}</Value>
                      </td>
                      <td>
                        <Hash>{reference.reference_hash}</Hash>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </TableShell>
          </>
        )}
      </ReadBoundary>
      <AppLink
        className="button button--secondary"
        href={selectedHref(location, {
          detail_source_file: null,
          detail_source_ordinal: null,
        })}
      >
        Close exact row detail
      </AppLink>
    </Card>
  );
}

function SnapshotRows({
  snapshot,
  location,
}: {
  snapshot: ResultReconciliationSnapshotSummary;
  location: URL;
}) {
  const category = location.searchParams.get("category");
  const boundedCategory = snapshot.counts.some(
    (count) => count.category === category,
  )
    ? category
    : null;
  const cursor = location.searchParams.get("row_cursor");
  const detailSourceFile = location.searchParams.get("detail_source_file");
  const detailOrdinalText = location.searchParams.get("detail_source_ordinal");
  const detailSourceOrdinal = detailOrdinalText
    ? Number(detailOrdinalText)
    : Number.NaN;
  const hasExactSelection =
    detailSourceFile !== null &&
    detailSourceFile.length > 0 &&
    Number.isSafeInteger(detailSourceOrdinal) &&
    detailSourceOrdinal > 0;
  const read = useRead<ResultReconciliationRowList>(
    apiQuery(
      `/api/v1/result-reconciliation/snapshots/${safeId(snapshot.snapshot_id)}/rows`,
      {
        page_size: pageSize(location),
        cursor,
        category: boundedCategory,
      },
    ),
    SCHEMAS.resultReconciliationRows,
  );
  function filter(event: FormEvent<HTMLFormElement>): void {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const selected = form.get("category");
    navigate(
      selectedHref(location, {
        category: typeof selected === "string" && selected ? selected : null,
        row_cursor: null,
      }),
    );
  }
  return (
    <Card title="Bounded row classifications" className="card--wide">
      <form className="inline-control" onSubmit={filter}>
        <label htmlFor="result-category-filter">
          Category
          <select
            id="result-category-filter"
            name="category"
            defaultValue={boundedCategory ?? ""}
          >
            <option value="">All categories</option>
            {snapshot.counts.map((count) => (
              <option key={count.category} value={count.category}>
                {count.category}
              </option>
            ))}
          </select>
        </label>
        <button className="button button--secondary" type="submit">
          Apply row filter
        </button>
      </form>
      <p className="supporting">
        Category is the final governed row outcome. Explanation codes are the
        contributing reason chain and do not override that outcome—for example,
        a decision-related code does not turn an otherwise blocked row into an
        included row. This broad list contains compact identities and lineage
        hashes only; it never renders source or effective canonical values.
      </p>
      <ReadBoundary {...read}>
        {(response) =>
          response.items.length ? (
            <>
              <TableShell label="Value-free reconciliation rows">
                <table>
                  <thead>
                    <tr>
                      <th scope="col">Accepted row</th>
                      <th scope="col">Entity</th>
                      <th scope="col">Category</th>
                      <th scope="col">Explanation</th>
                      <th scope="col">Lineage evidence</th>
                      <th scope="col">Inspect</th>
                    </tr>
                  </thead>
                  <tbody>
                    {response.items.map((row) => (
                      <tr
                        key={`${row.source_file}\u0000${row.source_ordinal}\u0000${row.entity_id}`}
                      >
                        <th scope="row">
                          <Value>{row.source_file}</Value>
                          <small>
                            ordinal {row.source_ordinal} · lines{" "}
                            {row.line_start}–{row.line_end}
                          </small>
                        </th>
                        <td>
                          <Value>{row.entity_id}</Value>
                        </td>
                        <td>
                          <Status value={row.category} />
                        </td>
                        <td>{row.explanation_codes.join(", ") || "None"}</td>
                        <td>{row.lineage_hashes.length} hash(es)</td>
                        <td>
                          <AppLink
                            href={selectedHref(location, {
                              detail_source_file: row.source_file,
                              detail_source_ordinal: String(row.source_ordinal),
                            })}
                          >
                            Inspect exact lineage
                          </AppLink>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </TableShell>
              <ResultPagination
                label="Reconciliation row pagination"
                page={response.page}
                firstHref={
                  cursor ? selectedHref(location, { row_cursor: null }) : null
                }
                nextHref={
                  response.page.next_cursor
                    ? selectedHref(location, {
                        row_cursor: response.page.next_cursor,
                      })
                    : null
                }
              />
              {hasExactSelection ? (
                <ExactReconciliationRow
                  snapshot={snapshot}
                  sourceFile={detailSourceFile}
                  sourceOrdinal={detailSourceOrdinal}
                  location={location}
                />
              ) : null}
            </>
          ) : (
            <EmptyState>No row classification matches this filter.</EmptyState>
          )
        }
      </ReadBoundary>
    </Card>
  );
}

function SnapshotDetail({
  snapshotId,
  location,
}: {
  snapshotId: string;
  location: URL;
}) {
  const read = useRead<ResultReconciliationSnapshot>(
    `/api/v1/result-reconciliation/snapshots/${safeId(snapshotId)}`,
    SCHEMAS.resultReconciliationSnapshot,
  );
  return (
    <ReadBoundary {...read}>
      {(response) => {
        const snapshot = response.data;
        return (
          <div className="dashboard-grid result-evidence-section">
            <CategoryClosure snapshot={snapshot} />
            <Card title="Snapshot identity">
              <DefinitionGrid
                items={[
                  { label: "Snapshot", value: snapshot.snapshot_id },
                  {
                    label: "Lifecycle",
                    value: <Status value={snapshot.state} />,
                  },
                  {
                    label: "Currentness",
                    value: (
                      <Status value={snapshot.current ? "CURRENT" : "STALE"} />
                    ),
                  },
                  { label: "Job", value: snapshot.job_id },
                  {
                    label: "Governing input hash",
                    value: <Hash>{snapshot.governing_input_hash}</Hash>,
                  },
                  {
                    label: "Snapshot hash",
                    value: <Hash>{snapshot.snapshot_hash}</Hash>,
                  },
                ]}
              />
            </Card>
            <Card title="Warnings and blockers">
              <h3>Warnings</h3>
              <Codes
                values={snapshot.warning_codes}
                empty="No warning code is reported."
              />
              <h3>Blockers</h3>
              <Codes
                values={snapshot.blocker_codes}
                empty="No blocker code is reported."
              />
            </Card>
            {snapshot.snapshot_hash ? (
              <SnapshotRows snapshot={snapshot} location={location} />
            ) : (
              <Card title="No complete row evidence" className="card--wide">
                <p>
                  A non-complete attempt has no canonical snapshot hash and
                  cannot support a candidate.
                </p>
              </Card>
            )}
          </div>
        );
      }}
    </ReadBoundary>
  );
}

function CalculationForm({ onCommitted }: { onCommitted: () => void }) {
  const runner = useCommandRunner(onCommitted);
  function calculate(event: FormEvent<HTMLFormElement>): void {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const actor = formText(form, "calculation_actor").trim();
    const reason = formText(form, "calculation_reason").trim();
    runner.submit("/api/v1/result-reconciliation/calculate", {
      schema_version: RESULT_COMMAND_SCHEMAS.calculate,
      actor,
      reason,
    });
  }
  return (
    <Card title="Calculate reconciliation" eyebrow="Immutable local evidence">
      <p>
        Calculate one closed, policy-bound snapshot. Calculation does not create
        a candidate or grant approval.
      </p>
      <form className="decision-form" onSubmit={calculate}>
        <label>
          Practitioner identifier
          <input
            name="calculation_actor"
            required
            maxLength={256}
            autoComplete="off"
          />
        </label>
        <label>
          Calculation reason
          <textarea name="calculation_reason" required maxLength={4096} />
        </label>
        <label className="acknowledgment">
          <input type="checkbox" required />I understand calculation records
          local reconciliation evidence only and does not approve a migration
          result or publication.
        </label>
        <button
          className="button"
          type="submit"
          disabled={runner.state.status === "submitting"}
        >
          Calculate immutable reconciliation
        </button>
      </form>
      <ResultCommandFeedback
        state={runner.state}
        retry={runner.retry}
        reset={runner.reset}
      />
    </Card>
  );
}

function CandidateCreationForm({
  snapshot,
  onCommitted,
}: {
  snapshot: ResultReconciliationSnapshotSummary | null;
  onCommitted: () => void;
}) {
  const runner = useCommandRunner(onCommitted);
  const accountedRows =
    snapshot?.counts.reduce((sum, item) => sum + item.row_count, 0) ?? 0;
  const unavailableReason = !snapshot
    ? "Select a reconciliation snapshot."
    : snapshot.state !== "COMPLETE"
      ? "The selected calculation has not completed."
      : !snapshot.current
        ? "The selected snapshot is not the current governed result."
        : !snapshot.snapshot_hash
          ? "The selected snapshot has no completed evidence hash."
          : accountedRows !== snapshot.accepted_row_count
            ? "The eight categories do not account for every accepted source row."
            : null;
  function create(event: FormEvent<HTMLFormElement>): void {
    event.preventDefault();
    if (!snapshot?.snapshot_hash) return;
    const form = new FormData(event.currentTarget);
    const command: CreateResultCandidateCommand = {
      schema_version: RESULT_COMMAND_SCHEMAS.createCandidate,
      snapshot_id: snapshot.snapshot_id,
      snapshot_hash: snapshot.snapshot_hash,
      creator: formText(form, "candidate_creator").trim(),
      preparation_reason: formText(form, "candidate_preparation_reason").trim(),
    };
    runner.submit("/api/v1/result-candidates/create", command);
  }
  return (
    <Card title="Prepare immutable result candidate">
      <p className="supporting">
        <strong>COMPLETE</strong> means the calculation finished;{" "}
        <strong>CURRENT</strong> means it is the newest governed result. A
        candidate requires both and full eight-category row accounting. Blocking
        evidence makes that candidate ineligible for approval, but it may still
        be prepared for explicit rejection.
      </p>
      {snapshot?.snapshot_hash && unavailableReason === null ? (
        <form className="decision-form" onSubmit={create}>
          {snapshot.blocker_codes.length > 0 ? (
            <div
              className="notice notice--attention"
              role="note"
              aria-label="Candidate eligibility warning"
            >
              <strong>Candidate will be ineligible</strong>
              <p>
                This snapshot contains blocking evidence. The immutable
                candidate may be reviewed and rejected, but it cannot be
                approved.
              </p>
            </div>
          ) : null}
          <p>
            Candidate source: <Value>{snapshot.snapshot_id}</Value>
          </p>
          <label>
            Candidate creator
            <input
              name="candidate_creator"
              required
              maxLength={256}
              autoComplete="off"
            />
          </label>
          <label>
            Preparation reason
            <textarea
              name="candidate_preparation_reason"
              required
              maxLength={4096}
            />
          </label>
          <label className="acknowledgment">
            <input type="checkbox" required />I understand candidate creation
            does not grant approval or publication authority.
          </label>
          <button
            className="button"
            type="submit"
            disabled={runner.state.status === "submitting"}
          >
            Create immutable candidate
          </button>
        </form>
      ) : (
        <div className="notice notice--attention" role="note">
          <strong>Candidate unavailable</strong>
          <p>{unavailableReason}</p>
        </div>
      )}
      <ResultCommandFeedback
        state={runner.state}
        retry={runner.retry}
        reset={runner.reset}
      />
    </Card>
  );
}

function CandidateHistory({
  response,
  selectedId,
  location,
}: {
  response: ResultCandidateList;
  selectedId: string | null;
  location: URL;
}) {
  if (!response.items.length) {
    return <EmptyState>No immutable result candidate exists.</EmptyState>;
  }
  return (
    <>
      <TableShell label="Immutable migration-result candidates">
        <table>
          <thead>
            <tr>
              <th scope="col">Candidate</th>
              <th scope="col">Snapshot</th>
              <th scope="col">Currentness</th>
              <th scope="col">Eligibility</th>
              <th scope="col">Included</th>
              <th scope="col">Excluded</th>
              <th scope="col">Blocked</th>
              <th scope="col">Inspect</th>
            </tr>
          </thead>
          <tbody>
            {response.items.map((candidate) => (
              <tr key={candidate.candidate_id}>
                <th scope="row">
                  <Value>{candidate.candidate_id}</Value>
                  <small>
                    <Hash>{candidate.candidate_hash}</Hash>
                  </small>
                </th>
                <td>
                  <Value>{candidate.snapshot_id}</Value>
                </td>
                <td>
                  <Status value={candidate.current ? "CURRENT" : "STALE"} />
                </td>
                <td>
                  <Status
                    value={candidate.eligible ? "ELIGIBLE" : "INELIGIBLE"}
                  />
                </td>
                <td>{candidate.included_count}</td>
                <td>{candidate.excluded_count}</td>
                <td>{candidate.blocked_count}</td>
                <td>
                  <AppLink
                    href={selectedHref(location, {
                      candidate_id: candidate.candidate_id,
                      action_cursor: null,
                    })}
                    current={candidate.candidate_id === selectedId}
                  >
                    Inspect candidate
                  </AppLink>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </TableShell>
      <ResultPagination
        label="Candidate history pagination"
        page={response.page}
        firstHref={
          location.searchParams.has("candidate_cursor")
            ? selectedHref(location, { candidate_cursor: null })
            : null
        }
        nextHref={
          response.page.next_cursor
            ? selectedHref(location, {
                candidate_cursor: response.page.next_cursor,
              })
            : null
        }
      />
    </>
  );
}

type ResultActionChoice = "approve" | "reject" | "revoke" | "supersede";

const ACTION_LABELS: Record<ResultActionChoice, string> = {
  approve: "approval",
  reject: "rejection",
  revoke: "revocation",
  supersede: "supersession",
};

interface PreparedAction {
  choice: ResultActionChoice;
  path: string;
  command: ResultCommand;
  actor: string;
  reason: string;
  acknowledgedWarnings: string[];
}

function actionSchema(choice: ResultActionChoice) {
  switch (choice) {
    case "approve":
      return RESULT_COMMAND_SCHEMAS.approve;
    case "reject":
      return RESULT_COMMAND_SCHEMAS.reject;
    case "revoke":
      return RESULT_COMMAND_SCHEMAS.revoke;
    case "supersede":
      return RESULT_COMMAND_SCHEMAS.supersede;
  }
}

function actionRoute(candidateId: string, choice: ResultActionChoice): string {
  return `/api/v1/result-candidates/${safeId(candidateId)}/${choice}`;
}

function ActionConfirmation({
  prepared,
  candidate,
  busy,
  onCommit,
  onEdit,
}: {
  prepared: PreparedAction;
  candidate: ResultCandidateSummary;
  busy: boolean;
  onCommit: () => void;
  onEdit: () => void;
}) {
  return (
    <Card
      title={`Confirm local result ${ACTION_LABELS[prepared.choice]}`}
      eyebrow="Consequential local evidence"
    >
      <DefinitionGrid
        items={[
          { label: "Candidate", value: candidate.candidate_id },
          {
            label: "Candidate hash",
            value: <Hash>{candidate.candidate_hash}</Hash>,
          },
          { label: "Snapshot", value: candidate.snapshot_id },
          {
            label: "Snapshot hash",
            value: <Hash>{candidate.snapshot_hash}</Hash>,
          },
          { label: "Included rows", value: candidate.included_count },
          { label: "Excluded rows", value: candidate.excluded_count },
          { label: "Blocked rows", value: candidate.blocked_count },
          { label: "Actor", value: prepared.actor },
          { label: "Reason", value: <Value>{prepared.reason}</Value> },
          {
            label: "Acknowledged warnings",
            value: prepared.acknowledgedWarnings.join(", ") || "None",
          },
        ]}
      />
      <p className="notice notice--attention">{AUTHORITY_BOUNDARY}</p>
      <div className="filter-actions">
        <button
          className="button button--secondary"
          type="button"
          disabled={busy}
          onClick={onEdit}
        >
          Back to edit
        </button>
        <button
          className="button"
          type="button"
          disabled={busy}
          onClick={onCommit}
        >
          {busy
            ? "Committing safely…"
            : `Commit immutable ${ACTION_LABELS[prepared.choice]}`}
        </button>
      </div>
    </Card>
  );
}

function ResultActionForm({
  candidate,
  eligibility,
  candidates,
  onCommitted,
}: {
  candidate: ResultCandidateSummary;
  eligibility: ResultApprovalEligibility["data"];
  candidates: ResultCandidateSummary[];
  onCommitted: () => void;
}) {
  const [choice, setChoice] = useState<ResultActionChoice>("approve");
  const [prepared, setPrepared] = useState<PreparedAction | null>(null);
  const runner = useCommandRunner(onCommitted);

  function review(event: FormEvent<HTMLFormElement>): void {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const actor = formText(form, "result_actor").trim();
    const reason = formText(form, "result_reason").trim();
    const acknowledgedWarnings = form
      .getAll("warning_acknowledgments")
      .filter((value): value is string => typeof value === "string")
      .sort();
    const base = {
      schema_version: actionSchema(choice),
      candidate_id: candidate.candidate_id,
      candidate_hash: candidate.candidate_hash,
      actor,
      reason,
      warning_acknowledgments: acknowledgedWarnings,
      acknowledge_local_only_boundary: true as const,
      expected_active_action_id: eligibility.expected_active_action_id,
      expected_generation: eligibility.expected_generation,
    };
    let command: ResultCommand;
    if (choice === "supersede") {
      const replacementId = formText(form, "replacement_candidate_id");
      const replacement = candidates.find(
        (item) => item.candidate_id === replacementId,
      );
      if (!replacement) return;
      command = {
        ...base,
        schema_version: RESULT_COMMAND_SCHEMAS.supersede,
        replacement_candidate_id: replacement.candidate_id,
        replacement_candidate_hash: replacement.candidate_hash,
      };
    } else {
      command = {
        ...base,
        schema_version: actionSchema(choice),
      } as ResultCommand;
    }
    setPrepared({
      choice,
      command,
      path: actionRoute(candidate.candidate_id, choice),
      actor,
      reason,
      acknowledgedWarnings,
    });
  }

  function edit(): void {
    setPrepared(null);
    runner.reset();
  }

  if (prepared) {
    if (runner.state.status === "result" || runner.state.status === "error") {
      return (
        <Card title="Migration-result action outcome">
          <ResultCommandFeedback
            state={runner.state}
            retry={runner.retry}
            reset={edit}
          />
        </Card>
      );
    }
    return (
      <ActionConfirmation
        prepared={prepared}
        candidate={candidate}
        busy={runner.state.status === "submitting"}
        onEdit={edit}
        onCommit={() => runner.submit(prepared.path, prepared.command)}
      />
    );
  }

  const approvalUnavailable =
    choice === "approve" && (!eligibility.current || !eligibility.eligible);
  const replacementCandidates = candidates.filter(
    (item) => item.candidate_id !== candidate.candidate_id && item.current,
  );
  return (
    <Card title="Record immutable result action">
      <form className="decision-form" onSubmit={review}>
        <label>
          Action
          <select
            name="result_action"
            value={choice}
            onChange={(event) =>
              setChoice(event.currentTarget.value as ResultActionChoice)
            }
          >
            <option value="approve">Approve local readiness evidence</option>
            <option value="reject">Reject candidate</option>
            <option value="revoke">Revoke current approval</option>
            <option value="supersede">Supersede current approval</option>
          </select>
        </label>
        <label>
          Actor / practitioner identifier
          <input
            name="result_actor"
            required
            maxLength={256}
            autoComplete="off"
          />
        </label>
        <label>
          Mandatory reason
          <textarea name="result_reason" required maxLength={4096} />
        </label>
        {eligibility.required_acknowledgments.length ? (
          <fieldset className="acknowledgment-fieldset">
            <legend>Required warning acknowledgments</legend>
            {eligibility.required_acknowledgments.map((code) => (
              <label className="acknowledgment" key={code}>
                <input
                  type="checkbox"
                  name="warning_acknowledgments"
                  value={code}
                  required={choice === "approve"}
                />
                <Value>{code}</Value>
              </label>
            ))}
          </fieldset>
        ) : (
          <p className="supporting">
            No warning acknowledgment is required for this candidate.
          </p>
        )}
        {choice === "supersede" ? (
          <label>
            Replacement candidate
            <select name="replacement_candidate_id" required>
              <option value="">Select current replacement</option>
              {replacementCandidates.map((replacement) => (
                <option
                  key={replacement.candidate_id}
                  value={replacement.candidate_id}
                >
                  {replacement.candidate_id}
                </option>
              ))}
            </select>
          </label>
        ) : null}
        <label className="acknowledgment authority-acknowledgment">
          <input
            name="authority_boundary_acknowledgment"
            type="checkbox"
            required
          />
          {AUTHORITY_BOUNDARY}
        </label>
        {approvalUnavailable ? (
          <p className="notice notice--critical" role="alert">
            Approval is unavailable because the candidate is stale or fails
            current eligibility. No client-side action can override the server.
          </p>
        ) : null}
        <button className="button" type="submit" disabled={approvalUnavailable}>
          Review {ACTION_LABELS[choice]}
        </button>
      </form>
    </Card>
  );
}

function CandidateDetail({
  candidate,
  candidates,
  onCommitted,
}: {
  candidate: ResultCandidateSummary;
  candidates: ResultCandidateSummary[];
  onCommitted: () => void;
}) {
  const read = useRead<ResultApprovalEligibility>(
    `/api/v1/result-candidates/${safeId(candidate.candidate_id)}/eligibility`,
    SCHEMAS.resultEligibility,
  );
  return (
    <ReadBoundary {...read}>
      {(response) => {
        const eligibility = response.data;
        return (
          <div className="dashboard-grid result-evidence-section">
            <Card title="Candidate identity">
              <DefinitionGrid
                items={[
                  { label: "Candidate", value: candidate.candidate_id },
                  {
                    label: "Candidate hash",
                    value: <Hash>{candidate.candidate_hash}</Hash>,
                  },
                  { label: "Snapshot", value: candidate.snapshot_id },
                  {
                    label: "Snapshot hash",
                    value: <Hash>{candidate.snapshot_hash}</Hash>,
                  },
                  { label: "Creator", value: candidate.creator },
                  {
                    label: "Preparation reason",
                    value: <Value>{candidate.preparation_reason}</Value>,
                  },
                ]}
              />
            </Card>
            <Card title="Current eligibility">
              <DefinitionGrid
                items={[
                  {
                    label: "Currentness",
                    value: (
                      <Status
                        value={eligibility.current ? "CURRENT" : "STALE"}
                      />
                    ),
                  },
                  {
                    label: "Approval eligibility",
                    value: (
                      <Status
                        value={eligibility.eligible ? "ELIGIBLE" : "INELIGIBLE"}
                      />
                    ),
                  },
                  {
                    label: "Observed generation",
                    value: eligibility.expected_generation,
                  },
                  {
                    label: "Active action",
                    value:
                      eligibility.expected_active_action_id ??
                      "No active action",
                  },
                ]}
              />
              <h3>Eligibility reason codes</h3>
              <Codes
                values={eligibility.reason_codes}
                empty="No blocking eligibility reason is reported."
              />
            </Card>
            <ResultActionForm
              candidate={candidate}
              eligibility={eligibility}
              candidates={candidates}
              onCommitted={onCommitted}
            />
            <Card title="Warnings and acknowledgments">
              <h3>Candidate warnings</h3>
              <Codes
                values={candidate.warning_codes}
                empty="No candidate warning is reported."
              />
              <h3>Required at approval</h3>
              <Codes
                values={eligibility.required_acknowledgments}
                empty="No warning acknowledgment is required."
              />
            </Card>
          </div>
        );
      }}
    </ReadBoundary>
  );
}

function ActionHistory({
  response,
  location,
}: {
  response: ResultActionHistory;
  location: URL;
}) {
  if (!response.items.length) {
    return <EmptyState>No immutable result action exists.</EmptyState>;
  }
  return (
    <>
      <div
        className="card-list"
        aria-label="Immutable migration-result action history"
      >
        {response.items.map((action) => (
          <Card
            key={action.action_id}
            eyebrow={action.action_kind}
            title={action.action_id}
          >
            <DefinitionGrid
              items={[
                { label: "Candidate", value: action.candidate_id },
                {
                  label: "Candidate hash",
                  value: <Hash>{action.candidate_hash}</Hash>,
                },
                { label: "Actor", value: <Value>{action.actor}</Value> },
                { label: "Reason", value: <Value>{action.reason}</Value> },
                { label: "Generation", value: action.generation },
                {
                  label: "Predecessor",
                  value: action.predecessor_action_id ?? "None",
                },
                { label: "Recorded", value: action.occurred_at },
                {
                  label: "Action hash",
                  value: <Hash>{action.action_hash}</Hash>,
                },
              ]}
            />
          </Card>
        ))}
      </div>
      <ResultPagination
        label="Result action history pagination"
        page={response.page}
        firstHref={
          location.searchParams.has("action_cursor")
            ? selectedHref(location, { action_cursor: null })
            : null
        }
        nextHref={
          response.page.next_cursor
            ? selectedHref(location, {
                action_cursor: response.page.next_cursor,
              })
            : null
        }
      />
    </>
  );
}

export function ResultApprovalPage({
  location,
  publicationAvailable,
}: {
  location: URL;
  publicationAvailable: boolean;
}) {
  const size = pageSize(location);
  const snapshots = useRead<ResultReconciliationSnapshotList>(
    apiQuery("/api/v1/result-reconciliation/snapshots", {
      page_size: size,
      cursor: location.searchParams.get("snapshot_cursor"),
    }),
    SCHEMAS.resultReconciliationSnapshots,
  );
  const candidates = useRead<ResultCandidateList>(
    apiQuery("/api/v1/result-candidates", {
      page_size: size,
      cursor: location.searchParams.get("candidate_cursor"),
    }),
    SCHEMAS.resultCandidates,
  );
  const actions = useRead<ResultActionHistory>(
    apiQuery("/api/v1/result-actions", {
      page_size: size,
      cursor: location.searchParams.get("action_cursor"),
    }),
    SCHEMAS.resultActions,
  );
  const refresh = () => {
    snapshots.retry();
    candidates.retry();
    actions.retry();
  };
  return (
    <>
      <PageHeader
        eyebrow="Local readiness evidence only"
        title="Result Reconciliation & Approval"
        description="Closed decision-aware row accounting, immutable candidates, explicit local result actions, and attributable history. Nothing here publishes data or authorizes a target system."
      />
      <div className="notice notice--attention" role="note">
        <strong>Authority boundary</strong>
        <p>
          Local practitioner identity is attributable evidence, not an
          authenticated identity or electronic signature.{" "}
          {publicationAvailable
            ? "Governed local publication remains independently eligibility-gated; target-system capabilities remain disabled."
            : "Publication and target-system capabilities remain disabled."}
        </p>
      </div>
      <nav
        className="section-index"
        aria-label="Local Result Approval sections"
      >
        <strong>On this page</strong>
        <ul>
          <li>
            <a href="#snapshot-history-title">Reconciliation snapshots</a>
          </li>
          <li>
            <a href="#candidate-history-title">Result candidates</a>
          </li>
          <li>
            <a href="#action-history-title">Result-action history</a>
          </li>
        </ul>
      </nav>
      <div className="dashboard-grid">
        <CalculationForm onCommitted={refresh} />
        <ReadBoundary {...snapshots}>
          {(snapshotResponse) => {
            const requestedSnapshotId =
              location.searchParams.get("snapshot_id");
            const selectedSnapshotId = snapshotResponse.items.some(
              (item) => item.snapshot_id === requestedSnapshotId,
            )
              ? requestedSnapshotId
              : (snapshotResponse.items[0]?.snapshot_id ?? null);
            const selectedSnapshot =
              snapshotResponse.items.find(
                (item) => item.snapshot_id === selectedSnapshotId,
              ) ?? null;
            return (
              <CandidateCreationForm
                snapshot={selectedSnapshot}
                onCommitted={refresh}
              />
            );
          }}
        </ReadBoundary>
      </div>

      <section
        className="result-section"
        aria-labelledby="snapshot-history-title"
      >
        <h2 id="snapshot-history-title">Reconciliation snapshot history</h2>
        <ReadBoundary {...snapshots}>
          {(response) => {
            const requestedId = location.searchParams.get("snapshot_id");
            const selectedId = response.items.some(
              (item) => item.snapshot_id === requestedId,
            )
              ? requestedId
              : (response.items[0]?.snapshot_id ?? null);
            return (
              <>
                <SnapshotHistory
                  response={response}
                  selectedId={selectedId}
                  location={location}
                />
                {selectedId ? (
                  <SnapshotDetail
                    key={selectedId}
                    snapshotId={selectedId}
                    location={location}
                  />
                ) : null}
              </>
            );
          }}
        </ReadBoundary>
      </section>

      <section
        className="result-section"
        aria-labelledby="candidate-history-title"
      >
        <h2 id="candidate-history-title">Migration-result candidates</h2>
        <ReadBoundary {...candidates}>
          {(response) => {
            const selectedId =
              location.searchParams.get("candidate_id") ??
              response.items[0]?.candidate_id ??
              null;
            const selected =
              response.items.find((item) => item.candidate_id === selectedId) ??
              null;
            return (
              <>
                <CandidateHistory
                  response={response}
                  selectedId={selectedId}
                  location={location}
                />
                {selected ? (
                  <CandidateDetail
                    key={selected.candidate_id}
                    candidate={selected}
                    candidates={response.items}
                    onCommitted={refresh}
                  />
                ) : null}
              </>
            );
          }}
        </ReadBoundary>
      </section>

      <section
        className="result-section"
        aria-labelledby="action-history-title"
      >
        <h2 id="action-history-title">Immutable result-action history</h2>
        <ReadBoundary {...actions}>
          {(response) => (
            <ActionHistory response={response} location={location} />
          )}
        </ReadBoundary>
      </section>
    </>
  );
}
