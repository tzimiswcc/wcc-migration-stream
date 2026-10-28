import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import {
  ENGAGEMENT_COMMAND_SCHEMAS,
  COMPARED_FINDING_IDENTITY_EXPLANATIONS,
  REVISED_SOURCE_ACKNOWLEDGEMENT,
  SCHEMAS,
  decodeContract,
  type ComparedFinding,
  type TourChain,
} from "./contracts";
import {
  contextFixture,
  engagementContextFixture,
  engagementMappingFixture,
  engagementObjectPacksFixture,
  engagementValidationCompleteDetailFixture,
  engagementValidationCompleteStateFixture,
  engagementValidationFixture,
  journeyFixture,
  readyPreflightFixture,
  sampleFreeProfilingFixture,
  sourceProfileRulesFixture,
} from "./test/fixtures";
import { errorResponse, mockApi, type CapturedRequest } from "./test/mock-api";
import {
  RevisedValidationPanel,
  SALES_DOCUMENT_PACK_ID,
  TourComparisonPage,
} from "./tour2-pages";

const hash = (character: string) => character.repeat(64);

const tour1 = {
  tourId: "tour-1-immutable",
  tourNumber: 1,
  predecessorTourId: null,
  sourceHash: hash("1"),
  manifestHash: hash("2"),
  profilingRunId: "profiling-tour-1",
  profileId: SALES_DOCUMENT_PACK_ID,
  profileVersion: "1.0.0",
  profileHash: hash("3"),
  mappingId: "sales-document.mapping",
  mappingVersion: "1.0.0",
  mappingHash: hash("4"),
  mappingAuthorityId: "mapping-authority-tour-1",
  mappingAuthorityHash: hash("4"),
  validationRunId: "validation-tour-1",
  validationResultHash: hash("5"),
  tourHash: hash("6"),
  completedAt: "2026-07-29T10:00:00Z",
};

const tour2 = {
  ...tour1,
  tourId: "tour-2-immutable",
  tourNumber: 2,
  predecessorTourId: tour1.tourId,
  sourceHash: hash("7"),
  manifestHash: hash("8"),
  profilingRunId: "profiling-tour-2",
  validationRunId: "validation-tour-2",
  validationResultHash: hash("9"),
  tourHash: hash("a"),
  completedAt: "2026-07-29T10:05:00Z",
};

function finding(
  category: ComparedFinding["category"],
  suffix: string,
  issueCode: string,
): ComparedFinding {
  return {
    findingId: `comparison-finding-${suffix}`,
    category,
    fingerprintHash: hash(suffix),
    occurrence: 1,
    severity: "ERROR",
    ruleId: `rule.${suffix}`,
    issueCode,
    displayMessage: `${category} value-suppressed finding ${suffix}.`,
    entityId: "SalesDocumentItem",
    fieldId: "sourceItemId",
    relationshipId: null,
    relationshipRole: null,
    baselineIssueIds: category === "NEW" ? [] : [`baseline-issue-${suffix}`],
    baselineIssueHashes: category === "NEW" ? [] : [hash("b")],
    candidateIssueIds:
      category === "CLOSED" ? [] : [`candidate-issue-${suffix}`],
    candidateIssueHashes: category === "CLOSED" ? [] : [hash("c")],
    identityBasis: category === "NEW" ? "FULL_ROW_FALLBACK" : "DECLARED_KEY",
    identityBasisExplanation:
      COMPARED_FINDING_IDENTITY_EXPLANATIONS[
        category === "NEW" ? "FULL_ROW_FALLBACK" : "DECLARED_KEY"
      ],
  };
}

const findings = [
  finding("CLOSED", "1", "DUPLICATE_HEADER_KEY"),
  finding("CLOSED", "2", "DUPLICATE_ITEM_KEY"),
  finding("CLOSED", "3", "ORPHAN_ITEM"),
  finding("CLOSED", "4", "MISSING_CHILD"),
  finding("REMAINING", "5", "QUANTITY_PATTERN"),
  finding("NEW", "6", "REQUESTED_DATE_PATTERN"),
];

const initialChain: TourChain = {
  schemaVersion: SCHEMAS.tourChain,
  engagementGeneration: 8,
  activeTour: null,
  pendingAttempt: null,
  latestComparison: null,
  tours: [],
  comparisons: [],
};

const startedAttempt: NonNullable<TourChain["pendingAttempt"]> = {
  tourAttemptId: "tour-attempt-2",
  tourNumber: 2,
  baselineTourId: tour1.tourId,
  state: "STARTED",
  generation: 9,
  preflightId: null,
  sourceHash: null,
  manifestHash: null,
  jobId: null,
  profilingRunId: null,
  structureHash: null,
  reuseAuthorityId: null,
  reuseAuthorityHash: null,
  validationRunId: null,
  errorCode: null,
  errorMessage: null,
  actor: "Synthetic practitioner",
  reason: "Compare a revised local package.",
  createdAt: "2026-07-29T10:01:00Z",
  updatedAt: "2026-07-29T10:01:00Z",
};

const startedChain: TourChain = {
  ...initialChain,
  engagementGeneration: 9,
  activeTour: tour1,
  pendingAttempt: startedAttempt,
  tours: [tour1],
};

const readyToValidateAttempt: NonNullable<TourChain["pendingAttempt"]> = {
  ...startedAttempt,
  state: "READY_TO_VALIDATE",
  generation: 11,
  preflightId: "revised-preflight-2",
  sourceHash: hash("7"),
  manifestHash: hash("8"),
  jobId: "job-tour-2",
  profilingRunId: "profiling-tour-2",
  structureHash: hash("f"),
  reuseAuthorityId: "mapping-reuse-tour-2",
  reuseAuthorityHash: hash("0"),
  updatedAt: "2026-07-29T10:03:00Z",
};

const readyToValidateChain: TourChain = {
  ...startedChain,
  engagementGeneration: 11,
  pendingAttempt: readyToValidateAttempt,
};

const completedChain: TourChain = {
  schemaVersion: SCHEMAS.tourChain,
  engagementGeneration: 13,
  activeTour: tour2,
  pendingAttempt: null,
  latestComparison: {
    schemaVersion: SCHEMAS.comparedValidation,
    comparisonId: "comparison-tour-1-tour-2",
    baselineTourId: tour1.tourId,
    candidateTourId: tour2.tourId,
    baselineValidationRunId: tour1.validationRunId,
    candidateValidationRunId: tour2.validationRunId,
    baselineResultHash: tour1.validationResultHash,
    candidateResultHash: tour2.validationResultHash,
    algorithmVersion: "migration-stream/compared-validation-fingerprint/v1",
    closedCount: 4,
    remainingCount: 1,
    newCount: 1,
    inputHash: hash("d"),
    comparisonHash: hash("e"),
    actor: "Synthetic practitioner",
    reason: "Review the exact revised synthetic source.",
    createdAt: "2026-07-29T10:05:00Z",
    findings,
  },
  tours: [tour1, tour2],
  comparisons: [],
};

completedChain.comparisons = [completedChain.latestComparison!];

const tour3 = {
  ...tour2,
  tourId: "tour-3-immutable",
  tourNumber: 3,
  predecessorTourId: tour2.tourId,
  sourceHash: hash("f"),
  manifestHash: hash("0"),
  profilingRunId: "profiling-tour-3",
  validationRunId: "validation-tour-3",
  validationResultHash: hash("1"),
  tourHash: hash("2"),
  completedAt: "2026-07-29T10:10:00Z",
};

const comparison23: NonNullable<TourChain["latestComparison"]> = {
  schemaVersion: SCHEMAS.comparedValidation,
  comparisonId: "comparison-tour-2-tour-3",
  baselineTourId: tour2.tourId,
  candidateTourId: tour3.tourId,
  baselineValidationRunId: tour2.validationRunId,
  candidateValidationRunId: tour3.validationRunId,
  baselineResultHash: tour2.validationResultHash,
  candidateResultHash: tour3.validationResultHash,
  algorithmVersion: "migration-stream/compared-validation-fingerprint/v1",
  closedCount: 1,
  remainingCount: 1,
  newCount: 1,
  inputHash: hash("3"),
  comparisonHash: hash("4"),
  actor: "Synthetic practitioner",
  reason: "Review the third exact source revision.",
  createdAt: "2026-07-29T10:10:00Z",
  findings: [
    finding("CLOSED", "7", "CLOSED_THIRD_REVISION"),
    finding("REMAINING", "8", "REMAINING_THIRD_REVISION"),
    finding("NEW", "9", "NEW_THIRD_REVISION"),
  ],
};

const thirdRevisionChain: TourChain = {
  schemaVersion: SCHEMAS.tourChain,
  engagementGeneration: 17,
  activeTour: tour3,
  pendingAttempt: null,
  latestComparison: comparison23,
  tours: [tour1, tour2, tour3],
  comparisons: [completedChain.latestComparison!, comparison23],
};

function requestBody(
  request: CapturedRequest | undefined,
): Record<string, unknown> {
  expect(request?.body).toEqual(expect.any(String));
  return JSON.parse(request?.body ?? "{}") as Record<string, unknown>;
}

describe("Tour 2 compared validation", () => {
  beforeEach(() => {
    window.history.replaceState(null, "", "/setup");
  });

  it("starts one revised tour with exact generation, attribution, and bounded authority", async () => {
    const user = userEvent.setup();
    let reads = 0;
    const { requests } = mockApi({
      "GET /api/v1/engagement/tours": () => {
        reads += 1;
        return reads === 1 ? initialChain : startedChain;
      },
      "POST /api/v1/engagement/tours/start": startedChain,
    });
    render(<RevisedValidationPanel />);

    await user.type(
      await screen.findByLabelText("Operator attribution"),
      "Synthetic practitioner",
    );
    await user.type(
      screen.getByLabelText("Reason for revised validation"),
      "Compare a revised local package.",
    );
    await user.click(screen.getByRole("checkbox"));
    await user.click(
      screen.getByRole("button", { name: "Start source refresh" }),
    );

    const request = await waitFor(() => {
      const match = requests.find(
        (candidate) =>
          candidate.method === "POST" &&
          candidate.url.pathname === "/api/v1/engagement/tours/start",
      );
      expect(match).toBeDefined();
      if (!match) throw new Error("start request was not captured");
      return match;
    });
    const body = requestBody(request);
    expect(body).toEqual({
      schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.startRevisedTour,
      idempotencyKey: expect.any(String),
      expectedEngagementGeneration: 8,
      actor: "Synthetic practitioner",
      reason: "Compare a revised local package.",
      acknowledgement: REVISED_SOURCE_ACKNOWLEDGEMENT,
    });
    expect(request.headers.get("idempotency-key")).toBe(body.idempotencyKey);
    expect(request.headers.get("x-migration-stream-intent")).toBe(
      "engagement-revised-tour-start-v1",
    );
    expect(
      await screen.findByRole("heading", {
        name: "Choose and accept the revised ZIP",
      }),
    ).toBeVisible();
  });

  it("reserves and uploads the revised ZIP with the exact same retry identity", async () => {
    const user = userEvent.setup();
    const receiving = {
      ...readyPreflightFixture,
      preflightId: "revised-preflight-2",
      state: "RECEIVING",
      sourceName: "sales-document-tour-2.zip",
      sourceHash: null,
      sourceSize: null,
      manifestHash: null,
      manifest: null,
      errorCode: null,
      errorMessage: null,
      completedAt: null,
    };
    const ready = {
      ...readyPreflightFixture,
      preflightId: receiving.preflightId,
      sourceName: receiving.sourceName,
    };
    const readyChain: TourChain = {
      ...startedChain,
      engagementGeneration: 10,
      pendingAttempt: {
        ...startedAttempt,
        state: "PREFLIGHT_READY",
        generation: 10,
        preflightId: ready.preflightId,
        sourceHash: ready.sourceHash,
        manifestHash: ready.manifestHash,
        updatedAt: "2026-07-29T10:02:00Z",
      },
    };
    let reads = 0;
    const { requests } = mockApi({
      "GET /api/v1/engagement/tours": () => {
        reads += 1;
        return reads === 1 ? startedChain : readyChain;
      },
      "POST /api/v1/engagement/tours/revised-source-preflights": {
        preflight: receiving,
      },
      [`PUT /api/v1/engagement/source-preflights/${receiving.preflightId}/content`]:
        ready,
    });
    render(<RevisedValidationPanel />);

    const file = new File(
      ["PK\u0003\u0004synthetic-revised-tour"],
      receiving.sourceName,
      { type: "application/zip" },
    );
    await user.upload(await screen.findByLabelText("Revised source ZIP"), file);
    await user.click(
      screen.getByRole("button", {
        name: "Run revised-package safety check",
      }),
    );

    expect(await screen.findByLabelText("Accepting operator")).toBeVisible();
    const reserve = requests.find(
      (request) =>
        request.method === "POST" &&
        request.url.pathname ===
          "/api/v1/engagement/tours/revised-source-preflights",
    );
    const upload = requests.find(
      (request) =>
        request.method === "PUT" &&
        request.url.pathname.endsWith(
          `/source-preflights/${receiving.preflightId}/content`,
        ),
    );
    expect(reserve).toBeDefined();
    expect(upload).toBeDefined();
    expect(upload?.headers.get("idempotency-key")).toBe(
      reserve?.headers.get("idempotency-key"),
    );
    expect(upload?.headers.get("x-migration-stream-intent")).toBe(
      "engagement-source-preflight-content-v1",
    );
  });

  it("accepts the exact revised manifest and rehydrates verified structural reuse", async () => {
    const user = userEvent.setup();
    const preflightReadyChain: TourChain = {
      ...startedChain,
      engagementGeneration: 10,
      pendingAttempt: {
        ...startedAttempt,
        state: "PREFLIGHT_READY",
        generation: 10,
        preflightId: readyToValidateAttempt.preflightId,
        sourceHash: readyToValidateAttempt.sourceHash,
        manifestHash: readyToValidateAttempt.manifestHash,
        updatedAt: "2026-07-29T10:02:00Z",
      },
    };
    const reuseAuthority = {
      reuseAuthorityId: readyToValidateAttempt.reuseAuthorityId!,
      reuseAuthorityHash: readyToValidateAttempt.reuseAuthorityHash!,
      baselineTourId: tour1.tourId,
      candidateTourAttemptId: startedAttempt.tourAttemptId,
      baselineSourceHash: tour1.sourceHash,
      mappingOriginSourceHash: tour1.sourceHash,
      candidateSourceHash: readyToValidateAttempt.sourceHash!,
      profileId: tour1.profileId,
      profileVersion: tour1.profileVersion,
      profileHash: tour1.profileHash,
      profileApprovalId: "profile-approval-tour-1",
      profileApprovalHash: hash("b"),
      mappingId: tour1.mappingId,
      mappingVersion: tour1.mappingVersion,
      mappingHash: tour1.mappingHash,
      mappingAuthorityId: tour1.mappingAuthorityId,
      mappingAuthorityHash: tour1.mappingAuthorityHash,
      mappingApprovalId: "mapping-approval-tour-1",
      mappingApprovalHash: hash("c"),
      structureHash: readyToValidateAttempt.structureHash!,
      createdAt: "2026-07-29T10:03:00Z",
    };
    let reads = 0;
    const { requests } = mockApi({
      "GET /api/v1/engagement/tours": () => {
        reads += 1;
        return reads === 1 ? preflightReadyChain : readyToValidateChain;
      },
      [`POST /api/v1/engagement/tours/revised-source-preflights/${readyToValidateAttempt.preflightId}/accept`]:
        {
          schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.acceptRevisedSource,
          engagementGeneration: 11,
          tourAttempt: readyToValidateAttempt,
          reuseAuthority,
          profilingResultHash: hash("d"),
          reusedProfile: true,
          idempotentReplay: false,
        },
    });
    render(<RevisedValidationPanel />);

    const actor = await screen.findByLabelText("Accepting operator");
    await user.clear(actor);
    await user.type(actor, "Synthetic practitioner");
    await user.clear(screen.getByLabelText("Acceptance reason"));
    await user.type(
      screen.getByLabelText("Acceptance reason"),
      "Accept the exact revised synthetic manifest.",
    );
    await user.click(screen.getByRole("checkbox"));
    await user.click(
      screen.getByRole("button", { name: "Accept revised source locally" }),
    );

    const request = await waitFor(() => {
      const match = requests.find(
        (candidate) =>
          candidate.method === "POST" &&
          candidate.url.pathname.endsWith(
            `/${readyToValidateAttempt.preflightId}/accept`,
          ),
      );
      if (!match) throw new Error("accept request was not captured");
      return match;
    });
    expect(requestBody(request)).toEqual({
      schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.acceptRevisedSource,
      idempotencyKey: expect.any(String),
      expectedEngagementGeneration: 10,
      tourAttemptId: startedAttempt.tourAttemptId,
      preflightId: readyToValidateAttempt.preflightId,
      expectedSourceHash: readyToValidateAttempt.sourceHash,
      expectedManifestHash: readyToValidateAttempt.manifestHash,
      actor: "Synthetic practitioner",
      reason: "Accept the exact revised synthetic manifest.",
      acknowledgement: REVISED_SOURCE_ACKNOWLEDGEMENT,
    });
    expect(request.headers.get("x-migration-stream-intent")).toBe(
      "engagement-revised-source-accept-v1",
    );
    expect(
      await screen.findByText("Profile and mapping authority may be reused"),
    ).toBeVisible();
  });

  it("launches only the exact replayed reuse authority with explicit attribution", async () => {
    const user = userEvent.setup();
    let reads = 0;
    const { requests } = mockApi({
      "GET /api/v1/engagement/tours": () => {
        reads += 1;
        return reads === 1 ? readyToValidateChain : completedChain;
      },
      "POST /api/v1/engagement/tours/validation/launch": {
        schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.launchRevisedValidation,
        engagementGeneration: completedChain.engagementGeneration,
        activeTour: tour2,
        comparison: completedChain.latestComparison,
        idempotentReplay: false,
      },
    });
    render(<RevisedValidationPanel />);

    expect(
      await screen.findByText("Profile and mapping authority may be reused"),
    ).toBeVisible();
    expect(
      screen.getByText(new RegExp(tour1.profileId.replaceAll(".", "\\."))),
    ).toBeVisible();
    expect(
      screen.getByText(readyToValidateAttempt.reuseAuthorityId!),
    ).toBeVisible();
    await user.clear(screen.getByLabelText("Validation operator"));
    await user.type(
      screen.getByLabelText("Validation operator"),
      "Synthetic practitioner",
    );
    await user.clear(screen.getByLabelText("Validation and comparison reason"));
    await user.type(
      screen.getByLabelText("Validation and comparison reason"),
      "Compare the exact accepted revision.",
    );
    await user.click(screen.getByRole("checkbox"));
    await user.click(
      screen.getByRole("button", { name: "Launch Revision 2 validation" }),
    );

    const request = await waitFor(() => {
      const match = requests.find(
        (candidate) =>
          candidate.method === "POST" &&
          candidate.url.pathname ===
            "/api/v1/engagement/tours/validation/launch",
      );
      expect(match).toBeDefined();
      if (!match) throw new Error("launch request was not captured");
      return match;
    });
    expect(requestBody(request)).toEqual({
      schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.launchRevisedValidation,
      idempotencyKey: expect.any(String),
      expectedEngagementGeneration: 11,
      tourAttemptId: startedAttempt.tourAttemptId,
      expectedReuseAuthorityId: readyToValidateAttempt.reuseAuthorityId,
      expectedReuseAuthorityHash: readyToValidateAttempt.reuseAuthorityHash,
      actor: "Synthetic practitioner",
      reason: "Compare the exact accepted revision.",
      acknowledgement: REVISED_SOURCE_ACKNOWLEDGEMENT,
    });
    expect(request.headers.get("x-migration-stream-intent")).toBe(
      "engagement-revised-validation-launch-v1",
    );
    expect(
      await screen.findByRole("link", { name: "Open compared validation" }),
    ).toBeVisible();
  });

  it("rehydrates the immutable comparison, filters categories, and exposes no received values", async () => {
    const user = userEvent.setup();
    mockApi({
      "GET /api/v1/engagement/tours": completedChain,
      "GET /api/v1/engagement/readiness-packages": {
        schema_version: SCHEMAS.engagementReadinessPackages,
        engagement_id: "engagement-tour-001",
        total_count: 0,
        returned_count: 0,
        truncated: false,
        items: [],
      },
    });
    render(<TourComparisonPage />);

    expect(
      await screen.findByRole("heading", {
        name: "Revision 1 versus Revision 2",
      }),
    ).toBeVisible();
    const counts = screen.getByRole("button", { name: /4 CLOSED/ });
    expect(counts).toHaveAttribute("aria-pressed", "false");
    expect(screen.getByRole("button", { name: /1 REMAINING/ })).toBeVisible();
    expect(screen.getByRole("button", { name: /1 NEW/ })).toBeVisible();
    expect(
      screen.getAllByText(completedChain.latestComparison!.comparisonId),
    ).toHaveLength(2);
    expect(screen.queryByText("received value")).not.toBeInTheDocument();
    expect(screen.queryByText("SD-1001")).not.toBeInTheDocument();

    await user.selectOptions(screen.getByLabelText("Finding category"), "NEW");
    const list = screen.getByLabelText("Compared findings");
    expect(within(list).getByText("REQUESTED_DATE_PATTERN")).toBeVisible();
    expect(
      within(list).queryByText("DUPLICATE_HEADER_KEY"),
    ).not.toBeInTheDocument();
    expect(
      screen.getByText(/active revision begins fresh downstream governance/),
    ).toBeVisible();
  });

  it("rejects count drift and extra broad evidence in the closed comparison contract", () => {
    expect(decodeContract(completedChain, SCHEMAS.tourChain)).toEqual(
      completedChain,
    );
    expect(() =>
      decodeContract(
        {
          ...completedChain,
          latestComparison: {
            ...completedChain.latestComparison,
            newCount: 2,
          },
        },
        SCHEMAS.tourChain,
      ),
    ).toThrowError(/required contract shape/);
    expect(() =>
      decodeContract(
        {
          ...completedChain,
          latestComparison: {
            ...completedChain.latestComparison,
            receivedValues: ["SD-1001"],
          },
        },
        SCHEMAS.tourChain,
      ),
    ).toThrowError(/required contract shape/);
  });

  it("keeps third and later revisions navigable with stable identity explanations and package history", async () => {
    window.history.replaceState(
      null,
      "",
      `/comparison?comparison_id=${comparison23.comparisonId}`,
    );
    mockApi({
      "GET /api/v1/engagement/tours": thirdRevisionChain,
      "GET /api/v1/engagement/readiness-packages": {
        schema_version: SCHEMAS.engagementReadinessPackages,
        engagement_id: "engagement-tour-001",
        total_count: 1,
        returned_count: 1,
        truncated: false,
        items: [
          {
            publication_id: "publication-tour-1",
            job_id: "job-tour-1",
            candidate_id: "candidate-tour-1",
            approval_id: "result-approval-tour-1",
            status: "COMPLETE",
            practitioner: "Synthetic practitioner",
            publication_reason: "Retain the exact first-revision package.",
            semantic_publication_hash: hash("5"),
            physical_archive_digest: hash("6"),
            package_size_bytes: 8192,
            archive_relative_path:
              "publications/publication-tour-1/readiness.zip",
            created_at: "2026-07-29T10:01:00Z",
            completed_at: "2026-07-29T10:01:01Z",
          },
        ],
      },
    });
    render(<TourComparisonPage entityCount={1} />);

    expect(
      await screen.findByRole("heading", {
        name: "Revision 2 versus Revision 3",
      }),
    ).toBeVisible();
    expect(screen.getByRole("rowheader", { name: /Revision 3/ })).toBeVisible();
    expect(
      screen.getByRole("heading", { name: "Start source revision 4" }),
    ).toBeVisible();
    expect(
      screen.getByText(
        COMPARED_FINDING_IDENTITY_EXPLANATIONS.FULL_ROW_FALLBACK,
      ),
    ).toBeVisible();
    expect(
      await screen.findByRole("link", { name: "Download ZIP" }),
    ).toHaveAttribute(
      "href",
      "/api/v1/engagement/readiness-packages/publication-tour-1/download",
    );
    expect(
      screen.queryByText("publications/publication-tour-1/readiness.zip"),
    ).not.toBeInTheDocument();
  });

  it("fails closed when a comparison deep link is stale or belongs elsewhere", async () => {
    window.history.replaceState(
      null,
      "",
      "/comparison?comparison_id=comparison-not-in-this-engagement",
    );
    mockApi({
      "GET /api/v1/engagement/tours": thirdRevisionChain,
      "GET /api/v1/engagement/readiness-packages": {
        schema_version: SCHEMAS.engagementReadinessPackages,
        engagement_id: "engagement-tour-001",
        total_count: 0,
        returned_count: 0,
        truncated: false,
        items: [],
      },
    });
    render(<TourComparisonPage entityCount={1} />);

    expect(
      await screen.findByText("Comparison evidence unavailable"),
    ).toBeVisible();
    expect(
      screen.getByRole("option", { name: "Requested comparison unavailable" }),
    ).toBeDisabled();
    expect(
      screen.queryByRole("heading", { name: "Revision 2 versus Revision 3" }),
    ).not.toBeInTheDocument();
  });

  it("routes structurally incompatible refreshed source to the existing Mapping Workshop", async () => {
    const mappingRequiredAttempt = {
      ...startedAttempt,
      state: "INCOMPATIBLE",
      generation: 12,
      preflightId: "revised-preflight-mapping-required",
      sourceHash: hash("7"),
      manifestHash: hash("8"),
      jobId: "job-mapping-required",
      profilingRunId: "profiling-mapping-required",
      structureHash: hash("9"),
      errorCode: "MAPPING_REQUIRED",
      errorMessage:
        "The refreshed source structure differs from approved routing; a new governed mapping is required.",
      updatedAt: "2026-07-29T10:03:00Z",
    } satisfies NonNullable<TourChain["pendingAttempt"]>;
    mockApi({
      "GET /api/v1/engagement/tours": {
        ...startedChain,
        engagementGeneration: 12,
        pendingAttempt: mappingRequiredAttempt,
      } satisfies TourChain,
      "GET /api/v1/engagement/readiness-packages": {
        schema_version: SCHEMAS.engagementReadinessPackages,
        engagement_id: "engagement-tour-001",
        total_count: 0,
        returned_count: 0,
        truncated: false,
        items: [],
      },
    });
    render(<TourComparisonPage entityCount={1} />);

    expect(
      await screen.findByText("A new governed mapping is required"),
    ).toBeVisible();
    expect(screen.getByText("MAPPING_REQUIRED")).toBeVisible();
    expect(
      screen.getByRole("link", {
        name: "Open Mapping Workshop for Revision 2",
      }),
    ).toHaveAttribute("href", "/mapping");
  });

  it("renders comparison in the downstream workbench route as well as the engagement shell", async () => {
    window.history.replaceState(null, "", "/comparison");
    const journey = {
      ...journeyFixture,
      currentStageId: "COMPARE",
      binding: {
        ...journeyFixture.binding,
        engagementId: "engagement-tour-001",
        engagementGeneration: completedChain.engagementGeneration,
        activeTourId: tour2.tourId,
        activeTourNumber: tour2.tourNumber,
        activeTourHash: tour2.tourHash,
        comparisonId: completedChain.latestComparison!.comparisonId,
        comparisonHash: completedChain.latestComparison!.comparisonHash,
      },
      stages: journeyFixture.stages.map((stage) =>
        stage.id === "COMPARE"
          ? {
              ...stage,
              status: "CURRENT" as const,
              available: true,
              href: "/comparison",
              unavailableReason: null,
            }
          : stage.id === "ISSUES"
            ? { ...stage, status: "AVAILABLE" as const }
            : stage,
      ),
    };
    mockApi({
      "GET /api/v1/context": contextFixture,
      "GET /api/v1/journey": journey,
      "GET /api/v1/engagement/tours": completedChain,
      "GET /api/v1/engagement/readiness-packages": {
        schema_version: SCHEMAS.engagementReadinessPackages,
        engagement_id: "engagement-tour-001",
        total_count: 0,
        returned_count: 0,
        truncated: false,
        items: [],
      },
    });
    render(<App />);

    expect(
      await screen.findByRole("heading", {
        name: "Source revisions and compared validation",
      }),
    ).toBeVisible();
    expect(
      screen.getByRole("link", { name: "Compare Current" }),
    ).toHaveAttribute("aria-current", "page");
    expect(screen.queryByText(/multi-entity authority stops/i)).toBeNull();
  });

  it("renders a bounded read error without leaking an unhandled console failure", async () => {
    const consoleError = vi
      .spyOn(console, "error")
      .mockImplementation(() => undefined);
    mockApi({
      "GET /api/v1/engagement/tours": errorResponse(
        {
          schema_version: SCHEMAS.error,
          ok: false,
          error: {
            code: "TOUR_INTEGRITY_FAILED",
            message:
              "The immutable tour chain did not pass independent integrity replay.",
            request_id: "request-tour-integrity",
            retryable: false,
          },
        },
        409,
      ),
    });
    render(<TourComparisonPage />);

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "TOUR_INTEGRITY_FAILED",
    );
    expect(consoleError).not.toHaveBeenCalled();
    consoleError.mockRestore();
  });

  it("keeps the completed comparison reachable beside active First Issues", async () => {
    window.history.replaceState(null, "", "/comparison");
    mockApi({
      "GET /api/v1/context": {
        ...engagementContextFixture,
        engagement: {
          ...engagementValidationCompleteStateFixture,
          active: {
            ...engagementValidationCompleteStateFixture.active,
            objectPackId: SALES_DOCUMENT_PACK_ID,
            destinationOrigin: "CONTROLLED_PACK",
            destinationAuthorityId: SALES_DOCUMENT_PACK_ID,
            destinationOriginHash: tour1.profileHash,
            profileId: tour1.profileId,
            profileVersion: tour1.profileVersion,
            profileHash: tour1.profileHash,
            validationRunId: tour2.validationRunId,
          },
        },
      },
      "GET /api/v1/engagement/tours": completedChain,
      "GET /api/v1/engagement/readiness-packages": {
        schema_version: SCHEMAS.engagementReadinessPackages,
        engagement_id: engagementValidationCompleteStateFixture.engagementId,
        total_count: 0,
        returned_count: 0,
        truncated: false,
        items: [],
      },
    });
    render(<App />);

    expect(
      await screen.findByRole("heading", {
        name: "Source revisions and compared validation",
      }),
    ).toBeVisible();
    expect(
      screen.getByRole("link", { name: "Compare Available" }),
    ).toHaveAttribute("aria-current", "page");
    expect(screen.getByRole("link", { name: "Issues Current" })).toBeVisible();
    expect(
      screen.getByLabelText("Multi-entity First Issues authority boundary"),
    ).toHaveTextContent("authority stops at First Issues");
  });

  it("offers the profile-neutral source refresh after any completed validation", async () => {
    const genericCompleteState = engagementValidationCompleteStateFixture;
    const { requests } = mockApi({
      "GET /api/v1/context": {
        ...engagementContextFixture,
        engagement: genericCompleteState,
      },
      "GET /api/v1/engagement": engagementValidationCompleteDetailFixture,
      "GET /api/v1/profiling": sampleFreeProfilingFixture,
      "GET /api/v1/engagement/object-packs": engagementObjectPacksFixture,
      "GET /api/v1/engagement/source-profile-rules": sourceProfileRulesFixture,
      "GET /api/v1/engagement/mapping-coverage": engagementMappingFixture,
      "GET /api/v1/validation": engagementValidationFixture,
      "GET /api/v1/engagement/tours": initialChain,
    });
    render(<App />);

    expect(
      await screen.findByRole("heading", { name: "First-issues handoff" }),
    ).toBeVisible();
    expect(
      await screen.findByRole("heading", { name: "Start source revision 2" }),
    ).toBeVisible();
    expect(
      requests.some(
        (request) => request.url.pathname === "/api/v1/engagement/tours",
      ),
    ).toBe(true);
    expect(readyPreflightFixture.sourceHash).not.toBeNull();
  });
});
