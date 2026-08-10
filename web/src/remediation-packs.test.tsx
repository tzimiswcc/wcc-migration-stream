import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import {
  ENGAGEMENT_COMMAND_SCHEMAS,
  REMEDIATION_LOCAL_EXPORT_ACKNOWLEDGEMENT,
  REMEDIATION_OFFENDING_VALUES_ACKNOWLEDGEMENT,
  SCHEMAS,
  decodeContract,
  type RemediationPackOptions,
  type RemediationPackView,
} from "./contracts";
import { RemediationPackPanel } from "./remediation-packs";
import { mockApi, type CapturedRequest } from "./test/mock-api";

const hash = (character: string) => character.repeat(64);

const options: RemediationPackOptions = {
  schemaVersion: SCHEMAS.remediationPackOptions,
  engagementId: "engagement-remediation-001",
  engagementGeneration: 21,
  workspaceId: "workspace-remediation-001",
  workspaceDescriptorHash: hash("1"),
  jobId: "job-remediation-001",
  sourceHash: hash("2"),
  manifestHash: hash("3"),
  profilingRunId: "profiling-remediation-001",
  profilingResultHash: hash("4"),
  profileId: "workspace.customer.destination",
  profileVersion: "1.0.0",
  profileHash: hash("5"),
  profileApprovalId: "profile-approval-remediation-001",
  profileApprovalHash: hash("6"),
  mappingId: "workspace.customer.mapping",
  mappingVersion: "1.0.0",
  mappingHash: hash("7"),
  mappingAuthorityId: "mapping-approval-remediation-001",
  mappingAuthorityHash: hash("8"),
  validationRunId: "validation-remediation-001",
  validationResultHash: hash("9"),
  issueCount: 3,
  optionsHash: hash("a"),
  files: [
    {
      entityId: "Customer",
      sourceFile: "customers.csv",
      columns: [
        {
          sourceColumnOrdinal: 0,
          sourceColumn: "legacy_customer_key",
          suggested: true,
          selected: false,
        },
        {
          sourceColumnOrdinal: 1,
          sourceColumn: "account_group",
          suggested: false,
          selected: false,
        },
      ],
    },
    {
      entityId: "Address",
      sourceFile: "addresses.csv",
      columns: [
        {
          sourceColumnOrdinal: 0,
          sourceColumn: "legacy_address_key",
          suggested: true,
          selected: false,
        },
      ],
    },
  ],
};

const selectedKeyColumns = options.files.map((file) => ({
  entityId: file.entityId,
  sourceFile: file.sourceFile,
  sourceColumnOrdinal: file.columns[0].sourceColumnOrdinal,
  sourceColumn: file.columns[0].sourceColumn,
}));

function pack(includeOffendingValues: boolean): RemediationPackView {
  return {
    packId: includeOffendingValues
      ? "remediation-pack-values-001"
      : "remediation-pack-keys-001",
    state: "COMPLETE",
    engagementId: options.engagementId,
    validationRunId: options.validationRunId,
    sourceHash: options.sourceHash,
    selectedKeyColumns,
    includeOffendingValues,
    actor: "Synthetic practitioner",
    reason: "Prepare a bounded local remediation review.",
    issueCount: options.issueCount,
    memberCount: 3,
    archiveSizeBytes: 4096,
    archiveSha256: hash(includeOffendingValues ? "b" : "c"),
    membershipHash: hash("d"),
    authorityHash: hash("e"),
    downloadToken: hash(includeOffendingValues ? "b" : "c"),
    createdAt: "2026-08-03T08:00:00Z",
    completedAt: "2026-08-03T08:00:01Z",
  };
}

function requestBody(request: CapturedRequest): Record<string, unknown> {
  return JSON.parse(request.body ?? "{}") as Record<string, unknown>;
}

describe("governed local remediation packs", () => {
  it("accepts zero-based source ordinals and rejects broad value leakage", () => {
    expect(decodeContract(options, SCHEMAS.remediationPackOptions)).toEqual(
      options,
    );
    expect(() =>
      decodeContract(
        { ...options, receivedKeyValues: ["CUST-0042"] },
        SCHEMAS.remediationPackOptions,
      ),
    ).toThrowError(/required contract shape/);
    expect(() =>
      decodeContract(
        {
          schemaVersion: SCHEMAS.remediationPackHistory,
          totalCount: 1,
          returnedCount: 1,
          truncated: false,
          items: [{ ...pack(false), offendingValues: ["bad"] }],
        },
        SCHEMAS.remediationPackHistory,
      ),
    ).toThrowError(/required contract shape/);
  });

  it("starts suggestions unchecked and creates a key-only pack with exact authority", async () => {
    const user = userEvent.setup();
    const completed = pack(false);
    const { requests } = mockApi({
      "GET /api/v1/engagement/remediation-packs/options": options,
      "GET /api/v1/engagement/remediation-packs": {
        schemaVersion: SCHEMAS.remediationPackHistory,
        totalCount: 0,
        returnedCount: 0,
        truncated: false,
        items: [],
      },
      "POST /api/v1/engagement/remediation-packs": {
        schemaVersion: SCHEMAS.remediationPack,
        pack: completed,
        reusedAttempt: false,
      },
    });
    render(<RemediationPackPanel />);

    const firstKey = await screen.findByRole("checkbox", {
      name: /legacy_customer_key/i,
    });
    const secondKey = screen.getByRole("checkbox", {
      name: /legacy_address_key/i,
    });
    expect(firstKey).not.toBeChecked();
    expect(secondKey).not.toBeChecked();
    const create = screen.getByRole("button", {
      name: "Create remediation pack",
    });
    expect(create).toBeDisabled();

    await user.click(firstKey);
    await user.click(secondKey);
    await user.type(
      screen.getByLabelText("Exporting practitioner"),
      "Synthetic practitioner",
    );
    await user.type(
      screen.getByLabelText("Local remediation reason"),
      completed.reason,
    );
    await user.click(
      screen.getByRole("checkbox", {
        name: /authorize this local data-bearing export/i,
      }),
    );
    expect(
      screen.getByRole("checkbox", { name: /include exact offending/i }),
    ).not.toBeChecked();
    expect(create).toBeEnabled();
    await user.click(create);

    const request = await waitFor(() => {
      const match = requests.find(
        (item) =>
          item.method === "POST" &&
          item.url.pathname === "/api/v1/engagement/remediation-packs",
      );
      expect(match).toBeDefined();
      if (!match) throw new Error("remediation POST was not captured");
      return match;
    });
    expect(requestBody(request)).toEqual({
      schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.createRemediationPack,
      expectedEngagementGeneration: options.engagementGeneration,
      expectedValidationRunId: options.validationRunId,
      expectedOptionsHash: options.optionsHash,
      selectedKeyColumns,
      actor: completed.actor,
      reason: completed.reason,
      localDataExportAcknowledgement: REMEDIATION_LOCAL_EXPORT_ACKNOWLEDGEMENT,
      includeOffendingValues: false,
      offendingValuesAcknowledgement: null,
      idempotencyKey: expect.any(String),
    });
    expect(request.headers.get("x-migration-stream-intent")).toBe(
      "engagement-remediation-pack-create-v1",
    );
    expect(request.headers.get("idempotency-key")).toBe(
      requestBody(request).idempotencyKey,
    );
    expect(
      await screen.findByRole("link", { name: "Download remediation pack" }),
    ).toHaveAttribute(
      "href",
      `/api/v1/engagement/remediation-packs/${completed.packId}/download`,
    );
    expect(screen.getByText("Not included")).toBeVisible();
    expect(screen.queryByText("CUST-0042")).not.toBeInTheDocument();
  });

  it("discloses when the bounded value-free history omits older packs", async () => {
    mockApi({
      "GET /api/v1/engagement/remediation-packs/options": options,
      "GET /api/v1/engagement/remediation-packs": {
        schemaVersion: SCHEMAS.remediationPackHistory,
        totalCount: 201,
        returnedCount: 1,
        truncated: true,
        items: [pack(false)],
      },
    });
    render(<RemediationPackPanel />);

    expect(
      await screen.findByText(/showing the newest 1 of 201 immutable packs/i),
    ).toBeVisible();
    expect(screen.queryByText("CUST-0042")).not.toBeInTheDocument();
  });

  it("requires a separate acknowledgement before offending values can be authorized", async () => {
    const user = userEvent.setup();
    const completed = pack(true);
    const { requests } = mockApi({
      "GET /api/v1/engagement/remediation-packs/options": options,
      "GET /api/v1/engagement/remediation-packs": {
        schemaVersion: SCHEMAS.remediationPackHistory,
        totalCount: 1,
        returnedCount: 1,
        truncated: false,
        items: [pack(false)],
      },
      "POST /api/v1/engagement/remediation-packs": {
        schemaVersion: SCHEMAS.remediationPack,
        pack: completed,
        reusedAttempt: false,
      },
    });
    render(<RemediationPackPanel />);

    await user.click(
      await screen.findByRole("checkbox", { name: /legacy_customer_key/i }),
    );
    await user.click(
      screen.getByRole("checkbox", { name: /legacy_address_key/i }),
    );
    await user.type(
      screen.getByLabelText("Exporting practitioner"),
      "Synthetic practitioner",
    );
    await user.type(
      screen.getByLabelText("Local remediation reason"),
      completed.reason,
    );
    await user.click(
      screen.getByRole("checkbox", {
        name: /authorize this local data-bearing export/i,
      }),
    );
    await user.click(
      screen.getByRole("checkbox", { name: /include exact offending/i }),
    );
    const create = screen.getByRole("button", {
      name: "Create remediation pack",
    });
    expect(create).toBeDisabled();
    await user.click(
      screen.getByRole("checkbox", {
        name: /separately authorize exact offending/i,
      }),
    );
    expect(create).toBeEnabled();
    await user.click(create);

    const request = await waitFor(() => {
      const match = requests.find(
        (item) =>
          item.method === "POST" &&
          item.url.pathname === "/api/v1/engagement/remediation-packs",
      );
      if (!match) throw new Error("remediation POST was not captured");
      return match;
    });
    expect(requestBody(request)).toMatchObject({
      includeOffendingValues: true,
      offendingValuesAcknowledgement:
        REMEDIATION_OFFENDING_VALUES_ACKNOWLEDGEMENT,
    });
    expect(
      await screen.findByText(
        "Explicitly authorized where exact source-column lineage applies",
      ),
    ).toBeVisible();
    const historyDownload = screen.getByRole("link", { name: "Download ZIP" });
    expect(historyDownload).toHaveAttribute(
      "href",
      "/api/v1/engagement/remediation-packs/remediation-pack-keys-001/download",
    );
  });
});
