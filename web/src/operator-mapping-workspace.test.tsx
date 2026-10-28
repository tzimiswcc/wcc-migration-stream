import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import axe from "axe-core";
import { beforeEach, describe, expect, it } from "vitest";
import App from "./App";
import {
  ENGAGEMENT_COMMAND_SCHEMAS,
  MAPPING_OPERATIONS,
  SCHEMAS,
  type EngagementState,
  type MappingContractCoverage,
  type MappingDraftSnapshot,
  type MappingDraftView,
  type MappingWorkspace,
  type SourceProfileRules,
} from "./contracts";
import {
  engagementApprovedDetailFixture,
  engagementContextFixture,
  engagementMappingFixture,
  engagementValidationCompleteDetailFixture,
  issuesFixture,
  readyPreflightFixture,
  sampleFreeProfilingFixture,
} from "./test/fixtures";
import { mockApi, type CapturedRequest } from "./test/mock-api";

const hash = (character: string) => character.repeat(64);
const profileHash = hash("a");
const mappingHash = hash("b");
const draftHash = hash("d");
const revisedMappingId = "engagement.sales-document.mapping.revised";
const revisedMappingVersion = "1.1.0";

const sourceColumns = {
  "deal_book.csv": [
    "case_ref",
    "party_ref",
    "booked_on",
    "route_code",
    "money_iso",
    "buy_ref",
    "owning_desk",
  ],
  "deal_detail.csv": [
    "case_link",
    "seq_ref",
    "offer_ref",
    "demand_qty",
    "measure",
    "net_rate",
    "need_by",
  ],
} as const;

const fieldDefinitions = {
  SalesDocument: [
    ["sourceDocumentId", "Source document ID", "identifier", true],
    ["customerId", "Customer ID", "identifier", true],
    ["documentDate", "Document date", "date", true],
    ["documentType", "Document type", "code", true],
    ["currency", "Currency", "currency", true],
    ["sellingUnit", "Selling unit", "code", true],
    ["customerReference", "Customer reference", "text", false],
  ],
  SalesDocumentItem: [
    ["parentDocumentId", "Parent document ID", "identifier", true],
    ["sourceItemId", "Source item ID", "identifier", true],
    ["productId", "Product ID", "identifier", true],
    ["quantity", "Quantity", "decimal", true],
    ["unitOfMeasure", "Unit of measure", "code", true],
    ["unitPrice", "Unit price", "decimal", false],
    ["requestedDate", "Requested date", "date", false],
  ],
} as const;

const profileFields = Object.entries(fieldDefinitions).flatMap(
  ([entityId, fields]) =>
    fields.map(([fieldId, label, logicalType, required]) => ({
      entityId,
      fieldId,
      label,
      logicalType,
      required,
      sensitive: false,
    })),
) satisfies SourceProfileRules["profileFields"];

const salesSourceProfileRules = {
  schemaVersion: SCHEMAS.sourceProfileRules,
  jobId: "job-sales-document-001",
  profilingRunId: "profiling-sales-document-001",
  storedSampleValues: false,
  sourceColumns: Object.entries(sourceColumns).flatMap(
    ([sourceFile, columns]) =>
      columns.map((sourceColumn, sourceColumnOrdinal) => ({
        sourceFile,
        sourceColumn,
        sourceColumnOrdinal,
      })),
  ),
  profileFields,
  rules: profileFields
    .filter((field) => field.required)
    .map((field) => ({
      ruleId: `required.${field.entityId}.${field.fieldId}`,
      severity: "error",
      kind: "required",
      target: `${field.entityId}.${field.fieldId}`,
      message: `${field.label} is required.`,
      parameters: {},
    })),
} satisfies SourceProfileRules;

const salesObjectPack = {
  objectPackId: "synthetic.sales-document.readiness",
  version: "1.0.0",
  title: "Basic sales document readiness",
  description:
    "Synthetic target-neutral header and item mapping readiness evidence.",
  scope: "Synthetic local source-to-canonical readiness only",
  rootEntityId: "SalesDocument",
  mappingMode: "OPERATOR_GOVERNED",
  limitations: [
    "No target compatibility claim.",
    "Multi-entity authority stops at First Issues.",
  ],
  mappingSuggestions: [],
  referenceTables: [
    {
      entityId: "SalesDocument",
      tableId: "sales-document-type-domain",
      version: "1.0.0",
      values: { STANDARD: "STANDARD", RETURN: "RETURN" },
    },
    {
      entityId: "SalesDocument",
      tableId: "sales-document-route-map",
      version: "1.0.0",
      values: { N: "STANDARD", R: "RETURN" },
    },
  ],
} as const;

const salesState: EngagementState = {
  ...engagementApprovedDetailFixture.state,
  phase: "AWAITING_MAPPING",
  generation: 7,
  active: {
    ...engagementApprovedDetailFixture.state.active,
    jobId: "job-sales-document-001",
    profilingRunId: "profiling-sales-document-001",
    objectPackId: salesObjectPack.objectPackId,
    destinationOrigin: "CONTROLLED_PACK",
    destinationAuthorityId: salesObjectPack.objectPackId,
    destinationOriginHash: profileHash,
    profileId: salesObjectPack.objectPackId,
    profileVersion: salesObjectPack.version,
    profileHash,
  },
  profileStage: "APPROVED",
  mappingStage: "NOT_PREPARED",
  validationStage: "NOT_LAUNCHED",
};

const salesDetail = {
  ...engagementApprovedDetailFixture,
  state: salesState,
  availableObjectPackCount: 2,
};

const relationship: MappingWorkspace["relationships"][number] = {
  relationshipId: "sales-document.items",
  fromEntityId: "SalesDocument",
  toEntityId: "SalesDocumentItem",
  cardinality: "1..*",
  keyPairs: [
    {
      fromFieldId: "sourceDocumentId",
      toFieldId: "parentDocumentId",
    },
  ],
};

const workspaceBase = {
  schemaVersion: SCHEMAS.mappingWorkspace,
  engagementGeneration: 7,
  jobId: "job-sales-document-001",
  sourceHash: readyPreflightFixture.sourceHash,
  profilingRunId: "profiling-sales-document-001",
  destinationOrigin: "CONTROLLED_PACK",
  objectPackId: salesObjectPack.objectPackId,
  objectPackVersion: salesObjectPack.version,
  profileId: salesObjectPack.objectPackId,
  profileVersion: salesObjectPack.version,
  profileHash,
  mappingMode: "OPERATOR_GOVERNED",
  sourceFiles: Object.entries(sourceColumns).map(([sourceFile, columns]) => ({
    sourceFile,
    columns: columns.map((sourceColumn, sourceColumnOrdinal) => ({
      sourceFile,
      sourceColumn,
      sourceColumnOrdinal,
    })),
  })),
  entities: [
    {
      entityId: "SalesDocument",
      root: true,
      key: ["sourceDocumentId"],
      fields: profileFields.filter(
        (field) => field.entityId === "SalesDocument",
      ),
    },
    {
      entityId: "SalesDocumentItem",
      root: false,
      key: ["parentDocumentId", "sourceItemId"],
      fields: profileFields.filter(
        (field) => field.entityId === "SalesDocumentItem",
      ),
    },
  ],
  relationships: [relationship],
  allowedOperations: [...MAPPING_OPERATIONS],
  controlledReferenceTables: salesObjectPack.referenceTables.map((table) => ({
    ...table,
    values: { ...table.values },
  })),
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

const sourceByTarget = {
  "SalesDocument.sourceDocumentId": ["deal_book.csv", "case_ref", "direct"],
  "SalesDocument.customerId": ["deal_book.csv", "party_ref", "trim"],
  "SalesDocument.documentDate": ["deal_book.csv", "booked_on", "direct"],
  "SalesDocument.documentType": ["deal_book.csv", "route_code", "lookup"],
  "SalesDocument.currency": ["deal_book.csv", "money_iso", "uppercase_ascii"],
  "SalesDocument.sellingUnit": [
    "deal_book.csv",
    "owning_desk",
    "uppercase_ascii",
  ],
  "SalesDocument.customerReference": [
    "deal_book.csv",
    "buy_ref",
    "empty_as_null",
  ],
  "SalesDocumentItem.parentDocumentId": [
    "deal_detail.csv",
    "case_link",
    "direct",
  ],
  "SalesDocumentItem.sourceItemId": ["deal_detail.csv", "seq_ref", "direct"],
  "SalesDocumentItem.productId": ["deal_detail.csv", "offer_ref", "trim"],
  "SalesDocumentItem.quantity": ["deal_detail.csv", "demand_qty", "direct"],
  "SalesDocumentItem.unitOfMeasure": [
    "deal_detail.csv",
    "measure",
    "uppercase_ascii",
  ],
  "SalesDocumentItem.unitPrice": [
    "deal_detail.csv",
    "net_rate",
    "empty_as_null",
  ],
  "SalesDocumentItem.requestedDate": [
    "deal_detail.csv",
    "need_by",
    "empty_as_null",
  ],
} as const;

function exactSnapshot(): MappingDraftSnapshot {
  return {
    sourceEntityAssignments: [
      { sourceFile: "deal_book.csv", entityId: "SalesDocument" },
      { sourceFile: "deal_detail.csv", entityId: "SalesDocumentItem" },
    ],
    bindings: Object.entries(sourceByTarget).map(
      ([qualifiedTarget, [sourceFile, sourceColumn, kind]], index) => {
        const [targetEntityId, targetFieldId] = qualifiedTarget.split(".");
        const acceptedColumns: readonly string[] = sourceColumns[sourceFile];
        const sourceColumnOrdinal = acceptedColumns.indexOf(sourceColumn);
        return {
          bindingId: `operator.binding.${index + 1}`,
          targetEntityId,
          targetFieldId,
          sourceFile,
          sourceColumn,
          sourceColumnOrdinal,
          kind,
          ...(kind === "lookup"
            ? { lookupTableId: "sales-document-route-map" }
            : {}),
        };
      },
    ),
    relationshipBindings: [relationship],
    referenceTables: workspaceBase.controlledReferenceTables.map((table) => ({
      ...table,
      values: { ...table.values },
    })),
    author: "Sales mapping operator",
  };
}

function draft(
  state: MappingDraftView["state"],
  submitted = state !== "EDITABLE",
): MappingDraftView {
  return {
    schemaVersion: SCHEMAS.mappingDraft,
    draftId: "sales-document-draft-001",
    draftGeneration: 3,
    draftHash,
    state,
    snapshot: exactSnapshot(),
    submittedMappingId: submitted ? "engagement.sales-document.mapping" : null,
    submittedMappingVersion: submitted ? "1.0.0" : null,
    submittedMappingHash: submitted ? mappingHash : null,
    updatedAt: "2026-07-28T14:00:00Z",
  };
}

const salesMappingCoverage = {
  ...engagementMappingFixture,
  scope: {
    mapping_id: "engagement.sales-document.mapping",
    mapping_version: "1.0.0",
    mapping_hash: mappingHash,
  },
  mapping: {
    ...engagementMappingFixture.mapping,
    mapping_id: "engagement.sales-document.mapping",
    mapping_version: "1.0.0",
    mapping_hash: mappingHash,
    profile_id: salesObjectPack.objectPackId,
    profile_version: salesObjectPack.version,
    profile_hash: profileHash,
    source_hash: readyPreflightFixture.sourceHash,
    validated_job_id: "job-sales-document-001",
  },
  items: [
    {
      kind: "MAPPED",
      mapping_kind: "DIRECT",
      source_file: "deal_book.csv",
      source_column_ordinal: 0,
      source_column_name: "case_ref",
      entity_id: "SalesDocument",
      field_id: "sourceDocumentId",
      binding_id: "operator.binding.1",
      issue_code: null,
      detail: null,
    },
    {
      kind: "MAPPED",
      mapping_kind: "DIRECT",
      source_file: "deal_detail.csv",
      source_column_ordinal: 0,
      source_column_name: "case_link",
      entity_id: "SalesDocumentItem",
      field_id: "parentDocumentId",
      binding_id: "operator.binding.8",
      issue_code: null,
      detail: null,
    },
  ],
  page: { page_size: 200, returned: 2, next_cursor: null },
  canonical_hash: hash("c"),
};

const mappingContractCoverage = {
  schemaVersion: SCHEMAS.mappingContractCoverage,
  authority: {
    workspaceId: "11111111-1111-4111-8111-111111111111",
    workspaceDescriptorHash: hash("1"),
    engagementId: salesState.engagementId,
    engagementGeneration: salesState.generation,
    jobId: "job-sales-document-001",
    sourceHash: readyPreflightFixture.sourceHash,
    profilingRunId: "profiling-sales-document-001",
    destinationOrigin: "CONTROLLED_PACK",
    profileId: salesObjectPack.objectPackId,
    profileVersion: salesObjectPack.version,
    profileHash,
    profileApprovalId: "profile-approval-001",
    profileApprovalHash: hash("2"),
    draftId: null,
    draftGeneration: null,
    draftHash: null,
    draftState: null,
    mappingId: null,
    mappingVersion: null,
    mappingHash: null,
    mappingApprovalId: null,
    mappingApprovalHash: null,
  },
  overallStatus: "UNRESOLVED",
  summary: { complete: 0, unresolved: 1, warnings: 0, blockers: 0 },
  fields: [],
  sourceColumns: [],
  rules: [],
  tables: [],
  relationships: [],
  coverageHash: hash("3"),
} satisfies MappingContractCoverage;

function at(path = "/setup"): void {
  window.history.replaceState(null, "", path);
}

function operatorContext(state: EngagementState = salesState) {
  return {
    ...engagementContextFixture,
    workspaceLabel: "Sales document local workspace",
    engagement: state,
  };
}

function renderWorkspace(
  workspace: MappingWorkspace,
  overrides: Record<string, unknown> = {},
) {
  return mockApi({
    "GET /api/v1/context": operatorContext(),
    "GET /api/v1/engagement": salesDetail,
    "GET /api/v1/profiling": {
      ...sampleFreeProfilingFixture,
      scope: {
        ...sampleFreeProfilingFixture.scope,
        run_id: "profiling-sales-document-001",
        job_id: "job-sales-document-001",
      },
      items: [],
      page: { page_size: 200, returned: 0, next_cursor: null },
    },
    "GET /api/v1/engagement/object-packs": {
      schemaVersion: SCHEMAS.objectPacks,
      items: [salesObjectPack],
    },
    "GET /api/v1/engagement/source-profile-rules": salesSourceProfileRules,
    "GET /api/v1/engagement/mapping-workspace": workspace,
    "GET /api/v1/engagement/mapping-contract-coverage": mappingContractCoverage,
    "GET /api/v1/engagement/mapping-coverage": salesMappingCoverage,
    ...overrides,
  });
}

function requestBody(request: CapturedRequest | undefined) {
  expect(request?.body).toEqual(expect.any(String));
  return JSON.parse(request?.body ?? "{}") as Record<string, unknown>;
}

function commandRequest(
  requests: CapturedRequest[],
  pathname: string,
): CapturedRequest | undefined {
  return requests.find(
    (request) => request.method === "POST" && request.url.pathname === pathname,
  );
}

describe("operator-governed sales-document Mapping Workspace", () => {
  beforeEach(() => at());

  it("renders unfamiliar accepted identities and sends only one bounded operation per entity-qualified binding", async () => {
    const user = userEvent.setup();
    const { requests } = renderWorkspace(workspaceBase, {
      "POST /api/v1/engagement/mapping-drafts/save": (
        _url: URL,
        request: CapturedRequest,
      ) => {
        const body = requestBody(request);
        return {
          schemaVersion: SCHEMAS.mappingDraft,
          engagementGeneration: 8,
          draft: {
            ...draft("EDITABLE", false),
            draftGeneration: 1,
            snapshot: body.snapshot,
          },
          idempotentReplay: false,
        };
      },
    });
    render(<App />);

    expect(
      await screen.findByRole("heading", { name: "Mapping Workspace" }),
    ).toBeVisible();
    expect(
      screen.getByText(/accepted file and column identities/i),
    ).toBeVisible();
    expect(
      screen.getByText(/One accepted source column may feed more than one/i),
    ).toBeVisible();
    expect(
      screen.getByText(/No sample values, joins, expressions/),
    ).toBeVisible();
    expect(
      screen.getByText(
        /Quantity is evaluated as positive decimal text in the reviewed format/,
      ),
    ).toBeVisible();
    expect(screen.getByText("deal_book.csv")).toBeVisible();
    expect(screen.getByText("deal_detail.csv")).toBeVisible();

    const relationshipKeyOperation = screen.getByLabelText(
      "Operation for SalesDocument.sourceDocumentId",
    );
    expect(
      within(relationshipKeyOperation)
        .getAllByRole("option")
        .map((option) => option.getAttribute("value")),
    ).toEqual(["direct"]);
    expect(
      within(screen.getByLabelText("Operation for SalesDocument.customerId"))
        .getAllByRole("option")
        .map((option) => option.getAttribute("value")),
    ).toEqual([...MAPPING_OPERATIONS]);

    await user.selectOptions(
      screen.getByLabelText("Assign deal_book.csv to entity"),
      "SalesDocument",
    );
    await user.selectOptions(
      screen.getByLabelText("Assign deal_detail.csv to entity"),
      "SalesDocumentItem",
    );

    for (const [qualifiedTarget, [, sourceColumn, kind]] of Object.entries(
      sourceByTarget,
    )) {
      await user.selectOptions(
        screen.getByLabelText(`Source for ${qualifiedTarget}`),
        sourceColumn,
      );
      if (kind !== "direct") {
        await user.selectOptions(
          screen.getByLabelText(`Operation for ${qualifiedTarget}`),
          kind,
        );
      }
    }

    const lookup = screen.getByLabelText(
      "Lookup for SalesDocument.documentType",
    );
    expect(lookup).toHaveValue("");
    expect(lookup).toHaveAttribute("aria-invalid", "true");
    expect(
      within(lookup)
        .getAllByRole("option")
        .map((option) => option.getAttribute("value")),
    ).toEqual(["", "sales-document-type-domain", "sales-document-route-map"]);
    expect(
      screen.getByText(
        /A lookup table is required for SalesDocument.documentType/,
      ),
    ).toBeVisible();
    await user.selectOptions(lookup, "sales-document-route-map");
    expect(lookup).toHaveValue("sales-document-route-map");
    expect(lookup).toHaveAttribute("aria-invalid", "false");

    expect(
      screen.getAllByRole("option", { name: "case_ref" }).length,
    ).toBeGreaterThan(0);
    expect(
      screen.getAllByRole("option", { name: "case_link" }).length,
    ).toBeGreaterThan(0);
    expect(screen.getByText("sales-document.items")).toBeVisible();
    expect(
      screen.getByText(
        /validation enforces parent existence and at least one child/i,
      ),
    ).toBeVisible();
    expect(screen.getByLabelText("Route code N")).toHaveValue("STANDARD");
    expect(screen.getByLabelText("Route code R")).toHaveValue("RETURN");
    expect(
      screen.getByText(
        /N and R are fixed pack-configured source keys.*not discovered.*received data/i,
      ),
    ).toBeVisible();

    await user.click(
      screen.getByRole("checkbox", {
        name: /I confirm the exact profile-declared parent key pair/,
      }),
    );
    await user.type(
      screen.getByLabelText("Mapping author"),
      "Sales mapping operator",
    );
    await user.click(
      screen.getByRole("button", { name: "Save durable draft" }),
    );

    await waitFor(() =>
      expect(
        commandRequest(requests, "/api/v1/engagement/mapping-drafts/save"),
      ).toBeDefined(),
    );
    const save = commandRequest(
      requests,
      "/api/v1/engagement/mapping-drafts/save",
    );
    const body = requestBody(save);
    expect(body).toMatchObject({
      schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.saveMappingDraft,
      expectedEngagementGeneration: 7,
      expectedDraftId: null,
      expectedDraftGeneration: null,
      expectedDraftHash: null,
      snapshot: {
        sourceEntityAssignments: [
          { sourceFile: "deal_book.csv", entityId: "SalesDocument" },
          {
            sourceFile: "deal_detail.csv",
            entityId: "SalesDocumentItem",
          },
        ],
        relationshipBindings: [relationship],
        author: "Sales mapping operator",
      },
    });
    const snapshot = body.snapshot as MappingDraftSnapshot;
    expect(snapshot.bindings).toHaveLength(14);
    expect(
      snapshot.bindings.every((binding) =>
        /^[a-z0-9][a-z0-9.-]*$/.test(binding.bindingId),
      ),
    ).toBe(true);
    expect(
      snapshot.bindings.find(
        (binding) =>
          binding.targetEntityId === "SalesDocument" &&
          binding.targetFieldId === "documentType",
      ),
    ).toMatchObject({
      sourceFile: "deal_book.csv",
      sourceColumn: "route_code",
      kind: "lookup",
      lookupTableId: "sales-document-route-map",
    });
    expect(
      snapshot.bindings.find(
        (binding) =>
          binding.targetEntityId === "SalesDocumentItem" &&
          binding.targetFieldId === "productId",
      ),
    ).toMatchObject({
      sourceFile: "deal_detail.csv",
      sourceColumn: "offer_ref",
      kind: "trim",
    });
    expect(
      snapshot.bindings.filter((binding) =>
        ["sourceDocumentId", "parentDocumentId"].includes(
          binding.targetFieldId,
        ),
      ),
    ).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ kind: "direct" }),
        expect.objectContaining({ kind: "direct" }),
      ]),
    );
    expect(body).not.toHaveProperty("expressions");
    expect(save?.headers.get("x-migration-stream-intent")).toBe(
      "engagement-mapping-draft-save-v1",
    );
    expect(save?.headers.get("idempotency-key")).toBe(body.idempotencyKey);
  });

  it("opens the dedicated workshop, treats root-only relationship coverage as complete, and preserves fixed-value and source-reuse semantics", async () => {
    at("/mapping");
    const user = userEvent.setup();
    const rootFields = profileFields
      .filter((field) => field.entityId === "SalesDocument")
      .slice(0, 3);
    const rootOnlyWorkspace = {
      ...workspaceBase,
      sourceFiles: [workspaceBase.sourceFiles[0]],
      entities: [
        {
          entityId: "SalesDocument",
          root: true,
          key: ["sourceDocumentId"],
          fields: rootFields,
        },
      ],
      relationships: [],
      controlledReferenceTables: [],
      draft: null,
    } satisfies MappingWorkspace;
    const { requests } = renderWorkspace(rootOnlyWorkspace, {
      "POST /api/v1/engagement/mapping-drafts/save": (
        _url: URL,
        request: CapturedRequest,
      ) => ({
        schemaVersion: SCHEMAS.mappingDraft,
        engagementGeneration: 8,
        draft: {
          schemaVersion: SCHEMAS.mappingDraft,
          draftId: "root-only-draft-001",
          draftGeneration: 1,
          draftHash,
          state: "EDITABLE",
          snapshot: requestBody(request).snapshot,
          submittedMappingId: null,
          submittedMappingVersion: null,
          submittedMappingHash: null,
          updatedAt: "2026-07-30T18:00:00Z",
        },
        idempotentReplay: false,
      }),
    });
    render(<App />);

    expect(
      await screen.findByRole("heading", {
        name: "Build the mapping contract",
      }),
    ).toBeVisible();
    expect(
      await screen.findByRole("heading", { name: "Mapping Workspace" }),
    ).toBeVisible();
    expect(
      screen.getByRole("option", {
        name: "Root entity · SalesDocument",
      }),
    ).toBeInTheDocument();
    expect(
      screen.queryByText("Govern route-code reference data"),
    ).not.toBeInTheDocument();
    expect(
      screen.getByText(
        /No relationships are declared by this profile.*complete for this root-only contract/i,
      ),
    ).toBeVisible();
    expect(
      screen.queryByRole("checkbox", {
        name: /profile-declared parent key pair/i,
      }),
    ).not.toBeInTheDocument();

    await user.selectOptions(
      screen.getByLabelText("Assign deal_book.csv to entity"),
      "SalesDocument",
    );
    await user.selectOptions(
      screen.getByLabelText("Source for SalesDocument.sourceDocumentId"),
      "case_ref",
    );
    await user.selectOptions(
      screen.getByLabelText("Source for SalesDocument.customerId"),
      "case_ref",
    );
    await user.selectOptions(
      screen.getByLabelText("Operation for SalesDocument.documentDate"),
      "constant",
    );
    expect(
      screen.getAllByRole("option", { name: "fixed value (no source)" }),
    ).not.toHaveLength(0);
    await user.type(
      screen.getByLabelText("Fixed value for SalesDocument.documentDate"),
      "2026-09-01",
    );

    const saveButton = screen.getByRole("button", {
      name: "Save durable draft",
    });
    expect(saveButton).toBeEnabled();
    await user.click(saveButton);

    const save = await waitFor(() => {
      const request = commandRequest(
        requests,
        "/api/v1/engagement/mapping-drafts/save",
      );
      expect(request).toBeDefined();
      return request;
    });
    const saved = requestBody(save).snapshot as MappingDraftSnapshot;
    const documentId = saved.bindings.find(
      (binding) => binding.targetFieldId === "sourceDocumentId",
    );
    const customerId = saved.bindings.find(
      (binding) => binding.targetFieldId === "customerId",
    );
    expect(documentId).toMatchObject({
      sourceFile: "deal_book.csv",
      sourceColumn: "case_ref",
      sourceColumnOrdinal: 0,
    });
    expect(customerId).toMatchObject({
      sourceFile: "deal_book.csv",
      sourceColumn: "case_ref",
      sourceColumnOrdinal: 0,
    });
    expect(customerId?.bindingId).not.toBe(documentId?.bindingId);
    expect(
      saved.bindings.find(
        (binding) => binding.targetFieldId === "documentDate",
      ),
    ).toEqual(
      expect.objectContaining({
        kind: "constant",
        constant: "2026-09-01",
      }),
    );
    expect(
      saved.bindings.find(
        (binding) => binding.targetFieldId === "documentDate",
      ),
    ).not.toHaveProperty("sourceFile");
    expect(saved.relationshipBindings).toEqual([]);
    expect(await axe.run(document.body)).toMatchObject({ violations: [] });
  });

  it("authors exact workspace-profile tables and keeps server-derived coverage value-free", async () => {
    at("/mapping");
    const user = userEvent.setup();
    const rootFields = profileFields
      .filter((field) => field.entityId === "SalesDocument")
      .slice(0, 3);
    const workspaceProfileState = {
      ...salesState,
      active: {
        ...salesState.active,
        objectPackId: "workspace.customer.destination",
        destinationOrigin: "WORKSPACE_PROFILE",
        destinationAuthorityId: "destination-profile-origin-001",
        destinationOriginHash: hash("4"),
        profileId: "workspace.customer.destination",
        profileVersion: "1.0.0",
        profileHash: hash("5"),
      },
    } satisfies EngagementState;
    const workspaceProfile = {
      ...workspaceBase,
      destinationOrigin: "WORKSPACE_PROFILE",
      objectPackId: "workspace.customer.destination",
      objectPackVersion: "1.0.0",
      profileId: "workspace.customer.destination",
      profileVersion: "1.0.0",
      profileHash: hash("5"),
      sourceFiles: [workspaceBase.sourceFiles[0]],
      entities: [
        {
          entityId: "SalesDocument",
          root: true,
          key: ["sourceDocumentId"],
          fields: rootFields,
        },
      ],
      relationships: [],
      controlledReferenceTables: [],
      referenceTableObligations: [
        {
          entityId: "SalesDocument",
          tableId: "customer-status-domain",
          tableVersion: "1.0.0",
          ruleIds: ["reference-customer-status"],
          targetFields: ["SalesDocument.customerId"],
        },
      ],
      draft: null,
    } satisfies MappingWorkspace;
    const valueFreeCoverage = {
      ...mappingContractCoverage,
      authority: {
        ...mappingContractCoverage.authority,
        destinationOrigin: "WORKSPACE_PROFILE",
        profileId: workspaceProfile.profileId,
        profileVersion: workspaceProfile.profileVersion,
        profileHash: workspaceProfile.profileHash,
      },
      overallStatus: "BLOCKER",
      summary: { complete: 4, unresolved: 0, warnings: 1, blockers: 1 },
      fields: [
        {
          entityId: "SalesDocument",
          fieldId: "customerId",
          bindingId: "operator.salesdocument.customerid",
          operation: "lookup",
          sourceFile: "deal_book.csv",
          sourceColumn: "party_ref",
          sourceColumnOrdinal: 1,
          fixedValueConfigured: false,
          lookupTableId: "customer-status-map",
          lookupTableVersion: "1.0.0",
          ruleIds: ["reference-customer-status"],
          ruleKinds: ["reference_lookup"],
          status: "COMPLETE",
          reasons: [],
        },
      ],
      sourceColumns: [
        {
          sourceFile: "deal_book.csv",
          sourceColumn: "party_ref",
          sourceColumnOrdinal: 1,
          status: "COMPLETE",
          reason: "Used by at least one destination binding.",
        },
        {
          sourceFile: "deal_book.csv",
          sourceColumn: "unused_note",
          sourceColumnOrdinal: 7,
          status: "WARNING",
          reason: "Accepted column is intentionally or currently unused.",
        },
      ],
      rules: [
        {
          ruleId: "reference-customer-status",
          kind: "reference_lookup",
          entityId: "SalesDocument",
          fieldId: "customerId",
          referenceTableId: "customer-status-domain",
          referenceTableVersion: "1.0.0",
          status: "COMPLETE",
          reasons: [],
        },
        {
          ruleId: "customer-name-required-active",
          kind: "conditional_required",
          entityId: "SalesDocument",
          fieldId: "customerId",
          referenceTableId: null,
          referenceTableVersion: null,
          status: "BLOCKER",
          reasons: [
            "The same-entity condition field has no complete durable binding.",
          ],
        },
      ],
      tables: [
        {
          entityId: "SalesDocument",
          tableId: "customer-status-domain",
          tableVersion: "1.0.0",
          entryCount: 1,
          roles: ["REFERENCE_DOMAIN"],
          status: "COMPLETE",
          reasons: [],
        },
      ],
    } satisfies MappingContractCoverage;
    const { requests } = renderWorkspace(workspaceProfile, {
      "GET /api/v1/context": operatorContext(workspaceProfileState),
      "GET /api/v1/engagement": {
        ...salesDetail,
        state: workspaceProfileState,
      },
      "GET /api/v1/engagement/mapping-contract-coverage": valueFreeCoverage,
      "POST /api/v1/engagement/mapping-drafts/save": (
        _url: URL,
        request: CapturedRequest,
      ) => ({
        schemaVersion: SCHEMAS.mappingDraft,
        engagementGeneration: 8,
        draft: {
          schemaVersion: SCHEMAS.mappingDraft,
          draftId: "workspace-profile-draft-001",
          draftGeneration: 1,
          draftHash,
          state: "EDITABLE",
          snapshot: requestBody(request).snapshot,
          submittedMappingId: null,
          submittedMappingVersion: null,
          submittedMappingHash: null,
          updatedAt: "2026-07-30T18:00:00Z",
        },
        idempotentReplay: false,
      }),
    });
    render(<App />);

    expect(
      await screen.findByRole("heading", { name: "Mapping Workspace" }),
    ).toBeVisible();
    expect(
      screen.getByRole("heading", { name: "Exact local reference tables" }),
    ).toBeVisible();
    expect(
      screen.queryByText("Govern route-code reference data"),
    ).not.toBeInTheDocument();
    const coverageCard = (
      await screen.findByText("Value-free contract coverage")
    ).closest<HTMLElement>(".card");
    expect(coverageCard).not.toBeNull();
    const tableCoverage = within(coverageCard!).getByRole("region", {
      name: "Exact reference-table obligations",
    });
    expect(
      within(tableCoverage).getByRole("row", {
        name: /customer-status-domain/,
      }),
    ).toBeVisible();
    expect(
      within(coverageCard!).getByRole("row", {
        name: /unused_note.*WARNING.*intentionally or currently unused/i,
      }),
    ).toBeVisible();
    expect(
      within(coverageCard!).getByRole("row", {
        name: /customer-name-required-active.*BLOCKER.*same-entity condition field/i,
      }),
    ).toBeVisible();
    expect(within(coverageCard!).queryByText("ACTIVE")).not.toBeInTheDocument();
    expect(await axe.run(coverageCard!)).toMatchObject({ violations: [] });

    await user.click(screen.getByRole("button", { name: "Add exact entry" }));
    await user.type(
      screen.getByLabelText("Exact key in customer-status-domain"),
      "ACTIVE",
    );
    await user.type(
      screen.getByLabelText("Exact value in customer-status-domain"),
      "ACTIVE",
    );
    await user.selectOptions(screen.getByLabelText("Entity"), "SalesDocument");
    await user.type(screen.getByLabelText("Table ID"), "customer-status-map");
    await user.type(screen.getByLabelText("Version"), "1.0.0");
    await user.click(screen.getByRole("button", { name: "Add exact table" }));
    const addEntryButtons = screen.getAllByRole("button", {
      name: "Add exact entry",
    });
    await user.click(addEntryButtons[1]);
    await user.type(
      screen.getByLabelText("Exact key in customer-status-map"),
      "A",
    );
    await user.type(
      screen.getByLabelText("Exact value in customer-status-map"),
      "ACTIVE",
    );

    await user.selectOptions(
      screen.getByLabelText("Assign deal_book.csv to entity"),
      "SalesDocument",
    );
    await user.selectOptions(
      screen.getByLabelText("Source for SalesDocument.sourceDocumentId"),
      "case_ref",
    );
    await user.selectOptions(
      screen.getByLabelText("Source for SalesDocument.customerId"),
      "party_ref",
    );
    await user.selectOptions(
      screen.getByLabelText("Operation for SalesDocument.customerId"),
      "lookup",
    );
    await user.selectOptions(
      screen.getByLabelText("Lookup for SalesDocument.customerId"),
      "customer-status-map",
    );
    await user.selectOptions(
      screen.getByLabelText("Operation for SalesDocument.documentDate"),
      "constant",
    );
    await user.type(
      screen.getByLabelText("Fixed value for SalesDocument.documentDate"),
      "2026-09-01",
    );
    await user.click(
      screen.getByRole("button", { name: "Save durable draft" }),
    );

    const save = await waitFor(() => {
      const request = commandRequest(
        requests,
        "/api/v1/engagement/mapping-drafts/save",
      );
      expect(request).toBeDefined();
      return request;
    });
    const saved = requestBody(save).snapshot as MappingDraftSnapshot;
    expect(saved.referenceTables).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          entityId: "SalesDocument",
          tableId: "customer-status-domain",
          version: "1.0.0",
          values: { ACTIVE: "ACTIVE" },
        }),
        expect.objectContaining({
          entityId: "SalesDocument",
          tableId: "customer-status-map",
          version: "1.0.0",
          values: { A: "ACTIVE" },
        }),
      ]),
    );
  });

  it("fails closed on a missing or stale-invalid controlled lookup selection", async () => {
    const invalidSnapshot = exactSnapshot();
    const documentType = invalidSnapshot.bindings.find(
      (binding) =>
        binding.targetEntityId === "SalesDocument" &&
        binding.targetFieldId === "documentType",
    );
    expect(documentType).toBeDefined();
    if (documentType) {
      documentType.lookupTableId = "not-in-the-reviewed-pack";
    }
    const invalidDraft = {
      ...draft("EDITABLE", false),
      snapshot: invalidSnapshot,
    } satisfies MappingDraftView;
    const workspace = {
      ...workspaceBase,
      engagementGeneration: 10,
      draft: invalidDraft,
    } satisfies MappingWorkspace;
    const { requests } = renderWorkspace(workspace);
    render(<App />);

    expect(
      await screen.findByRole("heading", { name: "Mapping Workspace" }),
    ).toBeVisible();
    const lookup = screen.getByLabelText(
      "Lookup for SalesDocument.documentType",
    );
    expect(lookup).toHaveValue("");
    expect(lookup).toHaveAttribute("aria-invalid", "true");
    expect(
      screen.getByText(
        /Missing or unknown table identities cannot be saved or frozen/,
      ),
    ).toBeVisible();
    expect(
      screen.getByRole("button", { name: "Save durable draft" }),
    ).toBeDisabled();
    expect(
      screen.getByRole("button", {
        name: "Submit and freeze this generation",
      }),
    ).toBeDisabled();

    await userEvent.setup().selectOptions(lookup, "sales-document-route-map");
    expect(lookup).toHaveValue("sales-document-route-map");
    expect(lookup).toHaveAttribute("aria-invalid", "false");
    expect(
      screen.getByRole("button", { name: "Save durable draft" }),
    ).toBeEnabled();
    expect(
      commandRequest(requests, "/api/v1/engagement/mapping-drafts/save"),
    ).toBeUndefined();
  });

  it("reopens the durable exact generation and submits its immutable identity", async () => {
    const editableDraft = draft("EDITABLE", false);
    const workspace = {
      ...workspaceBase,
      engagementGeneration: 10,
      draft: editableDraft,
    } satisfies MappingWorkspace;
    const { requests } = renderWorkspace(workspace, {
      "POST /api/v1/engagement/mapping-drafts/submit": {
        schemaVersion: SCHEMAS.submittedMapping,
        engagementGeneration: 11,
        draft: {
          ...draft("SUBMITTED"),
          submittedMappingId: revisedMappingId,
          submittedMappingVersion: revisedMappingVersion,
        },
        preparationId: "sales-document-preparation-001",
        mappingHash,
        coverage: salesMappingCoverage,
        idempotentReplay: false,
      },
    });
    render(<App />);

    expect(
      await screen.findByRole("heading", { name: "Mapping Workspace" }),
    ).toBeVisible();
    expect(screen.getByLabelText("Mapping author")).toHaveValue(
      "Sales mapping operator",
    );
    expect(screen.getByLabelText("Assign deal_book.csv to entity")).toHaveValue(
      "SalesDocument",
    );
    expect(
      screen.getByLabelText("Assign deal_detail.csv to entity"),
    ).toHaveValue("SalesDocumentItem");
    expect(
      screen.getByLabelText("Source for SalesDocument.sourceDocumentId"),
    ).toHaveDisplayValue("case_ref");
    expect(
      screen.getByLabelText("Source for SalesDocumentItem.parentDocumentId"),
    ).toHaveDisplayValue("case_link");
    expect(screen.getByText(draftHash)).toBeVisible();

    const user = userEvent.setup();
    expect(screen.getByLabelText("New mapping version")).toHaveValue("1.0.0");
    await user.type(
      screen.getByLabelText("New mapping identity"),
      revisedMappingId,
    );
    await user.clear(screen.getByLabelText("New mapping version"));
    await user.type(
      screen.getByLabelText("New mapping version"),
      revisedMappingVersion,
    );
    await user.click(
      screen.getByRole("button", {
        name: "Submit and freeze this generation",
      }),
    );
    await waitFor(() =>
      expect(
        commandRequest(requests, "/api/v1/engagement/mapping-drafts/submit"),
      ).toBeDefined(),
    );
    const submit = commandRequest(
      requests,
      "/api/v1/engagement/mapping-drafts/submit",
    );
    const body = requestBody(submit);
    expect(body).toEqual({
      schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.submitMappingDraft,
      idempotencyKey: expect.any(String),
      expectedEngagementGeneration: 10,
      draftId: editableDraft.draftId,
      expectedDraftGeneration: editableDraft.draftGeneration,
      expectedDraftHash: draftHash,
      mappingId: revisedMappingId,
      version: revisedMappingVersion,
      acknowledgement:
        "migration-stream/operator-governed-mapping-reviewed-and-frozen/v1",
    });
    expect(submit?.headers.get("x-migration-stream-intent")).toBe(
      "engagement-mapping-draft-submit-v1",
    );
  });

  it("rehydrates a sparse carry-forward draft, resolves its new target, and submits a new explicit identity", async () => {
    const user = userEvent.setup();
    const rootFields = [
      ...profileFields
        .filter((field) => field.entityId === "SalesDocument")
        .map((field) => ({ ...field, entityId: "salesOrder" })),
      {
        entityId: "salesOrder",
        fieldId: "extensionReference",
        label: "Extension reference",
        logicalType: "text",
        required: false,
        sensitive: false,
      },
    ] satisfies SourceProfileRules["profileFields"];
    const itemFields = profileFields
      .filter((field) => field.entityId === "SalesDocumentItem")
      .slice(0, 3)
      .map((field) => ({ ...field, entityId: "salesOrderItem" }));
    const carryRelationship: MappingWorkspace["relationships"][number] = {
      relationshipId: "sales-order.items",
      fromEntityId: "salesOrder",
      toEntityId: "salesOrderItem",
      cardinality: "1..*",
      keyPairs: [
        {
          fromFieldId: "sourceDocumentId",
          toFieldId: "parentDocumentId",
        },
      ],
    };
    const clonedBindings = [
      ...rootFields
        .filter((field) => field.fieldId !== "extensionReference")
        .map((field, index) => ({
          bindingId:
            index === 0
              ? "operator.binding.0.7"
              : `carried.binding.root.${index}`,
          targetEntityId: "salesOrder",
          targetFieldId: field.fieldId,
          sourceFile: "deal_book.csv",
          sourceColumn: sourceColumns["deal_book.csv"][index],
          sourceColumnOrdinal: index,
          kind: "direct" as const,
        })),
      ...itemFields.map((field, index) => ({
        bindingId: `carried.binding.item.${index}`,
        targetEntityId: "salesOrderItem",
        targetFieldId: field.fieldId,
        sourceFile: "deal_detail.csv",
        sourceColumn: sourceColumns["deal_detail.csv"][index],
        sourceColumnOrdinal: index,
        kind: "direct" as const,
      })),
    ];
    expect(clonedBindings).toHaveLength(10);
    const carriedSnapshot: MappingDraftSnapshot = {
      sourceEntityAssignments: [
        { sourceFile: "deal_book.csv", entityId: "salesOrder" },
        { sourceFile: "deal_detail.csv", entityId: "salesOrderItem" },
      ],
      bindings: clonedBindings,
      relationshipBindings: [carryRelationship],
      referenceTables: [],
      author: "Carry-forward reviewer",
    };
    let phase: "carried" | "saved" | "submitted" | "approved" = "carried";
    let savedSnapshot = carriedSnapshot;
    const carriedDraft = (): MappingDraftView => ({
      schemaVersion: SCHEMAS.mappingDraft,
      draftId: "workspace-sales-order-carry-draft",
      draftGeneration: phase === "carried" ? 1 : 2,
      draftHash: phase === "carried" ? hash("e") : hash("f"),
      state:
        phase === "submitted"
          ? "SUBMITTED"
          : phase === "approved"
            ? "APPROVED"
            : "EDITABLE",
      snapshot: savedSnapshot,
      submittedMappingId:
        phase === "submitted" || phase === "approved"
          ? "workspace.sales-order.mapping.v2"
          : null,
      submittedMappingVersion:
        phase === "submitted" || phase === "approved" ? "1.1.0" : null,
      submittedMappingHash:
        phase === "submitted" || phase === "approved" ? hash("6") : null,
      updatedAt: "2026-07-30T12:00:00Z",
    });
    const carriedWorkspace = (): MappingWorkspace => ({
      ...workspaceBase,
      engagementGeneration:
        phase === "carried" ? 20 : phase === "saved" ? 21 : 22,
      objectPackId: "workspace.sales-order",
      objectPackVersion: "1.1.0",
      profileId: "workspace.sales-order",
      profileVersion: "1.1.0",
      profileHash: hash("5"),
      entities: [
        {
          entityId: "salesOrder",
          root: true,
          key: ["sourceDocumentId"],
          fields: rootFields,
        },
        {
          entityId: "salesOrderItem",
          root: false,
          key: ["parentDocumentId", "sourceItemId"],
          fields: itemFields,
        },
      ],
      relationships: [carryRelationship],
      controlledReferenceTables: [],
      draft: carriedDraft(),
    });
    const { requests } = renderWorkspace(carriedWorkspace(), {
      "GET /api/v1/engagement/mapping-workspace": () => carriedWorkspace(),
      "POST /api/v1/engagement/mapping-drafts/save": (
        _url: URL,
        request: CapturedRequest,
      ) => {
        savedSnapshot = requestBody(request).snapshot as MappingDraftSnapshot;
        phase = "saved";
        return {
          schemaVersion: SCHEMAS.mappingDraft,
          engagementGeneration: 21,
          draft: carriedDraft(),
          idempotentReplay: false,
        };
      },
      "POST /api/v1/engagement/mapping-drafts/submit": () => {
        phase = "submitted";
        return {
          schemaVersion: SCHEMAS.submittedMapping,
          engagementGeneration: 22,
          draft: carriedDraft(),
          preparationId: "workspace-sales-order-preparation",
          mappingHash: hash("6"),
          coverage: salesMappingCoverage,
          idempotentReplay: false,
        };
      },
      "POST /api/v1/engagement/mappings/approve": () => {
        phase = "approved";
        return {
          schemaVersion: SCHEMAS.mappingApproval,
          engagementGeneration: 23,
          approvalId: "workspace-sales-order-mapping-approval",
          approvalHash: hash("7"),
          mappingId: "workspace.sales-order.mapping.v2",
          mappingVersion: "1.1.0",
          mappingHash: hash("6"),
          actor: "Mapping reviewer",
          reason: "Approve the carried mapping with its resolved extension.",
          approvedAt: "2026-07-30T12:05:00Z",
          idempotentReplay: false,
        };
      },
    });
    render(<App />);

    const extensionSource = await screen.findByLabelText(
      "Source for salesOrder.extensionReference",
    );
    expect(extensionSource).toHaveValue("");
    expect(extensionSource.closest("tr")).toHaveTextContent("unresolved");
    expect(screen.getByLabelText("New mapping version")).toHaveValue("1.1.0");
    expect(
      screen.getByText(
        /Bind and durably save salesOrder\.extensionReference before submitting/,
      ),
    ).toBeVisible();
    expect(
      screen.getByRole("button", { name: "Save durable draft" }),
    ).toBeDisabled();

    await user.selectOptions(extensionSource, "buy_ref");
    await user.selectOptions(
      screen.getByLabelText("Operation for salesOrder.extensionReference"),
      "trim",
    );
    expect(extensionSource.closest("tr")).not.toHaveTextContent("unresolved");
    await user.click(
      screen.getByRole("button", { name: "Save durable draft" }),
    );

    const save = await waitFor(() => {
      const request = commandRequest(
        requests,
        "/api/v1/engagement/mapping-drafts/save",
      );
      expect(request).toBeDefined();
      return request;
    });
    const saved = requestBody(save).snapshot as MappingDraftSnapshot;
    expect(saved.bindings).toHaveLength(11);
    expect(saved.bindings.slice(0, 10)).toEqual(clonedBindings);
    expect(
      new Set(saved.bindings.map((binding) => binding.bindingId)).size,
    ).toBe(11);
    expect(
      saved.bindings.every((binding) =>
        /^[a-z0-9][a-z0-9.-]{0,127}$/.test(binding.bindingId),
      ),
    ).toBe(true);
    expect(
      saved.bindings.find(
        (binding) =>
          binding.targetEntityId === "salesOrder" &&
          binding.targetFieldId === "extensionReference",
      ),
    ).toEqual({
      bindingId: "operator.binding.0.7.2",
      targetEntityId: "salesOrder",
      targetFieldId: "extensionReference",
      sourceFile: "deal_book.csv",
      sourceColumn: "buy_ref",
      sourceColumnOrdinal: 5,
      kind: "trim",
    });

    expect(await screen.findByText(hash("f"))).toBeVisible();
    await user.type(
      screen.getByLabelText("New mapping identity"),
      "workspace.sales-order.mapping.v2",
    );
    expect(screen.getByLabelText("New mapping version")).toHaveValue("1.1.0");
    await user.click(
      screen.getByRole("button", {
        name: "Submit and freeze this generation",
      }),
    );
    const submit = await waitFor(() => {
      const request = commandRequest(
        requests,
        "/api/v1/engagement/mapping-drafts/submit",
      );
      expect(request).toBeDefined();
      return request;
    });
    expect(requestBody(submit)).toMatchObject({
      expectedEngagementGeneration: 21,
      draftId: "workspace-sales-order-carry-draft",
      expectedDraftGeneration: 2,
      expectedDraftHash: hash("f"),
      mappingId: "workspace.sales-order.mapping.v2",
      version: "1.1.0",
    });

    await user.type(
      await screen.findByLabelText("Mapping approver"),
      "Mapping reviewer",
    );
    await user.type(
      screen.getByLabelText("Approval reason"),
      "Approve the carried mapping with its resolved extension.",
    );
    await user.click(
      screen.getByRole("checkbox", {
        name: /I reviewed and approve the exact immutable mapping hash/,
      }),
    );
    await user.click(
      screen.getByRole("button", { name: "Approve exact mapping hash" }),
    );
    const approve = await waitFor(() => {
      const request = commandRequest(
        requests,
        "/api/v1/engagement/mappings/approve",
      );
      expect(request).toBeDefined();
      return request;
    });
    expect(requestBody(approve)).toMatchObject({
      expectedEngagementGeneration: 22,
      expectedMappingId: "workspace.sales-order.mapping.v2",
      expectedMappingVersion: "1.1.0",
      expectedMappingHash: hash("6"),
      actor: "Mapping reviewer",
    });
    expect(
      await screen.findByText("Exact mapping hash approved"),
    ).toBeVisible();
  });

  it("cannot freeze a durable generation while visible mapping changes are unsaved", async () => {
    const workspace = {
      ...workspaceBase,
      engagementGeneration: 10,
      draft: draft("EDITABLE", false),
    } satisfies MappingWorkspace;
    const { requests } = renderWorkspace(workspace);
    render(<App />);

    expect(
      await screen.findByRole("heading", { name: "Mapping Workspace" }),
    ).toBeVisible();
    const user = userEvent.setup();
    const submit = screen.getByRole("button", {
      name: "Submit and freeze this generation",
    });
    const author = screen.getByLabelText("Mapping author");
    const relationshipConfirmation = screen.getByRole("checkbox", {
      name: /I confirm the exact profile-declared parent key pair/,
    });

    await user.type(
      screen.getByLabelText("New mapping identity"),
      revisedMappingId,
    );
    expect(submit).toBeEnabled();
    await user.type(author, " changed");
    expect(submit).toBeDisabled();
    expect(
      screen.getByText(
        "Save the visible mapping changes as a new durable generation before submitting.",
      ),
    ).toHaveAttribute("role", "status");

    await user.clear(author);
    await user.type(author, "Sales mapping operator");
    expect(submit).toBeEnabled();

    await user.click(relationshipConfirmation);
    expect(submit).toBeDisabled();
    await user.click(relationshipConfirmation);
    expect(submit).toBeEnabled();

    await user.selectOptions(screen.getByLabelText("Route code N"), "RETURN");
    expect(submit).toBeDisabled();
    expect(
      commandRequest(requests, "/api/v1/engagement/mapping-drafts/submit"),
    ).toBeUndefined();
  });

  it("allows a revision-scoped draft save but keeps freeze and activation unavailable", async () => {
    const unavailableReason =
      "The refreshed source changed structure. Save a reviewed mapping draft here; the pending revision cannot activate it automatically.";
    const workspace = {
      ...workspaceBase,
      engagementGeneration: 18,
      profilingRunId: "profiling-revision-2-candidate",
      revisionDraftScope: {
        tourAttemptId: "tour-attempt-revision-2",
        tourNumber: 2,
        baselineTourId: "tour-1-immutable",
        profilingRunId: "profiling-revision-2-candidate",
        submissionAvailable: false,
        unavailableReason,
      },
      draft: draft("EDITABLE", false),
    } satisfies MappingWorkspace;
    const { requests } = renderWorkspace(workspace);
    render(<App />);

    expect(await screen.findByText("Revision 2 mapping review")).toBeVisible();
    expect(screen.getAllByText(unavailableReason)).toHaveLength(2);
    expect(
      screen.getByText(new RegExp(workspace.profilingRunId)),
    ).toBeVisible();
    expect(
      screen.getByRole("button", { name: "Save durable draft" }),
    ).toBeEnabled();

    await userEvent
      .setup()
      .type(
        screen.getByLabelText("New mapping identity"),
        "workspace.revision.mapping",
      );
    const submit = screen.getByRole("button", {
      name: "Submit and freeze this generation",
    });
    expect(submit).toBeDisabled();
    await userEvent.setup().click(submit);
    expect(
      commandRequest(requests, "/api/v1/engagement/mapping-drafts/submit"),
    ).toBeUndefined();
  });

  it("requires attribution before approving the exact submitted mapping hash", async () => {
    const submittedDraft = draft("SUBMITTED");
    const workspace = {
      ...workspaceBase,
      engagementGeneration: 11,
      draft: submittedDraft,
    } satisfies MappingWorkspace;
    const { requests } = renderWorkspace(workspace, {
      "POST /api/v1/engagement/mappings/approve": {
        schemaVersion: SCHEMAS.mappingApproval,
        engagementGeneration: 12,
        approvalId: "sales-document-mapping-approval-001",
        approvalHash: hash("a"),
        mappingId: "engagement.sales-document.mapping",
        mappingVersion: "1.0.0",
        mappingHash,
        actor: "Sales mapping reviewer",
        reason: "Reviewed the exact entity-qualified mapping.",
        approvedAt: "2026-07-28T14:05:00Z",
        idempotentReplay: false,
      },
    });
    render(<App />);

    expect(
      await screen.findByRole("heading", {
        name: "Approve this exact mapping hash",
      }),
    ).toBeVisible();
    expect(screen.getByText(mappingHash)).toBeVisible();
    expect(
      await screen.findByText(
        /Structural mapping coverage reports no blocking binding or relationship-key findings/,
      ),
    ).toBeVisible();
    expect(
      screen.getByText(
        /This evidence checks mapping structure only.*Received-value checks run when validation is launched/,
      ),
    ).toBeVisible();
    expect(
      screen.getByRole("button", { name: "Approve exact mapping hash" }),
    ).toBeDisabled();

    const user = userEvent.setup();
    await user.type(
      screen.getByLabelText("Mapping approver"),
      "Sales mapping reviewer",
    );
    await user.type(
      screen.getByLabelText("Approval reason"),
      "Reviewed the exact entity-qualified mapping.",
    );
    await user.click(
      screen.getByRole("checkbox", {
        name: /I reviewed and approve the exact immutable mapping hash/,
      }),
    );
    await user.click(
      screen.getByRole("button", { name: "Approve exact mapping hash" }),
    );

    await waitFor(() =>
      expect(
        commandRequest(requests, "/api/v1/engagement/mappings/approve"),
      ).toBeDefined(),
    );
    const approve = commandRequest(
      requests,
      "/api/v1/engagement/mappings/approve",
    );
    const body = requestBody(approve);
    expect(body).toEqual({
      schemaVersion: ENGAGEMENT_COMMAND_SCHEMAS.approveMappingVersion,
      idempotencyKey: expect.any(String),
      expectedEngagementGeneration: 11,
      draftId: submittedDraft.draftId,
      expectedDraftGeneration: submittedDraft.draftGeneration,
      expectedDraftHash: draftHash,
      expectedMappingId: "engagement.sales-document.mapping",
      expectedMappingVersion: "1.0.0",
      expectedMappingHash: mappingHash,
      actor: "Sales mapping reviewer",
      reason: "Reviewed the exact entity-qualified mapping.",
      acknowledgement: "migration-stream/local-mapping-version-approval/v1",
    });
    expect(approve?.headers.get("x-migration-stream-intent")).toBe(
      "engagement-mapping-version-approve-v1",
    );
  });

  it("stops completed multi-entity authority at read-only First Issues", async () => {
    at("/issues");
    const completedState: EngagementState = {
      ...engagementValidationCompleteDetailFixture.state,
      phase: "FIRST_ISSUES_READY",
      active: {
        ...engagementValidationCompleteDetailFixture.state.active,
        objectPackId: salesObjectPack.objectPackId,
        destinationOrigin: "CONTROLLED_PACK",
        destinationAuthorityId: salesObjectPack.objectPackId,
        destinationOriginHash: profileHash,
        profileId: salesObjectPack.objectPackId,
        profileVersion: salesObjectPack.version,
        profileHash,
        mappingId: "engagement.sales-document.mapping",
        mappingVersion: "1.0.0",
        mappingHash,
        validationRunId: "validation-sales-document-001",
      },
      profileStage: "APPROVED",
      mappingStage: "VALIDATED",
      validationStage: "COMPLETE",
    };
    mockApi({
      "GET /api/v1/context": operatorContext(completedState),
      "GET /api/v1/issues": issuesFixture,
    });
    render(<App />);

    const boundary = await screen.findByRole("complementary", {
      name: "Multi-entity First Issues authority boundary",
    });
    expect(
      within(boundary).getByText(
        "Multi-entity authority stops at First Issues",
      ),
    ).toBeVisible();
    expect(boundary).toHaveTextContent(
      "Header, item, and relationship findings",
    );
    for (const forbiddenAuthority of [
      "Governed decisions",
      "corrections",
      "reconciliation",
      "result approval",
      "publication",
      "external transmission",
      "source mutation",
      "mapping mutation",
      "target-system actions",
    ]) {
      expect(boundary).toHaveTextContent(forbiddenAuthority);
    }
    expect(screen.getByRole("link", { name: "Issues Current" })).toBeVisible();
    expect(
      screen.queryByRole("link", { name: /^Decisions\b/ }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("link", { name: "Readiness Packages" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("link", { name: /^Reconciliation\b/ }),
    ).not.toBeInTheDocument();
  });
});
