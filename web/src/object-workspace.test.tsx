import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";
import App from "./App";
import {
  OBJECT_WORKSPACE_COMMAND_SCHEMAS,
  type Journey,
  type ObjectWorkspaceStatus,
} from "./contracts";
import {
  activeObjectWorkspaceFixture,
  adoptRequiredObjectWorkspaceFixture,
  engagementContextFixture,
  inactiveEngagementContextFixture,
  initializeRequiredObjectWorkspaceFixture,
  journeyFixture,
  objectWorkspaceRecordFixture,
} from "./test/fixtures";
import { mockApi, type CapturedRequest } from "./test/mock-api";

function at(path = "/setup"): void {
  window.history.replaceState(null, "", path);
}

function requiredJourney(workspace: ObjectWorkspaceStatus): Journey {
  const reason = workspace.unavailableReason;
  return {
    ...journeyFixture,
    currentStageId: "SETUP",
    workspace,
    binding: {
      engagementId: null,
      engagementGeneration: null,
      jobId: null,
      sourceHash: null,
      manifestHash: null,
      profilingRunId: null,
      objectPackId: null,
      destinationOrigin: null,
      destinationAuthorityId: null,
      destinationOriginHash: null,
      profileId: null,
      profileVersion: null,
      profileHash: null,
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
    stages: journeyFixture.stages.map((stage) =>
      stage.id === "SETUP"
        ? {
            ...stage,
            status: "CURRENT",
            available: true,
            href: "/setup",
            unavailableReason: null,
          }
        : {
            ...stage,
            status: "LOCKED",
            available: false,
            href: null,
            unavailableReason: reason ?? "Verify this workspace.",
          },
    ) as Journey["stages"],
  };
}

function activeSetupJourney(): Journey {
  const active = requiredJourney(activeObjectWorkspaceFixture);
  return {
    ...active,
    stages: active.stages.map((stage) =>
      stage.id === "SETUP"
        ? stage
        : {
            ...stage,
            unavailableReason:
              stage.id === "SOURCE"
                ? "Start a local engagement before accepting source evidence."
                : "Complete the earlier journey stages first.",
          },
    ),
  };
}

function requestBody(request: CapturedRequest | undefined) {
  expect(request?.body).toEqual(expect.any(String));
  return JSON.parse(request?.body ?? "{}") as Record<string, unknown>;
}

async function completeWorkspaceForm(user: ReturnType<typeof userEvent.setup>) {
  await user.type(
    screen.getByLabelText("Workspace display name"),
    "Customer readiness workspace",
  );
  await user.type(
    screen.getByLabelText("Migration object name or scope"),
    "Customer master readiness",
  );
  await user.type(
    screen.getByLabelText("Creator / operator attribution"),
    "Synthetic evaluator",
  );
}

describe("object workspace gate", () => {
  beforeEach(() => at());

  it("renders only the server-required Initialize action and submits its exact bounded command", async () => {
    const user = userEvent.setup();
    const journey = requiredJourney(initializeRequiredObjectWorkspaceFixture);
    let established = false;
    const { requests } = mockApi({
      "GET /api/v1/context": () =>
        established
          ? engagementContextFixture
          : inactiveEngagementContextFixture,
      "GET /api/v1/journey": () =>
        established ? activeSetupJourney() : journey,
      "POST /api/v1/object-workspace/initialize": () => {
        established = true;
        return objectWorkspaceRecordFixture;
      },
    });
    render(<App />);

    expect(
      await screen.findByRole("heading", {
        name: "Initialize this object workspace",
      }),
    ).toBeVisible();
    expect(
      screen.queryByRole("button", { name: "Adopt existing workspace" }),
    ).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Engagement name")).not.toBeInTheDocument();
    expect(
      screen.queryByText(inactiveEngagementContextFixture.workspaceLabel),
    ).not.toBeInTheDocument();

    await completeWorkspaceForm(user);
    await user.click(
      screen.getByRole("button", { name: "Initialize object workspace" }),
    );

    await waitFor(() =>
      expect(
        requests.some(
          (request) =>
            request.method === "POST" &&
            request.url.pathname === "/api/v1/object-workspace/initialize",
        ),
      ).toBe(true),
    );
    const request = requests.find(
      (candidate) =>
        candidate.method === "POST" &&
        candidate.url.pathname === "/api/v1/object-workspace/initialize",
    );
    const body = requestBody(request);
    expect(body).toEqual({
      schemaVersion: OBJECT_WORKSPACE_COMMAND_SCHEMAS.initialize,
      expectedStateHash: initializeRequiredObjectWorkspaceFixture.stateHash,
      displayName: "Customer readiness workspace",
      migrationObjectNameOrScope: "Customer master readiness",
      creatorAttribution: "Synthetic evaluator",
      idempotencyKey: expect.any(String),
    });
    expect(request?.headers.get("idempotency-key")).toBe(body.idempotencyKey);
    expect(request?.headers.get("x-migration-stream-intent")).toBe(
      "object-workspace-initialize-v1",
    );
    expect(await screen.findByLabelText("Engagement name")).toBeVisible();
    expect(
      within(screen.getByLabelText("Object workspace identity")).getByText(
        `Workspace ID ${objectWorkspaceRecordFixture.workspaceId}`,
      ),
    ).toBeVisible();
  });

  it("renders only the server-required Adopt action without exposing a path", async () => {
    const user = userEvent.setup();
    const journey = requiredJourney(adoptRequiredObjectWorkspaceFixture);
    const { requests } = mockApi({
      "GET /api/v1/context": inactiveEngagementContextFixture,
      "GET /api/v1/journey": journey,
      "POST /api/v1/object-workspace/adopt": {
        ...objectWorkspaceRecordFixture,
        establishedBy: "ADOPTED",
      },
    });
    render(<App />);

    expect(
      await screen.findByRole("heading", {
        name: "Adopt this existing object workspace",
      }),
    ).toBeVisible();
    expect(
      screen.queryByRole("button", { name: "Initialize object workspace" }),
    ).not.toBeInTheDocument();
    expect(document.body).not.toHaveTextContent("/private/");
    expect(document.body).not.toHaveTextContent("state.sqlite3");

    await completeWorkspaceForm(user);
    await user.click(
      screen.getByRole("button", { name: "Adopt existing workspace" }),
    );

    const request = await waitFor(() => {
      const match = requests.find(
        (candidate) =>
          candidate.method === "POST" &&
          candidate.url.pathname === "/api/v1/object-workspace/adopt",
      );
      expect(match).toBeDefined();
      return match as CapturedRequest;
    });
    expect(requestBody(request)).toMatchObject({
      schemaVersion: OBJECT_WORKSPACE_COMMAND_SCHEMAS.adopt,
      expectedStateHash: adoptRequiredObjectWorkspaceFixture.stateHash,
    });
    expect(request.headers.get("x-migration-stream-intent")).toBe(
      "object-workspace-adopt-v1",
    );
  });

  it("shows the exact active identity and then preserves normal engagement creation", async () => {
    mockApi({
      "GET /api/v1/context": engagementContextFixture,
      "GET /api/v1/journey": {
        ...journeyFixture,
        workspace: activeObjectWorkspaceFixture,
      },
    });
    render(<App />);

    const header = await screen.findByLabelText("Object workspace identity");
    expect(
      within(header).getByText("Synthetic readiness workspace"),
    ).toBeVisible();
    expect(
      within(header).getByText(
        `Workspace ID ${objectWorkspaceRecordFixture.workspaceId}`,
      ),
    ).toBeVisible();
    expect(
      screen.getByText(objectWorkspaceRecordFixture.migrationObjectNameOrScope),
    ).toBeVisible();
    expect(
      screen.getByText(objectWorkspaceRecordFixture.creatorAttribution),
    ).toBeVisible();
    expect(
      screen.getByText(objectWorkspaceRecordFixture.descriptorHash),
    ).toBeVisible();
    expect(screen.getByLabelText("Engagement name")).toBeVisible();
    expect(
      screen.getByRole("button", { name: "Create local engagement" }),
    ).toBeVisible();
  });

  it("retains one retry identity when a workspace outcome is uncertain", async () => {
    const user = userEvent.setup();
    const journey = requiredJourney(initializeRequiredObjectWorkspaceFixture);
    const { requests } = mockApi({
      "GET /api/v1/context": inactiveEngagementContextFixture,
      "GET /api/v1/journey": journey,
      "POST /api/v1/object-workspace/initialize": () => {
        throw new TypeError("synthetic network interruption");
      },
    });
    render(<App />);
    await screen.findByLabelText("Workspace display name");
    await completeWorkspaceForm(user);

    const submit = screen.getByRole("button", {
      name: "Initialize object workspace",
    });
    await user.click(submit);
    expect(await screen.findByText("NETWORK_UNCERTAIN")).toBeVisible();
    await user.click(submit);
    await waitFor(() =>
      expect(
        requests.filter(
          (request) =>
            request.method === "POST" &&
            request.url.pathname === "/api/v1/object-workspace/initialize",
        ),
      ).toHaveLength(2),
    );
    const attempts = requests.filter(
      (request) =>
        request.method === "POST" &&
        request.url.pathname === "/api/v1/object-workspace/initialize",
    );
    expect(requestBody(attempts[0]).idempotencyKey).toBe(
      requestBody(attempts[1]).idempotencyKey,
    );
    expect(screen.getByLabelText("Workspace display name")).toHaveValue(
      "Customer readiness workspace",
    );
  });

  it.each([
    {
      label: "initialization",
      status: initializeRequiredObjectWorkspaceFixture,
      schemaVersion: OBJECT_WORKSPACE_COMMAND_SCHEMAS.initialize,
      route: "/api/v1/object-workspace/initialize",
      intent: "object-workspace-initialize-v1",
      heading: "Resume object workspace initialization",
      button: "Resume workspace initialization",
    },
    {
      label: "adoption",
      status: adoptRequiredObjectWorkspaceFixture,
      schemaVersion: OBJECT_WORKSPACE_COMMAND_SCHEMAS.adopt,
      route: "/api/v1/object-workspace/adopt",
      intent: "object-workspace-adopt-v1",
      heading: "Resume object workspace adoption",
      button: "Resume workspace adoption",
    },
  ])(
    "reopens a pending $label as read-only and resubmits the exact original command",
    async ({ status, schemaVersion, route, intent, heading, button }) => {
      const user = userEvent.setup();
      const pendingCommand = {
        schemaVersion,
        expectedStateHash: status.stateHash,
        displayName: "Recovered customer readiness workspace",
        migrationObjectNameOrScope: "Recovered customer master readiness",
        creatorAttribution: "Recovered synthetic evaluator",
        idempotencyKey: `pending-workspace-${status.requiredAction?.toLowerCase()}-0001`,
      };
      const pendingStatus = {
        ...status,
        stateHash: "9".repeat(64),
        unavailableReason: `Resume the exact interrupted workspace ${status.requiredAction?.toLowerCase()} command before continuing.`,
        pendingCommand,
      } satisfies ObjectWorkspaceStatus;
      const { requests } = mockApi({
        "GET /api/v1/context": inactiveEngagementContextFixture,
        "GET /api/v1/journey": requiredJourney(pendingStatus),
        [`POST ${route}`]: {
          ...objectWorkspaceRecordFixture,
          establishedBy:
            status.requiredAction === "INITIALIZE" ? "INITIALIZED" : "ADOPTED",
        },
      });
      render(<App />);

      expect(
        await screen.findByRole("heading", { name: heading }),
      ).toBeVisible();
      for (const [label, value] of [
        ["Workspace display name", pendingCommand.displayName],
        [
          "Migration object name or scope",
          pendingCommand.migrationObjectNameOrScope,
        ],
        ["Creator / operator attribution", pendingCommand.creatorAttribution],
      ] as const) {
        const field = screen.getByLabelText(label);
        expect(field).toHaveValue(value);
        expect(field).toHaveAttribute("readonly");
      }
      expect(screen.getByText("Exact pending command recovered")).toBeVisible();

      await user.click(screen.getByRole("button", { name: button }));
      const request = await waitFor(() => {
        const match = requests.find(
          (candidate) =>
            candidate.method === "POST" && candidate.url.pathname === route,
        );
        expect(match).toBeDefined();
        return match as CapturedRequest;
      });
      expect(requestBody(request)).toEqual(pendingCommand);
      expect(request.headers.get("idempotency-key")).toBe(
        pendingCommand.idempotencyKey,
      );
      expect(request.headers.get("x-migration-stream-intent")).toBe(intent);
    },
  );

  it("fails a later-stage deep link closed before reading its evidence", async () => {
    at("/issues");
    const reason =
      initializeRequiredObjectWorkspaceFixture.unavailableReason ?? "";
    const { requests } = mockApi({
      "GET /api/v1/context": inactiveEngagementContextFixture,
      "GET /api/v1/journey": requiredJourney(
        initializeRequiredObjectWorkspaceFixture,
      ),
    });
    render(<App />);

    expect(
      await screen.findByRole("heading", { name: "Issues is locked" }),
    ).toBeVisible();
    expect(screen.getAllByText(reason).length).toBeGreaterThan(0);
    expect(
      requests.some((request) => request.url.pathname === "/api/v1/issues"),
    ).toBe(false);
  });
});
