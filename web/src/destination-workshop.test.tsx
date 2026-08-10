import {
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";
import App from "./App";
import {
  DESTINATION_ACKNOWLEDGEMENTS,
  DESTINATION_COMMAND_SCHEMAS,
  SCHEMAS,
  decodeContract,
  validateDestinationCommand,
  type ActiveDestinationView,
  type ApprovedDestinationProfileView,
  type DestinationArtifactView,
  type DestinationCatalogueEntityPage,
  type DestinationCataloguePropertyPage,
  type DestinationCompatibilityReport,
  type DestinationDraftSnapshot,
  type DestinationDraftView,
  type DestinationProfileVersionView,
  type DestinationWorkshop,
  type FrozenDestinationProfileView,
  type Journey,
} from "./contracts";
import {
  activeObjectWorkspaceFixture,
  engagementContextFixture,
  engagementStateFixture,
  governedContextFixture,
  journeyFixture,
} from "./test/fixtures";
import { errorResponse, mockApi, type CapturedRequest } from "./test/mock-api";

const hash = (character: string) => character.repeat(64);

function destinationJourney(): Journey {
  return {
    ...journeyFixture,
    currentStageId: "DESTINATION",
    binding: {
      ...journeyFixture.binding,
      engagementId: engagementStateFixture.engagementId,
      engagementGeneration: engagementStateFixture.generation,
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
      entityCount: null,
    },
    stages: journeyFixture.stages.map((stage) => {
      if (stage.id === "SETUP") {
        return {
          ...stage,
          status: "COMPLETED",
          available: true,
          href: "/setup",
          unavailableReason: null,
        };
      }
      if (stage.id === "DESTINATION") {
        return {
          ...stage,
          status: "CURRENT",
          available: true,
          href: "/destination",
          unavailableReason: null,
        };
      }
      if (stage.id === "SOURCE") {
        return {
          ...stage,
          status: "AVAILABLE",
          available: true,
          href: "/setup",
          unavailableReason: null,
        };
      }
      return {
        ...stage,
        status: "LOCKED",
        available: false,
        href: null,
        unavailableReason: "Complete both source and destination authority.",
      };
    }) as Journey["stages"],
  };
}

const limits: DestinationWorkshop["limits"] = {
  maximumEdmxBytes: 1_000_000,
  maximumEntities: 8,
  maximumFields: 256,
  maximumRelationships: 32,
  maximumRules: 128,
  maximumPageSize: 100,
};

const emptyWorkshop = {
  schemaVersion: SCHEMAS.destinationWorkshop,
  workspaceId: activeObjectWorkspaceFixture.workspace.workspaceId,
  workspaceDescriptorHash:
    activeObjectWorkspaceFixture.workspace.descriptorHash,
  engagementId: engagementStateFixture.engagementId,
  engagementGeneration: engagementStateFixture.generation,
  destinationOrigin: null,
  activeDestination: null,
  reviewedCandidate: null,
  eligibleCarryForwardSource: null,
  latestArtifact: null,
  draft: null,
  allowedOrigins: ["CONTROLLED_PACK", "WORKSPACE_PROFILE"],
  limits,
} satisfies DestinationWorkshop;

const compatibility: DestinationCompatibilityReport = {
  reportHash: hash("5"),
  scopedCompletenessHash: hash("c"),
  algorithm: "migration-stream/edmx-compatibility/v1",
  odataGeneration: "V2",
  safelyExtractedCount: 42,
  unsupportedInventoryCount: 8,
  unsupportedIrrelevantCount: 0,
  unresolvedSelectedScopeCount: 0,
  findings: [
    {
      code: "QUALIFIED_EXTENSION_ATTRIBUTE",
      classification: "NON_STRUCTURAL_WARNING",
      technicalLocation: "Synthetic.Sales",
      count: 8,
      blocking: false,
      findingHash: hash("f"),
    },
  ],
};

const artifact = {
  schemaVersion: SCHEMAS.destinationArtifact,
  catalogueAuthorityId: "destination-catalogue-001",
  origin: "EDMX",
  artifactLabel: "Sales document metadata",
  system: "Synthetic source",
  clientOrTenant: "local-tenant",
  release: "fixture-v1",
  manualActor: null,
  manualReason: null,
  snapshotId: "destination-snapshot-001",
  snapshotSha256: hash("2"),
  snapshotBytes: 734,
  catalogueHash: hash("4"),
  cataloguePayloadSha256: hash("6"),
  catalogueFileSha256: hash("3"),
  parserName: "migration-stream-edmx",
  parserVersion: "2",
  odataGeneration: "V2",
  namespaceCount: 1,
  namespaces: ["Synthetic.Sales"],
  entityCount: 2,
  propertyCount: 37,
  navigationCount: 1,
  compatibility,
  createdAt: "2026-07-30T08:00:00Z",
} satisfies DestinationArtifactView;

const manualArtifact = {
  ...artifact,
  origin: "MANUAL",
  artifactLabel: "Manual sales",
  system: null,
  clientOrTenant: null,
  release: null,
  manualActor: "Local operator",
  manualReason: "No suitable offline metadata exists.",
  snapshotId: null,
  snapshotSha256: null,
  snapshotBytes: null,
  catalogueFileSha256: null,
  parserName: null,
  parserVersion: null,
  odataGeneration: null,
} satisfies DestinationArtifactView;

const entityPage = {
  schemaVersion: SCHEMAS.destinationCatalogueEntities,
  catalogueAuthorityId: artifact.catalogueAuthorityId,
  catalogueHash: artifact.catalogueHash,
  draftId: null,
  draftGeneration: null,
  draftHash: null,
  items: [
    {
      technicalEntity: "Synthetic.Sales.Header",
      namespace: "Synthetic.Sales",
      name: "Header",
      keyProperties: ["ID"],
      propertyCount: 30,
      navigationCount: 1,
      selected: false,
      root: false,
    },
  ],
  nextCursor: null,
} satisfies DestinationCatalogueEntityPage;

const snapshot = {
  profileId: "workspace.sales-document",
  title: "Sales document destination",
  owner: "Local operator",
  entities: [
    {
      entityId: "SalesDocument",
      label: "Sales document",
      root: true,
      technicalEntity: "Synthetic.Sales.Header",
      fields: [
        {
          fieldId: "sourceDocumentId",
          label: "Source document ID",
          logicalType: "identifier",
          businessRequired: true,
          sensitive: false,
          key: true,
          state: "ACTIVE",
          technicalProperty: "Synthetic.Sales.Header.ID",
          manualSupplement: null,
          technicalChangeReview: null,
          confirmMaxLength: false,
          maxLength: 35,
          diffState: null,
        },
      ],
    },
  ],
  relationships: [],
  rules: [
    {
      ruleId: "sales-document-id-required",
      severity: "error",
      kind: "required",
      target: "SalesDocument.sourceDocumentId",
      parameters: {},
      message: "Document ID is required.",
    },
  ],
  compatibilityAcknowledgements: [],
} satisfies DestinationDraftSnapshot;

const workingDraft = {
  schemaVersion: SCHEMAS.destinationDraft,
  draftId: "destination-draft-001",
  catalogueAuthorityId: artifact.catalogueAuthorityId,
  catalogueHash: artifact.catalogueHash,
  generation: 3,
  draftHash: hash("7"),
  state: "EDITABLE",
  snapshot,
  freezeBlockers: [],
  frozenProfileId: null,
  frozenProfileVersion: null,
  frozenProfileHash: null,
  updatedAt: "2026-07-30T08:10:00Z",
} satisfies DestinationDraftView;

const profileView = {
  schema_version: 1,
  profile_id: snapshot.profileId,
  profile_version: "1.0.0",
  profile_hash: hash("9"),
  schema_hash: hash("a"),
  validation_hash: hash("b"),
  declared_status: "REVIEWED_DRAFT",
  lifecycle_status: "REVIEWED_DRAFT",
  authority: "NONE",
  can_start_new_runs: false,
  registered_at: "2026-07-30T08:11:00Z",
  approval: null,
  lifecycle_events: [],
} satisfies DestinationProfileVersionView;

const reviewedActive = {
  origin: "WORKSPACE_PROFILE",
  destinationAuthorityId: "destination-origin-001",
  destinationOriginHash: hash("c"),
  profileId: profileView.profile_id,
  profileVersion: profileView.profile_version,
  profileHash: profileView.profile_hash,
  profileAuthority: "NONE",
  approvalId: null,
  approvalHash: null,
} satisfies ActiveDestinationView;

const approvedActive = {
  ...reviewedActive,
  profileAuthority: "LOCAL_MAPPING_VALIDATION",
  approvalId: "profile-approval-001",
  approvalHash: hash("d"),
} satisfies ActiveDestinationView;

const frozen = {
  schemaVersion: SCHEMAS.destinationReviewedProfile,
  engagementGeneration: 4,
  profile: profileView,
  originBindingId: reviewedActive.destinationAuthorityId,
  originBindingHash: reviewedActive.destinationOriginHash,
  draftId: workingDraft.draftId,
  draftGeneration: workingDraft.generation,
  draftHash: workingDraft.draftHash,
} satisfies FrozenDestinationProfileView;

const approved = {
  schemaVersion: SCHEMAS.destinationProfileApproval,
  engagementGeneration: 5,
  profile: {
    ...profileView,
    lifecycle_status: "APPROVED",
    authority: "LOCAL_MAPPING_VALIDATION",
    can_start_new_runs: true,
    approval: {
      approval_id: approvedActive.approvalId,
      approver: "Reviewing practitioner",
      reason: "Reviewed exact destination scope for local mapping.",
      acknowledged: true,
      approved_at: "2026-07-30T08:12:00Z",
      approval_hash: approvedActive.approvalHash,
    },
  },
  destinationOrigin: "WORKSPACE_PROFILE",
  destinationAuthorityId: reviewedActive.destinationAuthorityId,
  destinationOriginHash: reviewedActive.destinationOriginHash,
} satisfies ApprovedDestinationProfileView;

const approvedCandidate = {
  ...frozen,
  profile: approved.profile,
} satisfies FrozenDestinationProfileView;

const propertyPage = {
  schemaVersion: SCHEMAS.destinationCatalogueProperties,
  catalogueAuthorityId: artifact.catalogueAuthorityId,
  catalogueHash: artifact.catalogueHash,
  technicalEntity: "Synthetic.Sales.Header",
  draftId: workingDraft.draftId,
  draftGeneration: workingDraft.generation,
  draftHash: workingDraft.draftHash,
  items: [
    {
      technicalProperty: "Synthetic.Sales.Header.ID",
      technicalEntity: "Synthetic.Sales.Header",
      name: "ID",
      edmType: "Edm.String",
      nullable: false,
      maxLength: "35",
      precision: null,
      scale: null,
      keyCandidate: true,
      requiredCandidate: true,
      state: "ACTIVE",
      field: snapshot.entities[0].fields[0],
    },
  ],
  nextCursor: null,
} satisfies DestinationCataloguePropertyPage;

const revisedArtifact = {
  ...artifact,
  catalogueAuthorityId: "destination-catalogue-002",
  artifactLabel: "Sales document metadata revision",
  snapshotId: "destination-snapshot-002",
  snapshotSha256: hash("8"),
  catalogueHash: hash("e"),
  cataloguePayloadSha256: hash("1"),
  catalogueFileSha256: hash("0"),
  propertyCount: 38,
  createdAt: "2026-07-30T09:00:00Z",
} satisfies DestinationArtifactView;

const multiEntitySnapshot = {
  ...snapshot,
  entities: [
    {
      ...snapshot.entities[0],
      fields: [
        snapshot.entities[0].fields[0],
        {
          fieldId: "sellingUnit",
          label: "Selling unit",
          logicalType: "code",
          businessRequired: true,
          sensitive: false,
          key: true,
          state: "ACTIVE",
          technicalProperty: "Synthetic.Sales.Header.SellingUnit",
          manualSupplement: null,
          technicalChangeReview: null,
          confirmMaxLength: false,
          maxLength: 8,
          diffState: null,
        },
      ],
    },
    {
      entityId: "SalesDocumentItem",
      label: "Sales document item",
      root: false,
      technicalEntity: "Synthetic.Sales.Item",
      fields: [
        {
          fieldId: "parentDocumentId",
          label: "Parent document ID",
          logicalType: "identifier",
          businessRequired: true,
          sensitive: false,
          key: false,
          state: "ACTIVE",
          technicalProperty: "Synthetic.Sales.Item.ParentID",
          manualSupplement: null,
          technicalChangeReview: null,
          confirmMaxLength: false,
          maxLength: 35,
          diffState: null,
        },
        {
          fieldId: "parentSellingUnit",
          label: "Parent selling unit",
          logicalType: "code",
          businessRequired: true,
          sensitive: false,
          key: false,
          state: "ACTIVE",
          technicalProperty: "Synthetic.Sales.Item.ParentSellingUnit",
          manualSupplement: null,
          technicalChangeReview: null,
          confirmMaxLength: false,
          maxLength: 8,
          diffState: null,
        },
        {
          fieldId: "sourceItemId",
          label: "Source item ID",
          logicalType: "identifier",
          businessRequired: true,
          sensitive: false,
          key: true,
          state: "ACTIVE",
          technicalProperty: "Synthetic.Sales.Item.ID",
          manualSupplement: null,
          technicalChangeReview: null,
          confirmMaxLength: false,
          maxLength: 10,
          diffState: null,
        },
      ],
    },
  ],
} satisfies DestinationDraftSnapshot;

const multiEntityDraft = {
  ...workingDraft,
  draftId: "destination-draft-multi",
  draftHash: hash("8"),
  snapshot: multiEntitySnapshot,
} satisfies DestinationDraftView;

const manualSnapshot = {
  ...snapshot,
  entities: [
    {
      ...snapshot.entities[0],
      fields: [
        ...snapshot.entities[0].fields,
        {
          fieldId: "customerReference",
          label: "Customer reference",
          logicalType: "text",
          businessRequired: false,
          sensitive: false,
          key: false,
          state: "NEEDS_REVIEW",
          technicalProperty: null,
          manualSupplement: {
            supplementId: "manual-customer-reference",
            actor: "Profile author",
            reason: "No suitable technical property existed in the first EDMX.",
          },
          technicalChangeReview: null,
          confirmMaxLength: false,
          maxLength: null,
          diffState: null,
        },
        {
          fieldId: "operatorNote",
          label: "Operator note",
          logicalType: "text",
          businessRequired: false,
          sensitive: false,
          key: false,
          state: "NEEDS_REVIEW",
          technicalProperty: null,
          manualSupplement: {
            supplementId: "manual-operator-note",
            actor: "Profile author",
            reason: "Local business meaning requires an explicit supplement.",
          },
          technicalChangeReview: null,
          confirmMaxLength: false,
          maxLength: null,
          diffState: null,
        },
      ],
    },
  ],
} satisfies DestinationDraftSnapshot;

const manualDraft = {
  ...workingDraft,
  draftId: "destination-draft-manual",
  draftHash: hash("6"),
  snapshot: manualSnapshot,
} satisfies DestinationDraftView;

function propertiesFor(
  draft: DestinationDraftView,
  items: DestinationCataloguePropertyPage["items"],
  nextCursor: string | null = null,
): DestinationCataloguePropertyPage {
  return {
    ...propertyPage,
    draftId: draft.draftId,
    draftGeneration: draft.generation,
    draftHash: draft.draftHash,
    items,
    nextCursor,
  };
}

function context() {
  return {
    ...engagementContextFixture,
    engagement: engagementStateFixture,
  };
}

function renderDestination(
  overrides: Record<string, unknown>,
): ReturnType<typeof mockApi> {
  const api = mockApi({
    "GET /api/v1/context": context(),
    "GET /api/v1/journey": destinationJourney(),
    ...overrides,
  });
  render(<App />);
  return api;
}

function body(request: CapturedRequest | undefined): Record<string, unknown> {
  return JSON.parse(request?.body ?? "{}") as Record<string, unknown>;
}

describe("destination contract workshop", () => {
  beforeEach(() => window.history.replaceState(null, "", "/destination"));

  it("strictly rejects server paths or expanded authority contracts", () => {
    expect(decodeContract(artifact, SCHEMAS.destinationArtifact)).toEqual(
      artifact,
    );
    expect(() =>
      decodeContract(
        { ...artifact, namespaceCount: 2 },
        SCHEMAS.destinationArtifact,
      ),
    ).toThrowError(/required contract shape/);
    expect(() =>
      decodeContract(
        {
          ...artifact,
          namespaceCount: 2,
          namespaces: ["Synthetic.Z", "Synthetic.A"],
        },
        SCHEMAS.destinationArtifact,
      ),
    ).toThrowError(/required contract shape/);
    expect(() =>
      decodeContract(
        {
          ...artifact,
          compatibility: {
            ...artifact.compatibility,
            findings: [
              {
                ...artifact.compatibility.findings[0],
                classification: "UNRECOGNIZED_CLASSIFICATION",
              },
            ],
          },
        },
        SCHEMAS.destinationArtifact,
      ),
    ).toThrowError(/required contract shape/);
    expect(() =>
      decodeContract(
        {
          ...artifact,
          compatibility: {
            ...artifact.compatibility,
            findings: [
              {
                ...artifact.compatibility.findings[0],
                classification: "UNRESOLVED_SELECTED_SCOPE",
                blocking: false,
              },
            ],
          },
        },
        SCHEMAS.destinationArtifact,
      ),
    ).toThrowError(/required contract shape/);
    expect(decodeContract(emptyWorkshop, SCHEMAS.destinationWorkshop)).toEqual(
      emptyWorkshop,
    );
    expect(() =>
      decodeContract(
        { ...emptyWorkshop, workspacePath: "/private/hidden" },
        SCHEMAS.destinationWorkshop,
      ),
    ).toThrowError(/required contract shape/);
    expect(() =>
      decodeContract(
        {
          ...emptyWorkshop,
          latestArtifact: {
            ...artifact,
            snapshotPath: "/private/hidden",
          },
        },
        SCHEMAS.destinationWorkshop,
      ),
    ).toThrowError(/required contract shape/);
    expect(() =>
      decodeContract(
        { ...workingDraft, state: "FROZEN" },
        SCHEMAS.destinationDraft,
      ),
    ).toThrowError(/required contract shape/);
    expect(() =>
      decodeContract(
        {
          ...emptyWorkshop,
          destinationOrigin: "WORKSPACE_PROFILE",
          activeDestination: {
            ...reviewedActive,
            profileAuthority: "LOCAL_MAPPING_VALIDATION",
          },
        },
        SCHEMAS.destinationWorkshop,
      ),
    ).toThrowError(/required contract shape/);
    expect(() =>
      decodeContract(
        {
          ...frozen,
          profile: approved.profile,
        },
        SCHEMAS.destinationReviewedProfile,
      ),
    ).toThrowError(/required contract shape/);
    expect(() =>
      decodeContract(
        {
          ...approved,
          profile: profileView,
        },
        SCHEMAS.destinationProfileApproval,
      ),
    ).toThrowError(/required contract shape/);
    expect(() =>
      decodeContract(
        {
          ...workingDraft,
          snapshot: {
            ...workingDraft.snapshot,
            entities: [
              {
                ...workingDraft.snapshot.entities[0],
                entityId: "Sales.Document",
              },
            ],
          },
        },
        SCHEMAS.destinationDraft,
      ),
    ).toThrowError(/required contract shape/);
  });

  it("matches the exact 512-byte optional EDMX provenance boundary", () => {
    const command = {
      schemaVersion: DESTINATION_COMMAND_SCHEMAS.reserveEdmx,
      expectedEngagementGeneration: 1,
      expectedWorkspaceId: activeObjectWorkspaceFixture.workspace.workspaceId,
      expectedWorkspaceDescriptorHash:
        activeObjectWorkspaceFixture.workspace.descriptorHash,
      sourceName: "destination.edmx",
      artifactLabel: "Destination metadata",
      system: "s".repeat(512),
      clientOrTenant: null,
      release: null,
      idempotencyKey: "destination-edmx-boundary-0001",
    };
    expect(validateDestinationCommand(command)).toEqual(command);
    expect(() =>
      validateDestinationCommand({ ...command, system: "s".repeat(513) }),
    ).toThrowError(
      /destination command is incomplete or outside its safe bounds/,
    );
    expect(
      validateDestinationCommand({ ...command, system: "é".repeat(256) }),
    ).toBeTruthy();
    expect(() =>
      validateDestinationCommand({ ...command, system: "é".repeat(257) }),
    ).toThrowError(
      /destination command is incomplete or outside its safe bounds/,
    );
  });

  it("opens one Destination surface while Source remains independently available", async () => {
    renderDestination({
      "GET /api/v1/engagement/destination": emptyWorkshop,
    });

    expect(
      await screen.findByRole("heading", {
        name: "Define the destination contract",
      }),
    ).toBeVisible();
    const journey = screen.getByRole("navigation", {
      name: "Migration journey",
    });
    expect(
      within(journey).getByRole("link", { name: "Destination" }),
    ).toHaveAttribute("aria-current", "page");
    expect(
      within(journey).getByRole("link", { name: "Source" }),
    ).toHaveAttribute("href", "/setup");
    expect(await screen.findByLabelText("EDMX file")).toBeVisible();
    expect(
      screen.getByRole("heading", {
        name: "Create a manual technical catalogue",
      }),
    ).toBeVisible();
  });

  it("keeps the same Destination workshop available from a completed downstream workbench deep link", async () => {
    const workspaceJourney = {
      ...journeyFixture,
      currentStageId: "ISSUES",
      binding: {
        ...journeyFixture.binding,
        objectPackId: approvedActive.profileId,
        destinationOrigin: "WORKSPACE_PROFILE",
        destinationAuthorityId: approvedActive.destinationAuthorityId,
        destinationOriginHash: approvedActive.destinationOriginHash,
        profileId: approvedActive.profileId,
        profileVersion: approvedActive.profileVersion,
        profileHash: approvedActive.profileHash,
      },
      stages: journeyFixture.stages.map((stage) =>
        stage.id === "DESTINATION" ? { ...stage, href: "/destination" } : stage,
      ) as Journey["stages"],
    } satisfies Journey;
    mockApi({
      "GET /api/v1/context": governedContextFixture,
      "GET /api/v1/journey": workspaceJourney,
      "GET /api/v1/engagement/destination": {
        ...emptyWorkshop,
        destinationOrigin: "WORKSPACE_PROFILE",
        activeDestination: approvedActive,
        latestArtifact: artifact,
      } satisfies DestinationWorkshop,
      "GET /api/v1/engagement/destination/catalogue/entities": entityPage,
    });
    render(<App />);

    expect(
      await screen.findByRole("heading", {
        name: "Define the destination contract",
      }),
    ).toBeVisible();
    expect(
      screen.getByRole("navigation", { name: "Migration journey" }),
    ).toBeVisible();
    expect(screen.getByRole("link", { name: "Destination" })).toHaveAttribute(
      "href",
      "/destination",
    );
    expect(
      await screen.findByText("Destination catalogue authority"),
    ).toBeVisible();
    expect(
      screen.queryByRole("heading", { name: "Migration Object Profile" }),
    ).toBeNull();
  });

  it("fails closed instead of soliciting an opaque mapping identity when no exact carry source is eligible", async () => {
    const priorActive = {
      ...approvedActive,
      profileId: "workspace.sales-document.prior",
      profileVersion: "0.9.0",
      profileHash: hash("8"),
    } satisfies ActiveDestinationView;
    const mismatchedSource = {
      mappingId: "mapping-001",
      mappingVersion: "1.0.0",
      mappingHash: hash("3"),
      mappingApprovalId: "mapping-approval-001",
      mappingApprovalHash: hash("2"),
      activeJobId: "job-current-source",
      activeSourceHash: hash("0"),
      validatedJobId: "job-mapping-validated",
      validatedSourceHash: hash("1"),
      profileId: "different.profile",
      profileVersion: "0.9.0",
      profileHash: hash("8"),
    };
    expect(() =>
      decodeContract(
        {
          ...emptyWorkshop,
          destinationOrigin: "WORKSPACE_PROFILE",
          activeDestination: priorActive,
          eligibleCarryForwardSource: mismatchedSource,
        },
        SCHEMAS.destinationWorkshop,
      ),
    ).toThrowError(/required contract shape/);
    expect(() =>
      decodeContract(
        {
          ...emptyWorkshop,
          destinationOrigin: "WORKSPACE_PROFILE",
          activeDestination: priorActive,
          eligibleCarryForwardSource: {
            ...mismatchedSource,
            profileId: priorActive.profileId,
            profileVersion: priorActive.profileVersion,
            profileHash: priorActive.profileHash,
            activeJobId: "same-job",
            validatedJobId: "same-job",
            activeSourceHash: hash("1"),
            validatedSourceHash: hash("2"),
          },
        },
        SCHEMAS.destinationWorkshop,
      ),
    ).toThrowError(/required contract shape/);

    renderDestination({
      "GET /api/v1/engagement/destination": {
        ...emptyWorkshop,
        destinationOrigin: "WORKSPACE_PROFILE",
        activeDestination: priorActive,
        reviewedCandidate: approvedCandidate,
        latestArtifact: artifact,
        draft: {
          ...workingDraft,
          state: "FROZEN",
          frozenProfileId: approvedCandidate.profile.profile_id,
          frozenProfileVersion: approvedCandidate.profile.profile_version,
          frozenProfileHash: approvedCandidate.profile.profile_hash,
        },
      } satisfies DestinationWorkshop,
      "GET /api/v1/engagement/destination/catalogue/properties": propertyPage,
    });

    expect(
      await screen.findByRole("heading", {
        name: "Mapping carry-forward unavailable",
      }),
    ).toBeVisible();
    expect(screen.queryByLabelText("Prior mapping ID")).toBeNull();
    expect(
      screen.queryByRole("button", { name: "Create compatible mapping draft" }),
    ).toBeNull();
  });

  it("accepts only sorted unique carry relationship codes with an exact count", () => {
    const validCarry = {
      schemaVersion: SCHEMAS.destinationCarryForward,
      engagementGeneration: 8,
      carryForwardId: "destination-carry-forward-001",
      resultHash: hash("4"),
      clonedBindingCount: 6,
      unresolvedTargetCount: 1,
      unresolvedRelationshipCount: 2,
      unresolvedRelationships: [
        {
          relationshipId: "line-items",
          reason: "RELATIONSHIP_CHANGED",
        },
        {
          relationshipId: "schedule-lines",
          reason: "RELATIONSHIP_NOT_PRESENT",
        },
      ],
    };
    expect(decodeContract(validCarry, SCHEMAS.destinationCarryForward)).toEqual(
      validCarry,
    );
    expect(() =>
      decodeContract(
        {
          ...validCarry,
          unresolvedRelationships: [
            {
              relationshipId: "line-items",
              reason: "FREE_FORM_REASON",
            },
          ],
          unresolvedRelationshipCount: 1,
        },
        SCHEMAS.destinationCarryForward,
      ),
    ).toThrowError(/required contract shape/);
    expect(() =>
      decodeContract(
        {
          ...validCarry,
          unresolvedRelationships: [
            validCarry.unresolvedRelationships[0],
            {
              ...validCarry.unresolvedRelationships[1],
              relationshipId: "line-items",
            },
          ],
        },
        SCHEMAS.destinationCarryForward,
      ),
    ).toThrowError(/required contract shape/);
    expect(() =>
      decodeContract(
        { ...validCarry, unresolvedRelationshipCount: 1 },
        SCHEMAS.destinationCarryForward,
      ),
    ).toThrowError(/required contract shape/);
    expect(() =>
      decodeContract(
        {
          ...validCarry,
          unresolvedRelationships: [
            validCarry.unresolvedRelationships[1],
            validCarry.unresolvedRelationships[0],
          ],
        },
        SCHEMAS.destinationCarryForward,
      ),
    ).toThrowError(/required contract shape/);
  });

  it("reserves then uploads exact EDMX bytes without sending a server path", async () => {
    const user = userEvent.setup();
    const { requests } = renderDestination({
      "GET /api/v1/engagement/destination": emptyWorkshop,
      "POST /api/v1/engagement/destination/edmx-imports": {
        schemaVersion: SCHEMAS.destinationArtifactReservation,
        attemptId: "destination-import-001",
        state: "RECEIVING",
        artifactLabel: artifact.artifactLabel,
        sourceName: "sales-document.edmx",
        maximumBytes: limits.maximumEdmxBytes,
        createdAt: "2026-07-30T08:00:00Z",
      },
      "PUT /api/v1/engagement/destination/edmx-imports/destination-import-001/content":
        artifact,
    });

    const file = new File(
      [
        '<edmx:Edmx Version="1.0" xmlns:edmx="http://schemas.microsoft.com/ado/2007/06/edmx"/>',
      ],
      "sales-document.edmx",
      { type: "application/xml" },
    );
    await user.upload(await screen.findByLabelText("EDMX file"), file);
    await user.type(screen.getByLabelText("Artifact label"), "Sales metadata");
    await user.type(screen.getByLabelText("System (optional)"), "Sanitized");
    await user.click(
      screen.getByRole("button", { name: "Import offline EDMX" }),
    );

    await waitFor(() =>
      expect(requests.some((request) => request.method === "PUT")).toBe(true),
    );
    const reserve = requests.find(
      (request) =>
        request.method === "POST" &&
        request.url.pathname === "/api/v1/engagement/destination/edmx-imports",
    );
    const upload = requests.find((request) => request.method === "PUT");
    const reserveBody = body(reserve);
    expect(reserveBody).toEqual({
      schemaVersion: DESTINATION_COMMAND_SCHEMAS.reserveEdmx,
      expectedEngagementGeneration: emptyWorkshop.engagementGeneration,
      expectedWorkspaceId: emptyWorkshop.workspaceId,
      expectedWorkspaceDescriptorHash: emptyWorkshop.workspaceDescriptorHash,
      sourceName: "sales-document.edmx",
      artifactLabel: "Sales metadata",
      system: "Sanitized",
      clientOrTenant: null,
      release: null,
      idempotencyKey: expect.any(String),
    });
    expect(JSON.stringify(reserveBody)).not.toMatch(/path|private/i);
    expect(upload?.rawBody).toBe(file);
    expect(upload?.headers.get("content-type")).toBe("application/xml");
    expect(upload?.headers.get("idempotency-key")).toBe(
      reserveBody.idempotencyKey,
    );
    expect(upload?.headers.get("x-migration-stream-intent")).toBe(
      "engagement-destination-edmx-content-v1",
    );
    const fileInput = screen.getByLabelText("EDMX file");
    await waitFor(() => expect(fileInput).toHaveValue(""));
    expect(screen.getByLabelText("Artifact label")).toHaveValue("");
    await user.upload(fileInput, file);
    expect(
      screen.getByRole("button", { name: "Import offline EDMX" }),
    ).toBeEnabled();
  });

  it("reopens exact origin-appropriate provenance without implying review authority", async () => {
    renderDestination({
      "GET /api/v1/engagement/destination": {
        ...emptyWorkshop,
        latestArtifact: artifact,
      } satisfies DestinationWorkshop,
      "GET /api/v1/engagement/destination/catalogue/entities": entityPage,
    });

    expect(await screen.findByText("Synthetic source")).toBeVisible();
    expect(screen.getByText("local-tenant")).toBeVisible();
    expect(screen.getByText("fixture-v1")).toBeVisible();
    expect(
      screen.getByText(
        /immutable origin context, not reviewed business meaning/i,
      ),
    ).toBeVisible();

    cleanup();
    window.history.replaceState(null, "", "/destination");
    renderDestination({
      "GET /api/v1/engagement/destination": {
        ...emptyWorkshop,
        latestArtifact: manualArtifact,
      } satisfies DestinationWorkshop,
      "GET /api/v1/engagement/destination/catalogue/entities": {
        ...entityPage,
        catalogueAuthorityId: manualArtifact.catalogueAuthorityId,
        catalogueHash: manualArtifact.catalogueHash,
      } satisfies DestinationCatalogueEntityPage,
    });
    expect(await screen.findByText("Local operator")).toBeVisible();
    expect(
      screen.getByText("No suitable offline metadata exists."),
    ).toBeVisible();
    expect(screen.queryByText("Synthetic source")).toBeNull();
  });

  it("creates a bounded manual catalogue with explicit provenance", async () => {
    const user = userEvent.setup();
    const { requests } = renderDestination({
      "GET /api/v1/engagement/destination": emptyWorkshop,
      "POST /api/v1/engagement/destination/manual-catalogues": manualArtifact,
    });

    await screen.findByRole("heading", {
      name: "Create a manual technical catalogue",
    });
    await user.type(screen.getByLabelText("Catalogue label"), "Manual sales");
    await user.type(screen.getByLabelText("Namespace"), "Manual.Sales");
    await user.type(screen.getByLabelText("Technical entity name"), "Header");
    await user.type(screen.getByLabelText("Property name"), "DocumentId");
    await user.click(screen.getByLabelText("Technical key candidate"));
    await user.type(screen.getByLabelText("Author"), "Local operator");
    await user.type(
      screen.getByLabelText("Reason for manual technical definition"),
      "No suitable offline metadata exists.",
    );
    await user.click(
      screen.getByRole("button", {
        name: "Create manual technical catalogue",
      }),
    );

    await waitFor(() =>
      expect(
        requests.some(
          (request) =>
            request.url.pathname ===
            "/api/v1/engagement/destination/manual-catalogues",
        ),
      ).toBe(true),
    );
    expect(
      body(
        requests.find(
          (request) =>
            request.url.pathname ===
            "/api/v1/engagement/destination/manual-catalogues",
        ),
      ),
    ).toMatchObject({
      schemaVersion: DESTINATION_COMMAND_SCHEMAS.createManualCatalogue,
      artifactLabel: "Manual sales",
      actor: "Local operator",
      reason: "No suitable offline metadata exists.",
      entities: [
        {
          namespace: "Manual.Sales",
          name: "Header",
          properties: [
            {
              name: "DocumentId",
              edmType: "Edm.String",
              nullable: true,
              maxLength: null,
              keyCandidate: true,
            },
          ],
        },
      ],
    });
  });

  it("starts an explicit root-scoped draft from bounded catalogue evidence", async () => {
    const user = userEvent.setup();
    const workshop = {
      ...emptyWorkshop,
      latestArtifact: artifact,
    } satisfies DestinationWorkshop;
    const { requests } = renderDestination({
      "GET /api/v1/engagement/destination": workshop,
      "GET /api/v1/engagement/destination/catalogue/entities": entityPage,
      "POST /api/v1/engagement/destination/drafts/start": workingDraft,
    });

    expect(
      await screen.findByRole("heading", {
        name: "Destination catalogue authority",
      }),
    ).toBeVisible();
    const namespaceEvidence = screen.getByText("Namespaces").closest("div");
    expect(namespaceEvidence).not.toBeNull();
    expect(
      within(namespaceEvidence!).getByText("Synthetic.Sales"),
    ).toBeVisible();
    expect(
      within(namespaceEvidence!).getByText("1 exact namespace"),
    ).toBeVisible();
    expect(screen.getByText("QUALIFIED_EXTENSION_ATTRIBUTE · 8")).toBeVisible();
    expect(screen.getByText("NON_STRUCTURAL_WARNING")).toBeVisible();
    await user.click(await screen.findByLabelText("Root"));
    await user.type(screen.getByLabelText("Profile ID"), snapshot.profileId);
    await user.type(screen.getByLabelText("Profile title"), snapshot.title);
    await user.type(screen.getByLabelText("Profile owner"), snapshot.owner);
    await user.type(screen.getByLabelText("Draft author"), "Local operator");
    await user.type(
      screen.getByLabelText("Reason for selected technical scope"),
      "Header is the bounded migration root.",
    );
    await user.click(
      screen.getByRole("button", { name: "Start governed profile draft" }),
    );

    await waitFor(() =>
      expect(
        requests.some(
          (request) =>
            request.url.pathname ===
            "/api/v1/engagement/destination/drafts/start",
        ),
      ).toBe(true),
    );
    expect(
      body(
        requests.find(
          (request) =>
            request.url.pathname ===
            "/api/v1/engagement/destination/drafts/start",
        ),
      ),
    ).toMatchObject({
      schemaVersion: DESTINATION_COMMAND_SCHEMAS.startDraft,
      catalogueAuthorityId: artifact.catalogueAuthorityId,
      expectedCatalogueHash: artifact.catalogueHash,
      rootTechnicalEntity: "Synthetic.Sales.Header",
      childTechnicalEntities: [],
      profileId: snapshot.profileId,
    });
    const catalogueRead = requests.find(
      (request) =>
        request.method === "GET" &&
        request.url.pathname ===
          "/api/v1/engagement/destination/catalogue/entities",
    );
    expect(catalogueRead?.url.searchParams.get("catalogueAuthorityId")).toBe(
      artifact.catalogueAuthorityId,
    );
    expect(catalogueRead?.url.searchParams.get("pageSize")).toBe("50");
    expect(catalogueRead?.url.searchParams.has("catalogue_authority_id")).toBe(
      false,
    );
    expect(document.body.textContent).not.toContain("/private/");
  });

  it("freezes REVIEWED_DRAFT first and approves the exact origin separately", async () => {
    const user = userEvent.setup();
    let phase: "working" | "reviewed" | "approved" = "working";
    let contextReads = 0;
    let journeyReads = 0;
    let workshopReads = 0;
    const engagement = () => {
      const selected = phase !== "working";
      const selectedAuthority =
        phase === "approved" ? approvedActive : reviewedActive;
      return {
        ...engagementStateFixture,
        generation: phase === "working" ? 3 : phase === "reviewed" ? 4 : 5,
        active: selected
          ? {
              ...engagementStateFixture.active,
              objectPackId: selectedAuthority.profileId,
              destinationOrigin: "WORKSPACE_PROFILE" as const,
              destinationAuthorityId: selectedAuthority.destinationAuthorityId,
              destinationOriginHash: selectedAuthority.destinationOriginHash,
              profileId: selectedAuthority.profileId,
              profileVersion: selectedAuthority.profileVersion,
              profileHash: selectedAuthority.profileHash,
            }
          : engagementStateFixture.active,
        profileStage:
          phase === "working"
            ? ("NOT_SELECTED" as const)
            : phase === "reviewed"
              ? ("REVIEWED" as const)
              : ("APPROVED" as const),
      };
    };
    const journey = (): Journey => {
      const currentEngagement = engagement();
      const base = destinationJourney();
      const approvedPhase = phase === "approved";
      return {
        ...base,
        currentStageId: approvedPhase ? "SOURCE" : "DESTINATION",
        binding: {
          ...base.binding,
          engagementGeneration: currentEngagement.generation,
          objectPackId: currentEngagement.active.objectPackId,
          destinationOrigin: currentEngagement.active.destinationOrigin,
          destinationAuthorityId:
            currentEngagement.active.destinationAuthorityId,
          destinationOriginHash: currentEngagement.active.destinationOriginHash,
          profileId: currentEngagement.active.profileId,
          profileVersion: currentEngagement.active.profileVersion,
          profileHash: currentEngagement.active.profileHash,
        },
        stages: base.stages.map((stage) => {
          if (stage.id === "DESTINATION") {
            return approvedPhase
              ? {
                  ...stage,
                  status: "COMPLETED",
                  available: true,
                  href: "/destination",
                  unavailableReason: null,
                }
              : stage;
          }
          if (stage.id === "SOURCE") {
            return approvedPhase
              ? {
                  ...stage,
                  status: "CURRENT",
                  available: true,
                  href: "/setup",
                  unavailableReason: null,
                }
              : stage;
          }
          if (stage.id === "MAPPING") {
            return {
              ...stage,
              unavailableReason: approvedPhase
                ? "Accept and sample-free profile the source before preparing a mapping."
                : "Complete an approved destination and an accepted, sample-free source profile before mapping.",
            };
          }
          return stage;
        }),
      };
    };
    const workshop = (): DestinationWorkshop => ({
      ...emptyWorkshop,
      engagementGeneration:
        phase === "working" ? 3 : phase === "reviewed" ? 4 : 5,
      destinationOrigin: phase === "working" ? null : "WORKSPACE_PROFILE",
      activeDestination:
        phase === "working"
          ? null
          : phase === "reviewed"
            ? reviewedActive
            : approvedActive,
      reviewedCandidate:
        phase === "working"
          ? null
          : phase === "reviewed"
            ? frozen
            : approvedCandidate,
      latestArtifact: artifact,
      draft:
        phase === "working"
          ? workingDraft
          : {
              ...workingDraft,
              state: "FROZEN",
              frozenProfileId: profileView.profile_id,
              frozenProfileVersion: profileView.profile_version,
              frozenProfileHash: profileView.profile_hash,
            },
    });
    const { requests } = renderDestination({
      "GET /api/v1/context": () => {
        contextReads += 1;
        return { ...context(), engagement: engagement() };
      },
      "GET /api/v1/journey": () => {
        journeyReads += 1;
        return journey();
      },
      "GET /api/v1/engagement/destination": () => {
        workshopReads += 1;
        return workshop();
      },
      "GET /api/v1/engagement/destination/catalogue/properties": propertyPage,
      "POST /api/v1/engagement/destination/drafts/freeze": () => {
        phase = "reviewed";
        return frozen;
      },
      "POST /api/v1/engagement/destination/profiles/approve": () => {
        phase = "approved";
        return approved;
      },
    });

    await screen.findByRole("button", { name: "Submit Reviewed Draft" });
    await user.type(screen.getByLabelText("New profile version"), "1.0.0");
    await user.type(screen.getByLabelText("Reviewer"), "Profile reviewer");
    await user.type(
      screen.getByLabelText("Review reason"),
      "Exact scope reviewed.",
    );
    await user.click(
      screen.getByRole("checkbox", {
        name: /Freeze it as REVIEWED_DRAFT only/,
      }),
    );
    await user.click(
      screen.getByRole("button", { name: "Submit Reviewed Draft" }),
    );

    const approvalButton = await screen.findByRole("button", {
      name: "Approve exact profile hash",
    });
    await waitFor(() => {
      expect(contextReads).toBeGreaterThanOrEqual(2);
      expect(journeyReads).toBeGreaterThanOrEqual(2);
      expect(workshopReads).toBeGreaterThanOrEqual(2);
    });
    expect(screen.queryByRole("alert")).toBeNull();
    expect(document.body).not.toHaveTextContent("PROFILE_NOT_APPROVED");
    expect(
      screen.getByText("Reviewed profile awaiting approval"),
    ).toBeVisible();
    expect(
      screen.getByRole("heading", {
        name: "Immutable reviewed destination profile",
      }),
    ).toBeVisible();
    expect(
      screen.getAllByText(profileView.profile_hash).length,
    ).toBeGreaterThan(0);
    expect(
      screen.getAllByText(reviewedActive.destinationOriginHash).length,
    ).toBeGreaterThan(0);
    const reviewedJourney = screen.getByRole("navigation", {
      name: "Migration journey",
    });
    expect(
      within(reviewedJourney).queryByRole("link", { name: "Mapping" }),
    ).toBeNull();
    expect(
      within(reviewedJourney).getByText(
        "Complete an approved destination and an accepted, sample-free source profile before mapping.",
      ),
    ).toBeVisible();
    expect(screen.queryByText("Destination authority approved")).toBeNull();

    const freezeRequest = requests.find(
      (request) =>
        request.url.pathname === "/api/v1/engagement/destination/drafts/freeze",
    );
    expect(body(freezeRequest)).toEqual({
      schemaVersion: DESTINATION_COMMAND_SCHEMAS.freezeDraft,
      expectedEngagementGeneration: 3,
      draftId: workingDraft.draftId,
      expectedDraftGeneration: workingDraft.generation,
      expectedDraftHash: workingDraft.draftHash,
      profileVersion: "1.0.0",
      acknowledgement: DESTINATION_ACKNOWLEDGEMENTS.freeze,
      actor: "Profile reviewer",
      reason: "Exact scope reviewed.",
      idempotencyKey: expect.any(String),
    });
    expect(freezeRequest?.headers.get("idempotency-key")).toBe(
      body(freezeRequest).idempotencyKey,
    );
    expect(freezeRequest?.headers.get("x-migration-stream-intent")).toBe(
      "engagement-destination-draft-freeze-v1",
    );

    await user.type(screen.getByLabelText("Approver"), "Approving operator");
    await user.type(
      screen.getByLabelText("Approval reason"),
      "Approved for local mapping and validation.",
    );
    await user.click(
      screen.getByRole("checkbox", {
        name: /approve this exact REVIEWED_DRAFT profile/,
      }),
    );
    await user.click(approvalButton);

    expect(
      await screen.findByText("Destination authority approved"),
    ).toBeVisible();
    await waitFor(() => {
      expect(contextReads).toBeGreaterThanOrEqual(3);
      expect(journeyReads).toBeGreaterThanOrEqual(3);
      expect(workshopReads).toBeGreaterThanOrEqual(3);
    });
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByText("Approved profile")).toBeVisible();
    const approveRequest = requests.find(
      (request) =>
        request.url.pathname ===
        "/api/v1/engagement/destination/profiles/approve",
    );
    const propertyRead = requests.find(
      (request) =>
        request.method === "GET" &&
        request.url.pathname ===
          "/api/v1/engagement/destination/catalogue/properties",
    );
    expect(propertyRead?.url.searchParams.get("technicalEntity")).toBe(
      "Synthetic.Sales.Header",
    );
    expect(propertyRead?.url.searchParams.get("catalogueAuthorityId")).toBe(
      artifact.catalogueAuthorityId,
    );
    expect(body(approveRequest)).toEqual({
      schemaVersion: DESTINATION_COMMAND_SCHEMAS.approveProfile,
      expectedEngagementGeneration: 4,
      profileId: profileView.profile_id,
      profileVersion: profileView.profile_version,
      profileHash: reviewedActive.profileHash,
      originBindingHash: reviewedActive.destinationOriginHash,
      actor: "Approving operator",
      reason: "Approved for local mapping and validation.",
      authorityAcknowledgement:
        DESTINATION_ACKNOWLEDGEMENTS.localMappingValidation,
      idempotencyKey: expect.any(String),
    });
    expect(approveRequest?.headers.get("idempotency-key")).toBe(
      body(approveRequest).idempotencyKey,
    );
    expect(approveRequest?.headers.get("x-migration-stream-intent")).toBe(
      "engagement-destination-profile-approve-v1",
    );
  });

  it("keeps unsaved screen state separate from durable hash authority", async () => {
    const user = userEvent.setup();
    const workshop = {
      ...emptyWorkshop,
      latestArtifact: revisedArtifact,
      draft: workingDraft,
    } satisfies DestinationWorkshop;
    const { requests } = renderDestination({
      "GET /api/v1/engagement/destination": workshop,
      "GET /api/v1/engagement/destination/catalogue/properties": propertyPage,
    });

    expect(await screen.findByText("Destination profile")).toBeVisible();
    expect(screen.getByText("None")).toBeVisible();
    const title = screen.getByLabelText("Title");
    await user.type(title, " revised locally");

    expect(
      await screen.findByText("Unsaved local draft changes"),
    ).toBeVisible();
    expect(
      screen.getByRole("button", {
        name: "Rebase destination draft onto this snapshot",
      }),
    ).toBeDisabled();
    expect(
      screen.getByRole("button", { name: "Submit Reviewed Draft" }),
    ).toBeDisabled();
    expect(
      screen.getByRole("button", { name: "Save working draft" }),
    ).toBeEnabled();
    expect(
      requests.filter((request) => request.method === "POST"),
    ).toHaveLength(0);
  });

  it("preserves entity and field input focus while canonical IDs change", async () => {
    const user = userEvent.setup();
    renderDestination({
      "GET /api/v1/engagement/destination": {
        ...emptyWorkshop,
        latestArtifact: artifact,
        draft: workingDraft,
      } satisfies DestinationWorkshop,
      "GET /api/v1/engagement/destination/catalogue/properties": propertyPage,
    });

    const entityId = await screen.findByLabelText(
      "Canonical entity ID for Sales document",
    );
    await user.clear(entityId);
    await user.type(entityId, "Order");
    expect(entityId).toHaveValue("Order");
    expect(document.activeElement).toBe(entityId);

    const fieldId = screen.getByLabelText(
      "Canonical field ID for Order.sourceDocumentId",
    );
    await user.clear(fieldId);
    await user.type(fieldId, "orderId");
    expect(fieldId).toHaveValue("orderId");
    expect(document.activeElement).toBe(fieldId);
  });

  it("authors typed same-entity conditional-required and exact reference-lookup obligations", async () => {
    const user = userEvent.setup();
    const ruleSnapshot = {
      ...snapshot,
      entities: [
        {
          ...snapshot.entities[0],
          fields: [
            snapshot.entities[0].fields[0],
            {
              ...snapshot.entities[0].fields[0],
              fieldId: "documentType",
              label: "Document type",
              logicalType: "code",
              businessRequired: false,
              key: false,
              technicalProperty: "Synthetic.Sales.Header.DocumentType",
            },
            {
              ...snapshot.entities[0].fields[0],
              fieldId: "customerReference",
              label: "Customer reference",
              businessRequired: false,
              key: false,
              technicalProperty: "Synthetic.Sales.Header.CustomerReference",
            },
          ],
        },
        {
          entityId: "SalesDocumentItem",
          label: "Sales document item",
          root: false,
          technicalEntity: "Synthetic.Sales.Item",
          fields: [
            {
              ...snapshot.entities[0].fields[0],
              fieldId: "sourceItemId",
              label: "Source item ID",
              technicalProperty: "Synthetic.Sales.Item.ID",
            },
          ],
        },
      ],
      rules: [],
    } satisfies DestinationDraftSnapshot;
    const ruleDraft = {
      ...workingDraft,
      draftId: "destination-draft-typed-rules",
      draftHash: hash("3"),
      snapshot: ruleSnapshot,
    } satisfies DestinationDraftView;
    const savedRequests: CapturedRequest[] = [];
    renderDestination({
      "GET /api/v1/engagement/destination": {
        ...emptyWorkshop,
        latestArtifact: artifact,
        draft: ruleDraft,
      } satisfies DestinationWorkshop,
      "GET /api/v1/engagement/destination/catalogue/properties": {
        ...propertyPage,
        draftId: ruleDraft.draftId,
        draftGeneration: ruleDraft.generation,
        draftHash: ruleDraft.draftHash,
      } satisfies DestinationCataloguePropertyPage,
      "POST /api/v1/engagement/destination/drafts/save": (
        _url: URL,
        request: CapturedRequest,
      ) => {
        savedRequests.push(request);
        return {
          ...ruleDraft,
          generation: ruleDraft.generation + 1,
          draftHash: hash("4"),
          snapshot: body(request).snapshot,
          updatedAt: "2026-07-30T18:10:00Z",
        };
      },
    });

    await screen.findByRole("heading", { name: "Bounded validation rules" });
    await user.type(screen.getByLabelText("Rule ID"), "customer-ref-required");
    await user.selectOptions(
      screen.getByLabelText("Target field"),
      "SalesDocument.customerReference",
    );
    await user.selectOptions(
      screen.getByLabelText("Rule kind"),
      "conditional_required",
    );
    const conditionField = screen.getByLabelText("Condition field");
    expect(conditionField).toBeRequired();
    expect(
      within(conditionField)
        .getAllByRole("option")
        .map((option) => option.getAttribute("value")),
    ).toEqual([
      "",
      "SalesDocument.sourceDocumentId",
      "SalesDocument.documentType",
      "SalesDocument.customerReference",
    ]);
    expect(
      within(conditionField).queryByRole("option", {
        name: "SalesDocumentItem.sourceItemId",
      }),
    ).not.toBeInTheDocument();
    await user.selectOptions(conditionField, "SalesDocument.documentType");
    const conditionEquals = screen.getByLabelText("Condition equals");
    expect(conditionEquals).toBeRequired();
    await user.type(conditionEquals, "RETURN");
    await user.type(
      screen.getByLabelText("Message"),
      "Customer reference is required for returns.",
    );
    await user.click(screen.getByRole("button", { name: "Add bounded rule" }));

    await user.type(screen.getByLabelText("Rule ID"), "document-type-domain");
    await user.selectOptions(
      screen.getByLabelText("Target field"),
      "SalesDocument.documentType",
    );
    await user.selectOptions(
      screen.getByLabelText("Rule kind"),
      "reference_lookup",
    );
    const referenceTableId = screen.getByLabelText(
      "Required reference table ID",
    );
    const referenceTableVersion = screen.getByLabelText(
      "Required reference table version",
    );
    expect(referenceTableId).toBeRequired();
    expect(referenceTableId).toHaveAttribute("pattern", "[a-z0-9][a-z0-9.-]*");
    expect(referenceTableVersion).toBeRequired();
    expect(referenceTableVersion).toHaveAttribute(
      "pattern",
      "\\d+\\.\\d+\\.\\d+",
    );
    await user.type(referenceTableId, "document-type-domain");
    await user.type(referenceTableVersion, "1.0.0");
    await user.type(
      screen.getByLabelText("Message"),
      "Document type must exist in the reviewed domain.",
    );
    await user.click(screen.getByRole("button", { name: "Add bounded rule" }));

    const durableCard = screen
      .getByRole("heading", { name: "Durable draft generation" })
      .closest("section");
    expect(durableCard).not.toBeNull();
    await user.type(
      within(durableCard!).getByLabelText("Revision author"),
      "Rule author",
    );
    await user.type(
      within(durableCard!).getByLabelText("Revision reason"),
      "Add exact conditional and reference obligations.",
    );
    await user.click(
      within(durableCard!).getByRole("button", {
        name: "Save working draft",
      }),
    );

    await waitFor(() => expect(savedRequests).toHaveLength(1));
    expect(
      (body(savedRequests[0]).snapshot as DestinationDraftSnapshot).rules,
    ).toEqual([
      {
        ruleId: "customer-ref-required",
        severity: "error",
        kind: "conditional_required",
        target: "SalesDocument.customerReference",
        message: "Customer reference is required for returns.",
        parameters: {
          whenField: "SalesDocument.documentType",
          equals: "RETURN",
        },
      },
      {
        ruleId: "document-type-domain",
        severity: "error",
        kind: "reference_lookup",
        target: "SalesDocument.documentType",
        message: "Document type must exist in the reviewed domain.",
        parameters: {
          tableId: "document-type-domain",
          tableVersion: "1.0.0",
        },
      },
    ]);
  });

  it("keeps duplicate field IDs qualified by entity when one canonical field is renamed", async () => {
    const user = userEvent.setup();
    const qualifiedSnapshot = {
      ...snapshot,
      entities: [
        {
          ...snapshot.entities[0],
          entityId: "Root",
          fields: [
            {
              ...snapshot.entities[0].fields[0],
              fieldId: "id",
            },
          ],
        },
        {
          entityId: "Child",
          label: "Child",
          root: false,
          technicalEntity: "Synthetic.Sales.Item",
          fields: [
            {
              ...snapshot.entities[0].fields[0],
              fieldId: "id",
              label: "Child ID",
              technicalProperty: "Synthetic.Sales.Item.ID",
            },
          ],
        },
      ],
      rules: [
        {
          ruleId: "root-id-unique",
          severity: "error",
          kind: "unique",
          target: "Root.id",
          parameters: { scopeFields: ["id"] },
          message: "Root ID must be unique.",
        },
        {
          ruleId: "child-id-unique",
          severity: "error",
          kind: "unique",
          target: "Child.id",
          parameters: { scopeFields: ["id"] },
          message: "Child ID must be unique.",
        },
      ],
    } satisfies DestinationDraftSnapshot;
    const qualifiedDraft = {
      ...workingDraft,
      draftId: "destination-draft-qualified-fields",
      draftHash: hash("3"),
      snapshot: qualifiedSnapshot,
    } satisfies DestinationDraftView;
    const savedRequests: CapturedRequest[] = [];
    renderDestination({
      "GET /api/v1/engagement/destination": {
        ...emptyWorkshop,
        latestArtifact: artifact,
        draft: qualifiedDraft,
      } satisfies DestinationWorkshop,
      "GET /api/v1/engagement/destination/catalogue/properties": {
        ...propertyPage,
        draftId: qualifiedDraft.draftId,
        draftGeneration: qualifiedDraft.generation,
        draftHash: qualifiedDraft.draftHash,
        items: [
          {
            ...propertyPage.items[0],
            field: qualifiedSnapshot.entities[0].fields[0],
          },
        ],
      } satisfies DestinationCataloguePropertyPage,
      "POST /api/v1/engagement/destination/drafts/save": (
        _url: URL,
        request: CapturedRequest,
      ) => {
        savedRequests.push(request);
        return {
          ...qualifiedDraft,
          generation: qualifiedDraft.generation + 1,
          draftHash: hash("0"),
          snapshot: body(request).snapshot,
          updatedAt: "2026-07-30T09:06:00Z",
        };
      },
    });

    const rootField = await screen.findByLabelText(
      "Canonical field ID for Root.id",
    );
    await user.clear(rootField);
    await user.type(rootField, "rootId");
    const durableCard = screen
      .getByRole("heading", { name: "Durable draft generation" })
      .closest("section");
    expect(durableCard).not.toBeNull();
    await user.type(
      within(durableCard!).getByLabelText("Revision author"),
      "Profile author",
    );
    await user.type(
      within(durableCard!).getByLabelText("Revision reason"),
      "Rename only the qualified root field.",
    );
    await user.click(
      within(durableCard!).getByRole("button", {
        name: "Save working draft",
      }),
    );
    await waitFor(() => expect(savedRequests).toHaveLength(1));
    const savedSnapshot = body(savedRequests[0])
      .snapshot as DestinationDraftSnapshot;
    expect(savedSnapshot.rules).toEqual([
      expect.objectContaining({
        target: "Root.rootId",
        parameters: { scopeFields: ["rootId"] },
      }),
      expect.objectContaining({
        target: "Child.id",
        parameters: { scopeFields: ["id"] },
      }),
    ]);
  });

  it("removes stale required and maximum-length rules when their operator decisions are withdrawn", async () => {
    const user = userEvent.setup();
    const decisionSnapshot = {
      ...snapshot,
      entities: [
        {
          ...snapshot.entities[0],
          fields: [
            {
              ...snapshot.entities[0].fields[0],
              confirmMaxLength: true,
            },
          ],
        },
      ],
      rules: [
        ...snapshot.rules,
        {
          ruleId: "sales-document-id-length",
          severity: "error",
          kind: "max_length",
          target: "SalesDocument.sourceDocumentId",
          parameters: { maximum: 35 },
          message: "Document ID must fit the reviewed technical limit.",
        },
      ],
    } satisfies DestinationDraftSnapshot;
    const decisionDraft = {
      ...workingDraft,
      snapshot: decisionSnapshot,
    } satisfies DestinationDraftView;
    const savedRequests: CapturedRequest[] = [];
    renderDestination({
      "GET /api/v1/engagement/destination": {
        ...emptyWorkshop,
        latestArtifact: artifact,
        draft: decisionDraft,
      } satisfies DestinationWorkshop,
      "GET /api/v1/engagement/destination/catalogue/properties": {
        ...propertyPage,
        items: [
          {
            ...propertyPage.items[0],
            field: decisionSnapshot.entities[0].fields[0],
          },
        ],
      } satisfies DestinationCataloguePropertyPage,
      "POST /api/v1/engagement/destination/drafts/save": (
        _url: URL,
        request: CapturedRequest,
      ) => {
        savedRequests.push(request);
        const command = body(request);
        return {
          ...decisionDraft,
          generation: decisionDraft.generation + 1,
          draftHash: hash("0"),
          snapshot: command.snapshot,
          updatedAt: "2026-07-30T09:05:00Z",
        };
      },
    });

    await user.click(
      await screen.findByLabelText(
        "Business required for SalesDocument.sourceDocumentId",
      ),
    );
    await user.click(
      screen.getByLabelText(
        "Enforce reviewed maximum length for SalesDocument.sourceDocumentId",
      ),
    );
    expect(
      screen.getByText("No explicit validation rule is in this draft."),
    ).toBeVisible();

    const durableCard = screen
      .getByRole("heading", { name: "Durable draft generation" })
      .closest("section");
    expect(durableCard).not.toBeNull();
    await user.type(
      within(durableCard!).getByLabelText("Revision author"),
      "Profile author",
    );
    await user.type(
      within(durableCard!).getByLabelText("Revision reason"),
      "Withdraw both enforcement decisions.",
    );
    await user.click(
      within(durableCard!).getByRole("button", {
        name: "Save working draft",
      }),
    );

    await waitFor(() => expect(savedRequests).toHaveLength(1));
    expect(
      (body(savedRequests[0]).snapshot as DestinationDraftSnapshot).rules,
    ).toEqual([]);
  });

  it("requires attributed acceptance for changed technical evidence and never offers it for removed evidence", async () => {
    const user = userEvent.setup();
    const changedField = {
      ...snapshot.entities[0].fields[0],
      state: "ACTIVE",
      diffState: "TECHNICALLY_CHANGED",
      technicalChangeReview: null,
    } satisfies DestinationDraftSnapshot["entities"][number]["fields"][number];
    const removedField = {
      ...snapshot.entities[0].fields[0],
      fieldId: "legacyCode",
      label: "Legacy code",
      key: false,
      businessRequired: false,
      state: "NEEDS_REVIEW",
      technicalProperty: "Synthetic.Sales.Header.LegacyCode",
      diffState: "REMOVED",
      technicalChangeReview: null,
    } satisfies DestinationDraftSnapshot["entities"][number]["fields"][number];
    const changedSnapshot = {
      ...snapshot,
      entities: [
        {
          ...snapshot.entities[0],
          fields: [changedField, removedField],
        },
      ],
    } satisfies DestinationDraftSnapshot;
    const changedDraft = {
      ...workingDraft,
      draftId: "destination-draft-technical-change",
      catalogueAuthorityId: revisedArtifact.catalogueAuthorityId,
      catalogueHash: revisedArtifact.catalogueHash,
      draftHash: hash("8"),
      snapshot: changedSnapshot,
    } satisfies DestinationDraftView;
    const savedRequests: CapturedRequest[] = [];
    renderDestination({
      "GET /api/v1/engagement/destination": {
        ...emptyWorkshop,
        latestArtifact: revisedArtifact,
        draft: changedDraft,
      } satisfies DestinationWorkshop,
      "GET /api/v1/engagement/destination/catalogue/properties": {
        ...propertyPage,
        catalogueAuthorityId: revisedArtifact.catalogueAuthorityId,
        catalogueHash: revisedArtifact.catalogueHash,
        draftId: changedDraft.draftId,
        draftGeneration: changedDraft.generation,
        draftHash: changedDraft.draftHash,
        items: [
          {
            ...propertyPage.items[0],
            field: changedField,
          },
        ],
      } satisfies DestinationCataloguePropertyPage,
      "POST /api/v1/engagement/destination/drafts/save": (
        _url: URL,
        request: CapturedRequest,
      ) => {
        savedRequests.push(request);
        return {
          ...changedDraft,
          generation: changedDraft.generation + 1,
          draftHash: hash("0"),
          snapshot: body(request).snapshot,
          updatedAt: "2026-07-30T09:08:00Z",
        };
      },
    });

    const changedRow = (
      await screen.findByLabelText(
        "Canonical field ID for SalesDocument.sourceDocumentId",
      )
    ).closest("tr");
    const removedRow = screen
      .getByLabelText("Canonical field ID for SalesDocument.legacyCode")
      .closest("tr");
    expect(changedRow).not.toBeNull();
    expect(removedRow).not.toBeNull();
    expect(
      within(changedRow!).queryByLabelText(
        "Confirm field SalesDocument.sourceDocumentId",
      ),
    ).toBeNull();
    expect(
      within(removedRow!).queryByRole("button", {
        name: "Accept revised technical evidence",
      }),
    ).toBeNull();

    await user.type(
      within(changedRow!).getByLabelText("Technical-change reviewer"),
      "Change reviewer",
    );
    await user.type(
      within(changedRow!).getByLabelText("Technical-change reason"),
      "The revised bounded technical signature was checked.",
    );
    await user.click(
      within(changedRow!).getByRole("button", {
        name: "Accept revised technical evidence",
      }),
    );
    expect(
      within(changedRow!).getByText(
        "Revised technical evidence accepted by Change reviewer",
      ),
    ).toBeVisible();

    const durableCard = screen
      .getByRole("heading", { name: "Durable draft generation" })
      .closest("section");
    expect(durableCard).not.toBeNull();
    await user.type(
      within(durableCard!).getByLabelText("Revision author"),
      "Change reviewer",
    );
    await user.type(
      within(durableCard!).getByLabelText("Revision reason"),
      "Record exact technical-change acceptance.",
    );
    await user.click(
      within(durableCard!).getByRole("button", {
        name: "Save working draft",
      }),
    );
    await waitFor(() => expect(savedRequests).toHaveLength(1));
    const savedSnapshot = body(savedRequests[0])
      .snapshot as DestinationDraftSnapshot;
    expect(savedSnapshot.entities[0].fields[0]).toMatchObject({
      state: "ACTIVE",
      diffState: "TECHNICALLY_CHANGED",
      technicalChangeReview: {
        decision: "ACCEPT_REVISED_TECHNICAL_EVIDENCE",
        actor: "Change reviewer",
        reason: "The revised bounded technical signature was checked.",
      },
    });

    await user.click(
      within(changedRow!).getByLabelText(
        "Return SalesDocument.sourceDocumentId to review",
      ),
    );
    expect(
      within(changedRow!).queryByText(/accepted by Change reviewer/),
    ).toBeNull();
    expect(
      within(changedRow!).getByRole("button", {
        name: "Accept revised technical evidence",
      }),
    ).toBeVisible();
  });

  it("blocks field deactivation until dependent relationships are deliberately removed", async () => {
    const user = userEvent.setup();
    const relationshipSnapshot = {
      ...multiEntitySnapshot,
      relationships: [
        {
          relationshipId: "sales-document.items",
          fromEntityId: "SalesDocument",
          toEntityId: "SalesDocumentItem",
          cardinality: "1..*",
          technicalNavigation: "Synthetic.Sales.Header.Items",
          keyPairs: [
            {
              fromFieldId: "sellingUnit",
              toFieldId: "parentSellingUnit",
            },
          ],
          confirmed: true,
        },
      ],
      rules: [
        ...multiEntitySnapshot.rules,
        {
          ruleId: "selling-unit-required",
          severity: "error",
          kind: "required",
          target: "SalesDocument.sellingUnit",
          parameters: {},
          message: "Selling unit is required.",
        },
        {
          ruleId: "document-selling-unit-unique",
          severity: "error",
          kind: "unique",
          target: "SalesDocument.sourceDocumentId",
          parameters: {
            scopeFields: ["sourceDocumentId", "sellingUnit"],
          },
          message: "Document and selling unit must be unique.",
        },
      ],
    } satisfies DestinationDraftSnapshot;
    const relationshipDraft = {
      ...multiEntityDraft,
      draftHash: hash("a"),
      snapshot: relationshipSnapshot,
    } satisfies DestinationDraftView;
    const savedRequests: CapturedRequest[] = [];
    const rootItems: DestinationCataloguePropertyPage["items"] = [
      {
        ...propertyPage.items[0],
        field: relationshipSnapshot.entities[0].fields[0],
      },
      {
        ...propertyPage.items[0],
        technicalProperty: "Synthetic.Sales.Header.SellingUnit",
        name: "SellingUnit",
        maxLength: "8",
        field: relationshipSnapshot.entities[0].fields[1],
      },
    ];
    renderDestination({
      "GET /api/v1/engagement/destination": {
        ...emptyWorkshop,
        latestArtifact: artifact,
        draft: relationshipDraft,
      } satisfies DestinationWorkshop,
      "GET /api/v1/engagement/destination/catalogue/properties": propertiesFor(
        relationshipDraft,
        rootItems,
      ),
      "POST /api/v1/engagement/destination/drafts/save": (
        _url: URL,
        request: CapturedRequest,
      ) => {
        savedRequests.push(request);
        return {
          ...relationshipDraft,
          generation: relationshipDraft.generation + 1,
          draftHash: hash("0"),
          snapshot: body(request).snapshot,
          updatedAt: "2026-07-30T09:09:00Z",
        };
      },
    });

    const propertyDeactivate = await screen.findByLabelText(
      "Deactivate Synthetic.Sales.Header.SellingUnit",
    );
    const draftDeactivate = screen.getByLabelText(
      "Deactivate SalesDocument.sellingUnit",
    );
    expect(propertyDeactivate).toBeDisabled();
    expect(draftDeactivate).toBeDisabled();
    expect(
      screen.getAllByText(
        /Remove or remap relationship sales-document.items before deactivating this field/,
      ),
    ).toHaveLength(3);

    await user.click(
      screen.getByRole("button", { name: "Remove relationship" }),
    );
    expect(propertyDeactivate).toBeEnabled();
    expect(draftDeactivate).toBeEnabled();
    await user.click(draftDeactivate);

    const durableCard = screen
      .getByRole("heading", { name: "Durable draft generation" })
      .closest("section");
    expect(durableCard).not.toBeNull();
    await user.type(
      within(durableCard!).getByLabelText("Revision author"),
      "Relationship reviewer",
    );
    await user.type(
      within(durableCard!).getByLabelText("Revision reason"),
      "Remove the relationship before deactivating its key field.",
    );
    await user.click(
      within(durableCard!).getByRole("button", {
        name: "Save working draft",
      }),
    );
    await waitFor(() => expect(savedRequests).toHaveLength(1));
    const savedSnapshot = body(savedRequests[0])
      .snapshot as DestinationDraftSnapshot;
    expect(savedSnapshot.relationships).toEqual([]);
    expect(
      savedSnapshot.entities[0].fields.some(
        (field) => field.fieldId === "sellingUnit",
      ),
    ).toBe(false);
    expect(
      savedSnapshot.rules.some(
        (rule) =>
          rule.target === "SalesDocument.sellingUnit" ||
          (rule.kind === "unique" &&
            JSON.stringify(rule.parameters).includes("sellingUnit")),
      ),
    ).toBe(false);
  });

  it("records a bounded composite relationship without treating child foreign keys as canonical keys", async () => {
    const user = userEvent.setup();
    const savedRequests: CapturedRequest[] = [];
    const rootItems: DestinationCataloguePropertyPage["items"] = [
      {
        ...propertyPage.items[0],
        field: multiEntitySnapshot.entities[0].fields[0],
      },
      {
        ...propertyPage.items[0],
        technicalProperty: "Synthetic.Sales.Header.SellingUnit",
        name: "SellingUnit",
        maxLength: "8",
        field: multiEntitySnapshot.entities[0].fields[1],
      },
    ];
    const { requests } = renderDestination({
      "GET /api/v1/engagement/destination": {
        ...emptyWorkshop,
        latestArtifact: artifact,
        draft: multiEntityDraft,
      } satisfies DestinationWorkshop,
      "GET /api/v1/engagement/destination/catalogue/properties": propertiesFor(
        multiEntityDraft,
        rootItems,
      ),
      "POST /api/v1/engagement/destination/drafts/save": (
        _url: URL,
        request: CapturedRequest,
      ) => {
        savedRequests.push(request);
        const command = body(request);
        return {
          ...multiEntityDraft,
          generation: multiEntityDraft.generation + 1,
          draftHash: hash("0"),
          snapshot: command.snapshot,
          updatedAt: "2026-07-30T09:10:00Z",
        };
      },
    });

    await user.type(
      await screen.findByLabelText("Relationship ID"),
      "sales-document.items",
    );
    await user.selectOptions(
      screen.getByLabelText("Root canonical key · pair 1"),
      "sourceDocumentId",
    );
    await user.selectOptions(
      screen.getByLabelText("Child relationship field · pair 1"),
      "parentDocumentId",
    );
    await user.click(
      screen.getByRole("button", { name: "Add relationship key pair" }),
    );
    await user.selectOptions(
      screen.getByLabelText("Root canonical key · pair 2"),
      "sellingUnit",
    );
    await user.selectOptions(
      screen.getByLabelText("Child relationship field · pair 2"),
      "parentSellingUnit",
    );
    await user.selectOptions(screen.getByLabelText("Cardinality"), "1..*");
    await user.click(
      screen.getByRole("checkbox", {
        name: /I confirm this exact key-pair set/,
      }),
    );
    await user.click(
      screen.getByRole("button", { name: "Add confirmed relationship" }),
    );

    expect(
      screen.getByText(
        "SalesDocument.sourceDocumentId = SalesDocumentItem.parentDocumentId",
      ),
    ).toBeVisible();
    expect(
      screen.getByText(
        /SalesDocument\.sellingUnit = SalesDocumentItem\.parentSellingUnit/,
      ),
    ).toBeVisible();

    const durableCard = screen
      .getByRole("heading", { name: "Durable draft generation" })
      .closest("section");
    expect(durableCard).not.toBeNull();
    await user.type(
      within(durableCard!).getByLabelText("Revision author"),
      "Relationship reviewer",
    );
    await user.type(
      within(durableCard!).getByLabelText("Revision reason"),
      "Confirm exact two-part parent key.",
    );
    await user.click(
      within(durableCard!).getByRole("button", {
        name: "Save working draft",
      }),
    );

    await waitFor(() => expect(savedRequests).toHaveLength(1));
    const command = body(savedRequests[0]);
    const savedSnapshot = command.snapshot as DestinationDraftSnapshot;
    expect(savedSnapshot.relationships).toEqual([
      expect.objectContaining({
        relationshipId: "sales-document.items",
        cardinality: "1..*",
        keyPairs: [
          {
            fromFieldId: "sourceDocumentId",
            toFieldId: "parentDocumentId",
          },
          {
            fromFieldId: "sellingUnit",
            toFieldId: "parentSellingUnit",
          },
        ],
      }),
    ]);
    expect(
      savedSnapshot.entities[1].fields.find(
        (field) => field.fieldId === "parentDocumentId",
      )?.key,
    ).toBe(false);
    expect(
      requests.some(
        (request) =>
          request.url.pathname ===
          "/api/v1/engagement/destination/drafts/freeze",
      ),
    ).toBe(false);

    await user.click(
      screen.getByLabelText("Canonical key for SalesDocument.sourceDocumentId"),
    );
    expect(screen.getByText("Needs explicit confirmation")).toBeVisible();
  });

  it("rejects duplicate manual IDs and requires an explicit technical link while retaining manual provenance", async () => {
    const user = userEvent.setup();
    const availableProperty: DestinationCataloguePropertyPage["items"][number] =
      {
        ...propertyPage.items[0],
        technicalProperty: "Synthetic.Sales.Header.ExternalReference",
        name: "ExternalReference",
        nullable: true,
        maxLength: "40",
        keyCandidate: false,
        requiredCandidate: false,
        state: "AVAILABLE",
        field: null,
      };
    const savedRequests: CapturedRequest[] = [];
    renderDestination({
      "GET /api/v1/engagement/destination": {
        ...emptyWorkshop,
        latestArtifact: artifact,
        draft: manualDraft,
      } satisfies DestinationWorkshop,
      "GET /api/v1/engagement/destination/catalogue/properties": propertiesFor(
        manualDraft,
        [
          {
            ...propertyPage.items[0],
            field: manualSnapshot.entities[0].fields[0],
          },
          availableProperty,
        ],
      ),
      "POST /api/v1/engagement/destination/drafts/save": (
        _url: URL,
        request: CapturedRequest,
      ) => {
        savedRequests.push(request);
        const command = body(request);
        return {
          ...manualDraft,
          generation: manualDraft.generation + 1,
          draftHash: hash("5"),
          snapshot: command.snapshot,
          updatedAt: "2026-07-30T09:15:00Z",
        };
      },
    });

    const manualCard = (
      await screen.findByRole("heading", {
        name: "Add a manual supplemental field",
      })
    ).closest("section");
    expect(manualCard).not.toBeNull();
    await user.selectOptions(
      within(manualCard!).getByLabelText("Entity"),
      "SalesDocument",
    );
    await user.type(
      within(manualCard!).getByLabelText("Supplement ID"),
      "duplicate-attempt",
    );
    await user.type(
      within(manualCard!).getByLabelText("Canonical field ID"),
      "sourceDocumentId",
    );
    await user.type(
      within(manualCard!).getByLabelText("Label"),
      "Should not replace",
    );
    await user.type(
      within(manualCard!).getByLabelText("Actor"),
      "Profile author",
    );
    await user.type(
      within(manualCard!).getByLabelText("Reason"),
      "Exercise duplicate protection.",
    );
    await user.click(
      within(manualCard!).getByRole("button", {
        name: "Add manual field for review",
      }),
    );
    expect(within(manualCard!).getByRole("alert")).toHaveTextContent(
      "SalesDocument.sourceDocumentId already exists",
    );

    const linkSelection = screen.getByLabelText(
      "Manual field to link to Synthetic.Sales.Header.ExternalReference",
    );
    expect(
      within(linkSelection).getByRole("option", {
        name: "customerReference",
      }),
    ).toBeVisible();
    expect(
      within(linkSelection).getByRole("option", { name: "operatorNote" }),
    ).toBeVisible();
    await user.selectOptions(linkSelection, "customerReference");
    await user.click(
      screen.getByRole("button", {
        name: "Link Synthetic.Sales.Header.ExternalReference to selected manual field",
      }),
    );

    expect(
      screen.getByText(
        /Manual supplement manual-customer-reference by Profile author · explicitly linked/,
      ),
    ).toBeVisible();

    const durableCard = screen
      .getByRole("heading", { name: "Durable draft generation" })
      .closest("section");
    expect(durableCard).not.toBeNull();
    await user.type(
      within(durableCard!).getByLabelText("Revision author"),
      "Profile author",
    );
    await user.type(
      within(durableCard!).getByLabelText("Revision reason"),
      "Link the retained canonical field to exact revised evidence.",
    );
    await user.click(
      within(durableCard!).getByRole("button", {
        name: "Save working draft",
      }),
    );

    await waitFor(() => expect(savedRequests).toHaveLength(1));
    const savedSnapshot = body(savedRequests[0])
      .snapshot as DestinationDraftSnapshot;
    const linked = savedSnapshot.entities[0].fields.find(
      (field) => field.fieldId === "customerReference",
    );
    expect(linked).toMatchObject({
      technicalProperty: "Synthetic.Sales.Header.ExternalReference",
      manualSupplement: {
        supplementId: "manual-customer-reference",
        actor: "Profile author",
      },
    });
    expect(
      savedSnapshot.entities[0].fields.filter(
        (field) => field.fieldId === "sourceDocumentId",
      ),
    ).toHaveLength(1);
  });

  it("approves a revised candidate without silently replacing the current active authority", async () => {
    const user = userEvent.setup();
    let candidateApproved = false;
    const priorActive = {
      ...approvedActive,
      destinationAuthorityId: "destination-origin-prior-approval",
      destinationOriginHash: hash("6"),
      profileId: "workspace.sales-document.prior",
      profileVersion: "0.9.0",
      profileHash: hash("8"),
      approvalId: "profile-approval-prior",
      approvalHash: hash("7"),
    } satisfies ActiveDestinationView;
    const eligiblePriorMapping = {
      mappingId: "mapping-prior-approval",
      mappingVersion: "0.9.0",
      mappingHash: hash("3"),
      mappingApprovalId: "mapping-approval-prior",
      mappingApprovalHash: hash("2"),
      activeJobId: "job-prior",
      activeSourceHash: hash("1"),
      validatedJobId: "job-prior",
      validatedSourceHash: hash("1"),
      profileId: priorActive.profileId,
      profileVersion: priorActive.profileVersion,
      profileHash: priorActive.profileHash,
    };
    const frozenDraft = {
      ...workingDraft,
      state: "FROZEN",
      frozenProfileId: profileView.profile_id,
      frozenProfileVersion: profileView.profile_version,
      frozenProfileHash: profileView.profile_hash,
    } satisfies DestinationDraftView;
    const { requests } = renderDestination({
      "GET /api/v1/engagement/destination": () =>
        ({
          ...emptyWorkshop,
          engagementGeneration: candidateApproved ? 8 : 7,
          destinationOrigin: "WORKSPACE_PROFILE",
          activeDestination: priorActive,
          reviewedCandidate: candidateApproved ? approvedCandidate : frozen,
          eligibleCarryForwardSource: eligiblePriorMapping,
          latestArtifact: artifact,
          draft: frozenDraft,
        }) satisfies DestinationWorkshop,
      "GET /api/v1/engagement/destination/catalogue/properties": propertyPage,
      "POST /api/v1/engagement/destination/profiles/approve": () => {
        candidateApproved = true;
        return approved;
      },
    });

    await user.type(await screen.findByLabelText("Approver"), "New reviewer");
    await user.type(
      screen.getByLabelText("Approval reason"),
      "Approve the revised exact destination candidate.",
    );
    await user.click(
      screen.getByRole("checkbox", {
        name: /approve this exact REVIEWED_DRAFT profile/,
      }),
    );
    await user.click(
      screen.getByRole("button", { name: "Approve exact profile hash" }),
    );

    const candidateNotice = (
      await screen.findByText("Revised destination candidate approved")
    ).closest("[role='status']");
    expect(candidateNotice).not.toBeNull();
    expect(candidateNotice).toHaveTextContent(
      /Current active authority remains workspace.sales-document.prior/,
    );
    expect(
      body(
        requests.find(
          (request) =>
            request.url.pathname ===
            "/api/v1/engagement/destination/profiles/approve",
        ),
      ),
    ).toMatchObject({
      profileId: profileView.profile_id,
      profileVersion: profileView.profile_version,
      profileHash: profileView.profile_hash,
      originBindingHash: frozen.originBindingHash,
      actor: "New reviewer",
    });
  });

  it("rebases exact durable authority and carries a mapping only from the approved frozen profile", async () => {
    const user = userEvent.setup();
    const rebasedDraft = {
      ...workingDraft,
      draftId: "destination-draft-002",
      catalogueAuthorityId: revisedArtifact.catalogueAuthorityId,
      catalogueHash: revisedArtifact.catalogueHash,
      generation: 1,
      draftHash: hash("1"),
      updatedAt: "2026-07-30T09:20:00Z",
    } satisfies DestinationDraftView;
    const { requests } = renderDestination({
      "GET /api/v1/engagement/destination": {
        ...emptyWorkshop,
        latestArtifact: revisedArtifact,
        draft: workingDraft,
      } satisfies DestinationWorkshop,
      "GET /api/v1/engagement/destination/catalogue/properties": propertyPage,
      "POST /api/v1/engagement/destination/drafts/rebase": {
        schemaVersion: SCHEMAS.destinationRebase,
        engagementGeneration: 4,
        rebaseId: "destination-rebase-001",
        diffHash: hash("2"),
        counts: {
          UNCHANGED: 1,
          ADDED: 1,
          REMOVED: 0,
          TECHNICALLY_CHANGED: 0,
          MANUAL_POTENTIAL_MATCH: 0,
        },
        draft: rebasedDraft,
      },
    });

    await user.type(
      await screen.findByLabelText("Rebase operator"),
      "Profile reviewer",
    );
    await user.type(
      screen.getByLabelText("Rebase reason"),
      "Preserve exact unchanged decisions and expose additions.",
    );
    await user.click(
      screen.getByRole("button", {
        name: "Rebase destination draft onto this snapshot",
      }),
    );
    expect(await screen.findByText("New editable draft created")).toBeVisible();
    expect(
      body(requests.find((request) => request.method === "POST")),
    ).toMatchObject({
      schemaVersion: DESTINATION_COMMAND_SCHEMAS.rebaseDraft,
      draftId: workingDraft.draftId,
      expectedDraftGeneration: workingDraft.generation,
      expectedDraftHash: workingDraft.draftHash,
      targetCatalogueAuthorityId: revisedArtifact.catalogueAuthorityId,
      expectedTargetCatalogueHash: revisedArtifact.catalogueHash,
    });

    cleanup();
    window.history.replaceState(null, "", "/destination");
    const frozenDraft = {
      ...workingDraft,
      state: "FROZEN",
      frozenProfileId: approvedActive.profileId,
      frozenProfileVersion: approvedActive.profileVersion,
      frozenProfileHash: approvedActive.profileHash,
    } satisfies DestinationDraftView;
    const priorActive = {
      ...approvedActive,
      destinationAuthorityId: "destination-origin-prior-001",
      destinationOriginHash: hash("6"),
      profileId: "workspace.sales-document.prior",
      profileVersion: "0.9.0",
      profileHash: hash("8"),
      approvalId: "profile-approval-prior-001",
      approvalHash: hash("7"),
    } satisfies ActiveDestinationView;
    const eligibleCarryForwardSource = {
      mappingId: "mapping-001",
      mappingVersion: "1.0.0",
      mappingHash: hash("3"),
      mappingApprovalId: "mapping-approval-001",
      mappingApprovalHash: hash("2"),
      activeJobId: "job-current-source",
      activeSourceHash: hash("0"),
      validatedJobId: "job-mapping-validated",
      validatedSourceHash: hash("1"),
      profileId: priorActive.profileId,
      profileVersion: priorActive.profileVersion,
      profileHash: priorActive.profileHash,
    };
    const carry = mockApi({
      "GET /api/v1/context": context(),
      "GET /api/v1/journey": destinationJourney(),
      "GET /api/v1/engagement/destination": {
        ...emptyWorkshop,
        engagementGeneration: 7,
        destinationOrigin: "WORKSPACE_PROFILE",
        activeDestination: priorActive,
        reviewedCandidate: approvedCandidate,
        eligibleCarryForwardSource,
        latestArtifact: artifact,
        draft: frozenDraft,
      } satisfies DestinationWorkshop,
      "GET /api/v1/engagement/destination/catalogue/properties": propertyPage,
      "POST /api/v1/engagement/destination/mappings/carry-forward": {
        schemaVersion: SCHEMAS.destinationCarryForward,
        engagementGeneration: 8,
        carryForwardId: "destination-carry-forward-001",
        resultHash: hash("4"),
        clonedBindingCount: 10,
        unresolvedTargetCount: 1,
        unresolvedRelationshipCount: 1,
        unresolvedRelationships: [
          {
            relationshipId: "sales-document.items",
            reason: "RELATIONSHIP_CHANGED",
          },
        ],
      },
    });
    render(<App />);
    expect(
      await screen.findByText("mapping-approval-001", { exact: false }),
    ).toBeVisible();
    expect(screen.getByText("job-current-source")).toBeVisible();
    expect(screen.getByText("job-mapping-validated")).toBeVisible();
    expect(screen.queryByLabelText("Prior mapping ID")).toBeNull();
    await user.type(screen.getByLabelText("Operator"), "Mapping reviewer");
    await user.type(
      screen.getByLabelText("Carry-forward reason"),
      "Carry only exact compatible target slots.",
    );
    await user.click(
      screen.getByRole("button", {
        name: "Create compatible mapping draft",
      }),
    );
    expect(
      await screen.findByText("Editable carry-forward draft created"),
    ).toBeVisible();
    expect(
      screen.getByText(
        /10 exact compatible bindings cloned · 1 target slots unresolved/,
      ),
    ).toBeVisible();
    expect(
      body(
        carry.requests.find(
          (request) =>
            request.url.pathname ===
            "/api/v1/engagement/destination/mappings/carry-forward",
        ),
      ),
    ).toMatchObject({
      schemaVersion: DESTINATION_COMMAND_SCHEMAS.carryForwardMapping,
      sourceMappingId: "mapping-001",
      sourceMappingVersion: "1.0.0",
      sourceMappingHash: hash("3"),
      targetProfileId: approvedActive.profileId,
      targetProfileVersion: approvedActive.profileVersion,
      targetProfileHash: approvedActive.profileHash,
    });
    expect(screen.getByText(/1 relationships unresolved/)).toBeVisible();
    expect(screen.getByText("sales-document.items")).toBeVisible();
    expect(
      screen.getByText(/exact entities, cardinality or key pairs changed/),
    ).toBeVisible();
  });

  it("allocates a visible collision-safe field ID when an activated technical name normalizes to an existing ID", async () => {
    const user = userEvent.setup();
    const collidingProperty: DestinationCataloguePropertyPage["items"][number] =
      {
        ...propertyPage.items[0],
        technicalProperty: "Synthetic.Sales.Header.Source-Document-Identifier",
        name: "SourceDocumentId",
        nullable: true,
        maxLength: "50",
        keyCandidate: false,
        requiredCandidate: false,
        state: "AVAILABLE",
        field: null,
      };
    renderDestination({
      "GET /api/v1/engagement/destination": {
        ...emptyWorkshop,
        latestArtifact: artifact,
        draft: workingDraft,
      } satisfies DestinationWorkshop,
      "GET /api/v1/engagement/destination/catalogue/properties": propertiesFor(
        workingDraft,
        [propertyPage.items[0], collidingProperty],
      ),
    });

    await user.click(
      await screen.findByLabelText(
        `Activate ${collidingProperty.technicalProperty} for review`,
      ),
    );

    expect(
      screen.getByLabelText(
        "Canonical field ID for SalesDocument.sourceDocumentId_2",
      ),
    ).toHaveValue("sourceDocumentId_2");
    expect(
      screen.getAllByText(collidingProperty.technicalProperty),
    ).toHaveLength(2);
  });

  it("paginates entities with unique controls and surfaces stale draft conflicts without false authority", async () => {
    const user = userEvent.setup();
    const firstPage = {
      ...entityPage,
      nextCursor: "cursor-2",
    } satisfies DestinationCatalogueEntityPage;
    const secondPage = {
      ...entityPage,
      items: [
        {
          technicalEntity: "Synthetic.Sales.Item",
          namespace: "Synthetic.Sales",
          name: "Item",
          keyProperties: ["ID"],
          propertyCount: 18,
          navigationCount: 0,
          selected: false,
          root: false,
        },
      ],
      nextCursor: null,
    } satisfies DestinationCatalogueEntityPage;
    const start = renderDestination({
      "GET /api/v1/engagement/destination": {
        ...emptyWorkshop,
        latestArtifact: artifact,
      } satisfies DestinationWorkshop,
      "GET /api/v1/engagement/destination/catalogue/entities": (url: URL) =>
        url.searchParams.get("cursor") === "cursor-2" ? secondPage : firstPage,
    });

    expect(
      await screen.findByLabelText("Synthetic.Sales.Header as root"),
    ).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Next entity page" }));
    expect(
      await screen.findByLabelText("Synthetic.Sales.Item as root"),
    ).toBeVisible();
    expect(
      screen.getByLabelText("Synthetic.Sales.Item as direct child"),
    ).toBeVisible();
    expect(
      start.requests.some(
        (request) =>
          request.url.pathname ===
            "/api/v1/engagement/destination/catalogue/entities" &&
          request.url.searchParams.get("cursor") === "cursor-2",
      ),
    ).toBe(true);

    cleanup();
    window.history.replaceState(null, "", "/destination");
    const stale = mockApi({
      "GET /api/v1/context": context(),
      "GET /api/v1/journey": destinationJourney(),
      "GET /api/v1/engagement/destination": {
        ...emptyWorkshop,
        latestArtifact: artifact,
        draft: workingDraft,
      } satisfies DestinationWorkshop,
      "GET /api/v1/engagement/destination/catalogue/properties": propertyPage,
      "POST /api/v1/engagement/destination/drafts/save": errorResponse(
        {
          schema_version: SCHEMAS.error,
          ok: false,
          error: {
            code: "STALE_DESTINATION_DRAFT",
            message:
              "The durable destination draft changed before this revision.",
            request_id: "request-stale-destination-draft",
            retryable: false,
          },
        },
        409,
      ),
    });
    render(<App />);
    await user.type(await screen.findByLabelText("Title"), " stale edit");
    const durableCard = screen
      .getByRole("heading", { name: "Durable draft generation" })
      .closest("section");
    expect(durableCard).not.toBeNull();
    await user.type(
      within(durableCard!).getByLabelText("Revision author"),
      "Profile author",
    );
    await user.type(
      within(durableCard!).getByLabelText("Revision reason"),
      "Exercise the stale CAS path.",
    );
    await user.click(
      within(durableCard!).getByRole("button", {
        name: "Save working draft",
      }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "STALE_DESTINATION_DRAFT",
    );
    expect(
      screen.getByRole("button", { name: "Submit Reviewed Draft" }),
    ).toBeDisabled();
    expect(
      stale.requests.some(
        (request) =>
          request.url.pathname ===
          "/api/v1/engagement/destination/drafts/freeze",
      ),
    ).toBe(false);
  });
});
