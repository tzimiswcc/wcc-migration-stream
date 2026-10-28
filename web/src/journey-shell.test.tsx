import { render, screen, waitFor, within } from "@testing-library/react";
import axe from "axe-core";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";
import App from "./App";
import { AUTHORITY_CHANGED_EVENT } from "./api";
import { SCHEMAS, type Journey } from "./contracts";
import { JourneyRail, journeyStageForLocation } from "./journey-rail";
import {
  engagementContextFixture,
  engagementDetailFixture,
  engagementStateFixture,
  engagementValidationFixture,
  journeyFixture,
  publicationContextFixture,
  sourceProfileRulesFixture,
} from "./test/fixtures";
import { mockApi } from "./test/mock-api";

const hash = (character: string) => character.repeat(64);
const multiEntityReason =
  "Multi-entity downstream authority is not implemented yet.";

function noDestinationAuthority() {
  return {
    objectPackId: null,
    destinationOrigin: null,
    destinationAuthorityId: null,
    destinationOriginHash: null,
    profileId: null,
    profileVersion: null,
    profileHash: null,
  };
}

function controlledPackAuthority(
  objectPackId: string,
  profileHash: string,
  profileVersion = "1.0.0",
) {
  return {
    objectPackId,
    destinationOrigin: "CONTROLLED_PACK" as const,
    destinationAuthorityId: objectPackId,
    destinationOriginHash: profileHash,
    profileId: objectPackId,
    profileVersion,
    profileHash,
  };
}

function stage(
  id: (typeof journeyFixture.stages)[number]["id"],
  status: "COMPLETED" | "CURRENT" | "AVAILABLE" | "LOCKED",
  href: string | null,
  unavailableReason: string | null,
) {
  const original = journeyFixture.stages.find((item) => item.id === id);
  if (!original) throw new Error(`Missing journey fixture stage ${id}`);
  return {
    ...original,
    status,
    available: href !== null,
    href,
    unavailableReason,
  };
}

function journey(
  currentStageId: string,
  stages: ReturnType<typeof stage>[],
  binding: Record<string, unknown> = {},
) {
  return {
    ...journeyFixture,
    currentStageId,
    binding: { ...journeyFixture.binding, ...binding },
    stages,
  };
}

function initialJourney() {
  return journey(
    "SETUP",
    [
      stage("SETUP", "CURRENT", "/setup", null),
      stage(
        "DESTINATION",
        "LOCKED",
        null,
        "Accept and profile a source before choosing a destination profile.",
      ),
      stage(
        "SOURCE",
        "LOCKED",
        null,
        "Open the local engagement before accepting a source.",
      ),
      stage("MAPPING", "LOCKED", null, "Approve a profile before mapping."),
      stage("VALIDATE", "LOCKED", null, "Approve a mapping before validation."),
      stage(
        "COMPARE",
        "LOCKED",
        null,
        "Complete a first validation before comparing a revised source.",
      ),
      stage("ISSUES", "LOCKED", null, "Complete validation first."),
      stage("DECISIONS", "LOCKED", null, multiEntityReason),
      stage("RECONCILIATION", "LOCKED", null, multiEntityReason),
      stage("PACKAGE", "LOCKED", null, multiEntityReason),
    ],
    {
      engagementId: null,
      engagementGeneration: null,
      jobId: null,
      sourceHash: null,
      manifestHash: null,
      profilingRunId: null,
      ...noDestinationAuthority(),
      mappingId: null,
      mappingVersion: null,
      mappingHash: null,
      validationRunId: null,
      validationResultHash: null,
      activeTourId: null,
      activeTourNumber: null,
      activeTourHash: null,
      comparisonId: null,
      comparisonHash: null,
      entityCount: null,
    },
  );
}

function sourceJourney() {
  return journey(
    "SOURCE",
    [
      stage("SETUP", "COMPLETED", "/setup", null),
      stage(
        "DESTINATION",
        "LOCKED",
        null,
        "Profile the accepted source before choosing a destination profile.",
      ),
      stage("SOURCE", "CURRENT", "/setup", null),
      stage("MAPPING", "LOCKED", null, "Approve a profile before mapping."),
      stage("VALIDATE", "LOCKED", null, "Approve a mapping before validation."),
      stage(
        "COMPARE",
        "LOCKED",
        null,
        "Complete a first validation before comparing a revised source.",
      ),
      stage("ISSUES", "LOCKED", null, "Complete validation first."),
      stage("DECISIONS", "LOCKED", null, multiEntityReason),
      stage("RECONCILIATION", "LOCKED", null, multiEntityReason),
      stage("PACKAGE", "LOCKED", null, multiEntityReason),
    ],
    {
      engagementId: engagementStateFixture.engagementId,
      engagementGeneration: engagementStateFixture.generation,
      jobId: null,
      sourceHash: null,
      manifestHash: null,
      profilingRunId: null,
      ...noDestinationAuthority(),
      mappingId: null,
      mappingVersion: null,
      mappingHash: null,
      validationRunId: null,
      validationResultHash: null,
      activeTourId: null,
      activeTourNumber: null,
      activeTourHash: null,
      comparisonId: null,
      comparisonHash: null,
      entityCount: null,
    },
  );
}

function controlledMappingJourney() {
  return journey(
    "MAPPING",
    [
      stage("SETUP", "COMPLETED", "/setup", null),
      stage("DESTINATION", "COMPLETED", "/setup#object-pack", null),
      stage("SOURCE", "COMPLETED", "/setup#profiled-source", null),
      stage("MAPPING", "CURRENT", "/mapping", null),
      stage("VALIDATE", "LOCKED", null, "Approve a mapping before validation."),
      stage(
        "COMPARE",
        "LOCKED",
        null,
        "Complete a first validation before comparing a revised source.",
      ),
      stage("ISSUES", "LOCKED", null, "Complete validation first."),
      stage("DECISIONS", "LOCKED", null, "Decision authority is unavailable."),
      stage(
        "RECONCILIATION",
        "LOCKED",
        null,
        "Reconciliation authority is unavailable.",
      ),
      stage("PACKAGE", "LOCKED", null, "Package authority is unavailable."),
    ],
    {
      engagementId: engagementStateFixture.engagementId,
      engagementGeneration: 4,
      jobId: "job-customer-001",
      sourceHash: hash("1"),
      manifestHash: hash("2"),
      profilingRunId: "profiling-customer-001",
      ...controlledPackAuthority("synthetic.customer.readiness", hash("3")),
      entityCount: 1,
    },
  );
}

function multiEntityJourney(compareStatus: "AVAILABLE" | "COMPLETED") {
  return journey(
    "ISSUES",
    [
      stage("SETUP", "COMPLETED", "/setup", null),
      stage("DESTINATION", "COMPLETED", "/setup#object-pack", null),
      stage("SOURCE", "COMPLETED", "/setup#profiled-source", null),
      stage("MAPPING", "COMPLETED", "/mapping", null),
      stage("VALIDATE", "COMPLETED", "/validation", null),
      stage("COMPARE", compareStatus, "/comparison", null),
      stage("ISSUES", "CURRENT", "/issues", null),
      stage("DECISIONS", "LOCKED", null, multiEntityReason),
      stage("RECONCILIATION", "LOCKED", null, multiEntityReason),
      stage("PACKAGE", "LOCKED", null, multiEntityReason),
    ],
    {
      engagementId: "engagement-sales-001",
      engagementGeneration: 20,
      jobId: "job-sales-tour-2",
      sourceHash: hash("1"),
      manifestHash: hash("2"),
      profilingRunId: "profiling-sales-tour-2",
      ...controlledPackAuthority(
        "synthetic.sales-document.readiness",
        hash("3"),
      ),
      mappingId: "sales.mapping",
      mappingVersion: "1.0.0",
      mappingHash: hash("4"),
      validationRunId: "validation-sales-tour-2",
      validationResultHash: hash("5"),
      activeTourId: "tour-sales-002",
      activeTourNumber: 2,
      activeTourHash: hash("6"),
      comparisonId: "comparison-sales-001",
      comparisonHash: hash("7"),
      entityCount: 2,
    },
  );
}

function prevalidationMultiEntityJourney() {
  return journey(
    "VALIDATE",
    [
      stage("SETUP", "COMPLETED", "/setup", null),
      stage("DESTINATION", "COMPLETED", "/setup#object-pack", null),
      stage("SOURCE", "COMPLETED", "/setup#profiled-source", null),
      stage("MAPPING", "COMPLETED", "/mapping", null),
      stage("VALIDATE", "CURRENT", "/validation", null),
      stage(
        "COMPARE",
        "LOCKED",
        null,
        "Complete a first validation before comparing a revised source.",
      ),
      stage(
        "ISSUES",
        "LOCKED",
        null,
        "Complete validation before opening issue evidence.",
      ),
      stage("DECISIONS", "LOCKED", null, multiEntityReason),
      stage("RECONCILIATION", "LOCKED", null, multiEntityReason),
      stage("PACKAGE", "LOCKED", null, multiEntityReason),
    ],
    {
      engagementId: "engagement-sales-001",
      engagementGeneration: 9,
      jobId: "job-sales-tour-1",
      ...controlledPackAuthority(
        "synthetic.sales-document.readiness",
        hash("3"),
      ),
      entityCount: 2,
      validationRunId: null,
      validationResultHash: null,
    },
  );
}

function zeroIssueMultiEntityJourney() {
  return journey(
    "VALIDATE",
    [
      stage("SETUP", "COMPLETED", "/setup", null),
      stage("DESTINATION", "COMPLETED", "/setup#object-pack", null),
      stage("SOURCE", "COMPLETED", "/setup#profiled-source", null),
      stage("MAPPING", "COMPLETED", "/mapping", null),
      stage("VALIDATE", "CURRENT", "/validation", null),
      stage("COMPARE", "AVAILABLE", "/comparison", null),
      stage(
        "ISSUES",
        "LOCKED",
        null,
        "This validation completed without issue findings.",
      ),
      stage("DECISIONS", "LOCKED", null, multiEntityReason),
      stage("RECONCILIATION", "LOCKED", null, multiEntityReason),
      stage("PACKAGE", "LOCKED", null, multiEntityReason),
    ],
    {
      engagementId: "engagement-sales-001",
      engagementGeneration: 10,
      jobId: "job-sales-tour-1",
      ...controlledPackAuthority(
        "synthetic.sales-document.readiness",
        hash("3"),
      ),
      validationRunId: "validation-sales-clean",
      validationResultHash: hash("8"),
      entityCount: 2,
    },
  );
}

function singleEntityDownstreamJourney() {
  return journey(
    "PACKAGE",
    [
      stage("SETUP", "COMPLETED", "/", null),
      stage("DESTINATION", "COMPLETED", "/profile", null),
      stage("SOURCE", "COMPLETED", "/files", null),
      stage("MAPPING", "COMPLETED", "/mapping", null),
      stage("VALIDATE", "COMPLETED", "/validation", null),
      stage("COMPARE", "AVAILABLE", "/comparison", null),
      stage("ISSUES", "COMPLETED", "/issues", null),
      stage("DECISIONS", "COMPLETED", "/decisions", null),
      stage("RECONCILIATION", "COMPLETED", "/reconciliation", null),
      stage("PACKAGE", "CURRENT", "/publication", null),
    ],
    { entityCount: 1 },
  );
}

function salesContext(phase = "FIRST_ISSUES_READY") {
  const validationComplete = phase !== "READY_TO_VALIDATE";
  return {
    ...engagementContextFixture,
    engagement: {
      ...engagementStateFixture,
      phase,
      profileStage: "APPROVED",
      mappingStage: "VALIDATED",
      validationStage: validationComplete ? "COMPLETE" : "NOT_LAUNCHED",
      active: {
        ...engagementStateFixture.active,
        jobId: "job-sales-tour-1",
        profilingRunId: "profiling-sales-tour-1",
        ...controlledPackAuthority(
          "synthetic.sales-document.readiness",
          hash("3"),
        ),
        mappingId: "sales.mapping",
        mappingVersion: "1.0.0",
        mappingHash: hash("4"),
        validationRunId: validationComplete ? "validation-sales-tour-1" : null,
      },
    },
    capabilities: {
      ...engagementContextFixture.capabilities,
      sourceIntake: false,
    },
  };
}

describe("unified migration journey shell", () => {
  beforeEach(() => window.history.replaceState(null, "", "/setup"));

  it("includes visible stage labels and statuses in journey link names", () => {
    const railJourney = {
      ...journeyFixture,
      stages: [
        stage("SETUP", "COMPLETED", "/setup", null),
        stage("ISSUES", "CURRENT", "/issues", null),
        stage("RECONCILIATION", "AVAILABLE", "/reconciliation", null),
      ],
    } as Journey;
    render(
      <JourneyRail
        journey={railJourney}
        location={new URL("/issues", window.location.origin)}
      />,
    );
    const rail = screen.getByRole("navigation", { name: "Migration journey" });
    for (const name of [
      "Setup / Workspace Complete",
      "Issues Current",
      "Reconciliation Available",
    ]) {
      expect(within(rail).getByRole("link", { name })).toBeVisible();
    }
    expect(within(rail).getAllByRole("link")).toHaveLength(3);
  });

  it("renders the same persistent journey rail in setup and downstream modes", async () => {
    const setup = mockApi({
      "GET /api/v1/context": engagementContextFixture,
      "GET /api/v1/journey": initialJourney(),
      "GET /api/v1/engagement": engagementDetailFixture,
    });
    const first = render(<App />);
    const setupRail = await screen.findByRole("navigation", {
      name: "Migration journey",
    });
    expect(within(setupRail).getAllByRole("listitem")).toHaveLength(10);
    expect(
      within(setupRail).getByRole("link", {
        name: "Setup / Workspace Current",
      }),
    ).toHaveAttribute("aria-current", "page");
    expect(setup.requests).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          url: expect.objectContaining({ pathname: "/api/v1/journey" }),
        }),
      ]),
    );
    first.unmount();

    window.history.replaceState(null, "", "/decisions");
    mockApi({
      "GET /api/v1/context": publicationContextFixture,
      "GET /api/v1/journey": singleEntityDownstreamJourney(),
    });
    render(<App />);
    const downstreamRail = await screen.findByRole("navigation", {
      name: "Migration journey",
    });
    expect(within(downstreamRail).getAllByRole("listitem")).toHaveLength(10);
    expect(
      within(downstreamRail).getByRole("link", { name: "Decisions Complete" }),
    ).toHaveAttribute("href", "/decisions");
  });

  it("provides accessible read-only stage help in both journey shells", async () => {
    const setup = mockApi({
      "GET /api/v1/context": engagementContextFixture,
      "GET /api/v1/journey": initialJourney(),
      "GET /api/v1/engagement": engagementDetailFixture,
    });
    const first = render(<App />);
    const setupHelp = await screen.findByRole("region", {
      name: "Help for this stage",
    });
    expect(setupHelp).toHaveTextContent("Set up one local workspace");
    await userEvent
      .setup()
      .click(within(setupHelp).getByRole("button", { name: "Show help" }));
    expect(setupHelp).toHaveTextContent(
      "The browser does not choose a server path.",
    );
    expect(await axe.run(document.body)).toMatchObject({ violations: [] });
    expect(setup.requests.every((request) => request.method === "GET")).toBe(
      true,
    );
    first.unmount();

    window.history.replaceState(null, "", "/decisions");
    const downstream = mockApi({
      "GET /api/v1/context": publicationContextFixture,
      "GET /api/v1/journey": singleEntityDownstreamJourney(),
    });
    render(<App />);
    const workbenchHelp = await screen.findByRole("region", {
      name: "Help for this stage",
    });
    expect(workbenchHelp).toHaveTextContent("Record governed local decisions");
    await userEvent
      .setup()
      .click(within(workbenchHelp).getByRole("button", { name: "Show help" }));
    expect(workbenchHelp).toHaveTextContent(
      "do not change the accepted source",
    );
    expect(await axe.run(document.body)).toMatchObject({ violations: [] });
    expect(
      downstream.requests.every((request) => request.method === "GET"),
    ).toBe(true);
  });

  it("keeps single-entity downstream continuity available after a /setup reload", async () => {
    mockApi({
      "GET /api/v1/context": publicationContextFixture,
      "GET /api/v1/journey": singleEntityDownstreamJourney(),
    });
    render(<App />);

    expect(
      await screen.findByRole("heading", { name: "Overview" }),
    ).toBeVisible();
    const rail = screen.getByRole("navigation", {
      name: "Migration journey",
    });
    expect(
      within(rail).getByRole("link", { name: "Decisions Complete" }),
    ).toHaveAttribute("href", "/decisions");
    expect(
      within(rail).getByRole("link", { name: "Reconciliation Complete" }),
    ).toHaveAttribute("href", "/reconciliation");
    expect(
      within(rail).getByRole("link", { name: "Compare Available" }),
    ).toHaveAttribute("href", "/comparison");
    expect(
      within(rail).getByRole("link", { name: "Package Current" }),
    ).toHaveAttribute("href", "/publication");
  });

  it("shows comparison and issues while locking multi-entity downstream authority", async () => {
    window.history.replaceState(null, "", "/issues");
    mockApi({
      "GET /api/v1/context": salesContext(),
      "GET /api/v1/journey": multiEntityJourney("AVAILABLE"),
    });
    render(<App />);

    expect(
      await screen.findByRole("heading", { name: "Issues" }),
    ).toBeVisible();
    const rail = screen.getByRole("navigation", {
      name: "Migration journey",
    });
    expect(
      within(rail).getByRole("link", { name: "Compare Available" }),
    ).toHaveAttribute("href", "/comparison");
    expect(
      within(rail).getByRole("link", { name: "Mapping Complete" }),
    ).toHaveAttribute("href", "/mapping");
    expect(
      within(rail).queryByRole("link", { name: /^Decisions\b/ }),
    ).not.toBeInTheDocument();
    expect(within(rail).getAllByText(multiEntityReason)).toHaveLength(3);
  });

  it("links controlled and operator mapping modes to the dedicated workshop", async () => {
    const controlledState = {
      ...engagementStateFixture,
      generation: 4,
      phase: "AWAITING_MAPPING",
      profileStage: "APPROVED",
      active: {
        ...engagementStateFixture.active,
        jobId: "job-customer-001",
        profilingRunId: "profiling-customer-001",
        ...controlledPackAuthority("synthetic.customer.readiness", hash("3")),
      },
    };
    mockApi({
      "GET /api/v1/context": {
        ...engagementContextFixture,
        engagement: controlledState,
      },
      "GET /api/v1/journey": controlledMappingJourney(),
      "GET /api/v1/engagement": {
        ...engagementDetailFixture,
        state: controlledState,
      },
    });
    render(<App />);

    const rail = await screen.findByRole("navigation", {
      name: "Migration journey",
    });
    expect(
      within(rail).getByRole("link", { name: "Mapping Current" }),
    ).toHaveAttribute("href", "/mapping");
  });

  it.each([
    ["", "SETUP", "Setup / Workspace"],
    ["#profiled-source", "SOURCE", "Source"],
    ["#object-pack", "DESTINATION", "Destination"],
    ["#mapping", "MAPPING", "Mapping"],
    ["#mapping-workspace", "MAPPING", "Mapping"],
    ["#validation", "VALIDATE", "Validate"],
  ] as const)(
    "treats /setup%s as the %s journey stage",
    (fragment, stageId, stageLabel) => {
      const location = new URL(`/setup${fragment}`, window.location.origin);
      const railJourney = journeyFixture as Journey;
      render(<JourneyRail journey={railJourney} location={location} />);

      expect(journeyStageForLocation(railJourney, location)?.id).toBe(stageId);
      const rail = screen.getByRole("navigation", {
        name: "Migration journey",
      });
      expect(
        within(rail).getByRole("link", { name: `${stageLabel} Complete` }),
      ).toHaveAttribute("aria-current", "page");
      expect(
        within(rail)
          .getAllByRole("link")
          .filter((link) => link.getAttribute("aria-current") === "page"),
      ).toHaveLength(1);
    },
  );

  it.each([
    [
      "/comparison",
      "Compare is locked",
      "Complete a first validation before comparing a revised source.",
    ],
    [
      "/issues",
      "Issues is locked",
      "Complete validation before opening issue evidence.",
    ],
    [
      "/rows/2?source_file=deal_detail.csv",
      "Issues is locked",
      "Complete validation before opening issue evidence.",
    ],
    ["/decisions", "Decisions is locked", multiEntityReason],
  ])(
    "fails the deep link %s closed with the server reason",
    async (path, heading, reason) => {
      window.history.replaceState(null, "", path);
      const { requests } = mockApi({
        "GET /api/v1/context": salesContext("READY_TO_VALIDATE"),
        "GET /api/v1/journey": prevalidationMultiEntityJourney(),
      });
      render(<App />);

      expect(
        await screen.findByRole("heading", { name: heading }),
      ).toBeVisible();
      expect(screen.getAllByText(reason).length).toBeGreaterThan(0);
      expect(
        requests.some(
          (request) =>
            request.url.pathname === "/api/v1/issues" ||
            request.url.pathname.startsWith("/api/v1/rows/") ||
            request.url.pathname === "/api/v1/decision-queue" ||
            request.url.pathname === "/api/v1/engagement/tours",
        ),
      ).toBe(false);
    },
  );

  it("shows zero-issue multi-entity validation without a First Issues claim", async () => {
    window.history.replaceState(null, "", "/validation");
    const cleanValidation = {
      ...engagementValidationFixture,
      data: {
        ...engagementValidationFixture.data,
        run_id: "validation-sales-clean",
        issue_count: 0,
        issue_breakdowns: {
          ...engagementValidationFixture.data.issue_breakdowns,
          by_severity: { items: [], truncated: false },
        },
      },
    };
    mockApi({
      "GET /api/v1/context": salesContext("VALIDATION_COMPLETE"),
      "GET /api/v1/journey": zeroIssueMultiEntityJourney(),
      "GET /api/v1/engagement": {
        ...engagementDetailFixture,
        state: salesContext("VALIDATION_COMPLETE").engagement,
      },
      "GET /api/v1/engagement/source-profile-rules": sourceProfileRulesFixture,
      "GET /api/v1/validation": cleanValidation,
    });
    render(<App />);

    expect(
      await screen.findByRole("heading", {
        name: "Validate the approved mapping",
      }),
    ).toBeVisible();
    expect(
      screen.getByLabelText("Multi-entity validation authority boundary"),
    ).toHaveTextContent("Validation evidence is read-only here");
    expect(
      screen.queryByText("Multi-entity authority stops at First Issues"),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("link", { name: /^Issues\b/ }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: "Compare Available" }),
    ).toHaveAttribute("href", "/comparison");
    expect(
      screen.getByText("This validation completed without issue findings."),
    ).toBeVisible();
  });

  it("refreshes context and journey after a stage-changing command", async () => {
    const user = userEvent.setup();
    let contextReads = 0;
    let journeyReads = 0;
    mockApi({
      "GET /api/v1/context": () => {
        contextReads += 1;
        return contextReads === 1
          ? engagementContextFixture
          : {
              ...engagementContextFixture,
              engagement: engagementStateFixture,
            };
      },
      "GET /api/v1/journey": () => {
        journeyReads += 1;
        return journeyReads === 1 ? initialJourney() : sourceJourney();
      },
      "POST /api/v1/engagement": {
        schemaVersion: SCHEMAS.engagementRecord,
        engagementId: engagementStateFixture.engagementId,
        displayName: engagementStateFixture.displayName,
        createdAt: "2026-07-28T10:00:00Z",
        reopened: false,
      },
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

    await waitFor(() => expect(contextReads).toBeGreaterThanOrEqual(2));
    await waitFor(() => expect(journeyReads).toBeGreaterThanOrEqual(2));
    const rail = screen.getByRole("navigation", {
      name: "Migration journey",
    });
    expect(
      within(rail).getByRole("link", { name: "Source Current" }),
    ).toHaveTextContent("Current");
  });

  it("switches from setup to downstream when refreshed server authority changes", async () => {
    let contextReads = 0;
    let journeyReads = 0;
    mockApi({
      "GET /api/v1/context": () => {
        contextReads += 1;
        return contextReads === 1
          ? {
              ...engagementContextFixture,
              engagement: engagementStateFixture,
            }
          : publicationContextFixture;
      },
      "GET /api/v1/journey": () => {
        journeyReads += 1;
        return journeyReads === 1
          ? sourceJourney()
          : singleEntityDownstreamJourney();
      },
      "GET /api/v1/engagement": engagementDetailFixture,
    });
    render(<App />);

    expect(
      await screen.findByRole("heading", {
        name: "Local migration readiness proof",
      }),
    ).toBeVisible();
    window.dispatchEvent(new Event(AUTHORITY_CHANGED_EVENT));

    expect(
      await screen.findByRole("heading", { name: "Overview" }),
    ).toBeVisible();
    const rail = screen.getByRole("navigation", {
      name: "Migration journey",
    });
    expect(
      within(rail).getByRole("link", { name: "Decisions Complete" }),
    ).toHaveAttribute("href", "/decisions");
    expect(
      within(rail).getByRole("link", { name: "Package Current" }),
    ).toHaveAttribute("href", "/publication");
  });
});
