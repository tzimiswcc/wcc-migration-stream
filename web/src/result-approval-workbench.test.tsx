import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import axe from "axe-core";
import { beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { submitResultCommand } from "./api";
import {
  RECONCILIATION_CATEGORIES,
  RESULT_COMMAND_SCHEMAS,
  SCHEMAS,
  canApproveMigrationResult,
  canGovernIssueDecisions,
  canPublishReadiness,
  decodeContext,
  decodeContract,
  validateResultCommand,
  type ApproveResultCandidateCommand,
} from "./contracts";
import {
  contextFixture,
  governedContextFixture,
  publicationContextFixture,
} from "./test/fixtures";
import { mockApi } from "./test/mock-api";

const hash = (character: string) => character.repeat(64);

const resultApprovalContextFixture = {
  ...governedContextFixture,
  schema_version: SCHEMAS.resultApprovalContext,
  capabilities: {
    ...governedContextFixture.capabilities,
    migration_result_approval: true,
  },
};

const categoryCounts = [
  { category: "INCLUDED_VALID", row_count: 4 },
  { category: "INCLUDED_CORRECTED", row_count: 1 },
  { category: "INCLUDED_WAIVED", row_count: 1 },
  { category: "INCLUDED_ACCEPTED_AS_IS", row_count: 1 },
  { category: "EXCLUDED", row_count: 1 },
  { category: "BLOCKED_UNRESOLVED", row_count: 0 },
  { category: "BLOCKED_INCOMPLETE", row_count: 0 },
  { category: "BLOCKED_CONFLICT", row_count: 0 },
] as const;

const snapshotSummary = {
  snapshot_id: "snapshot-synthetic-006",
  state: "COMPLETE",
  job_id: "job-synthetic-004",
  accepted_row_count: 8,
  governing_input_hash: hash("a"),
  snapshot_hash: hash("b"),
  current: true,
  counts: categoryCounts,
  warning_codes: ["REVIEW_SYNTHETIC"],
  blocker_codes: [],
  completed_at: "2026-07-22T20:00:00Z",
};

const snapshotListFixture = {
  schema_version: SCHEMAS.resultReconciliationSnapshots,
  scope: { job_id: "job-synthetic-004" },
  items: [snapshotSummary],
  page: { page_size: 50, returned: 1, next_cursor: null },
  canonical_hash: hash("c"),
};

const snapshotFixture = {
  schema_version: SCHEMAS.resultReconciliationSnapshot,
  data: snapshotSummary,
  canonical_hash: hash("d"),
};

const rowListFixture = {
  schema_version: SCHEMAS.resultReconciliationRows,
  scope: {
    job_id: "job-synthetic-004",
    snapshot_id: snapshotSummary.snapshot_id,
    snapshot_hash: snapshotSummary.snapshot_hash,
  },
  filters: {
    category: null,
    source_file: null,
    source_ordinal: null,
    entity_id: null,
    issue_id: null,
    decision_id: null,
  },
  items: [
    {
      source_file: "synthetic/rows.csv",
      source_ordinal: 1,
      line_start: 2,
      line_end: 2,
      entity_id: "SyntheticObject",
      category: "INCLUDED_VALID",
      explanation_codes: [],
      lineage_hashes: [hash("e")],
    },
  ],
  page: { page_size: 50, returned: 1, next_cursor: null },
  canonical_hash: hash("f"),
};

const rowDetailFixture = {
  schema_version: SCHEMAS.resultReconciliationRow,
  scope: rowListFixture.scope,
  data: {
    source_file: "synthetic/rows.csv",
    source_ordinal: 1,
    line_start: 2,
    line_end: 2,
    row_identity_hash: hash("9"),
    entity_id: "SyntheticObject",
    category: "INCLUDED_VALID",
    explanation_codes: [],
    lineage_references: [
      {
        reference_kind: "VALIDATION_RUN",
        reference_id: "validation-synthetic-006",
        reference_hash: hash("e"),
      },
    ],
    classification_hash: hash("8"),
  },
  canonical_hash: hash("7"),
};

const candidate = {
  candidate_id: "candidate-synthetic-006",
  snapshot_id: snapshotSummary.snapshot_id,
  snapshot_hash: snapshotSummary.snapshot_hash,
  candidate_hash: hash("1"),
  current: true,
  eligible: true,
  included_count: 7,
  excluded_count: 1,
  blocked_count: 0,
  warning_codes: ["REVIEW_SYNTHETIC"],
  required_acknowledgments: ["REVIEW_SYNTHETIC"],
  creator: "practitioner-synthetic",
  preparation_reason: '<img src=x onerror="alert(1)">',
  created_at: "2026-07-22T20:01:00Z",
};

const staleCandidate = {
  ...candidate,
  candidate_id: "candidate-stale-006",
  candidate_hash: hash("2"),
  current: false,
  eligible: false,
  preparation_reason: "Earlier immutable candidate now stale.",
};

const candidateListFixture = {
  schema_version: SCHEMAS.resultCandidates,
  scope: { job_id: "job-synthetic-004" },
  items: [candidate, staleCandidate],
  page: { page_size: 50, returned: 2, next_cursor: null },
  canonical_hash: hash("3"),
};

const eligibilityFixture = {
  schema_version: SCHEMAS.resultEligibility,
  data: {
    candidate_id: candidate.candidate_id,
    candidate_hash: candidate.candidate_hash,
    snapshot_id: candidate.snapshot_id,
    snapshot_hash: candidate.snapshot_hash,
    eligible: true,
    current: true,
    reason_codes: [],
    required_acknowledgments: ["REVIEW_SYNTHETIC"],
    expected_active_action_id: "action-synthetic-previous",
    expected_generation: 1,
  },
  canonical_hash: hash("4"),
};

const actionHistoryFixture = {
  schema_version: SCHEMAS.resultActions,
  scope: { job_id: "job-synthetic-004", candidate_id: null },
  items: [
    {
      action_id: "action-synthetic-previous",
      action_kind: "REJECTED",
      action_hash: hash("5"),
      candidate_id: candidate.candidate_id,
      candidate_hash: candidate.candidate_hash,
      actor: "practitioner-history",
      reason: '<script>alert("history")</script>',
      generation: 1,
      predecessor_action_id: null,
      occurred_at: "2026-07-22T20:02:00Z",
    },
  ],
  page: { page_size: 50, returned: 1, next_cursor: null },
  canonical_hash: hash("6"),
};

const actionResultFixture = {
  schema_version: SCHEMAS.resultActionCommand,
  data: {
    action_id: "action-synthetic-approved",
    action_kind: "APPROVED",
    action_hash: hash("7"),
    candidate_id: candidate.candidate_id,
    candidate_hash: candidate.candidate_hash,
    generation: 2,
    active_approval: true,
    idempotent_replay: false,
  },
  canonical_hash: hash("8"),
};

const reconciliationCommandResultFixture = {
  schema_version: SCHEMAS.resultReconciliationCommand,
  data: {
    snapshot_id: snapshotSummary.snapshot_id,
    snapshot_hash: snapshotSummary.snapshot_hash,
    state: "COMPLETE",
    current: true,
    accepted_row_count: 8,
    blocked_count: 0,
    idempotent_replay: false,
  },
  canonical_hash: hash("9"),
};

const candidateCommandResultFixture = {
  schema_version: SCHEMAS.resultCandidateCommand,
  data: {
    candidate_id: candidate.candidate_id,
    candidate_hash: candidate.candidate_hash,
    snapshot_id: candidate.snapshot_id,
    snapshot_hash: candidate.snapshot_hash,
    current: true,
    eligible: true,
    generation: 0,
    idempotent_replay: false,
  },
  canonical_hash: hash("a"),
};

function resultOverrides(
  actionResponse: unknown = actionResultFixture,
): Record<string, unknown> {
  return {
    "/api/v1/context": resultApprovalContextFixture,
    "/api/v1/result-reconciliation/snapshots": snapshotListFixture,
    [`/api/v1/result-reconciliation/snapshots/${snapshotSummary.snapshot_id}`]:
      snapshotFixture,
    [`/api/v1/result-reconciliation/snapshots/${snapshotSummary.snapshot_id}/rows`]:
      rowListFixture,
    [`/api/v1/result-reconciliation/snapshots/${snapshotSummary.snapshot_id}/rows/1`]:
      rowDetailFixture,
    "/api/v1/result-candidates": candidateListFixture,
    [`/api/v1/result-candidates/${candidate.candidate_id}/eligibility`]:
      eligibilityFixture,
    "/api/v1/result-actions": actionHistoryFixture,
    [`POST /api/v1/result-candidates/${candidate.candidate_id}/approve`]:
      actionResponse,
  };
}

function at(path: string): void {
  window.history.replaceState(null, "", path);
}

async function completeApprovalForm(user: ReturnType<typeof userEvent.setup>) {
  await screen.findByLabelText("Actor / practitioner identifier");
  await user.type(
    screen.getByLabelText("Actor / practitioner identifier"),
    "practitioner-approver",
  );
  await user.type(
    screen.getByLabelText("Mandatory reason"),
    "Approve this exact local synthetic candidate.",
  );
  await user.click(screen.getByRole("checkbox", { name: "REVIEW_SYNTHETIC" }));
  await user.click(
    screen.getByRole("checkbox", {
      name: /local Migration Stream readiness evidence only/,
    }),
  );
  await user.click(screen.getByRole("button", { name: "Review approval" }));
}

describe("WP-006 result reconciliation and approval workbench", () => {
  beforeEach(() => {
    at("/");
    window.localStorage.clear();
    window.sessionStorage.clear();
  });

  it("keeps v1/v2 modes separate and grants result actions only to exact v3", async () => {
    expect(canApproveMigrationResult(decodeContext(contextFixture))).toBe(
      false,
    );
    const governed = decodeContext(governedContextFixture);
    expect(canGovernIssueDecisions(governed)).toBe(true);
    expect(canApproveMigrationResult(governed)).toBe(false);
    const resultContext = decodeContext(resultApprovalContextFixture);
    expect(canGovernIssueDecisions(resultContext)).toBe(true);
    expect(canApproveMigrationResult(resultContext)).toBe(true);
    expect(canPublishReadiness(resultContext)).toBe(false);
    expect(resultApprovalContextFixture.capabilities.publication).toBe(false);
    const publicationContext = decodeContext(publicationContextFixture);
    expect(canGovernIssueDecisions(publicationContext)).toBe(true);
    expect(canApproveMigrationResult(publicationContext)).toBe(true);
    expect(canPublishReadiness(publicationContext)).toBe(true);
    if (!canPublishReadiness(publicationContext)) {
      throw new Error("v4 publication capability was not decoded");
    }
    expect(publicationContext.capabilities.target_write).toBe(false);

    at("/result-approval");
    const { calls } = mockApi({
      "/api/v1/context": governedContextFixture,
    });
    render(<App />);
    expect(
      await screen.findByRole("heading", {
        name: "Migration-result approval is disabled",
      }),
    ).toBeVisible();
    expect(
      screen.queryByRole("link", { name: "Local Result Approval" }),
    ).not.toBeInTheDocument();
    expect(
      calls.some((call) => call.pathname.includes("result-candidates")),
    ).toBe(false);
  });

  it("renders exact closure, value-free exact lineage, staleness, and HTML-like evidence safely", async () => {
    const user = userEvent.setup();
    at("/result-approval");
    mockApi(resultOverrides());
    const { container } = render(<App />);
    expect(
      await screen.findByRole("heading", {
        name: "Result Reconciliation & Approval",
      }),
    ).toBeVisible();
    expect(
      screen.getByRole("link", { name: "Local Result Approval" }),
    ).toHaveAttribute("aria-current", "page");
    const sectionIndex = screen.getByRole("navigation", {
      name: "Local Result Approval sections",
    });
    const snapshotLink = within(sectionIndex).getByRole("link", {
      name: "Reconciliation snapshots",
    });
    expect(snapshotLink).toHaveAttribute("href", "#snapshot-history-title");
    expect(
      within(sectionIndex).getByRole("link", { name: "Result candidates" }),
    ).toHaveAttribute("href", "#candidate-history-title");
    expect(
      within(sectionIndex).getByRole("link", { name: "Result-action history" }),
    ).toHaveAttribute("href", "#action-history-title");
    expect(document.querySelector("#snapshot-history-title")).toBeVisible();
    expect(document.querySelector("#candidate-history-title")).toBeVisible();
    expect(document.querySelector("#action-history-title")).toBeVisible();
    snapshotLink.focus();
    expect(snapshotLink).toHaveFocus();
    await user.keyboard("{Enter}");
    expect(window.location.pathname).toBe("/result-approval");
    expect(window.location.hash).toBe("#snapshot-history-title");
    const categories = await screen.findByLabelText("Closed row categories");
    expect(categories.querySelectorAll(".category-count")).toHaveLength(8);
    for (const category of RECONCILIATION_CATEGORIES) {
      expect(within(categories).getByText(category)).toBeVisible();
    }
    expect(await screen.findByText("STALE")).toBeVisible();
    expect(
      await screen.findByText('<img src=x onerror="alert(1)">'),
    ).toBeVisible();
    expect(
      await screen.findByText('<script>alert("history")</script>'),
    ).toBeVisible();
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector("script")).toBeNull();
    expect(screen.getByText(/never renders source or effective/)).toBeVisible();
    await user.click(
      await screen.findByRole("link", { name: "Inspect exact lineage" }),
    );
    expect(
      await screen.findByRole("heading", { name: "Exact row lineage" }),
    ).toBeVisible();
    expect(await screen.findByText("VALIDATION_RUN")).toBeVisible();
    expect(await screen.findByText("validation-synthetic-006")).toBeVisible();
    expect(
      screen.getByText(/does not expose received or effective values/),
    ).toBeVisible();
    expect(screen.queryByText("PRIVATE-SOURCE-VALUE")).not.toBeInTheDocument();
    expect(await axe.run(document.body)).toMatchObject({ violations: [] });
  });

  it("keeps a blocker-bearing closed candidate available only for review and rejection", async () => {
    at("/result-approval");
    const blockerBearingSnapshot = {
      ...snapshotSummary,
      counts: categoryCounts.map((item) =>
        item.category === "INCLUDED_VALID"
          ? { ...item, row_count: 2 }
          : item.category === "BLOCKED_UNRESOLVED"
            ? { ...item, row_count: 2 }
            : item,
      ),
      blocker_codes: ["BLOCKED_UNRESOLVED"],
    };
    mockApi({
      ...resultOverrides(),
      "/api/v1/result-reconciliation/snapshots": {
        ...snapshotListFixture,
        items: [blockerBearingSnapshot],
      },
      [`/api/v1/result-reconciliation/snapshots/${snapshotSummary.snapshot_id}`]:
        {
          ...snapshotFixture,
          data: blockerBearingSnapshot,
        },
    });
    render(<App />);

    expect(
      await screen.findByRole("heading", {
        name: "Prepare immutable result candidate",
      }),
    ).toBeVisible();
    expect(screen.getByLabelText("Candidate creator")).toBeVisible();
    expect(
      screen.getByRole("note", { name: "Candidate eligibility warning" }),
    ).toHaveTextContent(
      "The immutable candidate may be reviewed and rejected, but it cannot be approved.",
    );
  });

  it("requires explicit confirmation and submits only the bounded local action", async () => {
    const user = userEvent.setup();
    at("/result-approval");
    const { requests } = mockApi(resultOverrides());
    render(<App />);
    await completeApprovalForm(user);
    expect(
      screen.getByRole("heading", { name: "Confirm local result approval" }),
    ).toBeVisible();
    expect(
      requests.filter((request) => request.method === "POST"),
    ).toHaveLength(0);
    expect(
      screen.getAllByText(candidate.candidate_hash).length,
    ).toBeGreaterThan(0);
    expect(
      screen.getAllByText(snapshotSummary.snapshot_hash).length,
    ).toBeGreaterThan(0);
    await user.click(
      screen.getByRole("button", { name: "Commit immutable approval" }),
    );
    expect(
      await screen.findByText("Durable local evidence recorded"),
    ).toBeVisible();
    const posts = requests.filter((request) => request.method === "POST");
    expect(posts).toHaveLength(1);
    expect(posts[0].url.pathname).toBe(
      `/api/v1/result-candidates/${candidate.candidate_id}/approve`,
    );
    expect(posts[0].headers.get("x-migration-stream-intent")).toBe(
      "migration-result-approval-v1",
    );
    expect(posts[0].headers.get("idempotency-key")).toMatch(
      /^migration-result-/,
    );
    const body = JSON.parse(posts[0].body ?? "null") as Record<string, unknown>;
    expect(body).toMatchObject({
      schema_version: RESULT_COMMAND_SCHEMAS.approve,
      candidate_id: candidate.candidate_id,
      candidate_hash: candidate.candidate_hash,
      actor: "practitioner-approver",
      reason: "Approve this exact local synthetic candidate.",
      warning_acknowledgments: ["REVIEW_SYNTHETIC"],
      acknowledge_local_only_boundary: true,
      expected_active_action_id: "action-synthetic-previous",
      expected_generation: 1,
    });
    expect(body).not.toHaveProperty("publication");
    expect(body).not.toHaveProperty("source_values");
    expect(window.localStorage).toHaveLength(0);
    expect(window.sessionStorage).toHaveLength(0);
  });

  it("preserves exact command bytes and identity for a network-uncertain retry", async () => {
    const user = userEvent.setup();
    at("/result-approval");
    let attempts = 0;
    const { requests } = mockApi(
      resultOverrides(() => {
        attempts += 1;
        if (attempts === 1) {
          throw new TypeError("synthetic uncertain transport result");
        }
        return actionResultFixture;
      }),
    );
    render(<App />);
    await completeApprovalForm(user);
    await user.click(
      screen.getByRole("button", { name: "Commit immutable approval" }),
    );
    expect(await screen.findByText("NETWORK_UNCERTAIN")).toBeVisible();
    expect(
      screen.getByText(
        /Retry the exact command with the same migration-result identity/,
      ),
    ).toBeVisible();
    await user.click(
      screen.getByRole("button", { name: "Retry exact command identity" }),
    );
    expect(
      await screen.findByText("Durable local evidence recorded"),
    ).toBeVisible();
    const posts = requests.filter((request) => request.method === "POST");
    expect(posts).toHaveLength(2);
    expect(posts[1].headers.get("idempotency-key")).toBe(
      posts[0].headers.get("idempotency-key"),
    );
    expect(posts[1].body).toBe(posts[0].body);
  });

  it("validates all three result command envelopes and rejects route or payload expansion", async () => {
    expect(
      decodeContract(
        reconciliationCommandResultFixture,
        SCHEMAS.resultReconciliationCommand,
      ),
    ).toBe(reconciliationCommandResultFixture);
    expect(
      decodeContract(
        candidateCommandResultFixture,
        SCHEMAS.resultCandidateCommand,
      ),
    ).toBe(candidateCommandResultFixture);
    expect(
      decodeContract(actionResultFixture, SCHEMAS.resultActionCommand),
    ).toBe(actionResultFixture);

    const command: ApproveResultCandidateCommand = {
      schema_version: RESULT_COMMAND_SCHEMAS.approve,
      candidate_id: candidate.candidate_id,
      candidate_hash: candidate.candidate_hash,
      actor: "practitioner-validation",
      reason: "Validate the exact bounded command.",
      warning_acknowledgments: ["REVIEW_SYNTHETIC"],
      acknowledge_local_only_boundary: true,
      expected_active_action_id: "action-synthetic-previous",
      expected_generation: 1,
    };
    expect(validateResultCommand(command)).toBe(command);
    expect(() =>
      validateResultCommand({ ...command, publication: false }),
    ).toThrowError(/incomplete|closed action vocabulary/);
    expect(() =>
      validateResultCommand({
        ...command,
        acknowledge_local_only_boundary: false,
      }),
    ).toThrowError(/incomplete|closed action vocabulary/);

    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    await expect(
      submitResultCommand(
        `/api/v1/result-candidates/${candidate.candidate_id}/reject`,
        command,
        "migration-result-route-mismatch",
      ),
    ).rejects.toMatchObject({ code: "INVALID_RESULT_ROUTE" });
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
