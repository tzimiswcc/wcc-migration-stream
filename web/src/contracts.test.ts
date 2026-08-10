import { describe, expect, it, vi } from "vitest";
import {
  COMMAND_SCHEMAS,
  OBJECT_WORKSPACE_COMMAND_SCHEMAS,
  SCHEMAS,
  canGovernIssueDecisions,
  decodeApplicationContext,
  decodeContext,
  decodeContract,
  decodeHttpError,
  validateDecisionCommand,
  validateObjectWorkspaceCommand,
} from "./contracts";
import type { ContractError } from "./contracts";
import { requestContract } from "./api";
import {
  contextFixture,
  correctionFixture,
  decisionCommandResultFixture,
  decisionCountsFixture,
  decisionHistoryFixture,
  decisionIssueFixture,
  decisionQueueFixture,
  decisionResultFixture,
  filesFixture,
  governedContextFixture,
  engagementContextFixture,
  inactiveEngagementContextFixture,
  activeObjectWorkspaceFixture,
  initializeRequiredObjectWorkspaceFixture,
  issuesFixture,
  jobFixture,
  journeyFixture,
  objectWorkspaceRecordFixture,
  mappingFixture,
  operationsFixture,
  profileFixture,
  profilingFixture,
  reconciliationFixture,
  rowFixture,
  staleCursorError,
  validationFixture,
} from "./test/fixtures";

const correctionCommandFixture = {
  schema_version: COMMAND_SCHEMAS.corrected,
  validation_run_id: "validation-synthetic-004",
  issue_hash: "2".repeat(64),
  practitioner_identifier: "practitioner-synthetic",
  reason: "Exact synthetic correction.",
  evidence_reference: null,
  expected_active_decision_id: "decision-synthetic-002",
  expected_active_generation: 2,
  acknowledge_not_migration_result_approval: true,
  correction: {
    source_file: "objects/alpha.csv",
    source_ordinal: 2,
    entity_id: "SyntheticObject",
    field_id: "object_key",
    operation: "REPLACE_EFFECTIVE_TEXT",
    value: "SYNTHETIC-124",
    expected_effective_value_hash: "e".repeat(64),
  },
};

describe("contract decoding", () => {
  it("accepts only the exact active or fully locked engagement capability posture", () => {
    expect(decodeApplicationContext(engagementContextFixture)).toBe(
      engagementContextFixture,
    );
    expect(decodeApplicationContext(inactiveEngagementContextFixture)).toBe(
      inactiveEngagementContextFixture,
    );
    expect(() =>
      decodeApplicationContext({
        ...inactiveEngagementContextFixture,
        capabilities: {
          ...inactiveEngagementContextFixture.capabilities,
          sourceIntake: true,
        },
      }),
    ).toThrowError(/startup context is incomplete or grants unsafe authority/i);
  });

  it("accepts the exact expected schema identity", () => {
    expect(decodeContract(jobFixture, SCHEMAS.job)).toBe(jobFixture);
  });

  it.each([
    [SCHEMAS.context, contextFixture],
    [SCHEMAS.journey, journeyFixture],
    [SCHEMAS.objectWorkspace, objectWorkspaceRecordFixture],
    [SCHEMAS.objectWorkspaceStatus, activeObjectWorkspaceFixture],
    [SCHEMAS.job, jobFixture],
    [SCHEMAS.files, filesFixture],
    [SCHEMAS.profiling, profilingFixture],
    [SCHEMAS.profile, profileFixture],
    [SCHEMAS.mapping, mappingFixture],
    [SCHEMAS.validation, validationFixture],
    [SCHEMAS.issues, issuesFixture],
    [SCHEMAS.row, rowFixture],
    [SCHEMAS.reconciliation, reconciliationFixture],
    [SCHEMAS.operations, operationsFixture],
    [SCHEMAS.decisionQueue, decisionQueueFixture],
    [SCHEMAS.decisionCounts, decisionCountsFixture],
    [SCHEMAS.decisionIssue, decisionIssueFixture],
    [SCHEMAS.decisionHistory, decisionHistoryFixture],
    [SCHEMAS.correction, correctionFixture],
    [SCHEMAS.decisionResult, decisionResultFixture],
    [SCHEMAS.decisionCommandResult, decisionCommandResultFixture],
  ])("validates the required shape for %s", (schema, fixture) => {
    expect(decodeContract(fixture, schema)).toBe(fixture);
  });

  it("rejects a schema-labelled response without its required fields", () => {
    expect(() =>
      decodeContract({ schema_version: SCHEMAS.job, data: {} }, SCHEMAS.job),
    ).toThrowError(/required contract shape/);
  });

  it("rejects a journey that grants a locked stage a route", () => {
    expect(() =>
      decodeContract(
        {
          ...journeyFixture,
          stages: journeyFixture.stages.map((stage) =>
            stage.id === "DECISIONS"
              ? {
                  ...stage,
                  available: true,
                  href: "/decisions",
                }
              : stage,
          ),
        },
        SCHEMAS.journey,
      ),
    ).toThrowError(/required contract shape/);
  });

  it("rejects journey navigation that is not bound to its declared current stage", () => {
    expect(() =>
      decodeContract(
        {
          ...journeyFixture,
          currentStageId: "MAPPING",
        },
        SCHEMAS.journey,
      ),
    ).toThrowError(/required contract shape/);
  });

  it("rejects inconsistent object-workspace lifecycle variants", () => {
    const pendingInitialize = {
      schemaVersion: OBJECT_WORKSPACE_COMMAND_SCHEMAS.initialize,
      expectedStateHash: initializeRequiredObjectWorkspaceFixture.stateHash,
      displayName: "Customer readiness workspace",
      migrationObjectNameOrScope: "Customer master readiness",
      creatorAttribution: "Synthetic evaluator",
      idempotencyKey: "initialize-workspace-0001",
    } as const;
    expect(
      decodeContract(
        {
          ...initializeRequiredObjectWorkspaceFixture,
          stateHash: "1".repeat(64),
          unavailableReason:
            "Resume the exact interrupted workspace initialization before continuing.",
          pendingCommand: pendingInitialize,
        },
        SCHEMAS.objectWorkspaceStatus,
      ),
    ).toMatchObject({ pendingCommand: pendingInitialize });
    expect(() =>
      decodeContract(
        {
          ...initializeRequiredObjectWorkspaceFixture,
          requiredAction: "ADOPT",
        },
        SCHEMAS.objectWorkspaceStatus,
      ),
    ).toThrowError(/required contract shape/);
    expect(() =>
      decodeContract(
        {
          ...initializeRequiredObjectWorkspaceFixture,
          pendingCommand: {
            ...pendingInitialize,
            schemaVersion: OBJECT_WORKSPACE_COMMAND_SCHEMAS.adopt,
          },
        },
        SCHEMAS.objectWorkspaceStatus,
      ),
    ).toThrowError(/required contract shape/);
    expect(() =>
      decodeContract(
        {
          ...activeObjectWorkspaceFixture,
          workspace: null,
        },
        SCHEMAS.objectWorkspaceStatus,
      ),
    ).toThrowError(/required contract shape/);
    expect(() =>
      decodeContract(
        {
          ...activeObjectWorkspaceFixture,
          pendingCommand: pendingInitialize,
        },
        SCHEMAS.objectWorkspaceStatus,
      ),
    ).toThrowError(/required contract shape/);
    expect(() =>
      decodeContract(
        {
          ...objectWorkspaceRecordFixture,
          serverPath: "/private/hidden",
        },
        SCHEMAS.objectWorkspace,
      ),
    ).toThrowError(/required contract shape/);
  });

  it("accepts only the exact bounded object-workspace command", () => {
    const command = {
      schemaVersion: OBJECT_WORKSPACE_COMMAND_SCHEMAS.initialize,
      expectedStateHash: initializeRequiredObjectWorkspaceFixture.stateHash,
      displayName: "Customer readiness workspace",
      migrationObjectNameOrScope: "Customer master readiness",
      creatorAttribution: "Synthetic evaluator",
      idempotencyKey: "initialize-workspace-0001",
    };
    expect(validateObjectWorkspaceCommand(command)).toEqual(command);
    expect(() =>
      validateObjectWorkspaceCommand({
        ...command,
        workspacePath: "/private/hidden",
      }),
    ).toThrowError(/outside its safe bounds/);
    expect(() =>
      validateObjectWorkspaceCommand({
        ...command,
        creatorAttribution: ` ${command.creatorAttribution}`,
      }),
    ).toThrowError(/outside its safe bounds/);
    expect(() =>
      validateObjectWorkspaceCommand({
        ...command,
        displayName: "é".repeat(129),
      }),
    ).toThrowError(/outside its safe bounds/);
  });

  it("rejects malformed nested evidence before rendering", () => {
    const malformed = {
      ...filesFixture,
      items: [{ ...filesFixture.items[0], row_count: "2" }],
    };
    expect(() => decodeContract(malformed, SCHEMAS.files)).toThrowError(
      /required contract shape/,
    );
  });

  it("fails closed on a schema mismatch", () => {
    expect(() => decodeContract(jobFixture, SCHEMAS.files)).toThrowError(
      /Expected migration-stream\/read\/file-manifest\/v1/,
    );
  });

  it("requires an explicitly read-only startup context", () => {
    expect(() =>
      decodeContext({ ...contextFixture, read_only: false }),
    ).toThrowError(/not explicitly read-only/);
  });

  it("accepts only the exact governed capability context", () => {
    const context = decodeContext(governedContextFixture);
    expect(canGovernIssueDecisions(context)).toBe(true);
    expect(() =>
      decodeContext({
        ...governedContextFixture,
        capabilities: {
          ...governedContextFixture.capabilities,
          source_mutation: true,
        },
      }),
    ).toThrowError(/unsafe capabilities/);
    expect(() =>
      decodeContext({
        ...governedContextFixture,
        capabilities: {
          ...governedContextFixture.capabilities,
          target_delete: false,
        },
      }),
    ).toThrowError(/unsafe capabilities/);
    expect(() =>
      decodeContext({
        ...governedContextFixture,
        future_authority: false,
      }),
    ).toThrowError(/unsafe capabilities/);
  });

  it.each([
    [
      "queue top level",
      { ...decisionQueueFixture, unexpected: false },
      SCHEMAS.decisionQueue,
    ],
    [
      "queue item",
      {
        ...decisionQueueFixture,
        items: [{ ...decisionQueueFixture.items[0], unexpected: false }],
      },
      SCHEMAS.decisionQueue,
    ],
    [
      "latest attempt",
      {
        ...decisionQueueFixture,
        items: [
          {
            ...decisionQueueFixture.items[0],
            latest_attempt: {
              ...decisionQueueFixture.items[0].latest_attempt,
              unexpected: false,
            },
          },
        ],
      },
      SCHEMAS.decisionQueue,
    ],
    [
      "canonical lineage",
      {
        ...decisionIssueFixture,
        data: {
          ...decisionIssueFixture.data,
          field_context: {
            ...decisionIssueFixture.data.field_context,
            original_lineage: {
              ...decisionIssueFixture.data.field_context.original_lineage,
              unexpected: false,
            },
          },
        },
      },
      SCHEMAS.decisionIssue,
    ],
    [
      "correction detail",
      {
        ...correctionFixture,
        data: { ...correctionFixture.data, unexpected: false },
      },
      SCHEMAS.correction,
    ],
    [
      "decision result",
      {
        ...decisionCommandResultFixture,
        data: { ...decisionCommandResultFixture.data, unexpected: false },
      },
      SCHEMAS.decisionCommandResult,
    ],
  ])(
    "rejects an extra property in the %s response",
    (_label, value, schema) => {
      expect(() => decodeContract(value, schema)).toThrowError(
        /required contract shape/,
      );
    },
  );

  it("rejects decision data outside the closed disposition vocabulary", () => {
    const malformed = {
      ...decisionQueueFixture,
      items: [
        {
          ...decisionQueueFixture.items[0],
          active_decision: {
            ...decisionQueueFixture.items[0].active_decision,
            disposition: "ARBITRARY_OVERRIDE",
          },
        },
      ],
    };
    expect(() => decodeContract(malformed, SCHEMAS.decisionQueue)).toThrowError(
      /required contract shape/,
    );
  });

  it("rejects impossible lineage and pending decision-result authority", () => {
    const sourceLineage = {
      ...decisionIssueFixture,
      data: {
        ...decisionIssueFixture.data,
        field_context: {
          ...decisionIssueFixture.data.field_context,
          original_lineage: {
            ...decisionIssueFixture.data.field_context.original_lineage,
            kind: "SOURCE",
          },
        },
      },
    };
    expect(() => decodeContract(sourceLineage, SCHEMAS.decisionIssue)).toThrow(
      /required contract shape/,
    );
    const pendingResult = {
      ...decisionCommandResultFixture,
      data: {
        ...decisionCommandResultFixture.data,
        lifecycle_outcome: "PENDING_REEVALUATION",
        active: false,
        reevaluation_state: "RUNNING",
      },
    };
    expect(() =>
      decodeContract(pendingResult, SCHEMAS.decisionCommandResult),
    ).toThrow(/required contract shape/);
  });

  it("validates a bounded correction command and rejects extra authority", () => {
    const command = correctionCommandFixture;
    expect(validateDecisionCommand(command)).toBe(command);
    expect(() =>
      validateDecisionCommand({ ...command, publish: true }),
    ).toThrowError(/closed supported vocabulary/);
    expect(() =>
      validateDecisionCommand({
        ...command,
        schema_version: "migration-stream/write/arbitrary/v1",
      }),
    ).toThrowError(/closed supported vocabulary/);
  });

  it("matches Rust UTF-8, trim, identity, and source-path command bounds", () => {
    expect(
      validateDecisionCommand({
        ...correctionCommandFixture,
        practitioner_identifier: "é".repeat(128),
        reason: "é".repeat(2048),
        correction: {
          ...correctionCommandFixture.correction,
          entity_id: "a".repeat(256),
          field_id: "b".repeat(256),
        },
      }),
    ).toBeTruthy();

    for (const invalid of [
      {
        ...correctionCommandFixture,
        practitioner_identifier: " practitioner-synthetic ",
      },
      {
        ...correctionCommandFixture,
        practitioner_identifier: "é".repeat(129),
      },
      { ...correctionCommandFixture, reason: "é".repeat(2049) },
      { ...correctionCommandFixture, reason: "bad\u0085reason" },
      {
        ...correctionCommandFixture,
        correction: {
          ...correctionCommandFixture.correction,
          entity_id: "a".repeat(257),
        },
      },
    ]) {
      expect(() => validateDecisionCommand(invalid)).toThrow();
    }

    for (const source_file of [
      "../alpha.csv",
      "objects/../alpha.csv",
      "objects/ alpha.csv",
      "objects/alpha.csv ",
      "objects/alpha.PART",
      "objects/alpha.ToMbStOnE",
      "/objects/alpha.csv",
      "objects\\alpha.csv",
    ]) {
      expect(() =>
        validateDecisionCommand({
          ...correctionCommandFixture,
          correction: { ...correctionCommandFixture.correction, source_file },
        }),
      ).toThrow();
    }
  });

  it("decodes only the stable safe transport envelope", () => {
    expect(decodeHttpError(staleCursorError)?.error.code).toBe("STALE_CURSOR");
    expect(decodeHttpError({ ...staleCursorError, ok: true })).toBeNull();
  });

  it("refuses a request outside the same-origin v1 API", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    await expect(
      requestContract("https://external.invalid/api/v1/job", SCHEMAS.job),
    ).rejects.toMatchObject({ code: "UNSAFE_REQUEST" });
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("rejects non-JSON success responses", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(() => Promise.resolve(new Response("not json", { status: 200 }))),
    );
    await expect(
      requestContract("/api/v1/job", SCHEMAS.job),
    ).rejects.toMatchObject({
      code: "INVALID_CONTENT_TYPE",
    } satisfies Partial<ContractError>);
  });
});
