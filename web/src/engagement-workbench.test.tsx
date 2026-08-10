import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";
import App from "./App";
import {
  ENGAGEMENT_COMMAND_SCHEMAS,
  SCHEMAS,
  decodeApplicationContext,
  decodeContract,
  validateEngagementStateCommand,
  type MappingWorkspace,
} from "./contracts";
import {
  engagementApprovedDetailFixture,
  engagementContextFixture,
  engagementDetailFixture,
  engagementMappingFixture,
  engagementMappingReadyDetailFixture,
  engagementObjectPackFixture,
  engagementObjectPacksFixture,
  engagementProfileVersionFixture,
  engagementProfiledDetailFixture,
  engagementSelectedDetailFixture,
  engagementStateFixture,
  engagementValidationCompleteDetailFixture,
  engagementValidationCompleteStateFixture,
  engagementValidationFixture,
  journeyFixture,
  readyPreflightFixture,
  receivingPreflightFixture,
  sampleFreeProfilingFixture,
  sourceProfileRulesFixture,
} from "./test/fixtures";
import { mockApi, type CapturedRequest } from "./test/mock-api";

function at(path = "/setup"): void {
  window.history.replaceState(null, "", path);
}

function jsonBody(
  request: CapturedRequest | undefined,
): Record<string, unknown> {
  expect(request?.body).toEqual(expect.any(String));
  return JSON.parse(request?.body ?? "{}") as Record<string, unknown>;
}

function engagementContext(state: unknown) {
  return { ...engagementContextFixture, engagement: state };
}

function renderStage(
  detail: { state: unknown },
  overrides: Record<string, unknown> = {},
) {
  return mockApi({
    "GET /api/v1/context": engagementContext(detail.state),
    "GET /api/v1/engagement": detail,
    "GET /api/v1/profiling": sampleFreeProfilingFixture,
    "GET /api/v1/engagement/object-packs": engagementObjectPacksFixture,
    "GET /api/v1/engagement/source-profile-rules": sourceProfileRulesFixture,
    "GET /api/v1/engagement/mapping-coverage": engagementMappingFixture,
    "GET /api/v1/validation": engagementValidationFixture,
    ...overrides,
  });
}

const engagementRecordFixture = {
  schemaVersion: SCHEMAS.engagementRecord,
  engagementId: engagementStateFixture.engagementId,
  displayName: engagementStateFixture.displayName,
  createdAt: "2026-07-28T10:00:00Z",
  reopened: false,
};

const alternateSalesProofPack = {
  ...engagementObjectPackFixture,
  objectPackId: "synthetic.sales-document.readiness",
  title: "Basic sales document readiness",
  description:
    "Synthetic target-neutral header and item mapping readiness evidence.",
  rootEntityId: "SalesDocument",
  mappingMode: "OPERATOR_GOVERNED",
  mappingSuggestions: [],
  referenceTables: [],
};

const approvedProfileVersionFixture = {
  ...engagementProfileVersionFixture,
  lifecycle_status: "APPROVED",
  authority: "LOCAL_MAPPING_VALIDATION",
  can_start_new_runs: true,
  approval: {
    approval_id: "approval-tour-001",
    approver: "Tour presenter",
    reason: "Reviewed for the bounded Tour 1 proof.",
    acknowledged: true,
    approved_at: "2026-07-28T10:06:00Z",
    approval_hash: "8".repeat(64),
  },
  lifecycle_events: [
    ...engagementProfileVersionFixture.lifecycle_events,
    {
      sequence: 2,
      from_state: "REVIEWED_DRAFT",
      to_state: "APPROVED",
      actor: "Tour presenter",
      reason: "Reviewed for the bounded Tour 1 proof.",
      previous_event_hash: "7".repeat(64),
      event_hash: "8".repeat(64),
      occurred_at: "2026-07-28T10:06:00Z",
    },
  ],
};

const preparedMappingFixture = {
  schemaVersion: SCHEMAS.preparedMapping,
  preparationId: "mapping-preparation-tour-001",
  mappingHash: "9".repeat(64),
  document: {
    mappingId: "engagement.mapping",
    version: "1.0.0",
    selectedSuggestionIds: [
      "suggestion.object-key",
      "suggestion.classification",
    ],
  },
  reusedImmutablePreparation: false,
  validation: null,
};

const validationLaunchFixture = {
  schemaVersion: SCHEMAS.validationLaunch,
  status: {
    validationRunId: "validation-tour-001",
    state: "COMPLETE",
  },
  report: null,
  reusedAttempt: false,
};

describe("fresh engagement workbench", () => {
  beforeEach(() => at());

  it("accepts only the exact setup context and closed capabilities", () => {
    expect(decodeApplicationContext(engagementContextFixture)).toEqual(
      engagementContextFixture,
    );
    expect(() =>
      decodeApplicationContext({
        ...engagementContextFixture,
        capabilities: {
          ...engagementContextFixture.capabilities,
          targetDelete: false,
        },
      }),
    ).toThrowError(/unsafe authority/);
    expect(() =>
      decodeContract(
        { ...engagementObjectPacksFixture, inferredCompatibility: false },
        SCHEMAS.objectPacks,
      ),
    ).toThrowError(/required contract shape/);
    expect(() =>
      decodeContract(
        { ...engagementStateFixture, profileStage: "GENERATED_DRAFT" },
        SCHEMAS.engagementState,
      ),
    ).toThrowError(/required contract shape/);
    expect(() =>
      decodeContract(
        {
          ...engagementStateFixture,
          profileStage: "APPROVED",
          active: {
            ...engagementStateFixture.active,
            objectPackId: "workspace.profile",
            destinationOrigin: "WORKSPACE_PROFILE",
            destinationAuthorityId: null,
            destinationOriginHash: null,
            profileId: "workspace.profile",
            profileVersion: "1.0.0",
            profileHash: "a".repeat(64),
          },
        },
        SCHEMAS.engagementState,
      ),
    ).toThrowError(/required contract shape/);
    expect(() =>
      decodeContract(
        {
          ...journeyFixture,
          binding: {
            ...journeyFixture.binding,
            destinationAuthorityId: null,
          },
        },
        SCHEMAS.journey,
      ),
    ).toThrowError(/required contract shape/);
  });

  it("requires the exact engagement generation on a validation launch", () => {
    const command = {
      schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.launchValidation,
      idempotencyKey: "validation-launch-contract-001",
      expectedEngagementGeneration: 7,
      expectedJobId: "job-tour-001",
      expectedMappingId: "engagement.mapping",
      expectedMappingVersion: "1.0.0",
      expectedMappingHash: "9".repeat(64),
    };

    expect(validateEngagementStateCommand(command)).toEqual(command);
    const missing: Record<string, unknown> = { ...command };
    Reflect.deleteProperty(missing, "expectedEngagementGeneration");
    expect(() => validateEngagementStateCommand(missing)).toThrowError(
      /incomplete/,
    );
    expect(() =>
      validateEngagementStateCommand({ ...command, unexpected: true }),
    ).toThrowError(/incomplete/);
  });

  it("creates one local engagement with the body retry identity in its header", async () => {
    const user = userEvent.setup();
    const { requests } = mockApi({
      "GET /api/v1/context": engagementContextFixture,
      "POST /api/v1/engagement": engagementRecordFixture,
      "GET /api/v1/engagement": engagementDetailFixture,
    });
    render(<App />);

    await user.type(
      await screen.findByLabelText("Engagement name"),
      "Tour 1 migration readiness",
    );
    await user.click(
      screen.getByRole("button", { name: "Create local engagement" }),
    );

    await waitFor(() =>
      expect(
        requests.some(
          (request) =>
            request.method === "POST" &&
            request.url.pathname === "/api/v1/engagement",
        ),
      ).toBe(true),
    );
    const request = requests.find(
      (candidate) =>
        candidate.method === "POST" &&
        candidate.url.pathname === "/api/v1/engagement",
    );
    const body = jsonBody(request);
    expect(body).toEqual({
      schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.create,
      displayName: "Tour 1 migration readiness",
      idempotencyKey: expect.any(String),
    });
    expect(request?.headers.get("idempotency-key")).toBe(body.idempotencyKey);
    expect(request?.headers.get("x-migration-stream-intent")).toBe(
      "engagement-create-v1",
    );
  });

  it("reserves first, then uploads the selected ZIP as raw binary with the same retry identity", async () => {
    const user = userEvent.setup();
    let detailReads = 0;
    const { requests } = renderStage(engagementDetailFixture, {
      "GET /api/v1/engagement": () => {
        detailReads += 1;
        return detailReads === 1
          ? engagementDetailFixture
          : {
              ...engagementDetailFixture,
              state: {
                ...engagementStateFixture,
                phase: "PREFLIGHT_READY",
                active: {
                  ...engagementStateFixture.active,
                  preflightId: readyPreflightFixture.preflightId,
                },
              },
              preflights: [readyPreflightFixture],
            };
      },
      "POST /api/v1/engagement/source-preflights": {
        preflight: receivingPreflightFixture,
      },
      "PUT /api/v1/engagement/source-preflights/preflight-tour-001/content":
        readyPreflightFixture,
    });
    render(<App />);

    const file = new File(
      ["PK\u0003\u0004synthetic-tour"],
      "tour-1-source.zip",
      {
        type: "application/zip",
      },
    );
    await user.upload(await screen.findByLabelText("Source ZIP"), file);
    await user.click(
      screen.getByRole("button", { name: "Run local safety check" }),
    );

    expect(
      await screen.findByText("Package passed the local safety check."),
    ).toBeVisible();
    const reserve = requests.find(
      (request) =>
        request.method === "POST" &&
        request.url.pathname === "/api/v1/engagement/source-preflights",
    );
    const upload = requests.find((request) => request.method === "PUT");
    const reserveBody = jsonBody(reserve);
    expect(reserveBody).toEqual({
      schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.reserveSourcePreflight,
      sourceName: "tour-1-source.zip",
      idempotencyKey: expect.any(String),
    });
    expect(upload?.url.pathname).toBe(
      "/api/v1/engagement/source-preflights/preflight-tour-001/content",
    );
    expect(upload?.rawBody).toBe(file);
    expect(upload?.body).toBeNull();
    expect(upload?.headers.get("content-type")).toBe("application/zip");
    expect(upload?.headers.get("idempotency-key")).toBe(
      reserveBody.idempotencyKey,
    );
    expect(upload?.headers.get("x-migration-stream-intent")).toBe(
      "engagement-source-preflight-content-v1",
    );
  });

  it("reads only sample-free profiling and exposes no sample request control", async () => {
    const { requests } = renderStage(engagementProfiledDetailFixture);
    render(<App />);

    expect(
      await screen.findByRole("heading", { name: "Source profiled locally" }),
    ).toBeVisible();
    expect(
      screen.getByText("Value samples are not stored for this engagement."),
    ).toBeVisible();
    expect(
      screen.queryByRole("button", { name: /request samples/i }),
    ).not.toBeInTheDocument();
    expect(screen.queryByText("000123")).not.toBeInTheDocument();
    const profiling = requests.find(
      (request) => request.url.pathname === "/api/v1/profiling",
    );
    expect(profiling?.url.searchParams.get("include_samples")).toBe("false");
    expect(profiling?.url.searchParams.get("page_size")).toBe("200");
  });

  it("gives every proof-pack action a unique accessible name with its title", async () => {
    renderStage(engagementProfiledDetailFixture, {
      "GET /api/v1/engagement/object-packs": {
        schemaVersion: SCHEMAS.objectPacks,
        items: [engagementObjectPackFixture, alternateSalesProofPack],
      },
    });
    render(<App />);

    expect(
      await screen.findByRole("button", {
        name: `Use this proof pack: ${engagementObjectPackFixture.title}`,
      }),
    ).toBeVisible();
    expect(
      screen.getByRole("button", {
        name: `Use this proof pack: ${alternateSalesProofPack.title}`,
      }),
    ).toBeVisible();
  });

  it("lets the operator correct a selected proof pack before profile approval", async () => {
    const user = userEvent.setup();
    const { requests } = renderStage(engagementSelectedDetailFixture, {
      "GET /api/v1/engagement/object-packs": {
        schemaVersion: SCHEMAS.objectPacks,
        items: [engagementObjectPackFixture, alternateSalesProofPack],
      },
      "POST /api/v1/engagement/object-packs/select": {
        objectPack: alternateSalesProofPack,
        profile: engagementProfileVersionFixture,
      },
    });
    render(<App />);

    await user.click(
      await screen.findByRole("button", {
        name: `Change proof pack from ${engagementObjectPackFixture.title}`,
      }),
    );
    expect(
      screen.getByRole("button", {
        name: `Keep selected proof pack: ${engagementObjectPackFixture.title}`,
      }),
    ).toBeVisible();
    await user.click(
      screen.getByRole("button", {
        name: `Use this proof pack: ${alternateSalesProofPack.title}`,
      }),
    );

    await waitFor(() =>
      expect(
        requests.some(
          (request) =>
            request.method === "POST" &&
            request.url.pathname === "/api/v1/engagement/object-packs/select",
        ),
      ).toBe(true),
    );
    const request = requests.find(
      (candidate) =>
        candidate.method === "POST" &&
        candidate.url.pathname === "/api/v1/engagement/object-packs/select",
    );
    expect(jsonBody(request)).toEqual({
      schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.selectObjectPack,
      objectPackId: alternateSalesProofPack.objectPackId,
      idempotencyKey: expect.any(String),
    });
  });

  it("records explicit attributable local profile authority without changing REVIEWED_DRAFT provenance", async () => {
    const user = userEvent.setup();
    const { requests } = renderStage(engagementSelectedDetailFixture, {
      "POST /api/v1/engagement/object-packs/approve":
        approvedProfileVersionFixture,
    });
    render(<App />);

    expect(
      await screen.findByText(/Registration preserves REVIEWED_DRAFT/),
    ).toBeVisible();
    expect(screen.getByText("Object key")).toBeVisible();
    expect(screen.getByText("Object key is required.")).toBeVisible();
    await user.type(screen.getByLabelText("Approver"), "Tour presenter");
    await user.type(
      screen.getByLabelText("Approval reason"),
      "Reviewed for the bounded Tour 1 proof.",
    );
    for (const checkbox of screen.getAllByRole("checkbox")) {
      await user.click(checkbox);
    }
    await user.click(
      screen.getByRole("button", {
        name: "Approve for local mapping and validation",
      }),
    );

    await waitFor(() =>
      expect(
        requests.some(
          (request) =>
            request.method === "POST" &&
            request.url.pathname === "/api/v1/engagement/object-packs/approve",
        ),
      ).toBe(true),
    );
    const request = requests.find(
      (candidate) =>
        candidate.method === "POST" &&
        candidate.url.pathname === "/api/v1/engagement/object-packs/approve",
    );
    const body = jsonBody(request);
    expect(body).toEqual({
      schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.approveObjectPackProfile,
      actor: "Tour presenter",
      reason: "Reviewed for the bounded Tour 1 proof.",
      authorityAcknowledgement:
        "migration-stream/local-mapping-validation-authority/v1",
      idempotencyKey: expect.any(String),
    });
    expect(request?.headers.get("idempotency-key")).toBe(body.idempotencyKey);
  });

  it("submits only reviewed server-owned suggestion identities for mapping", async () => {
    at("/mapping");
    const user = userEvent.setup();
    const { requests } = renderStage(engagementApprovedDetailFixture, {
      "POST /api/v1/engagement/mappings/prepare": preparedMappingFixture,
    });
    render(<App />);

    expect(
      await screen.findByRole("heading", {
        name: "Review controlled mapping preparation",
      }),
    ).toBeVisible();
    expect(
      screen.queryByRole("heading", { name: "Build the mapping contract" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByText(/reviews only server-owned suggestion identities/i),
    ).toBeVisible();
    expect(
      screen.getByRole("heading", {
        name: "Review controlled mapping suggestions",
      }),
    ).toBeVisible();
    expect(
      screen.queryByLabelText(/Reference-table JSON/i),
    ).not.toBeInTheDocument();
    await user.type(screen.getByLabelText("Mapping author"), "Tour mapper");
    await user.click(
      screen.getByRole("checkbox", {
        name: /I reviewed the exact controlled suggestions/i,
      }),
    );
    await user.click(
      screen.getByRole("button", { name: "Check and prepare mapping" }),
    );

    await waitFor(() =>
      expect(
        requests.some(
          (request) =>
            request.method === "POST" &&
            request.url.pathname === "/api/v1/engagement/mappings/prepare",
        ),
      ).toBe(true),
    );
    const request = requests.find(
      (candidate) =>
        candidate.method === "POST" &&
        candidate.url.pathname === "/api/v1/engagement/mappings/prepare",
    );
    const body = jsonBody(request);
    expect(body).toEqual({
      schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.prepareMapping,
      idempotencyKey: expect.any(String),
      mappingId: "engagement.mapping",
      version: "1.0.0",
      selectedSuggestionIds: [
        "suggestion.object-key",
        "suggestion.classification",
      ],
      author: "Tour mapper",
      reviewer: null,
      acknowledgement:
        "migration-stream/local-assisted-mapping-reviewed-suggestions-no-target-compatibility/v1",
    });
    expect(body).not.toHaveProperty("bindings");
    expect(body).not.toHaveProperty("referenceTables");
    expect(request?.headers.get("idempotency-key")).toBe(body.idempotencyKey);
  });

  it("continues an approved workspace profile into operator-governed Mapping without a static pack", async () => {
    at("/mapping");
    const profileId = "workspace.sales-document";
    const profileHash = "c".repeat(64);
    const workspaceProfileState = {
      ...engagementApprovedDetailFixture.state,
      phase: "AWAITING_MAPPING",
      generation: 12,
      active: {
        ...engagementApprovedDetailFixture.state.active,
        objectPackId: profileId,
        destinationOrigin: "WORKSPACE_PROFILE",
        destinationAuthorityId: "destination-origin-workspace-001",
        destinationOriginHash: "d".repeat(64),
        profileId,
        profileVersion: "1.0.0",
        profileHash,
        mappingId: null,
        mappingVersion: null,
        mappingHash: null,
      },
      profileStage: "APPROVED",
      mappingStage: "NOT_PREPARED",
      validationStage: "NOT_LAUNCHED",
    } as const;
    const workspace = {
      schemaVersion: SCHEMAS.mappingWorkspace,
      engagementGeneration: workspaceProfileState.generation,
      jobId: workspaceProfileState.active.jobId,
      sourceHash: readyPreflightFixture.sourceHash,
      profilingRunId: "profiling-workspace-profile-001",
      destinationOrigin: "WORKSPACE_PROFILE",
      objectPackId: profileId,
      objectPackVersion: "1.0.0",
      profileId,
      profileVersion: "1.0.0",
      profileHash,
      mappingMode: "OPERATOR_GOVERNED",
      sourceFiles: [
        {
          sourceFile: "objects/tour.csv",
          columns: sourceProfileRulesFixture.sourceColumns,
        },
      ],
      entities: [
        {
          entityId: "TourObject",
          root: true,
          key: ["object_key"],
          fields: sourceProfileRulesFixture.profileFields,
        },
      ],
      relationships: [],
      allowedOperations: ["direct"],
      controlledReferenceTables: [],
      referenceTableObligations: [],
      limits: {
        maximumSourceFiles: 32,
        maximumBindings: 512,
        maximumRelationships: 256,
        maximumReferenceTables: 32,
        maximumReferenceEntries: 1_000,
        maximumReferenceBytes: 1_048_576,
        maximumReferenceKeyBytes: 1_024,
        maximumReferenceValueBytes: 4_096,
      },
      revisionDraftScope: null,
      draft: null,
    } satisfies MappingWorkspace;
    const detail = {
      ...engagementApprovedDetailFixture,
      state: workspaceProfileState,
    };
    const { requests } = renderStage(detail, {
      "GET /api/v1/engagement/mapping-workspace": workspace,
    });

    render(<App />);

    expect(
      await screen.findByRole("heading", { name: "Mapping Workspace" }),
    ).toBeVisible();
    expect(
      screen.getByRole("heading", { name: "Build the mapping contract" }),
    ).toBeVisible();
    expect(
      screen.getByRole("heading", {
        name: "Approved workspace destination profile",
      }),
    ).toBeVisible();
    expect(
      screen.getByText(/contains no embedded source-mapping answers/i),
    ).toBeVisible();
    expect(
      screen.queryByRole("heading", {
        name: "Review the synthetic object pack",
      }),
    ).not.toBeInTheDocument();
    expect(
      requests.some(
        (request) => request.url.pathname === "/api/v1/engagement/object-packs",
      ),
    ).toBe(false);
  });

  it("reloads validated setup mapping coverage without requesting validation-governed evidence", async () => {
    const { requests } = renderStage(engagementMappingReadyDetailFixture);
    render(<App />);

    expect(
      await screen.findByRole("heading", { name: "Mapping coverage" }),
    ).toBeVisible();
    expect(
      screen.getByText(
        "Structural mapping coverage reports no blocking binding or relationship-key findings.",
      ),
    ).toBeVisible();
    expect(
      screen.getByText(
        "This evidence checks mapping structure only. Received-value checks run when validation is launched.",
      ),
    ).toBeVisible();
    await waitFor(() =>
      expect(
        requests.some(
          (request) =>
            request.method === "GET" &&
            request.url.pathname === "/api/v1/engagement/mapping-coverage" &&
            request.url.searchParams.get("page_size") === "200",
        ),
      ).toBe(true),
    );
    expect(
      requests.some(
        (request) =>
          request.method === "GET" &&
          request.url.pathname === "/api/v1/mapping",
      ),
    ).toBe(false);
  });

  it("launches fixed-policy validation and hands completed evidence to the existing Issues route", async () => {
    at("/validation");
    const user = userEvent.setup();
    let launched = false;
    let contextReads = 0;
    let journeyReads = 0;
    const ready = renderStage(engagementMappingReadyDetailFixture, {
      "GET /api/v1/context": () => {
        contextReads += 1;
        return engagementContext(engagementMappingReadyDetailFixture.state);
      },
      "GET /api/v1/engagement": () =>
        launched
          ? engagementValidationCompleteDetailFixture
          : engagementMappingReadyDetailFixture,
      "GET /api/v1/journey": () => {
        journeyReads += 1;
        return journeyFixture;
      },
      "POST /api/v1/engagement/validation/launch": () => {
        launched = true;
        return validationLaunchFixture;
      },
    });
    render(<App />);

    expect(
      await screen.findByRole("heading", {
        name: "Review and launch validation",
      }),
    ).toBeVisible();
    expect(
      screen.getByRole("heading", { name: "Validate the approved mapping" }),
    ).toBeVisible();
    expect(screen.getByText(/Rules cannot be edited here/)).toBeVisible();
    await user.click(
      screen.getByRole("checkbox", {
        name: /I reviewed the immutable rules/i,
      }),
    );
    await user.click(
      screen.getByRole("button", { name: "Launch local validation" }),
    );

    await waitFor(() =>
      expect(
        ready.requests.some(
          (request) =>
            request.method === "POST" &&
            request.url.pathname === "/api/v1/engagement/validation/launch",
        ),
      ).toBe(true),
    );
    const launch = ready.requests.find(
      (request) =>
        request.method === "POST" &&
        request.url.pathname === "/api/v1/engagement/validation/launch",
    );
    const launchBody = jsonBody(launch);
    expect(launchBody).toEqual({
      schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.launchValidation,
      idempotencyKey: expect.any(String),
      expectedEngagementGeneration:
        engagementMappingReadyDetailFixture.state.generation,
      expectedJobId: "job-tour-001",
      expectedMappingId: "engagement.mapping",
      expectedMappingVersion: "1.0.0",
      expectedMappingHash: "9".repeat(64),
    });
    expect(launch?.headers.get("idempotency-key")).toBe(
      launchBody.idempotencyKey,
    );

    const issuesLink = await screen.findByRole("link", {
      name: "Review first issues",
    });
    expect(issuesLink).toHaveAttribute("href", "/issues");
    expect(contextReads).toBe(1);
    expect(journeyReads).toBeGreaterThanOrEqual(2);
    const issuesMetric = screen.getByText("Issues produced").parentElement;
    expect(issuesMetric).not.toBeNull();
    expect(within(issuesMetric!).getByText("2")).toBeVisible();
  });

  it("renders a zero-issue VALIDATION_COMPLETE phase without claiming issues were found", async () => {
    const zeroIssueState = {
      ...engagementValidationCompleteStateFixture,
      phase: "VALIDATION_COMPLETE",
    };
    const zeroIssueDetail = {
      ...engagementValidationCompleteDetailFixture,
      state: zeroIssueState,
    };
    renderStage(zeroIssueDetail, {
      "GET /api/v1/validation": {
        ...engagementValidationFixture,
        data: {
          ...engagementValidationFixture.data,
          issue_count: 0,
        },
      },
    });
    render(<App />);

    expect(
      await screen.findByText(
        "This run produced no issues. That is not a readiness or target-compatibility claim.",
      ),
    ).toBeVisible();
    expect(screen.getByText("Validation complete")).toBeVisible();
    expect(
      screen.queryByRole("link", { name: "Review first issues" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: "Review completed validation" }),
    ).toHaveAttribute("href", "/validation");
  });
});
