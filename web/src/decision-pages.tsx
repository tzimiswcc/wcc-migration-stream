import { type FormEvent, type ReactNode } from "react";
import { apiQuery, submitDecisionCommand, useRead } from "./api";
import {
  COMMAND_SCHEMAS,
  ContractError,
  DECISION_DISPOSITIONS,
  SCHEMAS,
  type CorrectionOverlayDetail,
  type DecisionAwareIssueDetail,
  type DecisionAttemptSummary,
  type DecisionCommand,
  type DecisionHistoryPage,
  type DecisionQueuePage,
  type DecisionResult,
} from "./contracts";
import {
  type DecisionAction,
  type DecisionDraft,
  parseDecisionAction,
  useDecisionFlow,
} from "./decision-flow";
import { AppLink, navigate } from "./router";
import {
  Card,
  DefinitionGrid,
  EmptyState,
  PageHeader,
  Pagination,
  ReadBoundary,
  Status,
  StructuredValue,
  TableShell,
  Value,
} from "./ui";

const ACTION_CONTENT: Record<
  DecisionAction,
  {
    label: string;
    disposition: string;
    meaning: string;
    acknowledgment: string;
  }
> = {
  correct: {
    label: "Correct",
    disposition: "CORRECTED",
    meaning:
      "Commit one non-destructive canonical overlay and re-evaluate its bounded validation scope.",
    acknowledgment:
      "I understand Correct records a non-destructive overlay and bounded re-evaluation only. It does not change accepted source evidence or approve a migration result, publication, external transmission, or target load.",
  },
  waive: {
    label: "Waive",
    disposition: "WAIVED",
    meaning:
      "Acknowledge the issue without changing source or canonical values.",
    acknowledgment:
      "I understand Waive keeps the row included as a governed exception without changing validation truth, source evidence, or the effective value; excluding the row is a separate decision. It does not approve a migration result, publication, external transmission, or target load.",
  },
  "accept-as-is": {
    label: "Accept as-is",
    disposition: "ACCEPTED_AS_IS",
    meaning:
      "Retain the exact current effective value while preserving the issue as validation truth.",
    acknowledgment:
      "I understand Accept as-is retains the exact current effective value and leaves validation truth unchanged. It does not approve a migration result, publication, external transmission, or target load.",
  },
  "exclude-row": {
    label: "Exclude row",
    disposition: "EXCLUDED",
    meaning:
      "Record row-scope evidence for a future migration-result candidate without deleting the row.",
    acknowledgment:
      "I understand Exclude row records evidence only for this exact accepted row in a future candidate. It does not delete source data or approve a migration result, publication, external transmission, or target load.",
  },
};

function pageRequest(location: URL): {
  pageSize: number;
  cursor: string | null;
} {
  const requested = Number(location.searchParams.get("page_size") ?? "50");
  return {
    pageSize:
      Number.isInteger(requested) && requested >= 1 && requested <= 200
        ? requested
        : 50,
    cursor: location.searchParams.get("cursor"),
  };
}

function safeId(value: string): string {
  return encodeURIComponent(value);
}

export function DecisionCapabilityUnavailablePage() {
  return (
    <>
      <PageHeader
        eyebrow="Capability unavailable"
        title="Governed decisions are disabled"
        description="This process retains the accepted read-only WP-004 mode. Restart with the explicit governed-decision capability to use consequential writes."
      />
      <Card title="No write authority inferred">
        <p>
          Loopback operation is not authentication, and this browser cannot
          enable decision authority itself.
        </p>
        <AppLink href="/issues">Return to Issues</AppLink>
      </Card>
    </>
  );
}

export function DecisionQueuePage({ location }: { location: URL }) {
  const { pageSize, cursor } = pageRequest(location);
  const disposition = location.searchParams.get("disposition");
  const read = useRead<DecisionQueuePage>(
    apiQuery("/api/v1/decision-queue", {
      page_size: pageSize,
      cursor,
      disposition,
    }),
    SCHEMAS.decisionQueue,
  );
  function apply(event: FormEvent<HTMLFormElement>): void {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const query = new URLSearchParams();
    query.set("page_size", String(pageSize));
    const selected = form.get("disposition");
    if (typeof selected === "string" && selected) {
      query.set("disposition", selected);
    }
    navigate(`/decisions?${query.toString()}`);
  }
  return (
    <>
      <PageHeader
        eyebrow="Governed practitioner evidence"
        title="Decision Queue"
        description="Bounded issue dispositions without source or corrected values. These records do not determine final job readiness."
      />
      <form
        className="filter-panel filter-panel--compact"
        aria-label="Decision queue filters"
        onSubmit={apply}
      >
        <label>
          Disposition
          <select name="disposition" defaultValue={disposition ?? ""}>
            <option value="">All states</option>
            <option value="UNRESOLVED">Unresolved</option>
            {DECISION_DISPOSITIONS.map((value) => (
              <option key={value} value={value}>
                {value.replaceAll("_", " ")}
              </option>
            ))}
          </select>
        </label>
        <div className="filter-actions">
          <button className="button" type="submit">
            Apply filter
          </button>
          <AppLink className="button button--secondary" href="/decisions">
            Clear
          </AppLink>
        </div>
      </form>
      <ReadBoundary {...read}>
        {(response) =>
          response.items.length ? (
            <>
              <TableShell label="Governed decision queue">
                <table>
                  <thead>
                    <tr>
                      <th scope="col">Issue</th>
                      <th scope="col">Canonical field</th>
                      <th scope="col">Accepted row</th>
                      <th scope="col">Disposition</th>
                      <th scope="col">Latest attempt</th>
                      <th scope="col">Detail</th>
                    </tr>
                  </thead>
                  <tbody>
                    {response.items.map((item) => (
                      <tr key={item.issue_id}>
                        <th scope="row">
                          <Status value={item.severity} />
                          <Value>{item.issue_code}</Value>
                        </th>
                        <td>
                          <Value>
                            {item.entity_id}.{item.field_id}
                          </Value>
                        </td>
                        <td>
                          <Value>
                            {item.source_file} · {item.source_ordinal}
                          </Value>
                        </td>
                        <td>
                          <Status
                            value={
                              item.active_decision?.disposition ?? "UNRESOLVED"
                            }
                          />
                        </td>
                        <td>
                          {item.latest_attempt ? (
                            <div>
                              <Status
                                value={item.latest_attempt.attempt_state}
                              />
                              <p className="supporting">
                                Lifecycle:{" "}
                                {item.latest_attempt.lifecycle_outcome}
                              </p>
                              {item.latest_attempt.correction_overlay_id ? (
                                <AppLink
                                  href={`/corrections/${safeId(item.latest_attempt.correction_overlay_id)}`}
                                >
                                  Inspect attempt overlay
                                </AppLink>
                              ) : null}
                            </div>
                          ) : (
                            "None"
                          )}
                        </td>
                        <td>
                          <AppLink href={`/issues/${safeId(item.issue_id)}`}>
                            Open governed detail
                          </AppLink>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </TableShell>
              <Pagination
                page={response.page}
                firstHref={cursor ? "/decisions" : null}
                nextHref={
                  response.page.next_cursor
                    ? apiQuery("/decisions", {
                        page_size: pageSize,
                        disposition,
                        cursor: response.page.next_cursor,
                      })
                    : null
                }
              />
            </>
          ) : (
            <EmptyState>No issue matches this decision queue.</EmptyState>
          )
        }
      </ReadBoundary>
    </>
  );
}

function IssueIdentity({ detail }: { detail: DecisionAwareIssueDetail }) {
  const issue = detail.data;
  return (
    <Card title="Issue and accepted row">
      <DefinitionGrid
        items={[
          { label: "Issue", value: issue.issue_id },
          { label: "Severity", value: <Status value={issue.severity} /> },
          { label: "Rule", value: issue.rule_id },
          {
            label: "Canonical field",
            value: `${issue.entity_id}.${issue.field_id}`,
          },
          {
            label: "Accepted row",
            value: `${issue.accepted_row.source_file} · ${issue.accepted_row.source_ordinal}`,
          },
          { label: "Validation truth", value: issue.display_message },
        ]}
      />
    </Card>
  );
}

function FieldEvidence({ detail }: { detail: DecisionAwareIssueDetail }) {
  const field = detail.data.field_context;
  return (
    <div className="row-evidence-grid">
      <Card eyebrow="Immutable evidence" title="Received and original">
        <DefinitionGrid
          items={[
            {
              label: "Received source value",
              value: <Value>{field.received_source_value ?? "Null"}</Value>,
            },
            {
              label: "Original canonical value",
              value: <Value>{field.original_canonical_value ?? "Null"}</Value>,
            },
          ]}
        />
        <h3>Original lineage</h3>
        <StructuredValue value={field.original_lineage} />
      </Card>
      <Card eyebrow="Governed derived view" title="Overlay and effective value">
        <DefinitionGrid
          items={[
            {
              label: "Active overlay",
              value: field.active_overlay_id ?? "None",
            },
            {
              label: "Overlay value",
              value: <Value>{field.active_overlay_value ?? "None"}</Value>,
            },
            {
              label: "Effective canonical value",
              value: <Value>{field.effective_canonical_value ?? "Null"}</Value>,
            },
            {
              label: "Correction supported",
              value: <Status value={field.correction_eligible} />,
            },
          ]}
        />
      </Card>
    </div>
  );
}

function LatestAttemptCard({
  attempt,
}: {
  attempt: DecisionAttemptSummary | null;
}) {
  return (
    <Card title="Latest command attempt">
      {attempt ? (
        <>
          <DefinitionGrid
            items={[
              { label: "Attempt", value: attempt.decision_attempt_id },
              { label: "Disposition", value: attempt.disposition },
              {
                label: "Attempt state",
                value: <Status value={attempt.attempt_state} />,
              },
              {
                label: "Re-evaluation",
                value: <Status value={attempt.reevaluation_state} />,
              },
              {
                label: "Lifecycle",
                value: <Status value={attempt.lifecycle_outcome} />,
              },
              { label: "Active", value: <Status value={attempt.active} /> },
              {
                label: "Integrity",
                value: <Status value={attempt.integrity_status} />,
              },
            ]}
          />
          {attempt.correction_overlay_id ? (
            <AppLink
              href={`/corrections/${safeId(attempt.correction_overlay_id)}`}
            >
              Inspect exact correction attempt
            </AppLink>
          ) : null}
        </>
      ) : (
        <p>No governed command attempt is recorded for this issue.</p>
      )}
    </Card>
  );
}

export function DecisionIssuePage({ issueId }: { issueId: string }) {
  const read = useRead<DecisionAwareIssueDetail>(
    `/api/v1/issues/${safeId(issueId)}/decision-detail`,
    SCHEMAS.decisionIssue,
  );
  return (
    <>
      <PageHeader
        eyebrow="Exact governed context"
        title="Issue Decision"
        description="Validation truth, immutable source evidence, effective canonical context, and practitioner disposition remain separate."
        action={
          <AppLink className="button button--secondary" href="/decisions">
            Back to Decision Queue
          </AppLink>
        }
      />
      <ReadBoundary {...read}>
        {(detail) => (
          <>
            <IssueIdentity detail={detail} />
            <FieldEvidence detail={detail} />
            <LatestAttemptCard attempt={detail.data.latest_attempt} />
            <Card title="Governed actions">
              <p>{detail.data.authority_notice}</p>
              <div
                className="decision-action-list"
                aria-label="Issue decisions"
              >
                {(Object.keys(ACTION_CONTENT) as DecisionAction[]).map(
                  (action) => (
                    <AppLink
                      key={action}
                      className="button"
                      href={`/issues/${safeId(issueId)}/decide/${action}`}
                    >
                      {ACTION_CONTENT[action].label}
                    </AppLink>
                  ),
                )}
              </div>
              {!detail.data.field_context.correction_eligible ? (
                <p className="supporting">
                  Correction is shown for vocabulary completeness but the server
                  remains authority and must reject an ineligible field.
                </p>
              ) : null}
            </Card>
            <Card title="Decision history">
              <p>
                {detail.data.decision_history_count} immutable decision
                record(s) reported.
              </p>
              <AppLink href={`/issues/${safeId(issueId)}/history`}>
                Inspect immutable history
              </AppLink>
            </Card>
          </>
        )}
      </ReadBoundary>
    </>
  );
}

function commonCommand(detail: DecisionAwareIssueDetail, draft: DecisionDraft) {
  return {
    validation_run_id: detail.data.validation_run_id,
    issue_hash: detail.data.issue_hash,
    practitioner_identifier: draft.practitionerIdentifier,
    reason: draft.reason,
    evidence_reference: draft.evidenceReference || null,
    expected_active_decision_id:
      detail.data.concurrency.expected_active_decision_id,
    expected_active_generation:
      detail.data.concurrency.expected_active_generation,
    acknowledge_not_migration_result_approval: true as const,
  };
}

function buildCommand(
  action: DecisionAction,
  detail: DecisionAwareIssueDetail,
  draft: DecisionDraft,
): DecisionCommand {
  const common = commonCommand(detail, draft);
  if (action === "correct") {
    return {
      ...common,
      schema_version: COMMAND_SCHEMAS.corrected,
      correction: {
        source_file: detail.data.accepted_row.source_file,
        source_ordinal: detail.data.accepted_row.source_ordinal,
        entity_id: detail.data.entity_id,
        field_id: detail.data.field_id,
        operation: draft.operation,
        value:
          draft.operation === "SET_EFFECTIVE_NULL"
            ? null
            : draft.operation === "SET_EFFECTIVE_EMPTY"
              ? ""
              : draft.proposedValue,
        expected_effective_value_hash:
          detail.data.concurrency.expected_effective_value_hash,
      },
    };
  }
  if (action === "waive") {
    return {
      ...common,
      schema_version: COMMAND_SCHEMAS.waived,
      acknowledge_waiver_limitations: true,
    };
  }
  if (action === "accept-as-is") {
    return {
      ...common,
      schema_version: COMMAND_SCHEMAS.acceptedAsIs,
      expected_effective_value_hash:
        detail.data.concurrency.expected_effective_value_hash,
      acknowledge_validation_truth_unchanged: true,
    };
  }
  return {
    ...common,
    schema_version: COMMAND_SCHEMAS.excluded,
    source_file: detail.data.accepted_row.source_file,
    source_ordinal: detail.data.accepted_row.source_ordinal,
    acknowledge_row_scope: true,
  };
}

function decisionEndpoint(issueId: string, action: DecisionAction): string {
  const endpoint =
    action === "accept-as-is"
      ? "accept-as-is"
      : action === "exclude-row"
        ? "exclude-row"
        : action;
  return `/api/v1/issues/${safeId(issueId)}/decisions/${endpoint}`;
}

function DecisionConfirmation({
  action,
  detail,
  draft,
  busy,
  onEdit,
  onCommit,
}: {
  action: DecisionAction;
  detail: DecisionAwareIssueDetail;
  draft: DecisionDraft;
  busy: boolean;
  onEdit: () => void;
  onCommit: () => void;
}) {
  return (
    <Card
      title="Confirm governed decision"
      eyebrow={ACTION_CONTENT[action].disposition}
    >
      <p>{ACTION_CONTENT[action].meaning}</p>
      <DefinitionGrid
        items={[
          { label: "Issue", value: detail.data.issue_id },
          {
            label: "Row and field",
            value: `${detail.data.accepted_row.source_file} · ${detail.data.accepted_row.source_ordinal} · ${detail.data.entity_id}.${detail.data.field_id}`,
          },
          {
            label: "Practitioner identifier",
            value: draft.practitionerIdentifier,
          },
          { label: "Reason", value: <Value>{draft.reason}</Value> },
          {
            label: "Concurrency generation",
            value: detail.data.concurrency.expected_active_generation,
          },
          ...(action === "correct"
            ? [
                {
                  label: "Proposed correction",
                  value: (
                    <Value>
                      {draft.operation === "REPLACE_EFFECTIVE_TEXT"
                        ? draft.proposedValue
                        : draft.operation}
                    </Value>
                  ),
                },
              ]
            : []),
        ]}
      />
      <p className="notice notice--attention">
        {ACTION_CONTENT[action].acknowledgment}
      </p>
      <div className="filter-actions">
        <button
          className="button button--secondary"
          type="button"
          onClick={onEdit}
          disabled={busy}
        >
          Back to edit
        </button>
        <button
          className="button"
          type="button"
          onClick={onCommit}
          disabled={busy}
        >
          {busy ? "Committing safely…" : "Commit immutable decision"}
        </button>
      </div>
    </Card>
  );
}

function DecisionForm({
  issueId,
  action,
  detail,
}: {
  issueId: string;
  action: DecisionAction;
  detail: DecisionAwareIssueDetail;
}) {
  const flow = useDecisionFlow(action);
  const { state } = flow;

  function review(event: FormEvent<HTMLFormElement>): void {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const text = (name: string) => {
      const value = form.get(name);
      return typeof value === "string" ? value : "";
    };
    flow.review({
      practitionerIdentifier: text("practitioner_identifier").trim(),
      reason: text("reason").trim(),
      evidenceReference: text("evidence_reference").trim(),
      operation: text("operation") as DecisionDraft["operation"],
      proposedValue: text("proposed_value"),
    });
  }

  async function commit(): Promise<void> {
    const command = buildCommand(action, detail, state.draft);
    flow.submit();
    try {
      const result = await submitDecisionCommand(
        decisionEndpoint(issueId, action),
        command,
        state.idempotencyKey,
      );
      flow.succeed(result);
    } catch (error) {
      flow.fail(
        error instanceof ContractError
          ? error
          : new ContractError(
              "UNEXPECTED_CLIENT_ERROR",
              "The command result could not be displayed safely.",
            ),
      );
    }
  }

  if (state.status === "result") {
    return (
      <div className="state-panel" role="status" aria-live="polite">
        <div>
          <p className="eyebrow">{state.result.data.lifecycle_outcome}</p>
          <strong>Durable decision recorded</strong>
          <p>
            Re-evaluation: {state.result.data.reevaluation_state}. Current
            active state: {state.result.data.active ? "ACTIVE" : "INACTIVE"}.
            Transport success alone has not been treated as authority.
          </p>
          <p className="supporting">Decision {state.result.data.decision_id}</p>
          <div className="filter-actions">
            <AppLink
              href={`/decisions/${safeId(state.result.data.decision_id)}`}
            >
              Inspect durable result
            </AppLink>
            <AppLink href={`/issues/${safeId(issueId)}/history`}>
              Inspect history
            </AppLink>
          </div>
        </div>
      </div>
    );
  }

  if (state.status === "error") {
    return (
      <div className="state-panel state-panel--error" role="alert">
        <div>
          <p className="eyebrow">{state.error.code}</p>
          <strong>Decision was not presented as committed</strong>
          <p>{state.error.message}</p>
          {state.error.requestId ? (
            <p className="supporting">Request {state.error.requestId}</p>
          ) : null}
          <div className="filter-actions">
            <button
              className="button button--secondary"
              type="button"
              onClick={() => flow.edit()}
            >
              Return to edit
            </button>
            {state.error.retryable ? (
              <button
                className="button"
                type="button"
                onClick={() => flow.retry()}
              >
                Retry same command identity
              </button>
            ) : null}
          </div>
        </div>
      </div>
    );
  }

  if (state.status === "confirming" || state.status === "submitting") {
    return (
      <DecisionConfirmation
        action={action}
        detail={detail}
        draft={state.draft}
        busy={state.status === "submitting"}
        onEdit={() => flow.edit()}
        onCommit={() => void commit()}
      />
    );
  }

  const content = ACTION_CONTENT[action];
  return (
    <form
      className="decision-form"
      aria-label={`${content.label} issue`}
      onSubmit={review}
    >
      <Card title={`${content.label} issue`} eyebrow={content.disposition}>
        <p>{content.meaning}</p>
        <label>
          Practitioner identifier
          <input
            name="practitioner_identifier"
            required
            maxLength={128}
            autoComplete="off"
            defaultValue={state.draft.practitionerIdentifier}
          />
        </label>
        <label>
          Reason
          <textarea
            name="reason"
            required
            maxLength={4096}
            rows={4}
            defaultValue={state.draft.reason}
          />
        </label>
        <label>
          Evidence reference (optional)
          <input
            name="evidence_reference"
            maxLength={1024}
            autoComplete="off"
            defaultValue={state.draft.evidenceReference}
          />
        </label>
        {action === "correct" ? (
          <>
            <label>
              Correction operation
              <select name="operation" defaultValue={state.draft.operation}>
                {detail.data.field_context.allowed_operations.map(
                  (operation) => (
                    <option key={operation} value={operation}>
                      {operation.replaceAll("_", " ")}
                    </option>
                  ),
                )}
              </select>
            </label>
            <label>
              Proposed overlay text
              <textarea
                name="proposed_value"
                rows={3}
                maxLength={65_536}
                defaultValue={state.draft.proposedValue}
              />
            </label>
          </>
        ) : null}
        <label className="acknowledgment">
          <input name="acknowledgment" type="checkbox" required />
          {content.acknowledgment}
        </label>
        <button className="button" type="submit">
          Review decision
        </button>
      </Card>
    </form>
  );
}

export function DecisionActionPage({
  issueId,
  actionName,
}: {
  issueId: string;
  actionName: string;
}) {
  const action = parseDecisionAction(actionName);
  if (!action) {
    return (
      <>
        <PageHeader
          eyebrow="Closed decision vocabulary"
          title="Unsupported decision action"
          description="Only Correct, Waive, Accept as-is, and Exclude row are supported."
        />
        <Card>
          <AppLink href={`/issues/${safeId(issueId)}`}>Return to issue</AppLink>
        </Card>
      </>
    );
  }
  return <SupportedDecisionActionPage issueId={issueId} action={action} />;
}

function SupportedDecisionActionPage({
  issueId,
  action,
}: {
  issueId: string;
  action: DecisionAction;
}) {
  const read = useRead<DecisionAwareIssueDetail>(
    `/api/v1/issues/${safeId(issueId)}/decision-detail`,
    SCHEMAS.decisionIssue,
  );
  return (
    <>
      <PageHeader
        eyebrow="Consequential local write"
        title={ACTION_CONTENT[action].label}
        description="Review one exact issue scope, provide attributable evidence, then explicitly confirm the immutable command."
        action={
          <AppLink
            className="button button--secondary"
            href={`/issues/${safeId(issueId)}`}
          >
            Cancel
          </AppLink>
        }
      />
      <ReadBoundary {...read}>
        {(detail) => (
          <>
            <IssueIdentity detail={detail} />
            <FieldEvidence detail={detail} />
            <DecisionForm issueId={issueId} action={action} detail={detail} />
          </>
        )}
      </ReadBoundary>
    </>
  );
}

export function DecisionHistoryPage({
  issueId,
  location,
}: {
  issueId: string;
  location: URL;
}) {
  const { pageSize, cursor } = pageRequest(location);
  const read = useRead<DecisionHistoryPage>(
    apiQuery(`/api/v1/issues/${safeId(issueId)}/decisions`, {
      page_size: pageSize,
      cursor,
    }),
    SCHEMAS.decisionHistory,
  );
  return (
    <>
      <PageHeader
        eyebrow="Append-only evidence"
        title="Decision History"
        description="Active, superseded, failed, interrupted, stale, and integrity-blocked outcomes remain distinct."
        action={
          <AppLink
            className="button button--secondary"
            href={`/issues/${safeId(issueId)}`}
          >
            Back to issue
          </AppLink>
        }
      />
      <ReadBoundary {...read}>
        {(response) =>
          response.items.length ? (
            <>
              <div className="card-list">
                {response.items.map((item) => (
                  <Card
                    key={item.decision_id}
                    eyebrow={item.active ? "ACTIVE" : item.lifecycle_outcome}
                    title={item.disposition.replaceAll("_", " ")}
                  >
                    <DefinitionGrid
                      items={[
                        { label: "Decision", value: item.decision_id },
                        {
                          label: "Practitioner",
                          value: item.practitioner_identifier,
                        },
                        {
                          label: "Reason",
                          value: <Value>{item.reason}</Value>,
                        },
                        { label: "Applied", value: item.applied_at },
                        { label: "Generation", value: item.generation },
                        {
                          label: "Predecessor",
                          value: item.prior_decision_id ?? "None",
                        },
                      ]}
                    />
                    {item.correction_overlay_id ? (
                      <AppLink
                        href={`/corrections/${safeId(item.correction_overlay_id)}`}
                      >
                        Inspect correction overlay
                      </AppLink>
                    ) : null}
                  </Card>
                ))}
              </div>
              <Pagination
                page={response.page}
                firstHref={cursor ? `/issues/${safeId(issueId)}/history` : null}
                nextHref={
                  response.page.next_cursor
                    ? apiQuery(`/issues/${safeId(issueId)}/history`, {
                        page_size: pageSize,
                        cursor: response.page.next_cursor,
                      })
                    : null
                }
              />
            </>
          ) : (
            <EmptyState>No immutable decision history exists.</EmptyState>
          )
        }
      </ReadBoundary>
    </>
  );
}

export function CorrectionOverlayPage({ overlayId }: { overlayId: string }) {
  const read = useRead<CorrectionOverlayDetail>(
    `/api/v1/corrections/${safeId(overlayId)}`,
    SCHEMAS.correction,
  );
  return (
    <>
      <PageHeader
        eyebrow="Bounded exact detail"
        title="Correction Overlay"
        description="Original, overlay, and effective canonical values remain distinct; received source evidence is never edited."
      />
      <ReadBoundary {...read}>
        {(response) => (
          <>
            <Card title="Overlay identity">
              <DefinitionGrid
                items={[
                  { label: "Overlay", value: response.data.overlay_id },
                  {
                    label: "Attempt",
                    value: response.data.decision_attempt_id,
                  },
                  {
                    label: "Decision",
                    value: response.data.decision_id ?? "No effective decision",
                  },
                  {
                    label: "Field",
                    value: `${response.data.entity_id}.${response.data.field_id}`,
                  },
                  { label: "Operation", value: response.data.operation },
                  {
                    label: "Active",
                    value: <Status value={response.data.active} />,
                  },
                  {
                    label: "Re-evaluation",
                    value: <Status value={response.data.reevaluation_state} />,
                  },
                  {
                    label: "Attempt state",
                    value: <Status value={response.data.attempt_state} />,
                  },
                  {
                    label: "Lifecycle",
                    value: <Status value={response.data.lifecycle_outcome} />,
                  },
                  { label: "Attempted", value: response.data.attempted_at },
                  {
                    label: "Applied",
                    value: response.data.applied_at ?? "Not applied",
                  },
                ]}
              />
            </Card>
            <div className="row-evidence-grid">
              <Card title="Original canonical evidence">
                <Value>
                  {response.data.original_canonical_value ?? "Null"}
                </Value>
                <StructuredValue value={response.data.original_lineage} />
              </Card>
              <Card title="Overlay and effective evidence">
                <DefinitionGrid
                  items={[
                    {
                      label: "Prior effective value",
                      value: (
                        <Value>
                          {response.data.prior_effective_value ?? "Null"}
                        </Value>
                      ),
                    },
                    {
                      label: "Overlay value",
                      value: (
                        <Value>{response.data.overlay_value ?? "Null"}</Value>
                      ),
                    },
                    {
                      label: "Effective value",
                      value: (
                        <Value>
                          {response.data.effective_canonical_value ?? "Null"}
                        </Value>
                      ),
                    },
                    {
                      label: "Reason",
                      value: <Value>{response.data.reason}</Value>,
                    },
                  ]}
                />
              </Card>
            </div>
            {response.data.error_code ? (
              <Card eyebrow="Inert attempt evidence" title="Attempt diagnostic">
                <DefinitionGrid
                  items={[
                    { label: "Error code", value: response.data.error_code },
                    {
                      label: "Error message",
                      value: (
                        <Value>
                          {response.data.error_message ?? "Unavailable"}
                        </Value>
                      ),
                    },
                  ]}
                />
                <p className="supporting">
                  This diagnostic does not create or reactivate a decision.
                </p>
              </Card>
            ) : null}
          </>
        )}
      </ReadBoundary>
    </>
  );
}

export function DecisionResultPage({ decisionId }: { decisionId: string }) {
  const read = useRead<DecisionResult>(
    `/api/v1/decisions/${safeId(decisionId)}`,
    SCHEMAS.decisionResult,
  );
  return (
    <>
      <PageHeader
        eyebrow="Durable command result"
        title="Decision Result"
        description="Persistence, correction effectiveness, re-evaluation, and issue truth are reported separately."
      />
      <ReadBoundary {...read}>
        {(response) => (
          <Card title={response.data.disposition.replaceAll("_", " ")}>
            <DefinitionGrid
              items={[
                { label: "Decision", value: response.data.decision_id },
                {
                  label: "Lifecycle outcome",
                  value: <Status value={response.data.lifecycle_outcome} />,
                },
                {
                  label: "Active",
                  value: <Status value={response.data.active} />,
                },
                {
                  label: "Re-evaluation",
                  value: <Status value={response.data.reevaluation_state} />,
                },
                {
                  label: "Original issue remains",
                  value: (
                    <Status value={response.data.original_issue_present} />
                  ),
                },
                {
                  label: "Residual issues",
                  value: response.data.residual_issue_count,
                },
                { label: "New issues", value: response.data.new_issue_count },
                {
                  label: "Integrity",
                  value: <Status value={response.data.integrity_status} />,
                },
              ]}
            />
          </Card>
        )}
      </ReadBoundary>
    </>
  );
}

export function DecisionBreadcrumbs({ children }: { children: ReactNode }) {
  return (
    <nav className="breadcrumbs" aria-label="Breadcrumb">
      <AppLink href="/decisions">Decision Queue</AppLink>
      <span aria-hidden="true">/</span>
      {children}
    </nav>
  );
}
