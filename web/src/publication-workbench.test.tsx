import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import axe from "axe-core";
import { describe, expect, it, vi, beforeEach } from "vitest";
import App from "./App";
import { PublicationView } from "./publication-workbench";
import {
  COMMAND_SCHEMAS,
  ContractError,
  decodeContract,
  SCHEMAS,
  type PublicationEligibility,
  type PublicationListPage,
  type PublicationRecord,
} from "./contracts";
import {
  governedContextFixture,
  publicationContextFixture,
} from "./test/fixtures";
import { mockApi } from "./test/mock-api";

const hash = (char: string) => char.repeat(64);

const eligibleFixture: PublicationEligibility = {
  schema_version: SCHEMAS.publicationEligibility,
  is_eligible: true,
  job_id: "job-synthetic-001",
  candidate_id: "cand-synthetic-001",
  candidate_hash: hash("c"),
  approval_id: "appr-synthetic-001",
  approval_hash: hash("a"),
  total_accepted_rows: 4,
  included_rows: 3,
  excluded_rows: 1,
  blocked_rows: 0,
  blockers: [],
  warnings: ["NON_BLOCKING_VALIDATION_WARNING"],
  required_acknowledgments: [
    "I confirm that this readiness package is local readiness evidence only.",
    "I acknowledge that this package does not execute or authorize a target load.",
  ],
  evaluated_at: "2026-07-24T08:00:00Z",
};

const ineligibleFixture: PublicationEligibility = {
  schema_version: SCHEMAS.publicationEligibility,
  is_eligible: false,
  job_id: "job-synthetic-001",
  candidate_id: null,
  candidate_hash: null,
  approval_id: null,
  approval_hash: null,
  total_accepted_rows: 0,
  included_rows: 0,
  excluded_rows: 0,
  blocked_rows: 2,
  blockers: ["NO_ACTIVE_APPROVED_CANDIDATE", "UNRESOLVED_BLOCKING_ISSUES"],
  warnings: [],
  required_acknowledgments: [],
  evaluated_at: "2026-07-24T08:00:00Z",
};

const emptyHistoryFixture: PublicationListPage = {
  schema_version: SCHEMAS.publicationList,
  job_id: "job-synthetic-001",
  items: [],
  total_count: 0,
};

const historyItemFixture = {
  publication_id: "pub-synthetic-001",
  job_id: "job-synthetic-001",
  candidate_id: "cand-synthetic-001",
  approval_id: "appr-synthetic-001",
  status: "COMPLETE",
  semantic_publication_hash: hash("e"),
  physical_archive_digest: hash("f"),
  package_size_bytes: 2048,
  archive_relative_path:
    "publications/completed/pub-synthetic-001/readiness.zip",
  practitioner: "synthetic-practitioner-01",
  publication_reason: "Local readiness package review",
  created_at: "2026-07-23T08:00:00Z",
  completed_at: "2026-07-23T08:00:00Z",
};

const historyPageFixture: PublicationListPage = {
  schema_version: SCHEMAS.publicationList,
  job_id: "job-synthetic-001",
  items: [historyItemFixture],
  total_count: 1,
};

const publicationRecordFixture: PublicationRecord = {
  schema_version: SCHEMAS.publicationDetail,
  publication_id: "pub-synthetic-001",
  job_id: "job-synthetic-001",
  candidate_id: "cand-synthetic-001",
  candidate_hash: hash("c"),
  approval_id: "appr-synthetic-001",
  approval_hash: hash("a"),
  contract_version: "migration-stream/readiness-package/v1",
  status: "COMPLETE",
  practitioner: "synthetic-practitioner-01",
  publication_reason: "Local readiness package review",
  semantic_publication_hash: hash("e"),
  physical_archive_digest: hash("f"),
  package_size_bytes: 2048,
  member_count: 2,
  included_row_count: 3,
  excluded_row_count: 1,
  archive_relative_path:
    "publications/completed/pub-synthetic-001/readiness.zip",
  manifest: {
    publication_id: "pub-synthetic-001",
    publication_version: "1.0.0",
    contract_version: "migration-stream/readiness-package/v1",
    application_version: "0.1.0",
    engine_version: "0.1.0",
    created_at: "2026-07-24T08:00:00Z",
    practitioner: "synthetic-practitioner-01",
    publication_reason: "Local readiness package review",
    disclaimer: "LOCAL READINESS PUBLICATION PACKAGE ONLY.",
    source_package_digest: hash("1"),
    profile_identity: "synthetic.customer.readiness",
    profile_version: "1.0.0",
    profile_digest: hash("2"),
    mapping_identity: "mapping-synthetic-001",
    mapping_version: "1.0.0",
    mapping_digest: hash("3"),
    validation_run_id: "validation-synthetic-001",
    validation_digest: hash("4"),
    reconciliation_snapshot_id: "snapshot-synthetic-001",
    reconciliation_snapshot_hash: hash("5"),
    candidate_id: "cand-synthetic-001",
    candidate_hash: hash("c"),
    approval_id: "appr-synthetic-001",
    approval_hash: hash("a"),
    approval_actor: "local-approver-01",
    approval_reason: "Reviewed local readiness closure and residual warning.",
    approval_occurred_at: "2026-07-24T07:55:00Z",
    governing_decision_count: 2,
    included_valid_count: 2,
    included_corrected_count: 0,
    included_waived_count: 0,
    included_accepted_as_is_count: 1,
    excluded_count: 1,
    blocked_unresolved_count: 0,
    blocked_incomplete_count: 0,
    blocked_conflict_count: 0,
    total_accepted_rows: 4,
    semantic_publication_hash: hash("e"),
    member_inventory: [
      {
        filename: "effective-rows.json",
        media_type: "application/json",
        row_count: 3,
        byte_size: 128,
        sha256_digest: hash("6"),
      },
      {
        filename: "approval-evidence.json",
        media_type: "application/json",
        byte_size: 256,
        sha256_digest: hash("7"),
      },
    ],
  },
  created_at: "2026-07-24T08:00:00Z",
  completed_at: "2026-07-24T08:00:00Z",
};

describe("PublicationWorkbench Page and Components", () => {
  const onRefreshList = vi.fn();

  beforeEach(() => {
    vi.clearAllMocks();
    window.history.replaceState(null, "", "/");
  });

  it("hides publication navigation and reads when capability is unavailable", async () => {
    window.history.replaceState(null, "", "/publication");
    const { calls } = mockApi({
      "/api/v1/context": governedContextFixture,
    });
    render(<App />);

    expect(
      await screen.findByRole("heading", {
        name: "Package is locked",
      }),
    ).toBeVisible();
    expect(
      screen.queryByRole("link", { name: "Readiness Packages" }),
    ).not.toBeInTheDocument();
    expect(
      calls.some((call) => call.pathname.startsWith("/api/v1/publications")),
    ).toBe(false);
  });

  it("capability-gates package evidence and performs no publication read", async () => {
    window.history.replaceState(null, "", "/publication/pub-synthetic-001");
    const { calls } = mockApi({
      "/api/v1/context": governedContextFixture,
    });
    render(<App />);

    expect(
      await screen.findByRole("heading", {
        name: "Package is locked",
      }),
    ).toBeVisible();
    expect(
      calls.some((call) => call.pathname.startsWith("/api/v1/publications")),
    ).toBe(false);
  });

  it("routes package evidence to its exact API path and renders recorded evidence", async () => {
    window.history.replaceState(null, "", "/publication/pub-synthetic-001");
    const { requests } = mockApi({
      "/api/v1/context": publicationContextFixture,
      "/api/v1/publications/pub-synthetic-001": publicationRecordFixture,
    });
    render(<App />);

    expect(
      await screen.findByRole("heading", {
        name: "Readiness Package Evidence",
      }),
    ).toBeVisible();
    expect(
      await screen.findByText("LOCAL READINESS PUBLICATION PACKAGE ONLY."),
    ).toBeVisible();
    expect(
      requests.some(
        (request) =>
          request.method === "GET" &&
          request.url.pathname === "/api/v1/publications/pub-synthetic-001",
      ),
    ).toBe(true);
    expect(
      within(
        screen.getByRole("navigation", { name: "Migration journey" }),
      ).getByRole("link", { name: "Package" }),
    ).toHaveAttribute("aria-current", "page");
    expect(
      screen.getByText(/local Migration Stream readiness evidence only/i),
    ).toBeVisible();
    expect(
      document.querySelector(".readiness-outcome-statement"),
    ).toHaveTextContent(
      /All 4 accepted source rows are accounted for: 3 included.*1 excluded.*0 blocked/i,
    );
    expect(screen.getByText("local-approver-01")).toBeVisible();
    expect(screen.getByText("synthetic-practitioner-01")).toBeVisible();
    expect(screen.getByText("Local readiness package review")).toBeVisible();
    expect(screen.getByText("Consequential decisions")).toBeVisible();
    expect(screen.getByText(/Exact row ordinals/)).toBeVisible();

    const provenance = screen.getByRole("list", {
      name: "Recorded package provenance",
    });
    for (const step of [
      "Source package",
      /Profile synthetic.customer.readiness/,
      /Mapping mapping-synthetic-001/,
      /Validation run validation-synthetic-001/,
      /Reconciliation snapshot snapshot-synthetic-001/,
      "Candidate cand-synthetic-001",
      "Local approval appr-synthetic-001",
    ]) {
      expect(within(provenance).getByText(step)).toBeVisible();
    }

    const categories = screen.getByLabelText("Eight recorded row categories");
    for (const [category, count] of [
      ["INCLUDED_VALID", 2],
      ["INCLUDED_CORRECTED", 0],
      ["INCLUDED_WAIVED", 0],
      ["INCLUDED_ACCEPTED_AS_IS", 1],
      ["EXCLUDED", 1],
      ["BLOCKED_UNRESOLVED", 0],
      ["BLOCKED_INCOMPLETE", 0],
      ["BLOCKED_CONFLICT", 0],
    ] as const) {
      const categoryCard = within(categories)
        .getByText(category)
        .closest(".category-count");
      expect(categoryCard).not.toBeNull();
      expect(
        within(categoryCard as HTMLElement).getByText(String(count)),
      ).toBeVisible();
    }

    expect(screen.getByText("Semantic publication hash")).toBeVisible();
    expect(screen.getByText("Physical archive digest")).toBeVisible();
    expect(
      screen.getByText("Total ZIP entries (content + manifest)"),
    ).toBeVisible();
    const inventory = screen.getByRole("region", {
      name: "Recorded manifest member inventory",
    });
    expect(within(inventory).getByText("effective-rows.json")).toBeVisible();
    expect(within(inventory).getByText("approval-evidence.json")).toBeVisible();
    expect(within(inventory).getByText("Not declared")).toBeVisible();

    const download = screen.getByRole("link", {
      name: "Download package ZIP",
    });
    expect(download).toHaveAttribute(
      "href",
      "/api/v1/publications/pub-synthetic-001/download",
    );
    expect(download).toHaveAttribute(
      "download",
      "wp007-readiness-job-synthetic-001-pub-synthetic-001.zip",
    );
    expect(
      screen.getByRole("link", { name: "Return to Readiness Packages" }),
    ).toHaveAttribute("href", "/publication");
    expect(await axe.run(document.body)).toMatchObject({ violations: [] });
  });

  it("fails closed when package detail does not match the strict decoder", async () => {
    window.history.replaceState(null, "", "/publication/pub-synthetic-001");
    mockApi({
      "/api/v1/context": publicationContextFixture,
      "/api/v1/publications/pub-synthetic-001": {
        ...publicationRecordFixture,
        manifest: {
          ...publicationRecordFixture.manifest,
          blocked_conflict_count: "0",
        },
      },
    });
    render(<App />);

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "INVALID_RESPONSE",
    );
    expect(
      screen.queryByRole("region", {
        name: "Recorded manifest member inventory",
      }),
    ).not.toBeInTheDocument();
  });

  it("fails closed when package record and manifest evidence disagree", async () => {
    window.history.replaceState(null, "", "/publication/pub-synthetic-001");
    mockApi({
      "/api/v1/context": publicationContextFixture,
      "/api/v1/publications/pub-synthetic-001": {
        ...publicationRecordFixture,
        manifest: {
          ...publicationRecordFixture.manifest,
          semantic_publication_hash: hash("9"),
        },
      },
    });
    render(<App />);

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "INVALID_RESPONSE",
    );
    expect(
      screen.queryByRole("heading", { name: "Local readiness outcome" }),
    ).not.toBeInTheDocument();
  });

  it("shows capability-gated publication evidence while blocked and never submits", async () => {
    const user = userEvent.setup();
    window.history.replaceState(null, "", "/publication");
    const blockedFixture: PublicationEligibility = {
      ...ineligibleFixture,
      blockers: ["BLOCKED_UNRESOLVED"],
    };
    const { requests } = mockApi({
      "/api/v1/context": publicationContextFixture,
      "/api/v1/publications/eligibility": blockedFixture,
      "/api/v1/publications": emptyHistoryFixture,
    });
    render(<App />);

    expect(
      await screen.findByRole("heading", {
        name: "Readiness Packages",
      }),
    ).toBeVisible();
    expect(screen.getByRole("link", { name: "Package" })).toBeVisible();
    const sectionIndex = screen.getByRole("navigation", {
      name: "Readiness Package sections",
    });
    const eligibilityLink = within(sectionIndex).getByRole("link", {
      name: "Eligibility",
    });
    expect(eligibilityLink).toHaveAttribute("href", "#package-eligibility");
    expect(
      within(sectionIndex).getByRole("link", { name: "Generate package" }),
    ).toHaveAttribute("href", "#package-generation");
    expect(
      within(sectionIndex).getByRole("link", { name: "Package history" }),
    ).toHaveAttribute("href", "#package-history");
    eligibilityLink.focus();
    expect(eligibilityLink).toHaveFocus();
    await user.keyboard("{Enter}");
    expect(window.location.pathname).toBe("/publication");
    expect(window.location.hash).toBe("#package-eligibility");
    for (const targetId of [
      "package-eligibility",
      "package-generation",
      "package-history",
    ]) {
      expect(
        document.querySelector(`#${targetId}`)?.nextElementSibling,
      ).toBeVisible();
    }
    expect(
      screen.getByText(/immutable technical Publication record/),
    ).toBeVisible();
    expect(
      screen.getByText(/does not transmit it, load a target/),
    ).toBeVisible();
    expect(await screen.findByText("INELIGIBLE")).toBeVisible();
    expect(screen.getByText("BLOCKED_UNRESOLVED")).toBeVisible();
    expect(screen.getByText("No local packages generated")).toBeVisible();

    await user.type(
      screen.getByLabelText(/Practitioner identifier \(local attribution\)/),
      "synthetic-evaluation-onboarding",
    );
    await user.type(
      screen.getByLabelText(/Local package-generation reason/),
      "Verify the rejected synthetic candidate remains blocked.",
    );
    await user.click(
      screen.getByLabelText(/Non-Authority Boundary Acknowledgment/),
    );

    expect(
      screen.getByRole("button", {
        name: /Generate Local Readiness Package/,
      }),
    ).toBeDisabled();
    expect(
      requests.some(
        (request) =>
          request.method === "POST" &&
          request.url.pathname === "/api/v1/publications",
      ),
    ).toBe(false);
  });

  it("renders eligible state correctly without blockers", () => {
    render(
      <PublicationView
        eligibility={eligibleFixture}
        listState={{ status: "success", data: emptyHistoryFixture }}
        onRefreshList={onRefreshList}
      />,
    );

    expect(
      screen.getByText("Package Generation Eligibility"),
    ).toBeInTheDocument();
    expect(document.querySelector(".status")?.textContent).toBe("ELIGIBLE");
    expect(screen.getByText("cand-synthetic-001")).toBeInTheDocument();
    expect(screen.getByText("appr-synthetic-001")).toBeInTheDocument();
    expect(
      screen.queryByText(/Package Generation Blockers/),
    ).not.toBeInTheDocument();
  });

  it("renders ineligible state with explicit blockers list", () => {
    render(
      <PublicationView
        eligibility={ineligibleFixture}
        listState={{ status: "success", data: emptyHistoryFixture }}
        onRefreshList={onRefreshList}
      />,
    );

    expect(screen.getByText("INELIGIBLE")).toBeInTheDocument();
    expect(
      screen.getByText(/Package Generation Blockers \(2\)/),
    ).toBeInTheDocument();
    expect(
      screen.getByText("NO_ACTIVE_APPROVED_CANDIDATE"),
    ).toBeInTheDocument();
    expect(screen.getByText("UNRESOLVED_BLOCKING_ISSUES")).toBeInTheDocument();
  });

  it("keeps publication disabled for an ineligible result after form completion", async () => {
    const user = userEvent.setup();
    render(
      <PublicationView
        eligibility={ineligibleFixture}
        listState={{ status: "success", data: emptyHistoryFixture }}
        onRefreshList={onRefreshList}
      />,
    );

    const button = screen.getByRole("button", {
      name: /Generate Local Readiness Package/,
    });
    await user.type(
      screen.getByLabelText(/Practitioner identifier \(local attribution\)/),
      "synthetic-evaluation-onboarding",
    );
    await user.type(
      screen.getByLabelText(/Local package-generation reason/),
      "Verify the rejected synthetic candidate remains blocked.",
    );
    await user.click(
      screen.getByLabelText(/Non-Authority Boundary Acknowledgment/),
    );

    expect(button).toBeDisabled();
  });

  it("disables publication button until practitioner, reason, boundary, and warning checkboxes are checked", async () => {
    const user = userEvent.setup();
    render(
      <PublicationView
        eligibility={eligibleFixture}
        listState={{ status: "success", data: emptyHistoryFixture }}
        onRefreshList={onRefreshList}
      />,
    );

    const button = screen.getByRole("button", {
      name: /Generate Local Readiness Package/,
    });
    expect(button).toBeDisabled();

    // Fill practitioner ID
    const practitionerInput = screen.getByLabelText(
      /Practitioner identifier \(local attribution\)/,
    );
    expect(practitionerInput).toHaveAttribute(
      "placeholder",
      "e.g. synthetic-practitioner-01",
    );
    await user.type(practitionerInput, "synthetic-practitioner-01");
    expect(button).toBeDisabled();

    // Fill reason
    const reasonInput = screen.getByLabelText(
      /Local package-generation reason/,
    );
    expect(reasonInput).toHaveAttribute(
      "placeholder",
      "Provide the local package-generation reason and review context...",
    );
    await user.type(reasonInput, "Local readiness package review");
    expect(button).toBeDisabled();

    // Check both server-required acknowledgments.
    for (const checkbox of screen.getAllByRole("checkbox").slice(0, 2)) {
      await user.click(checkbox);
    }
    expect(button).toBeDisabled();

    // Check boundary acknowledgment
    const boundaryCheckbox = screen.getByLabelText(
      /Non-Authority Boundary Acknowledgment/,
    );
    await user.click(boundaryCheckbox);

    // Now button should be enabled
    expect(button).toBeEnabled();
  });

  it("renders non-authority warning notice prominently", () => {
    render(
      <PublicationView
        eligibility={eligibleFixture}
        listState={{ status: "success", data: emptyHistoryFixture }}
        onRefreshList={onRefreshList}
      />,
    );

    expect(
      screen.getByText(
        /I understand this creates local Migration Stream readiness evidence only/,
      ),
    ).toBeInTheDocument();
    expect(screen.getByText(/does not transmit data/)).toBeInTheDocument();
  });

  it("renders publication history table and completed zip download link", () => {
    render(
      <PublicationView
        eligibility={eligibleFixture}
        listState={{ status: "success", data: historyPageFixture }}
        onRefreshList={onRefreshList}
      />,
    );

    expect(screen.getByText("pub-synthetic-001")).toBeInTheDocument();
    expect(screen.getByText("synthetic-practitioner-01")).toBeInTheDocument();
    expect(screen.getByText("COMPLETE")).toBeInTheDocument();
    expect(screen.getByText("2.0 KB")).toBeInTheDocument();

    const downloadLink = screen.getByRole("link", { name: "Download ZIP" });
    expect(downloadLink).toBeInTheDocument();
    expect(downloadLink).toHaveAttribute(
      "href",
      "/api/v1/publications/pub-synthetic-001/download",
    );
    expect(screen.getByRole("link", { name: "View evidence" })).toHaveAttribute(
      "href",
      "/publication/pub-synthetic-001",
    );
  });

  it("renders empty history state when no publications exist", () => {
    render(
      <PublicationView
        eligibility={eligibleFixture}
        listState={{ status: "success", data: emptyHistoryFixture }}
        onRefreshList={onRefreshList}
      />,
    );

    expect(screen.getByText("No local packages generated")).toBeInTheDocument();
    expect(
      screen.getByText(/Generated local readiness packages/),
    ).toBeInTheDocument();
  });

  it("rejects missing, wrong, unsupported, and malformed detail identities", () => {
    expect(() =>
      decodeContract(
        { ...publicationRecordFixture, schema_version: undefined },
        SCHEMAS.publicationDetail,
      ),
    ).toThrowError(ContractError);
    expect(() =>
      decodeContract(
        {
          ...publicationRecordFixture,
          schema_version: SCHEMAS.publicationList,
        },
        SCHEMAS.publicationDetail,
      ),
    ).toThrowError(/SCHEMA_MISMATCH|Expected/);
    expect(() =>
      decodeContract(
        {
          ...publicationRecordFixture,
          schema_version: "migration-stream/publication-detail/v2",
        },
        SCHEMAS.publicationDetail,
      ),
    ).toThrowError(/SCHEMA_MISMATCH|Expected/);
    expect(() =>
      decodeContract(
        { ...publicationRecordFixture, manifest: null },
        SCHEMAS.publicationDetail,
      ),
    ).toThrowError(/required contract shape/);
  });

  it("submits the existing command contract and renders authoritative returned evidence", async () => {
    const user = userEvent.setup();
    const { requests } = mockApi({
      "POST /api/v1/publications": publicationRecordFixture,
    });
    render(
      <PublicationView
        eligibility={eligibleFixture}
        listState={{ status: "success", data: emptyHistoryFixture }}
        onRefreshList={onRefreshList}
      />,
    );

    await user.type(
      screen.getByLabelText(/Practitioner identifier \(local attribution\)/),
      "synthetic-practitioner-01",
    );
    await user.type(
      screen.getByLabelText(/Local package-generation reason/),
      "Local readiness package review",
    );
    for (const acknowledgment of eligibleFixture.required_acknowledgments) {
      await user.click(screen.getByLabelText(acknowledgment));
    }
    await user.click(
      screen.getByLabelText(/Non-Authority Boundary Acknowledgment/),
    );
    await user.click(
      screen.getByRole("button", {
        name: "Generate Local Readiness Package",
      }),
    );

    expect(
      await screen.findByText("Local readiness package created"),
    ).toBeVisible();
    expect(screen.getByText("pub-synthetic-001")).toBeVisible();
    expect(
      screen.getByRole("link", { name: "Inspect package evidence" }),
    ).toHaveAttribute("href", "/publication/pub-synthetic-001");
    const commandRequest = requests.find(
      (request) =>
        request.method === "POST" &&
        request.url.pathname === "/api/v1/publications",
    );
    expect(commandRequest).toBeDefined();
    const body = JSON.parse(commandRequest?.body ?? "{}") as Record<
      string,
      unknown
    >;
    expect(body).toMatchObject({
      schema_version: COMMAND_SCHEMAS.publish,
      job_id: eligibleFixture.job_id,
      candidate_id: eligibleFixture.candidate_id,
      approval_id: eligibleFixture.approval_id,
      practitioner: "synthetic-practitioner-01",
      publication_reason: "Local readiness package review",
      acknowledgments: eligibleFixture.required_acknowledgments,
    });
    expect(body).not.toHaveProperty("warning_acknowledgments");
    expect(body).not.toHaveProperty(
      "acknowledge_local_only_readiness_boundary",
    );
    expect(onRefreshList).toHaveBeenCalledTimes(1);
  });
});
