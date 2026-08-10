export const SCHEMAS = {
  context: "migration-stream/workbench/context/v1",
  governedContext: "migration-stream/workbench/context/v2",
  resultApprovalContext: "migration-stream/workbench/context/v3",
  publicationContext: "migration-stream/workbench/context/v4",
  engagementContext: "migration-stream/engagement/context/v1",
  engagementRecord: "migration-stream/engagement/record/v1",
  engagementState: "migration-stream/engagement/state/v1",
  engagementDetail: "migration-stream/engagement/detail/v1",
  sourcePreflight: "migration-stream/engagement/source-preflight/v1",
  sourceAcceptance: "migration-stream/engagement/source-acceptance-result/v1",
  objectPacks: "migration-stream/engagement/object-packs/v1",
  sourceProfileRules: "migration-stream/engagement/source-profile-rules/v1",
  mappingWorkspace: "migration-stream/engagement/mapping-workspace/v1",
  mappingContractCoverage:
    "migration-stream/engagement/mapping-contract-coverage/v1",
  mappingDraft: "migration-stream/engagement/mapping-draft/v1",
  submittedMapping: "migration-stream/engagement/submitted-mapping/v1",
  mappingApproval: "migration-stream/engagement/mapping-approval/v1",
  preparedMapping: "migration-stream/engagement/prepared-mapping/v1",
  validationLaunch: "migration-stream/engagement/validation-result/v1",
  tourChain: "migration-stream/engagement/tour-chain/v1",
  comparedValidation: "migration-stream/engagement/compared-validation/v1",
  remediationPackOptions:
    "migration-stream/engagement/remediation-pack-options/v1",
  remediationPackHistory:
    "migration-stream/engagement/remediation-pack-history/v1",
  remediationPack: "migration-stream/engagement/remediation-pack/v1",
  journey: "migration-stream/journey/v2",
  objectWorkspace: "migration-stream/object-workspace/v1",
  objectWorkspaceStatus: "migration-stream/object-workspace/status/v1",
  destinationWorkshop: "migration-stream/engagement/destination-workshop/v1",
  destinationCatalogueEntities:
    "migration-stream/engagement/destination-catalogue-entities/v1",
  destinationCatalogueProperties:
    "migration-stream/engagement/destination-catalogue-properties/v1",
  destinationArtifactReservation:
    "migration-stream/engagement/destination-artifact-reservation/v1",
  destinationArtifact: "migration-stream/engagement/destination-artifact/v1",
  destinationDraft: "migration-stream/engagement/destination-draft/v1",
  destinationReviewedProfile:
    "migration-stream/engagement/destination-reviewed-profile/v1",
  destinationProfileApproval:
    "migration-stream/engagement/destination-profile-approval/v1",
  destinationRebase: "migration-stream/engagement/destination-rebase/v1",
  destinationCarryForward:
    "migration-stream/engagement/destination-mapping-carry-forward/v1",
  job: "migration-stream/read/job-summary/v1",
  files: "migration-stream/read/file-manifest/v1",
  profiling: "migration-stream/read/profiling-summary/v1",
  profile: "migration-stream/read/profile-view/v1",
  mapping: "migration-stream/read/mapping-coverage/v1",
  validation: "migration-stream/read/validation-summary/v1",
  issues: "migration-stream/read/issue-list/v1",
  row: "migration-stream/read/row-detail/v1",
  reconciliation: "migration-stream/read/reconciliation-summary/v1",
  operations: "migration-stream/read/operational-cleanup/v1",
  decisionQueue: "migration-stream/read/decision-queue/v1",
  decisionCounts: "migration-stream/read/decision-counts/v1",
  decisionIssue: "migration-stream/read/decision-aware-issue-detail/v1",
  decisionHistory: "migration-stream/read/decision-history/v1",
  correction: "migration-stream/read/correction-overlay-detail/v1",
  decisionResult: "migration-stream/read/decision-result/v1",
  decisionCommandResult: "migration-stream/write/decision-command-result/v1",
  resultReconciliationSnapshots:
    "migration-stream/read/result-reconciliation-snapshot-list/v1",
  resultReconciliationSnapshot:
    "migration-stream/read/result-reconciliation-snapshot-summary/v1",
  resultReconciliationRows:
    "migration-stream/read/result-reconciliation-row-list/v1",
  resultReconciliationRow:
    "migration-stream/read/result-reconciliation-row-detail/v1",
  resultCandidates: "migration-stream/read/result-candidate-list/v1",
  resultEligibility: "migration-stream/read/result-approval-eligibility/v1",
  resultActions: "migration-stream/read/result-action-history/v1",
  resultReconciliationCommand:
    "migration-stream/write/reconciliation-calculation-result/v1",
  resultCandidateCommand:
    "migration-stream/write/result-candidate-command-result/v1",
  resultActionCommand: "migration-stream/write/result-action-command-result/v1",
  publicationEligibility: "migration-stream/publication-eligibility/v1",
  publicationList: "migration-stream/publication-list/v1",
  publicationDetail: "migration-stream/publication-detail/v1",
  engagementReadinessPackages:
    "migration-stream/engagement-publication-list/v1",
  error: "migration-stream/http-error/v1",
} as const;

export const COMMAND_SCHEMAS = {
  corrected: "migration-stream/write/correct-issue/v1",
  waived: "migration-stream/write/waive-issue/v1",
  acceptedAsIs: "migration-stream/write/accept-issue-as-is/v1",
  excluded: "migration-stream/write/exclude-issue-row/v1",
  publish: "migration-stream/publication-command/v1",
} as const;

export const RESULT_COMMAND_SCHEMAS = {
  calculate: "migration-stream/write/calculate-reconciliation/v1",
  createCandidate: "migration-stream/write/create-result-candidate/v1",
  approve: "migration-stream/write/approve-result-candidate/v1",
  reject: "migration-stream/write/reject-result-candidate/v1",
  revoke: "migration-stream/write/revoke-result-approval/v1",
  supersede: "migration-stream/write/supersede-result-approval/v1",
} as const;

export const ENGAGEMENT_COMMAND_SCHEMAS = {
  create: "migration-stream/engagement/write/create/v1",
  reserveSourcePreflight:
    "migration-stream/engagement/write/reserve-source-preflight/v1",
  acceptSource: "migration-stream/engagement/write/accept-source/v1",
  selectObjectPack: "migration-stream/engagement/write/select-object-pack/v1",
  approveObjectPackProfile:
    "migration-stream/engagement/write/approve-object-pack-profile/v1",
  prepareMapping: "migration-stream/engagement/assisted-mapping-command/v1",
  saveMappingDraft: "migration-stream/engagement/write/save-mapping-draft/v1",
  submitMappingDraft:
    "migration-stream/engagement/write/submit-mapping-draft/v1",
  approveMappingVersion:
    "migration-stream/engagement/write/approve-mapping-version/v1",
  launchValidation: "migration-stream/engagement/write/launch-validation/v1",
  startRevisedTour: "migration-stream/engagement/write/start-revised-tour/v1",
  reserveRevisedSourcePreflight:
    "migration-stream/engagement/write/reserve-revised-source-preflight/v1",
  acceptRevisedSource:
    "migration-stream/engagement/write/accept-revised-source/v1",
  launchRevisedValidation:
    "migration-stream/engagement/write/launch-revised-validation/v1",
  createRemediationPack:
    "migration-stream/engagement/write/create-remediation-pack/v1",
} as const;

export const OBJECT_WORKSPACE_COMMAND_SCHEMAS = {
  initialize: "migration-stream/object-workspace/write/initialize/v1",
  adopt: "migration-stream/object-workspace/write/adopt/v1",
} as const;

export const DESTINATION_COMMAND_SCHEMAS = {
  reserveEdmx: "migration-stream/engagement/write/reserve-destination-edmx/v1",
  createManualCatalogue:
    "migration-stream/engagement/write/create-manual-destination/v1",
  startDraft: "migration-stream/engagement/write/start-destination-draft/v1",
  saveDraft: "migration-stream/engagement/write/save-destination-draft/v1",
  rebaseDraft: "migration-stream/engagement/write/rebase-destination-draft/v1",
  freezeDraft: "migration-stream/engagement/write/freeze-destination-draft/v1",
  approveProfile:
    "migration-stream/engagement/write/approve-destination-profile/v1",
  carryForwardMapping:
    "migration-stream/engagement/write/carry-forward-mapping/v1",
} as const;

export const DESTINATION_ACKNOWLEDGEMENTS = {
  freeze: "migration-stream/destination-profile-reviewed-and-frozen/v1",
  localMappingValidation:
    "migration-stream/local-mapping-validation-authority/v1",
} as const;

export type SchemaName = (typeof SCHEMAS)[keyof typeof SCHEMAS];

export interface SelectedGovernance {
  profile_id: string;
  profile_version: string;
  profile_hash: string;
  mapping_id: string;
  mapping_version: string;
  mapping_hash: string;
  validation_run_id: string;
  validation_state: string;
}

interface WorkbenchContextBase {
  workspace_label: string;
  job_id: string;
  profiling_run_id: string | null;
  selected_governance: SelectedGovernance | null;
  supported_contracts: string[];
}

export interface ReadOnlyWorkbenchContext extends WorkbenchContextBase {
  schema_version: typeof SCHEMAS.context;
  read_only: true;
}

export interface GovernedWorkbenchContext extends WorkbenchContextBase {
  schema_version: typeof SCHEMAS.governedContext;
  read_only: false;
  capabilities: {
    governed_issue_decisions: true;
    correction_overlays: true;
    authenticated_practitioner_identity: false;
    source_mutation: false;
    original_projection_mutation: false;
    profile_mutation: false;
    mapping_mutation: false;
    migration_result_approval: false;
    target_write: false;
    publication: false;
  };
}

export interface ResultApprovalWorkbenchContext extends WorkbenchContextBase {
  schema_version: typeof SCHEMAS.resultApprovalContext;
  read_only: false;
  capabilities: {
    governed_issue_decisions: true;
    correction_overlays: true;
    authenticated_practitioner_identity: false;
    source_mutation: false;
    original_projection_mutation: false;
    profile_mutation: false;
    mapping_mutation: false;
    migration_result_approval: true;
    target_write: false;
    publication: false;
  };
}

export interface PublicationWorkbenchContext extends WorkbenchContextBase {
  schema_version: typeof SCHEMAS.publicationContext;
  read_only: false;
  capabilities: {
    governed_issue_decisions: true;
    correction_overlays: true;
    authenticated_practitioner_identity: false;
    source_mutation: false;
    original_projection_mutation: false;
    profile_mutation: false;
    mapping_mutation: false;
    migration_result_approval: true;
    target_write: false;
    publication: true;
  };
}

export type WorkbenchContext =
  | ReadOnlyWorkbenchContext
  | GovernedWorkbenchContext
  | ResultApprovalWorkbenchContext
  | PublicationWorkbenchContext;

export interface EngagementContext {
  schemaVersion: typeof SCHEMAS.engagementContext;
  workspaceLabel: string;
  engagement: EngagementState | null;
  supportedContracts: string[];
  capabilities: {
    engagementSetup: boolean;
    sourceIntake: boolean;
    textualProfiling: boolean;
    reviewedObjectPackSelection: boolean;
    explicitProfileApproval: boolean;
    mappingPreparation: boolean;
    validationLaunch: boolean;
    governedIssueDecisions: boolean;
    migrationResultApproval: false;
    targetWrite: false;
    publication: false;
  };
}

export type ApplicationContext = WorkbenchContext | EngagementContext;

export const JOURNEY_STAGE_IDS = [
  "SETUP",
  "DESTINATION",
  "SOURCE",
  "MAPPING",
  "VALIDATE",
  "COMPARE",
  "ISSUES",
  "DECISIONS",
  "RECONCILIATION",
  "PACKAGE",
] as const;

export type JourneyStageId = (typeof JOURNEY_STAGE_IDS)[number];
export type JourneyStageStatus =
  "COMPLETED" | "CURRENT" | "AVAILABLE" | "LOCKED";

export type ObjectWorkspaceState =
  "INITIALIZE_REQUIRED" | "ADOPT_REQUIRED" | "ACTIVE";
export type ObjectWorkspaceAction = "INITIALIZE" | "ADOPT";

export interface ObjectWorkspaceRecord {
  schemaVersion: typeof SCHEMAS.objectWorkspace;
  workspaceId: string;
  generation: 1;
  displayName: string;
  migrationObjectNameOrScope: string;
  creatorAttribution: string;
  establishedBy: "INITIALIZED" | "ADOPTED";
  createdAt: string;
  descriptorHash: string;
}

export interface ObjectWorkspaceStatus {
  schemaVersion: typeof SCHEMAS.objectWorkspaceStatus;
  state: ObjectWorkspaceState;
  stateHash: string;
  requiredAction: ObjectWorkspaceAction | null;
  unavailableReason: string | null;
  pendingCommand: ObjectWorkspaceCommand | null;
  workspace: ObjectWorkspaceRecord | null;
}

export type DestinationOrigin = "CONTROLLED_PACK" | "WORKSPACE_PROFILE";
export type DestinationTechnicalOrigin = "EDMX" | "MANUAL";
export type DestinationFieldState = "AVAILABLE" | "ACTIVE" | "NEEDS_REVIEW";
export type DestinationDiffState =
  | "UNCHANGED"
  | "ADDED"
  | "REMOVED"
  | "TECHNICALLY_CHANGED"
  | "MANUAL_POTENTIAL_MATCH";
export type DestinationCompatibilityClassification =
  | "SCOPE_NEUTRAL_INVENTORY"
  | "NON_STRUCTURAL_WARNING"
  | "UNRESOLVED_SELECTED_SCOPE"
  | "UNSUPPORTED_IRRELEVANT"
  | "SELECTED_SCOPE_WARNING"
  | "SELECTED_SCOPE_OPERATOR_RESOLVED";

// Compatibility aliases used by the destination surface while its field names
// follow the exact Rust serde contract below.
export type DestinationActivationState = DestinationFieldState;
export type DestinationChangeState = DestinationDiffState;

export interface DestinationCompatibilityFinding {
  code: string;
  classification: DestinationCompatibilityClassification;
  technicalLocation: string;
  count: number;
  blocking: boolean;
  findingHash: string;
}

export interface DestinationCompatibilityReport {
  reportHash: string;
  scopedCompletenessHash: string | null;
  algorithm: string;
  odataGeneration: "V2" | "V4" | null;
  safelyExtractedCount: number;
  unsupportedInventoryCount: number;
  unsupportedIrrelevantCount: number;
  unresolvedSelectedScopeCount: number;
  findings: DestinationCompatibilityFinding[];
}

export interface DestinationArtifactView {
  schemaVersion: typeof SCHEMAS.destinationArtifact;
  catalogueAuthorityId: string;
  origin: DestinationTechnicalOrigin;
  artifactLabel: string;
  system: string | null;
  clientOrTenant: string | null;
  release: string | null;
  manualActor: string | null;
  manualReason: string | null;
  snapshotId: string | null;
  snapshotSha256: string | null;
  snapshotBytes: number | null;
  catalogueHash: string;
  cataloguePayloadSha256: string;
  catalogueFileSha256: string | null;
  parserName: string | null;
  parserVersion: string | null;
  odataGeneration: "V2" | "V4" | null;
  namespaceCount: number;
  namespaces: string[];
  entityCount: number;
  propertyCount: number;
  navigationCount: number;
  compatibility: DestinationCompatibilityReport;
  createdAt: string;
}

export interface ManualSupplementEvidence {
  supplementId: string;
  actor: string;
  reason: string;
}

export interface DestinationDraftField {
  fieldId: string;
  label: string;
  logicalType: string;
  businessRequired: boolean;
  sensitive: boolean;
  key: boolean;
  state: DestinationFieldState;
  technicalProperty: string | null;
  manualSupplement: ManualSupplementEvidence | null;
  technicalChangeReview: {
    decision: "ACCEPT_REVISED_TECHNICAL_EVIDENCE";
    actor: string;
    reason: string;
  } | null;
  confirmMaxLength: boolean;
  maxLength: number | null;
  diffState: DestinationDiffState | null;
}

export interface DestinationDraftEntity {
  entityId: string;
  label: string;
  root: boolean;
  technicalEntity: string | null;
  fields: DestinationDraftField[];
}

export interface DestinationRelationshipKeyPair {
  fromFieldId: string;
  toFieldId: string;
}

export interface DestinationDraftRelationship {
  relationshipId: string;
  fromEntityId: string;
  toEntityId: string;
  cardinality: "0..*" | "1..*";
  technicalNavigation: string | null;
  keyPairs: DestinationRelationshipKeyPair[];
  confirmed: boolean;
}

export type DestinationRuleKind =
  | "required"
  | "max_length"
  | "unique"
  | "allowed_values"
  | "pattern"
  | "conditional_required"
  | "reference_lookup";

export interface DestinationDraftRule {
  ruleId: string;
  severity: string;
  kind: DestinationRuleKind;
  target: string;
  message: string;
  parameters: JsonValue;
}

export interface DestinationCompatibilityAcknowledgement {
  findingHash: string;
  actor: string;
  reason: string;
}

export interface DestinationDraftSnapshot {
  profileId: string;
  title: string;
  owner: string;
  entities: DestinationDraftEntity[];
  relationships: DestinationDraftRelationship[];
  rules: DestinationDraftRule[];
  compatibilityAcknowledgements: DestinationCompatibilityAcknowledgement[];
}

export interface DestinationDraftView {
  schemaVersion: typeof SCHEMAS.destinationDraft;
  draftId: string;
  catalogueAuthorityId: string;
  catalogueHash: string;
  generation: number;
  draftHash: string;
  state: "EDITABLE" | "FROZEN";
  snapshot: DestinationDraftSnapshot;
  freezeBlockers: string[];
  frozenProfileId: string | null;
  frozenProfileVersion: string | null;
  frozenProfileHash: string | null;
  updatedAt: string;
}

export interface DestinationProfileApprovalEvidence {
  approval_id: string;
  approver: string;
  reason: string;
  acknowledged: boolean;
  approved_at: string;
  approval_hash: string;
}

export interface DestinationProfileLifecycleEvent {
  sequence: number;
  from_state: string | null;
  to_state:
    | "GENERATED_DRAFT"
    | "REVIEWED_DRAFT"
    | "APPROVED"
    | "DEPRECATED"
    | "RETIRED";
  actor: string;
  reason: string;
  previous_event_hash: string | null;
  event_hash: string;
  occurred_at: string;
}

export interface DestinationProfileVersionView {
  schema_version: number;
  profile_id: string;
  profile_version: string;
  profile_hash: string;
  schema_hash: string;
  validation_hash: string;
  declared_status: string;
  lifecycle_status:
    | "GENERATED_DRAFT"
    | "REVIEWED_DRAFT"
    | "APPROVED"
    | "DEPRECATED"
    | "RETIRED";
  authority: "NONE" | "LOCAL_MAPPING_VALIDATION";
  can_start_new_runs: boolean;
  registered_at: string;
  approval: DestinationProfileApprovalEvidence | null;
  lifecycle_events: DestinationProfileLifecycleEvent[];
}

export interface ActiveDestinationView {
  origin: DestinationOrigin;
  destinationAuthorityId: string;
  destinationOriginHash: string;
  profileId: string;
  profileVersion: string;
  profileHash: string;
  profileAuthority: "NONE" | "LOCAL_MAPPING_VALIDATION";
  approvalId: string | null;
  approvalHash: string | null;
}

export interface DestinationWorkshopLimits {
  maximumEdmxBytes: number;
  maximumEntities: number;
  maximumFields: number;
  maximumRelationships: number;
  maximumRules: number;
  maximumPageSize: number;
}

export interface DestinationWorkshop {
  schemaVersion: typeof SCHEMAS.destinationWorkshop;
  workspaceId: string;
  workspaceDescriptorHash: string;
  engagementId: string;
  engagementGeneration: number;
  destinationOrigin: DestinationOrigin | null;
  activeDestination: ActiveDestinationView | null;
  reviewedCandidate: FrozenDestinationProfileView | null;
  eligibleCarryForwardSource: EligibleCarryForwardSourceView | null;
  latestArtifact: DestinationArtifactView | null;
  draft: DestinationDraftView | null;
  allowedOrigins: DestinationOrigin[];
  limits: DestinationWorkshopLimits;
}

export interface EligibleCarryForwardSourceView {
  mappingId: string;
  mappingVersion: string;
  mappingHash: string;
  mappingApprovalId: string;
  mappingApprovalHash: string;
  activeJobId: string;
  activeSourceHash: string;
  validatedJobId: string;
  validatedSourceHash: string;
  profileId: string;
  profileVersion: string;
  profileHash: string;
}

export interface DestinationCatalogueEntity {
  technicalEntity: string;
  namespace: string;
  name: string;
  keyProperties: string[];
  propertyCount: number;
  navigationCount: number;
  selected: boolean;
  root: boolean;
}

export interface DestinationCatalogueEntityPage {
  schemaVersion: typeof SCHEMAS.destinationCatalogueEntities;
  catalogueAuthorityId: string;
  catalogueHash: string;
  draftId: string | null;
  draftGeneration: number | null;
  draftHash: string | null;
  items: DestinationCatalogueEntity[];
  nextCursor: string | null;
}

export interface DestinationCatalogueProperty {
  technicalProperty: string;
  technicalEntity: string;
  name: string;
  edmType: string;
  nullable: boolean | null;
  maxLength: string | null;
  precision: string | null;
  scale: string | null;
  keyCandidate: boolean;
  requiredCandidate: boolean;
  state: DestinationFieldState;
  field: DestinationDraftField | null;
}

export interface DestinationCataloguePropertyPage {
  schemaVersion: typeof SCHEMAS.destinationCatalogueProperties;
  catalogueAuthorityId: string;
  catalogueHash: string;
  technicalEntity: string;
  draftId: string | null;
  draftGeneration: number | null;
  draftHash: string | null;
  items: DestinationCatalogueProperty[];
  nextCursor: string | null;
}

export interface DestinationArtifactReservation {
  schemaVersion: typeof SCHEMAS.destinationArtifactReservation;
  attemptId: string;
  state: "RECEIVING";
  artifactLabel: string;
  sourceName: string;
  maximumBytes: number;
  createdAt: string;
}

export interface FrozenDestinationProfileView {
  schemaVersion: typeof SCHEMAS.destinationReviewedProfile;
  engagementGeneration: number;
  profile: DestinationProfileVersionView;
  originBindingId: string;
  originBindingHash: string;
  draftId: string;
  draftGeneration: number;
  draftHash: string;
}

export interface ApprovedDestinationProfileView {
  schemaVersion: typeof SCHEMAS.destinationProfileApproval;
  engagementGeneration: number;
  profile: DestinationProfileVersionView;
  destinationOrigin: DestinationOrigin;
  destinationAuthorityId: string;
  destinationOriginHash: string;
}

export interface DestinationRebaseView {
  schemaVersion: typeof SCHEMAS.destinationRebase;
  engagementGeneration: number;
  rebaseId: string;
  diffHash: string;
  counts: Record<string, number>;
  draft: DestinationDraftView;
}

export interface DestinationCarryForwardView {
  schemaVersion: typeof SCHEMAS.destinationCarryForward;
  engagementGeneration: number;
  carryForwardId: string;
  resultHash: string;
  clonedBindingCount: number;
  unresolvedTargetCount: number;
  unresolvedRelationshipCount: number;
  unresolvedRelationships: Array<{
    relationshipId: string;
    reason:
      | "RELATIONSHIP_KEY_BINDING_UNRESOLVED"
      | "RELATIONSHIP_CHANGED"
      | "RELATIONSHIP_NOT_PRESENT";
  }>;
}

export interface ReserveEdmxCommand {
  schemaVersion: typeof DESTINATION_COMMAND_SCHEMAS.reserveEdmx;
  expectedEngagementGeneration: number;
  expectedWorkspaceId: string;
  expectedWorkspaceDescriptorHash: string;
  sourceName: string;
  artifactLabel: string;
  system: string | null;
  clientOrTenant: string | null;
  release: string | null;
  idempotencyKey: string;
}

export interface ManualPropertyDefinition {
  name: string;
  edmType: string;
  nullable: boolean;
  maxLength: number | null;
  keyCandidate: boolean;
}

export interface ManualEntityDefinition {
  namespace: string;
  name: string;
  properties: ManualPropertyDefinition[];
}

export interface CreateManualCatalogueCommand {
  schemaVersion: typeof DESTINATION_COMMAND_SCHEMAS.createManualCatalogue;
  expectedEngagementGeneration: number;
  artifactLabel: string;
  actor: string;
  reason: string;
  entities: ManualEntityDefinition[];
  idempotencyKey: string;
}

export interface StartDestinationDraftCommand {
  schemaVersion: typeof DESTINATION_COMMAND_SCHEMAS.startDraft;
  expectedEngagementGeneration: number;
  catalogueAuthorityId: string;
  expectedCatalogueHash: string;
  rootTechnicalEntity: string;
  childTechnicalEntities: string[];
  profileId: string;
  title: string;
  owner: string;
  actor: string;
  reason: string;
  idempotencyKey: string;
}

export interface SaveDestinationDraftCommand {
  schemaVersion: typeof DESTINATION_COMMAND_SCHEMAS.saveDraft;
  expectedEngagementGeneration: number;
  draftId: string;
  expectedDraftGeneration: number;
  expectedDraftHash: string;
  snapshot: DestinationDraftSnapshot;
  actor: string;
  reason: string;
  idempotencyKey: string;
}

export interface RebaseDestinationDraftCommand {
  schemaVersion: typeof DESTINATION_COMMAND_SCHEMAS.rebaseDraft;
  expectedEngagementGeneration: number;
  draftId: string;
  expectedDraftGeneration: number;
  expectedDraftHash: string;
  targetCatalogueAuthorityId: string;
  expectedTargetCatalogueHash: string;
  actor: string;
  reason: string;
  idempotencyKey: string;
}

export interface FreezeDestinationDraftCommand {
  schemaVersion: typeof DESTINATION_COMMAND_SCHEMAS.freezeDraft;
  expectedEngagementGeneration: number;
  draftId: string;
  expectedDraftGeneration: number;
  expectedDraftHash: string;
  profileVersion: string;
  acknowledgement: typeof DESTINATION_ACKNOWLEDGEMENTS.freeze;
  actor: string;
  reason: string;
  idempotencyKey: string;
}

export interface ApproveDestinationProfileCommand {
  schemaVersion: typeof DESTINATION_COMMAND_SCHEMAS.approveProfile;
  expectedEngagementGeneration: number;
  profileId: string;
  profileVersion: string;
  profileHash: string;
  originBindingHash: string;
  actor: string;
  reason: string;
  authorityAcknowledgement: typeof DESTINATION_ACKNOWLEDGEMENTS.localMappingValidation;
  idempotencyKey: string;
}

export interface CarryForwardMappingCommand {
  schemaVersion: typeof DESTINATION_COMMAND_SCHEMAS.carryForwardMapping;
  expectedEngagementGeneration: number;
  sourceMappingId: string;
  sourceMappingVersion: string;
  sourceMappingHash: string;
  targetProfileId: string;
  targetProfileVersion: string;
  targetProfileHash: string;
  actor: string;
  reason: string;
  idempotencyKey: string;
}

export type DestinationCommand =
  | ReserveEdmxCommand
  | CreateManualCatalogueCommand
  | StartDestinationDraftCommand
  | SaveDestinationDraftCommand
  | RebaseDestinationDraftCommand
  | FreezeDestinationDraftCommand
  | ApproveDestinationProfileCommand
  | CarryForwardMappingCommand;

// Transitional export names retained so existing imports fail on field
// mismatches instead of on missing symbols while the UI adopts the exact
// public destination contract.
export type ReserveDestinationArtifactCommand = ReserveEdmxCommand;
export type StartManualDestinationCommand = CreateManualCatalogueCommand;
export type SubmitDestinationDraftCommand = FreezeDestinationDraftCommand;
export type DestinationDraft = DestinationDraftView;
export type DestinationReviewedProfile = FrozenDestinationProfileView;
export type DestinationProfileApproval = ApprovedDestinationProfileView;
export type DestinationArtifactResult = DestinationArtifactView;
export type DestinationRebase = DestinationRebaseView;

export interface JourneyBinding {
  engagementId: string | null;
  engagementGeneration: number | null;
  jobId: string | null;
  sourceHash: string | null;
  manifestHash: string | null;
  profilingRunId: string | null;
  objectPackId: string | null;
  destinationOrigin: DestinationOrigin | null;
  destinationAuthorityId: string | null;
  destinationOriginHash: string | null;
  profileId: string | null;
  profileVersion: string | null;
  profileHash: string | null;
  mappingId: string | null;
  mappingVersion: string | null;
  mappingHash: string | null;
  validationRunId: string | null;
  validationResultHash: string | null;
  activeTourId: string | null;
  activeTourNumber: number | null;
  activeTourHash: string | null;
  comparisonId: string | null;
  comparisonHash: string | null;
  entityCount: number | null;
}

export interface JourneyStage {
  id: JourneyStageId;
  label: string;
  status: JourneyStageStatus;
  available: boolean;
  href: string | null;
  unavailableReason: string | null;
}

export interface Journey {
  schemaVersion: typeof SCHEMAS.journey;
  currentStageId: JourneyStageId;
  workspace: ObjectWorkspaceStatus;
  binding: JourneyBinding;
  stages: JourneyStage[];
}

export const ENGAGEMENT_PHASES = [
  "AWAITING_SOURCE",
  "PREFLIGHT_RECEIVING",
  "PREFLIGHT_READY",
  "INGESTING",
  "PROFILED",
  "AWAITING_OBJECT_PACK",
  "AWAITING_PROFILE_APPROVAL",
  "AWAITING_MAPPING",
  "MAPPING_PREPARED",
  "READY_TO_VALIDATE",
  "VALIDATING",
  "VALIDATION_COMPLETE",
  "FIRST_ISSUES_READY",
  "FAILED",
] as const;

export type EngagementPhase = (typeof ENGAGEMENT_PHASES)[number];

export interface SourceArchiveManifest {
  entries: Array<{
    path: string;
    is_directory: boolean;
    is_csv: boolean;
    compressed_size: number;
    expanded_size: number;
    compression_ratio: number;
    crc32: number;
  }>;
  csv_paths: string[];
  total_compressed_bytes: number;
  total_expanded_bytes: number;
  maximum_observed_compression_ratio: number;
}

export interface SourcePreflight {
  schemaVersion: typeof SCHEMAS.sourcePreflight;
  preflightId: string;
  state: "RECEIVING" | "READY" | "REJECTED" | "INTERRUPTED";
  sourceName: string;
  sourceHash: string | null;
  sourceSize: number | null;
  policyHash: string;
  manifestHash: string | null;
  manifest: SourceArchiveManifest | null;
  errorCode: string | null;
  errorMessage: string | null;
  createdAt: string;
  updatedAt: string;
  completedAt: string | null;
}

export interface SourcePreflightDocument {
  preflight: SourcePreflight;
}

export interface EngagementState {
  schemaVersion: typeof SCHEMAS.engagementState;
  engagementId: string;
  displayName: string;
  phase: EngagementPhase;
  generation: number;
  active: {
    preflightId: string | null;
    jobId: string | null;
    profilingRunId: string | null;
    objectPackId: string | null;
    destinationOrigin: DestinationOrigin | null;
    destinationAuthorityId: string | null;
    destinationOriginHash: string | null;
    profileId: string | null;
    profileVersion: string | null;
    profileHash: string | null;
    mappingId: string | null;
    mappingVersion: string | null;
    mappingHash: string | null;
    validationRunId: string | null;
  };
  profileStage: "NOT_SELECTED" | "REVIEWED" | "APPROVED";
  mappingStage: "NOT_PREPARED" | "PREPARED" | "VALIDATED";
  validationStage:
    | "NOT_LAUNCHED"
    | "RUNNING"
    | "COMPLETE"
    | "FAILED"
    | "INTERRUPTED"
    | "BOUND_EXCEEDED";
  updatedAt: string;
}

export interface EngagementDetail {
  schemaVersion: typeof SCHEMAS.engagementDetail;
  state: EngagementState;
  preflights: SourcePreflight[];
  availableObjectPackCount: number;
}

export interface ObjectPack {
  objectPackId: string;
  version: string;
  title: string;
  description: string;
  scope: string;
  rootEntityId: string;
  mappingMode: "CONTROLLED_SUGGESTIONS" | "OPERATOR_GOVERNED";
  limitations: string[];
  mappingSuggestions: Array<{
    suggestionId: string;
    targetEntityId?: string;
    targetFieldId: string;
    sourceFile: string | null;
    sourceColumn: string | null;
    sourceColumnOrdinal: number | null;
    kind: MappingOperation;
    constant: string | null;
    lookupTableId: string | null;
  }>;
  referenceTables: ReferenceTableCommand[];
}

export interface ObjectPackList {
  schemaVersion: typeof SCHEMAS.objectPacks;
  items: ObjectPack[];
}

export interface SourceProfileRules {
  schemaVersion: typeof SCHEMAS.sourceProfileRules;
  jobId: string;
  profilingRunId: string;
  storedSampleValues: false;
  sourceColumns: Array<{
    sourceFile: string;
    sourceColumn: string;
    sourceColumnOrdinal: number;
  }>;
  profileFields: Array<{
    entityId: string;
    fieldId: string;
    label: string;
    logicalType: string;
    required: boolean;
    sensitive: boolean;
  }>;
  rules: Array<{
    ruleId: string;
    severity: string;
    kind: string;
    target: string;
    message: string;
    parameters: JsonValue;
  }>;
}

export const MAPPING_OPERATIONS = [
  "direct",
  "constant",
  "empty_as_null",
  "trim",
  "uppercase_ascii",
  "lowercase_ascii",
  "lookup",
] as const;

export type MappingOperation = (typeof MAPPING_OPERATIONS)[number];

export interface CreateEngagementCommand {
  schemaVersion: typeof ENGAGEMENT_COMMAND_SCHEMAS.create;
  displayName: string;
  idempotencyKey: string;
}

interface ObjectWorkspaceCommandFields {
  expectedStateHash: string;
  displayName: string;
  migrationObjectNameOrScope: string;
  creatorAttribution: string;
  idempotencyKey: string;
}

export interface InitializeObjectWorkspaceCommand extends ObjectWorkspaceCommandFields {
  schemaVersion: typeof OBJECT_WORKSPACE_COMMAND_SCHEMAS.initialize;
}

export interface AdoptObjectWorkspaceCommand extends ObjectWorkspaceCommandFields {
  schemaVersion: typeof OBJECT_WORKSPACE_COMMAND_SCHEMAS.adopt;
}

export type ObjectWorkspaceCommand =
  InitializeObjectWorkspaceCommand | AdoptObjectWorkspaceCommand;

export interface ReserveSourcePreflightCommand {
  schemaVersion: typeof ENGAGEMENT_COMMAND_SCHEMAS.reserveSourcePreflight;
  sourceName: string;
  idempotencyKey: string;
}

export interface AcceptSourceCommand {
  schemaVersion: typeof ENGAGEMENT_COMMAND_SCHEMAS.acceptSource;
  preflightId: string;
  expectedSourceHash: string;
  expectedManifestHash: string;
  idempotencyKey: string;
  acknowledgement: "migration-stream/source-acceptance/v1";
}

export interface SelectObjectPackCommand {
  schemaVersion: typeof ENGAGEMENT_COMMAND_SCHEMAS.selectObjectPack;
  objectPackId: string;
  idempotencyKey: string;
}

export interface ApproveObjectPackProfileCommand {
  schemaVersion: typeof ENGAGEMENT_COMMAND_SCHEMAS.approveObjectPackProfile;
  actor: string;
  reason: string;
  authorityAcknowledgement: "migration-stream/local-mapping-validation-authority/v1";
  idempotencyKey: string;
}

export interface ReferenceTableCommand {
  entityId?: string;
  tableId: string;
  version: string;
  values: Record<string, string>;
}

export interface PrepareMappingCommand {
  schemaVersion: typeof ENGAGEMENT_COMMAND_SCHEMAS.prepareMapping;
  idempotencyKey: string;
  mappingId: string;
  version: string;
  selectedSuggestionIds: string[];
  author: string | null;
  reviewer: string | null;
  acknowledgement: "migration-stream/local-assisted-mapping-reviewed-suggestions-no-target-compatibility/v1";
}

export interface SourceEntityAssignmentCommand {
  sourceFile: string;
  entityId: string;
}

export interface MappingBindingCommand {
  bindingId: string;
  targetEntityId: string;
  targetFieldId: string;
  sourceFile?: string;
  sourceColumn?: string;
  sourceColumnOrdinal?: number;
  kind: MappingOperation;
  constant?: string;
  lookupTableId?: string;
}

export interface RelationshipFieldPairCommand {
  fromFieldId: string;
  toFieldId: string;
}

export interface MappingRelationshipBindingCommand {
  relationshipId: string;
  fromEntityId: string;
  toEntityId: string;
  cardinality: "0..*" | "1..*";
  keyPairs: RelationshipFieldPairCommand[];
}

export interface MappingDraftSnapshot {
  sourceEntityAssignments: SourceEntityAssignmentCommand[];
  bindings: MappingBindingCommand[];
  relationshipBindings: MappingRelationshipBindingCommand[];
  referenceTables: ReferenceTableCommand[];
  author?: string;
}

export interface MappingDraftView {
  schemaVersion: typeof SCHEMAS.mappingDraft;
  draftId: string;
  draftGeneration: number;
  draftHash: string;
  state: "EDITABLE" | "SUBMITTED" | "APPROVED";
  snapshot: MappingDraftSnapshot;
  submittedMappingId: string | null;
  submittedMappingVersion: string | null;
  submittedMappingHash: string | null;
  updatedAt: string;
}

export interface MappingWorkspace {
  schemaVersion: typeof SCHEMAS.mappingWorkspace;
  engagementGeneration: number;
  jobId: string;
  sourceHash: string;
  profilingRunId: string;
  destinationOrigin: "CONTROLLED_PACK" | "WORKSPACE_PROFILE";
  objectPackId: string;
  objectPackVersion: string;
  profileId: string;
  profileVersion: string;
  profileHash: string;
  mappingMode: "OPERATOR_GOVERNED";
  sourceFiles: Array<{
    sourceFile: string;
    columns: SourceProfileRules["sourceColumns"];
  }>;
  entities: Array<{
    entityId: string;
    root: boolean;
    key: string[];
    fields: SourceProfileRules["profileFields"];
  }>;
  relationships: Array<{
    relationshipId: string;
    fromEntityId: string;
    toEntityId: string;
    cardinality: "0..*" | "1..*";
    keyPairs: RelationshipFieldPairCommand[];
  }>;
  allowedOperations: MappingOperation[];
  controlledReferenceTables: ReferenceTableCommand[];
  referenceTableObligations: Array<{
    entityId: string;
    tableId: string;
    tableVersion: string;
    ruleIds: string[];
    targetFields: string[];
  }>;
  limits: {
    maximumSourceFiles: number;
    maximumBindings: number;
    maximumRelationships: number;
    maximumReferenceTables: number;
    maximumReferenceEntries: number;
    maximumReferenceBytes: number;
    maximumReferenceKeyBytes: number;
    maximumReferenceValueBytes: number;
  };
  revisionDraftScope: {
    tourAttemptId: string;
    tourNumber: number;
    baselineTourId: string;
    profilingRunId: string;
    submissionAvailable: false;
    unavailableReason: string;
  } | null;
  draft: MappingDraftView | null;
}

export type MappingContractCoverageStatus =
  "COMPLETE" | "UNRESOLVED" | "WARNING" | "BLOCKER";

export interface MappingContractCoverage {
  schemaVersion: typeof SCHEMAS.mappingContractCoverage;
  authority: {
    workspaceId: string;
    workspaceDescriptorHash: string;
    engagementId: string;
    engagementGeneration: number;
    jobId: string;
    sourceHash: string;
    profilingRunId: string;
    destinationOrigin: "CONTROLLED_PACK" | "WORKSPACE_PROFILE";
    profileId: string;
    profileVersion: string;
    profileHash: string;
    profileApprovalId: string;
    profileApprovalHash: string;
    draftId: string | null;
    draftGeneration: number | null;
    draftHash: string | null;
    draftState: "EDITABLE" | "SUBMITTED" | "APPROVED" | null;
    mappingId: string | null;
    mappingVersion: string | null;
    mappingHash: string | null;
    mappingApprovalId: string | null;
    mappingApprovalHash: string | null;
  };
  overallStatus: MappingContractCoverageStatus;
  summary: {
    complete: number;
    unresolved: number;
    warnings: number;
    blockers: number;
  };
  fields: Array<{
    entityId: string;
    fieldId: string;
    bindingId: string | null;
    operation: MappingOperation | null;
    sourceFile: string | null;
    sourceColumn: string | null;
    sourceColumnOrdinal: number | null;
    fixedValueConfigured: boolean;
    lookupTableId: string | null;
    lookupTableVersion: string | null;
    ruleIds: string[];
    ruleKinds: string[];
    status: MappingContractCoverageStatus;
    reasons: string[];
  }>;
  sourceColumns: Array<{
    sourceFile: string;
    sourceColumn: string;
    sourceColumnOrdinal: number;
    status: MappingContractCoverageStatus;
    reason: string;
  }>;
  rules: Array<{
    ruleId: string;
    kind: string;
    entityId: string;
    fieldId: string;
    referenceTableId: string | null;
    referenceTableVersion: string | null;
    status: MappingContractCoverageStatus;
    reasons: string[];
  }>;
  tables: Array<{
    entityId: string;
    tableId: string;
    tableVersion: string;
    entryCount: number;
    roles: string[];
    status: MappingContractCoverageStatus;
    reasons: string[];
  }>;
  relationships: Array<{
    relationshipId: string;
    fromEntityId: string;
    toEntityId: string;
    cardinality: "0..*" | "1..*";
    keyPairs: RelationshipFieldPairCommand[];
    status: MappingContractCoverageStatus;
    reasons: string[];
  }>;
  coverageHash: string;
}

export interface SaveMappingDraftCommand {
  schemaVersion: typeof ENGAGEMENT_COMMAND_SCHEMAS.saveMappingDraft;
  idempotencyKey: string;
  expectedEngagementGeneration: number;
  expectedDraftId: string | null;
  expectedDraftGeneration: number | null;
  expectedDraftHash: string | null;
  snapshot: MappingDraftSnapshot;
}

export interface SubmitMappingDraftCommand {
  schemaVersion: typeof ENGAGEMENT_COMMAND_SCHEMAS.submitMappingDraft;
  idempotencyKey: string;
  expectedEngagementGeneration: number;
  draftId: string;
  expectedDraftGeneration: number;
  expectedDraftHash: string;
  mappingId: string;
  version: string;
  acknowledgement: "migration-stream/operator-governed-mapping-reviewed-and-frozen/v1";
}

export interface ApproveMappingVersionCommand {
  schemaVersion: typeof ENGAGEMENT_COMMAND_SCHEMAS.approveMappingVersion;
  idempotencyKey: string;
  expectedEngagementGeneration: number;
  draftId: string;
  expectedDraftGeneration: number;
  expectedDraftHash: string;
  expectedMappingId: string;
  expectedMappingVersion: string;
  expectedMappingHash: string;
  actor: string;
  reason: string;
  acknowledgement: "migration-stream/local-mapping-version-approval/v1";
}

export interface LaunchValidationCommand {
  schemaVersion: typeof ENGAGEMENT_COMMAND_SCHEMAS.launchValidation;
  idempotencyKey: string;
  expectedEngagementGeneration: number;
  expectedJobId: string;
  expectedMappingId: string;
  expectedMappingVersion: string;
  expectedMappingHash: string;
}

export const REVISED_SOURCE_ACKNOWLEDGEMENT =
  "migration-stream/revised-source-compared-validation/v1" as const;

export interface StartRevisedTourCommand {
  schemaVersion: typeof ENGAGEMENT_COMMAND_SCHEMAS.startRevisedTour;
  idempotencyKey: string;
  expectedEngagementGeneration: number;
  actor: string;
  reason: string;
  acknowledgement: typeof REVISED_SOURCE_ACKNOWLEDGEMENT;
}

export interface ReserveRevisedSourcePreflightCommand {
  schemaVersion: typeof ENGAGEMENT_COMMAND_SCHEMAS.reserveRevisedSourcePreflight;
  idempotencyKey: string;
  expectedEngagementGeneration: number;
  tourAttemptId: string;
  sourceName: string;
}

export interface AcceptRevisedSourceCommand {
  schemaVersion: typeof ENGAGEMENT_COMMAND_SCHEMAS.acceptRevisedSource;
  idempotencyKey: string;
  expectedEngagementGeneration: number;
  tourAttemptId: string;
  preflightId: string;
  expectedSourceHash: string;
  expectedManifestHash: string;
  actor: string;
  reason: string;
  acknowledgement: typeof REVISED_SOURCE_ACKNOWLEDGEMENT;
}

export interface LaunchRevisedValidationCommand {
  schemaVersion: typeof ENGAGEMENT_COMMAND_SCHEMAS.launchRevisedValidation;
  idempotencyKey: string;
  expectedEngagementGeneration: number;
  tourAttemptId: string;
  expectedReuseAuthorityId: string;
  expectedReuseAuthorityHash: string;
  actor: string;
  reason: string;
  acknowledgement: typeof REVISED_SOURCE_ACKNOWLEDGEMENT;
}

export type RevisedTourCommand =
  | StartRevisedTourCommand
  | ReserveRevisedSourcePreflightCommand
  | AcceptRevisedSourceCommand
  | LaunchRevisedValidationCommand;

export type RevisedTourAttemptState =
  | "STARTED"
  | "PREFLIGHT_RECEIVING"
  | "PREFLIGHT_READY"
  | "SOURCE_ACCEPTED"
  | "READY_TO_VALIDATE"
  | "VALIDATING"
  | "COMPLETE"
  | "INCOMPATIBLE"
  | "FAILED";

export interface TourSummary {
  tourId: string;
  tourNumber: number;
  predecessorTourId: string | null;
  sourceHash: string;
  manifestHash: string;
  profilingRunId: string;
  profileId: string;
  profileVersion: string;
  profileHash: string;
  mappingId: string;
  mappingVersion: string;
  mappingHash: string;
  mappingAuthorityId: string;
  mappingAuthorityHash: string;
  validationRunId: string;
  validationResultHash: string;
  tourHash: string;
  completedAt: string;
}

export interface RevisedTourAttempt {
  tourAttemptId: string;
  tourNumber: number;
  baselineTourId: string;
  state: RevisedTourAttemptState;
  generation: number;
  preflightId: string | null;
  sourceHash: string | null;
  manifestHash: string | null;
  jobId: string | null;
  profilingRunId: string | null;
  structureHash: string | null;
  reuseAuthorityId: string | null;
  reuseAuthorityHash: string | null;
  validationRunId: string | null;
  errorCode: string | null;
  errorMessage: string | null;
  actor: string;
  reason: string;
  createdAt: string;
  updatedAt: string;
}

export interface MappingReuseAuthority {
  reuseAuthorityId: string;
  reuseAuthorityHash: string;
  baselineTourId: string;
  candidateTourAttemptId: string;
  baselineSourceHash: string;
  mappingOriginSourceHash: string;
  candidateSourceHash: string;
  profileId: string;
  profileVersion: string;
  profileHash: string;
  profileApprovalId: string;
  profileApprovalHash: string;
  mappingId: string;
  mappingVersion: string;
  mappingHash: string;
  mappingAuthorityId: string;
  mappingAuthorityHash: string;
  mappingApprovalId: string | null;
  mappingApprovalHash: string | null;
  structureHash: string;
  createdAt: string;
}

export type ComparedFindingCategory = "CLOSED" | "REMAINING" | "NEW";

export const COMPARED_FINDING_IDENTITY_EXPLANATIONS = {
  DECLARED_KEY:
    "Compared by a private fingerprint of the complete declared business key; row and line positions are evidence only.",
  FULL_ROW_FALLBACK:
    "Compared as a multiset by a private full-row fingerprint because the declared key was incomplete. An edited row can appear as one CLOSED and one NEW finding; this does not assert record-to-record lineage.",
  UNIQUE_SCOPE:
    "Compared as a multiset by a private fingerprint of the rule's ordered unique-scope tuple; row and line positions are evidence only.",
} as const;

export interface ComparedFinding {
  findingId: string;
  category: ComparedFindingCategory;
  fingerprintHash: string;
  occurrence: number;
  severity: string;
  ruleId: string;
  issueCode: string;
  displayMessage: string;
  entityId: string;
  fieldId: string;
  relationshipId: string | null;
  relationshipRole: string | null;
  baselineIssueIds: string[];
  baselineIssueHashes: string[];
  candidateIssueIds: string[];
  candidateIssueHashes: string[];
  identityBasis: keyof typeof COMPARED_FINDING_IDENTITY_EXPLANATIONS;
  identityBasisExplanation: string;
}

export interface ComparedValidation {
  schemaVersion: typeof SCHEMAS.comparedValidation;
  comparisonId: string;
  baselineTourId: string;
  candidateTourId: string;
  baselineValidationRunId: string;
  candidateValidationRunId: string;
  baselineResultHash: string;
  candidateResultHash: string;
  algorithmVersion: string;
  closedCount: number;
  remainingCount: number;
  newCount: number;
  inputHash: string;
  comparisonHash: string;
  actor: string;
  reason: string;
  createdAt: string;
  findings: ComparedFinding[];
}

export interface TourChain {
  schemaVersion: typeof SCHEMAS.tourChain;
  engagementGeneration: number;
  activeTour: TourSummary | null;
  pendingAttempt: RevisedTourAttempt | null;
  latestComparison: ComparedValidation | null;
  tours: TourSummary[];
  comparisons: ComparedValidation[];
}

export const REMEDIATION_LOCAL_EXPORT_ACKNOWLEDGEMENT =
  "migration-stream/local-remediation-data-export/v1" as const;
export const REMEDIATION_OFFENDING_VALUES_ACKNOWLEDGEMENT =
  "migration-stream/local-remediation-offending-values-export/v1" as const;

export interface RemediationKeyColumn {
  entityId: string;
  sourceFile: string;
  sourceColumnOrdinal: number;
  sourceColumn: string;
}

export interface RemediationPackOptions {
  schemaVersion: typeof SCHEMAS.remediationPackOptions;
  engagementId: string;
  engagementGeneration: number;
  workspaceId: string;
  workspaceDescriptorHash: string;
  jobId: string;
  sourceHash: string;
  manifestHash: string;
  profilingRunId: string;
  profilingResultHash: string;
  profileId: string;
  profileVersion: string;
  profileHash: string;
  profileApprovalId: string;
  profileApprovalHash: string;
  mappingId: string;
  mappingVersion: string;
  mappingHash: string;
  mappingAuthorityId: string;
  mappingAuthorityHash: string;
  validationRunId: string;
  validationResultHash: string;
  issueCount: number;
  optionsHash: string;
  files: Array<{
    entityId: string;
    sourceFile: string;
    columns: Array<{
      sourceColumnOrdinal: number;
      sourceColumn: string;
      suggested: boolean;
      selected: false;
    }>;
  }>;
}

export interface CreateRemediationPackCommand {
  schemaVersion: typeof ENGAGEMENT_COMMAND_SCHEMAS.createRemediationPack;
  expectedEngagementGeneration: number;
  expectedValidationRunId: string;
  expectedOptionsHash: string;
  selectedKeyColumns: RemediationKeyColumn[];
  actor: string;
  reason: string;
  localDataExportAcknowledgement: typeof REMEDIATION_LOCAL_EXPORT_ACKNOWLEDGEMENT;
  includeOffendingValues: boolean;
  offendingValuesAcknowledgement:
    typeof REMEDIATION_OFFENDING_VALUES_ACKNOWLEDGEMENT | null;
  idempotencyKey: string;
}

export interface RemediationPackView {
  packId: string;
  state: "COMPLETE";
  engagementId: string;
  validationRunId: string;
  sourceHash: string;
  selectedKeyColumns: RemediationKeyColumn[];
  includeOffendingValues: boolean;
  actor: string;
  reason: string;
  issueCount: number;
  memberCount: number;
  archiveSizeBytes: number;
  archiveSha256: string;
  membershipHash: string;
  authorityHash: string;
  downloadToken: string;
  createdAt: string;
  completedAt: string;
}

export interface RemediationPackHistory {
  schemaVersion: typeof SCHEMAS.remediationPackHistory;
  totalCount: number;
  returnedCount: number;
  truncated: boolean;
  items: RemediationPackView[];
}

export interface RemediationPackCommandResult {
  schemaVersion: typeof SCHEMAS.remediationPack;
  pack: RemediationPackView;
  reusedAttempt: boolean;
}

export interface AcceptRevisedSourceResult {
  schemaVersion: typeof ENGAGEMENT_COMMAND_SCHEMAS.acceptRevisedSource;
  engagementGeneration: number;
  tourAttempt: RevisedTourAttempt;
  reuseAuthority: MappingReuseAuthority;
  profilingResultHash: string;
  reusedProfile: boolean;
  idempotentReplay: boolean;
}

export interface LaunchRevisedValidationResult {
  schemaVersion: typeof ENGAGEMENT_COMMAND_SCHEMAS.launchRevisedValidation;
  engagementGeneration: number;
  activeTour: TourSummary;
  comparison: ComparedValidation;
  idempotentReplay: boolean;
}

export type EngagementStateCommand =
  | CreateEngagementCommand
  | AcceptSourceCommand
  | SelectObjectPackCommand
  | ApproveObjectPackProfileCommand
  | PrepareMappingCommand
  | SaveMappingDraftCommand
  | SubmitMappingDraftCommand
  | ApproveMappingVersionCommand
  | LaunchValidationCommand;

export function canGovernIssueDecisions(
  context: WorkbenchContext,
): context is
  | GovernedWorkbenchContext
  | ResultApprovalWorkbenchContext
  | PublicationWorkbenchContext {
  return (
    (context.schema_version === SCHEMAS.governedContext ||
      context.schema_version === SCHEMAS.resultApprovalContext ||
      context.schema_version === SCHEMAS.publicationContext) &&
    !context.read_only &&
    context.capabilities.governed_issue_decisions &&
    context.capabilities.correction_overlays &&
    !context.capabilities.authenticated_practitioner_identity
  );
}

export function canApproveMigrationResult(
  context: WorkbenchContext,
): context is ResultApprovalWorkbenchContext | PublicationWorkbenchContext {
  return (
    (context.schema_version === SCHEMAS.resultApprovalContext ||
      context.schema_version === SCHEMAS.publicationContext) &&
    !context.read_only &&
    context.capabilities.governed_issue_decisions &&
    context.capabilities.correction_overlays &&
    context.capabilities.migration_result_approval &&
    !context.capabilities.authenticated_practitioner_identity &&
    !context.capabilities.source_mutation &&
    !context.capabilities.original_projection_mutation &&
    !context.capabilities.profile_mutation &&
    !context.capabilities.mapping_mutation &&
    !context.capabilities.target_write &&
    context.capabilities.publication ===
      (context.schema_version === SCHEMAS.publicationContext)
  );
}

export function canPublishReadiness(
  context: WorkbenchContext,
): context is PublicationWorkbenchContext {
  return (
    context.schema_version === SCHEMAS.publicationContext &&
    !context.read_only &&
    context.capabilities.governed_issue_decisions &&
    context.capabilities.correction_overlays &&
    context.capabilities.migration_result_approval &&
    context.capabilities.publication &&
    !context.capabilities.authenticated_practitioner_identity &&
    !context.capabilities.source_mutation &&
    !context.capabilities.original_projection_mutation &&
    !context.capabilities.profile_mutation &&
    !context.capabilities.mapping_mutation &&
    !context.capabilities.target_write
  );
}

export interface PublicationEligibility {
  schema_version: typeof SCHEMAS.publicationEligibility;
  is_eligible: boolean;
  job_id: string;
  candidate_id: string | null;
  candidate_hash: string | null;
  approval_id: string | null;
  approval_hash: string | null;
  total_accepted_rows: number;
  included_rows: number;
  excluded_rows: number;
  blocked_rows: number;
  blockers: string[];
  warnings: string[];
  required_acknowledgments: string[];
  evaluated_at: string;
}

export interface PublicationSummaryItem {
  publication_id: string;
  job_id: string;
  candidate_id: string;
  approval_id: string;
  status: string;
  practitioner: string;
  publication_reason: string;
  semantic_publication_hash: string | null;
  physical_archive_digest: string | null;
  package_size_bytes: number | null;
  archive_relative_path: string | null;
  created_at: string;
  completed_at: string | null;
}

export interface PublicationListPage {
  schema_version: typeof SCHEMAS.publicationList;
  job_id: string;
  items: PublicationSummaryItem[];
  total_count: number;
}

export interface EngagementReadinessPackageHistory {
  schema_version: typeof SCHEMAS.engagementReadinessPackages;
  engagement_id: string;
  total_count: number;
  returned_count: number;
  truncated: boolean;
  items: Array<{
    publication_id: string;
    job_id: string;
    candidate_id: string;
    approval_id: string;
    status: "COMPLETE";
    practitioner: string;
    publication_reason: string;
    semantic_publication_hash: string;
    physical_archive_digest: string;
    package_size_bytes: number;
    archive_relative_path: string;
    created_at: string;
    completed_at: string;
  }>;
}

export interface ManifestMember {
  filename: string;
  media_type: string;
  row_count?: number;
  byte_size: number;
  sha256_digest: string;
}

export interface PublicationManifest {
  publication_id: string;
  publication_version: string;
  contract_version: string;
  application_version: string;
  engine_version: string;
  created_at: string;
  practitioner: string;
  publication_reason: string;
  disclaimer: string;
  source_package_digest: string;
  profile_identity: string;
  profile_version: string;
  profile_digest: string;
  mapping_identity?: string;
  mapping_version?: string;
  mapping_digest: string;
  validation_run_id?: string;
  validation_digest: string;
  reconciliation_snapshot_id?: string;
  reconciliation_snapshot_hash: string;
  candidate_id: string;
  candidate_hash: string;
  approval_id: string;
  approval_hash: string;
  approval_actor?: string;
  approval_reason?: string;
  approval_occurred_at?: string;
  governing_decision_count?: number;
  included_valid_count: number;
  included_corrected_count: number;
  included_waived_count: number;
  included_accepted_as_is_count: number;
  excluded_count: number;
  blocked_unresolved_count: number;
  blocked_incomplete_count: number;
  blocked_conflict_count: number;
  total_accepted_rows: number;
  semantic_publication_hash: string;
  member_inventory: ManifestMember[];
}

export interface PublicationRecord {
  schema_version: typeof SCHEMAS.publicationDetail;
  publication_id: string;
  job_id: string;
  candidate_id: string;
  candidate_hash: string;
  approval_id: string;
  approval_hash: string;
  contract_version: string;
  status: string;
  semantic_publication_hash: string;
  physical_archive_digest: string;
  package_size_bytes: number;
  member_count: number;
  included_row_count: number;
  excluded_row_count: number;
  archive_relative_path: string;
  practitioner: string;
  publication_reason: string;
  manifest: PublicationManifest;
  created_at: string;
  completed_at: string;
}

export interface PublicationRequest {
  schema_version: typeof COMMAND_SCHEMAS.publish;
  job_id: string;
  candidate_id: string;
  approval_id: string;
  practitioner: string;
  publication_reason: string;
  acknowledgments: string[];
  idempotency_key: string;
}

export interface PageInfo {
  page_size: number;
  returned: number;
  next_cursor: string | null;
}

export interface JobSummary {
  schema_version: typeof SCHEMAS.job;
  data: {
    job_id: string;
    state: string;
    accepted_source: {
      source_name: string;
      source_hash: string;
      source_size: number;
      stored_archive: string;
    } | null;
    timestamps: {
      created_at: string;
      updated_at: string;
      completed_at: string | null;
    };
    counts: {
      accepted_file_count: number;
      accepted_row_count: number;
      accepted_field_count: number;
      pending_row_count: number;
      profiling_run_count: number;
    };
    reconciliation: {
      source_records: number;
      stored_records: number;
      pending_records: number;
      closed: boolean;
    };
    integrity_status: string;
    selected_governance: SelectedGovernance | null;
    warnings: Array<{ code: string; count: number }>;
  };
  canonical_hash: string;
}

export interface FileManifestPage {
  schema_version: typeof SCHEMAS.files;
  scope: { job_id: string; source_hash: string };
  items: Array<{
    file_id: string;
    path: string;
    source_ordinal: number;
    compressed_bytes: number;
    expanded_bytes: number;
    row_count: number;
    column_count: number;
    ingestion_state: string;
    profiling_state: string;
  }>;
  page: PageInfo;
  canonical_hash: string;
}

export interface ProfilingSummaryPage {
  schema_version: typeof SCHEMAS.profiling;
  scope: { run_id: string; job_id: string; result_hash: string };
  include_samples: boolean;
  items: Array<{
    path: string;
    column_ordinal: number;
    column_name: string;
    empty_count: number;
    nonempty_count: number;
    min_decoded_length: number | null;
    max_decoded_length: number | null;
    oversized_count: number;
    distinct_status: string;
    distinct_count: number | null;
    sample_count: number;
    samples?: Array<{
      sha256: string;
      utf8_bytes: number;
      decoded_length: number;
      value?: string;
    }>;
  }>;
  page: PageInfo;
  canonical_hash: string;
}

export type JsonValue =
  string | number | boolean | null | JsonValue[] | { [key: string]: JsonValue };

export interface ProfileView {
  schema_version: typeof SCHEMAS.profile;
  data: {
    profile_id: string;
    profile_version: string;
    profile_hash: string;
    schema_hash: string;
    document: JsonValue;
    declared_status: string;
    validation_hash: string;
    registered_at: string;
    lifecycle_state: string;
    approvals: Array<{
      approval_id: string;
      approver: string;
      reason: string;
      acknowledged: boolean;
      approved_at: string;
      approval_hash: string;
    }>;
    lifecycle_events: Array<{
      sequence: number;
      from_state: string | null;
      to_state: string;
      actor: string;
      reason: string;
      previous_event_hash: string | null;
      event_hash: string;
      occurred_at: string;
    }>;
  };
  canonical_hash: string;
}

export interface MappingCoveragePage {
  schema_version: typeof SCHEMAS.mapping;
  scope: {
    mapping_id: string;
    mapping_version: string;
    mapping_hash: string;
  };
  mapping: {
    mapping_id: string;
    mapping_version: string;
    mapping_hash: string;
    profile_id: string;
    profile_version: string;
    profile_hash: string;
    source_hash: string;
    validated_job_id: string;
  };
  items: Array<{
    kind: string;
    mapping_kind: string | null;
    source_file: string | null;
    source_column_ordinal: number | null;
    source_column_name: string | null;
    entity_id: string | null;
    field_id: string | null;
    binding_id: string | null;
    issue_code: string | null;
    detail: string | null;
  }>;
  page: PageInfo;
  canonical_hash: string;
}

export interface ValidationSummary {
  schema_version: typeof SCHEMAS.validation;
  data: {
    run_id: string;
    identity_hash: string;
    job_id: string;
    source_hash: string;
    profile_id: string;
    profile_version: string;
    profile_hash: string;
    mapping_id: string;
    mapping_version: string;
    mapping_hash: string;
    rule_set_hash: string;
    engine_version: string;
    policy_hash: string;
    state: string;
    result_hash: string | null;
    issue_count: number;
    rows_processed: number;
    error_code: string | null;
    error_message: string | null;
    timestamps: {
      created_at: string;
      updated_at: string;
      completed_at: string | null;
    };
    issue_breakdowns: Record<
      "by_severity" | "by_rule" | "by_entity" | "by_field" | "by_source_file",
      { items: Array<{ key: string; count: number }>; truncated: boolean }
    >;
  };
  canonical_hash: string;
}

export interface IssueListPage {
  schema_version: typeof SCHEMAS.issues;
  scope: {
    run_id: string;
    identity_hash: string;
    job_id: string;
    source_hash: string;
    profile_id: string;
    profile_version: string;
    profile_hash: string;
    mapping_id: string;
    mapping_version: string;
    mapping_hash: string;
    state: string;
    result_hash: string | null;
  };
  filters: {
    severity: string | null;
    rule_id: string | null;
    entity_id: string | null;
    field_id: string | null;
    source_file: string | null;
    source_ordinal: number | null;
  };
  items: Array<{
    issue_id: string;
    severity: string;
    rule_id: string;
    issue_code: string;
    display_message: string;
    entity_id: string;
    field_id: string;
    source_file: string;
    source_ordinal: number;
    line_start: number;
    line_end: number;
    issue_hash: string;
  }>;
  page: PageInfo;
  canonical_hash: string;
}

export interface RowDetail {
  schema_version: typeof SCHEMAS.row;
  data: {
    run_id: string;
    job_id: string;
    source_hash: string;
    source_file: string;
    source_ordinal: number;
    line_start: number;
    line_end: number;
    source_values: Array<{
      column_ordinal: number;
      column_name: string;
      value: string;
    }>;
    canonical_projection: Array<{
      entity_id: string;
      field_id: string;
      value: string | null;
      lineage: {
        kind: string;
        binding_id: string;
        source_column_ordinal: number | null;
        source_column_name: string | null;
        lookup_table_id: string | null;
      };
    }>;
    issues: Array<{
      issue_id: string;
      severity: string;
      rule_id: string;
      issue_code: string;
      display_message: string;
      entity_id: string;
      field_id: string;
      issue_hash: string;
    }>;
  };
  canonical_hash: string;
}

export const DECISION_DISPOSITIONS = [
  "CORRECTED",
  "WAIVED",
  "ACCEPTED_AS_IS",
  "EXCLUDED",
] as const;

export type DecisionDisposition = (typeof DECISION_DISPOSITIONS)[number];

export const CORRECTION_OPERATIONS = [
  "REPLACE_EFFECTIVE_TEXT",
  "SET_EFFECTIVE_EMPTY",
  "SET_EFFECTIVE_NULL",
] as const;

export type CorrectionOperation = (typeof CORRECTION_OPERATIONS)[number];

export const REEVALUATION_STATES = [
  "NOT_REQUIRED",
  "PLANNED",
  "RUNNING",
  "COMPLETE",
  "INTERRUPTED",
  "FAILED",
  "BOUND_EXCEEDED",
  "INTEGRITY_BLOCKED",
  "STALE_INPUT",
] as const;

export type ReevaluationState = (typeof REEVALUATION_STATES)[number];

export const DECISION_OUTCOMES = [
  "EFFECTIVE",
  "PENDING_REEVALUATION",
  "SUPERSEDED",
  "FAILED",
  "INTERRUPTED",
  "STALE_REJECTED",
  "INTEGRITY_BLOCKED",
] as const;

export type DecisionLifecycleOutcome = (typeof DECISION_OUTCOMES)[number];

export const DECISION_ATTEMPT_STATES = [
  "RECEIVED",
  "VALIDATED",
  "REJECTED",
  "CONFLICT",
  "OVERLAY_PERSISTED",
  "REEVALUATION_PLANNED",
  "REEVALUATION_RUNNING",
  "REEVALUATION_COMPLETE",
  "REEVALUATION_INTERRUPTED",
  "REEVALUATION_FAILED",
  "REEVALUATION_BOUND_EXCEEDED",
  "REEVALUATION_INTEGRITY_BLOCKED",
  "REEVALUATION_STALE_INPUT",
  "EFFECTIVE",
] as const;

export type DecisionAttemptState = (typeof DECISION_ATTEMPT_STATES)[number];

export const DECISION_INTEGRITY_STATUSES = [
  "NOT_CHECKED",
  "VERIFIED",
  "FAILED",
  "INTEGRITY_BLOCKED",
] as const;

export type DecisionIntegrityStatus =
  (typeof DECISION_INTEGRITY_STATUSES)[number];

export const CANONICAL_LINEAGE_KINDS = [
  "DIRECT",
  "CONSTANT",
  "EMPTY_AS_NULL",
  "TRIM",
  "UPPERCASE_ASCII",
  "LOWERCASE_ASCII",
  "LOOKUP",
] as const;

export type CanonicalLineage = {
  kind: (typeof CANONICAL_LINEAGE_KINDS)[number];
  binding_id: string;
  source_column_ordinal: number | null;
  source_column_name: string | null;
  lookup_table_id: string | null;
};

export interface DecisionSummary {
  decision_id: string;
  disposition: DecisionDisposition;
  practitioner_identifier: string;
  applied_at: string;
  reason_summary: string;
  correction_overlay_id: string | null;
  reevaluation_id: string | null;
  reevaluation_state: ReevaluationState;
  generation: number;
  supersession_count: number;
  lifecycle_outcome: DecisionLifecycleOutcome;
  integrity_status: DecisionIntegrityStatus;
  active: boolean;
}

export interface DecisionAttemptSummary {
  decision_attempt_id: string;
  disposition: DecisionDisposition;
  correction_overlay_id: string | null;
  reevaluation_id: string | null;
  attempt_state: DecisionAttemptState;
  reevaluation_state: ReevaluationState;
  lifecycle_outcome: DecisionLifecycleOutcome;
  integrity_status: DecisionIntegrityStatus;
  active: boolean;
}

export interface DecisionQueuePage {
  schema_version: typeof SCHEMAS.decisionQueue;
  scope: { job_id: string; validation_run_id: string };
  filters: {
    disposition: DecisionDisposition | "UNRESOLVED" | null;
    severity: string | null;
    entity_id: string | null;
    field_id: string | null;
    rule_id: string | null;
    source_file: string | null;
    reevaluation_state: ReevaluationState | null;
  };
  items: Array<{
    issue_id: string;
    severity: string;
    rule_id: string;
    issue_code: string;
    display_message: string;
    entity_id: string;
    field_id: string;
    source_file: string;
    source_ordinal: number;
    active_decision: DecisionSummary | null;
    latest_attempt: DecisionAttemptSummary | null;
  }>;
  page: PageInfo;
  canonical_hash: string;
}

export interface DecisionCounts {
  schema_version: typeof SCHEMAS.decisionCounts;
  scope: { job_id: string; validation_run_id: string };
  groups: Record<
    | "by_disposition"
    | "by_severity"
    | "by_entity"
    | "by_field"
    | "by_rule"
    | "by_source_file"
    | "by_reevaluation_state",
    { items: Array<{ key: string; count: number }>; truncated: boolean }
  >;
  canonical_hash: string;
}

export interface DecisionAwareIssueDetail {
  schema_version: typeof SCHEMAS.decisionIssue;
  data: {
    job_id: string;
    validation_run_id: string;
    issue_id: string;
    issue_hash: string;
    severity: string;
    rule_id: string;
    issue_code: string;
    display_message: string;
    entity_id: string;
    field_id: string;
    accepted_row: {
      source_file: string;
      source_ordinal: number;
      line_start: number;
      line_end: number;
    };
    field_context: {
      received_source_value: string | null;
      original_canonical_value: string | null;
      original_lineage: CanonicalLineage;
      active_overlay_value: string | null;
      effective_canonical_value: string | null;
      effective_value_hash: string;
      active_overlay_id: string | null;
      correction_eligible: boolean;
      allowed_operations: CorrectionOperation[];
      constraints: {
        maximum_characters: number | null;
        allow_empty: boolean;
        allow_null: boolean;
      };
    };
    active_decision: DecisionSummary | null;
    latest_attempt: DecisionAttemptSummary | null;
    decision_history_count: number;
    concurrency: {
      expected_active_decision_id: string | null;
      expected_active_generation: number;
      expected_effective_value_hash: string;
    };
    authority_notice: string;
  };
  canonical_hash: string;
}

export interface DecisionHistoryPage {
  schema_version: typeof SCHEMAS.decisionHistory;
  scope: { job_id: string; validation_run_id: string; issue_id: string };
  items: Array<{
    decision_id: string;
    disposition: DecisionDisposition;
    practitioner_identifier: string;
    applied_at: string;
    reason: string;
    evidence_reference: string | null;
    prior_decision_id: string | null;
    generation: number;
    correction_overlay_id: string | null;
    reevaluation_id: string | null;
    canonical_hash: string;
    lifecycle_outcome: DecisionLifecycleOutcome;
    active: boolean;
  }>;
  page: PageInfo;
  canonical_hash: string;
}

export interface CorrectionOverlayDetail {
  schema_version: typeof SCHEMAS.correction;
  data: {
    overlay_id: string;
    decision_attempt_id: string;
    decision_id: string | null;
    job_id: string;
    validation_run_id: string;
    issue_id: string;
    source_file: string;
    source_ordinal: number;
    entity_id: string;
    field_id: string;
    profile_id: string;
    profile_version: string;
    profile_hash: string;
    mapping_id: string;
    mapping_version: string;
    mapping_hash: string;
    operation: CorrectionOperation;
    original_canonical_value: string | null;
    original_lineage: CanonicalLineage;
    prior_effective_value: string | null;
    overlay_value: string | null;
    effective_canonical_value: string | null;
    effective_value_hash: string | null;
    practitioner_identifier: string;
    reason: string;
    attempted_at: string;
    applied_at: string | null;
    active: boolean;
    superseded_by_overlay_id: string | null;
    reevaluation_id: string | null;
    attempt_state: DecisionAttemptState;
    reevaluation_state: ReevaluationState;
    lifecycle_outcome: DecisionLifecycleOutcome;
    error_code: string | null;
    error_message: string | null;
    integrity_status: DecisionIntegrityStatus;
  };
  canonical_hash: string;
}

export interface DecisionResult {
  schema_version:
    typeof SCHEMAS.decisionResult | typeof SCHEMAS.decisionCommandResult;
  data: {
    decision_id: string;
    disposition: DecisionDisposition;
    lifecycle_outcome: "EFFECTIVE";
    active: boolean;
    idempotent_replay: boolean;
    correction_overlay_id: string | null;
    reevaluation_id: string | null;
    reevaluation_state: "NOT_REQUIRED" | "COMPLETE";
    original_issue_present: boolean;
    residual_issue_count: number;
    new_issue_count: number;
    integrity_status: "VERIFIED";
  };
  canonical_hash: string;
}

interface DecisionCommandBase {
  validation_run_id: string;
  issue_hash: string;
  practitioner_identifier: string;
  reason: string;
  evidence_reference: string | null;
  expected_active_decision_id: string | null;
  expected_active_generation: number;
  acknowledge_not_migration_result_approval: true;
}

export interface CorrectIssueCommand extends DecisionCommandBase {
  schema_version: typeof COMMAND_SCHEMAS.corrected;
  correction: {
    source_file: string;
    source_ordinal: number;
    entity_id: string;
    field_id: string;
    operation: CorrectionOperation;
    value: string | null;
    expected_effective_value_hash: string;
  };
}

export interface WaiveIssueCommand extends DecisionCommandBase {
  schema_version: typeof COMMAND_SCHEMAS.waived;
  acknowledge_waiver_limitations: true;
}

export interface AcceptIssueAsIsCommand extends DecisionCommandBase {
  schema_version: typeof COMMAND_SCHEMAS.acceptedAsIs;
  expected_effective_value_hash: string;
  acknowledge_validation_truth_unchanged: true;
}

export interface ExcludeIssueRowCommand extends DecisionCommandBase {
  schema_version: typeof COMMAND_SCHEMAS.excluded;
  source_file: string;
  source_ordinal: number;
  acknowledge_row_scope: true;
}

export type DecisionCommand =
  | CorrectIssueCommand
  | WaiveIssueCommand
  | AcceptIssueAsIsCommand
  | ExcludeIssueRowCommand;

export const RECONCILIATION_CATEGORIES = [
  "INCLUDED_VALID",
  "INCLUDED_CORRECTED",
  "INCLUDED_WAIVED",
  "INCLUDED_ACCEPTED_AS_IS",
  "EXCLUDED",
  "BLOCKED_UNRESOLVED",
  "BLOCKED_INCOMPLETE",
  "BLOCKED_CONFLICT",
] as const;

export type ReconciliationCategory = (typeof RECONCILIATION_CATEGORIES)[number];

export const RECONCILIATION_ATTEMPT_STATES = [
  "PLANNED",
  "RUNNING",
  "COMPLETE",
  "INTERRUPTED",
  "FAILED",
  "BOUND_EXCEEDED",
  "STALE_INPUT",
  "INTEGRITY_BLOCKED",
  "CONFLICTED",
] as const;

export type ReconciliationAttemptState =
  (typeof RECONCILIATION_ATTEMPT_STATES)[number];

export interface ReconciliationCategoryCount {
  category: ReconciliationCategory;
  row_count: number;
}

export interface ResultReconciliationSnapshotSummary {
  snapshot_id: string;
  state: ReconciliationAttemptState;
  job_id: string;
  accepted_row_count: number;
  governing_input_hash: string;
  snapshot_hash: string | null;
  current: boolean;
  counts: ReconciliationCategoryCount[];
  warning_codes: string[];
  blocker_codes: string[];
  completed_at: string | null;
}

export interface ResultReconciliationSnapshotList {
  schema_version: typeof SCHEMAS.resultReconciliationSnapshots;
  scope: { job_id: string };
  items: ResultReconciliationSnapshotSummary[];
  page: PageInfo;
  canonical_hash: string;
}

export interface ResultReconciliationSnapshot {
  schema_version: typeof SCHEMAS.resultReconciliationSnapshot;
  data: ResultReconciliationSnapshotSummary;
  canonical_hash: string;
}

export interface ResultReconciliationRowList {
  schema_version: typeof SCHEMAS.resultReconciliationRows;
  scope: {
    job_id: string;
    snapshot_id: string;
    snapshot_hash: string;
  };
  filters: {
    category: ReconciliationCategory | null;
    source_file: string | null;
    source_ordinal: number | null;
    entity_id: string | null;
    issue_id: string | null;
    decision_id: string | null;
  };
  items: Array<{
    source_file: string;
    source_ordinal: number;
    line_start: number;
    line_end: number;
    entity_id: string;
    category: ReconciliationCategory;
    explanation_codes: string[];
    lineage_hashes: string[];
  }>;
  page: PageInfo;
  canonical_hash: string;
}

export interface ResultReconciliationLineageReference {
  reference_kind:
    | "VALIDATION_RUN"
    | "VALIDATION_ISSUE"
    | "ISSUE_DECISION"
    | "CORRECTION_OVERLAY"
    | "CORRECTION_EFFECTIVATION"
    | "REEVALUATION_RUN";
  reference_id: string;
  reference_hash: string;
}

export interface ResultReconciliationRowDetail {
  schema_version: typeof SCHEMAS.resultReconciliationRow;
  scope: {
    job_id: string;
    snapshot_id: string;
    snapshot_hash: string;
  };
  data: {
    source_file: string;
    source_ordinal: number;
    line_start: number;
    line_end: number;
    row_identity_hash: string;
    entity_id: string;
    category: ReconciliationCategory;
    explanation_codes: string[];
    lineage_references: ResultReconciliationLineageReference[];
    classification_hash: string;
  };
  canonical_hash: string;
}

export interface ResultCandidateSummary {
  candidate_id: string;
  snapshot_id: string;
  snapshot_hash: string;
  candidate_hash: string;
  current: boolean;
  eligible: boolean;
  included_count: number;
  excluded_count: number;
  blocked_count: number;
  warning_codes: string[];
  required_acknowledgments: string[];
  creator: string;
  preparation_reason: string;
  created_at: string;
}

export interface ResultCandidateList {
  schema_version: typeof SCHEMAS.resultCandidates;
  scope: { job_id: string };
  items: ResultCandidateSummary[];
  page: PageInfo;
  canonical_hash: string;
}

export interface ResultApprovalEligibility {
  schema_version: typeof SCHEMAS.resultEligibility;
  data: {
    candidate_id: string;
    candidate_hash: string;
    snapshot_id: string;
    snapshot_hash: string;
    eligible: boolean;
    current: boolean;
    reason_codes: string[];
    required_acknowledgments: string[];
    expected_active_action_id: string | null;
    expected_generation: number;
  };
  canonical_hash: string;
}

export const RESULT_ACTION_KINDS = [
  "APPROVED",
  "REJECTED",
  "APPROVAL_REVOKED",
  "APPROVAL_SUPERSEDED",
] as const;

export type ResultActionKind = (typeof RESULT_ACTION_KINDS)[number];

export interface ResultActionSummary {
  action_id: string;
  action_kind: ResultActionKind;
  action_hash: string;
  candidate_id: string;
  candidate_hash: string;
  actor: string;
  reason: string;
  generation: number;
  predecessor_action_id: string | null;
  occurred_at: string;
}

export interface ResultActionHistory {
  schema_version: typeof SCHEMAS.resultActions;
  scope: { job_id: string; candidate_id: string | null };
  items: ResultActionSummary[];
  page: PageInfo;
  canonical_hash: string;
}

export interface CalculateReconciliationCommand {
  schema_version: typeof RESULT_COMMAND_SCHEMAS.calculate;
  actor: string;
  reason: string;
}

export interface CreateResultCandidateCommand {
  schema_version: typeof RESULT_COMMAND_SCHEMAS.createCandidate;
  snapshot_id: string;
  snapshot_hash: string;
  creator: string;
  preparation_reason: string;
}

interface ResultActionCommandBase {
  candidate_id: string;
  candidate_hash: string;
  actor: string;
  reason: string;
  warning_acknowledgments: string[];
  acknowledge_local_only_boundary: true;
  expected_active_action_id: string | null;
  expected_generation: number;
}

export interface ApproveResultCandidateCommand extends ResultActionCommandBase {
  schema_version: typeof RESULT_COMMAND_SCHEMAS.approve;
}

export interface RejectResultCandidateCommand extends ResultActionCommandBase {
  schema_version: typeof RESULT_COMMAND_SCHEMAS.reject;
}

export interface RevokeResultApprovalCommand extends ResultActionCommandBase {
  schema_version: typeof RESULT_COMMAND_SCHEMAS.revoke;
}

export interface SupersedeResultApprovalCommand extends ResultActionCommandBase {
  schema_version: typeof RESULT_COMMAND_SCHEMAS.supersede;
  replacement_candidate_id: string;
  replacement_candidate_hash: string;
}

export type ResultCommand =
  | CalculateReconciliationCommand
  | CreateResultCandidateCommand
  | ApproveResultCandidateCommand
  | RejectResultCandidateCommand
  | RevokeResultApprovalCommand
  | SupersedeResultApprovalCommand;

export interface ResultReconciliationCommandResult {
  schema_version: typeof SCHEMAS.resultReconciliationCommand;
  data: {
    snapshot_id: string;
    snapshot_hash: string;
    state: "COMPLETE";
    current: boolean;
    accepted_row_count: number;
    blocked_count: number;
    idempotent_replay: boolean;
  };
  canonical_hash: string;
}

export interface ResultCandidateCommandResult {
  schema_version: typeof SCHEMAS.resultCandidateCommand;
  data: {
    candidate_id: string;
    candidate_hash: string;
    snapshot_id: string;
    snapshot_hash: string;
    current: boolean;
    eligible: boolean;
    generation: number;
    idempotent_replay: boolean;
  };
  canonical_hash: string;
}

export interface ResultActionCommandResult {
  schema_version: typeof SCHEMAS.resultActionCommand;
  data: {
    action_id: string;
    action_kind: ResultActionKind;
    action_hash: string;
    candidate_id: string;
    candidate_hash: string;
    generation: number;
    active_approval: boolean;
    idempotent_replay: boolean;
  };
  canonical_hash: string;
}

export type ResultCommandResult =
  | ResultReconciliationCommandResult
  | ResultCandidateCommandResult
  | ResultActionCommandResult;

export interface ReconciliationSummary {
  schema_version: typeof SCHEMAS.reconciliation;
  scope: {
    job_id: string;
    source_hash: string;
    validation_run_id: string | null;
  };
  aggregate: {
    source_records: number;
    stored_records: number;
    pending_records: number;
    closed: boolean;
    validation_rows_processed: number | null;
    validation_issue_count: number | null;
  };
  items: Array<{
    path: string;
    source_records: number;
    stored_records: number;
    pending_records: number;
    closed: boolean;
    validation_issue_count: number | null;
  }>;
  page: PageInfo;
  canonical_hash: string;
}

export interface OperationalCleanupPage {
  schema_version: typeof SCHEMAS.operations;
  scope: { states: string[] };
  items: Array<{
    cleanup_id: string;
    namespace: string;
    job_id: string | null;
    state: string;
    cleanup_attempts: number;
    last_error_code: string | null;
    diagnostic_present: boolean;
    created_at: string;
    updated_at: string;
    quarantined_at: string | null;
    cleaned_at: string | null;
  }>;
  page: PageInfo;
  canonical_hash: string;
}

export interface HttpErrorEnvelope {
  schema_version: typeof SCHEMAS.error;
  ok: false;
  error: {
    code: string;
    message: string;
    request_id: string;
    retryable: boolean;
  };
}

export class ContractError extends Error {
  readonly code: string;
  readonly retryable: boolean;
  readonly requestId: string | null;

  constructor(
    code: string,
    message: string,
    retryable = false,
    requestId: string | null = null,
  ) {
    super(message);
    this.name = "ContractError";
    this.code = code;
    this.retryable = retryable;
    this.requestId = requestId;
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isText(value: unknown): value is string {
  return typeof value === "string";
}

function isNullableText(value: unknown): value is string | null {
  return value === null || isText(value);
}

function isCount(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function isNullableCount(value: unknown): value is number | null {
  return value === null || isCount(value);
}

function isBoundedArray<T>(
  value: unknown,
  maximum: number,
  guard: (item: unknown) => item is T,
): value is T[] {
  return Array.isArray(value) && value.length <= maximum && value.every(guard);
}

function isPageInfo(value: unknown): value is PageInfo {
  return (
    isRecord(value) &&
    hasExactKeys(value, ["page_size", "returned", "next_cursor"]) &&
    isCount(value.page_size) &&
    value.page_size >= 1 &&
    value.page_size <= 200 &&
    isCount(value.returned) &&
    value.returned <= 200 &&
    isNullableText(value.next_cursor)
  );
}

function isSelectedGovernance(value: unknown): value is SelectedGovernance {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "profile_id",
      "profile_version",
      "profile_hash",
      "mapping_id",
      "mapping_version",
      "mapping_hash",
      "validation_run_id",
      "validation_state",
    ]) &&
    isText(value.profile_id) &&
    isText(value.profile_version) &&
    isText(value.profile_hash) &&
    isText(value.mapping_id) &&
    isText(value.mapping_version) &&
    isText(value.mapping_hash) &&
    isText(value.validation_run_id) &&
    isText(value.validation_state)
  );
}

function isContextShape(value: Record<string, unknown>): boolean {
  const commonKeys = [
    "schema_version",
    "read_only",
    "workspace_label",
    "job_id",
    "profiling_run_id",
    "selected_governance",
    "supported_contracts",
  ];
  const common =
    isText(value.workspace_label) &&
    isText(value.job_id) &&
    isNullableText(value.profiling_run_id) &&
    (value.selected_governance === null ||
      isSelectedGovernance(value.selected_governance)) &&
    isBoundedArray(value.supported_contracts, 64, isText);
  if (!common) return false;
  if (value.schema_version === SCHEMAS.context) {
    return hasExactKeys(value, commonKeys) && value.read_only === true;
  }
  const capabilities = value.capabilities;
  const governedSchema =
    value.schema_version === SCHEMAS.governedContext ||
    value.schema_version === SCHEMAS.resultApprovalContext ||
    value.schema_version === SCHEMAS.publicationContext;
  const expectedResultApproval =
    value.schema_version === SCHEMAS.resultApprovalContext ||
    value.schema_version === SCHEMAS.publicationContext;
  const expectedPublication =
    value.schema_version === SCHEMAS.publicationContext;
  return (
    governedSchema &&
    hasExactKeys(value, [...commonKeys, "capabilities"]) &&
    value.read_only === false &&
    isRecord(capabilities) &&
    hasExactKeys(capabilities, [
      "governed_issue_decisions",
      "correction_overlays",
      "authenticated_practitioner_identity",
      "source_mutation",
      "original_projection_mutation",
      "profile_mutation",
      "mapping_mutation",
      "migration_result_approval",
      "target_write",
      "publication",
    ]) &&
    capabilities.governed_issue_decisions === true &&
    capabilities.correction_overlays === true &&
    capabilities.authenticated_practitioner_identity === false &&
    capabilities.source_mutation === false &&
    capabilities.original_projection_mutation === false &&
    capabilities.profile_mutation === false &&
    capabilities.mapping_mutation === false &&
    capabilities.migration_result_approval === expectedResultApproval &&
    capabilities.target_write === false &&
    capabilities.publication === expectedPublication
  );
}

const JOURNEY_STAGE_LABELS: Record<JourneyStageId, string> = {
  SETUP: "Setup / Workspace",
  DESTINATION: "Destination",
  SOURCE: "Source",
  MAPPING: "Mapping",
  VALIDATE: "Validate",
  COMPARE: "Compare",
  ISSUES: "Issues",
  DECISIONS: "Decisions",
  RECONCILIATION: "Reconciliation",
  PACKAGE: "Package",
};

const JOURNEY_STAGE_HREFS: Record<JourneyStageId, readonly string[]> = {
  SETUP: ["/setup", "/"],
  DESTINATION: ["/destination", "/setup#object-pack", "/profile"],
  SOURCE: ["/setup", "/setup#profiled-source", "/files"],
  MAPPING: ["/setup#mapping-workspace", "/setup#mapping", "/mapping"],
  VALIDATE: ["/setup#validation", "/validation"],
  COMPARE: ["/comparison"],
  ISSUES: ["/issues"],
  DECISIONS: ["/decisions"],
  RECONCILIATION: ["/reconciliation", "/result-approval"],
  PACKAGE: ["/publication"],
};

function isJourneyStageId(value: unknown): value is JourneyStageId {
  return (
    isText(value) && (JOURNEY_STAGE_IDS as readonly string[]).includes(value)
  );
}

function isJourneyStageShape(value: unknown): value is JourneyStage {
  if (
    !isRecord(value) ||
    !hasExactKeys(value, [
      "id",
      "label",
      "status",
      "available",
      "href",
      "unavailableReason",
    ]) ||
    !isJourneyStageId(value.id) ||
    value.label !== JOURNEY_STAGE_LABELS[value.id] ||
    !["COMPLETED", "CURRENT", "AVAILABLE", "LOCKED"].includes(
      String(value.status),
    ) ||
    typeof value.available !== "boolean" ||
    !isNullableText(value.href) ||
    !isNullableText(value.unavailableReason)
  ) {
    return false;
  }
  if (value.status === "LOCKED") {
    return (
      value.available === false &&
      value.href === null &&
      isText(value.unavailableReason) &&
      value.unavailableReason.trim().length > 0
    );
  }
  return (
    value.available === true &&
    value.href !== null &&
    JOURNEY_STAGE_HREFS[value.id].includes(value.href) &&
    value.unavailableReason === null
  );
}

function isJourneyBindingShape(value: unknown): value is JourneyBinding {
  const shape =
    isRecord(value) &&
    hasExactKeys(value, [
      "engagementId",
      "engagementGeneration",
      "jobId",
      "sourceHash",
      "manifestHash",
      "profilingRunId",
      "objectPackId",
      "destinationOrigin",
      "destinationAuthorityId",
      "destinationOriginHash",
      "profileId",
      "profileVersion",
      "profileHash",
      "mappingId",
      "mappingVersion",
      "mappingHash",
      "validationRunId",
      "validationResultHash",
      "activeTourId",
      "activeTourNumber",
      "activeTourHash",
      "comparisonId",
      "comparisonHash",
      "entityCount",
    ]) &&
    isNullableText(value.engagementId) &&
    isNullableCount(value.engagementGeneration) &&
    isNullableText(value.jobId) &&
    (value.sourceHash === null || isHash(value.sourceHash)) &&
    (value.manifestHash === null || isHash(value.manifestHash)) &&
    isNullableText(value.profilingRunId) &&
    isNullableText(value.objectPackId) &&
    (value.destinationOrigin === null ||
      (isText(value.destinationOrigin) &&
        ["CONTROLLED_PACK", "WORKSPACE_PROFILE"].includes(
          value.destinationOrigin,
        ))) &&
    isNullableText(value.destinationAuthorityId) &&
    (value.destinationOriginHash === null ||
      isHash(value.destinationOriginHash)) &&
    isNullableText(value.profileId) &&
    isNullableText(value.profileVersion) &&
    (value.profileHash === null || isHash(value.profileHash)) &&
    isNullableText(value.mappingId) &&
    isNullableText(value.mappingVersion) &&
    (value.mappingHash === null || isHash(value.mappingHash)) &&
    isNullableText(value.validationRunId) &&
    (value.validationResultHash === null ||
      isHash(value.validationResultHash)) &&
    isNullableText(value.activeTourId) &&
    isNullableCount(value.activeTourNumber) &&
    (value.activeTourHash === null || isHash(value.activeTourHash)) &&
    isNullableText(value.comparisonId) &&
    (value.comparisonHash === null || isHash(value.comparisonHash)) &&
    isNullableCount(value.entityCount);
  if (!shape) return false;

  const destination = [
    value.destinationOrigin,
    value.destinationAuthorityId,
    value.destinationOriginHash,
  ];
  const profile = [value.profileId, value.profileVersion, value.profileHash];
  const destinationEmpty = destination.every((item) => item === null);
  const destinationComplete = destination.every((item) => item !== null);
  const profileEmpty = profile.every((item) => item === null);
  const profileComplete = profile.every((item) => item !== null);
  if (
    (!destinationEmpty && !destinationComplete) ||
    (!profileEmpty && !profileComplete)
  ) {
    return false;
  }
  // Startup-selected legacy workbench views can expose an exact profile
  // triple without an engagement destination binding. Once a destination
  // binding is present, however, the browser accepts only the complete
  // engagement-era authority tuple.
  if (destinationEmpty) {
    return value.objectPackId === null;
  }
  if (
    !profileComplete ||
    value.objectPackId === null ||
    value.objectPackId !== value.profileId
  ) {
    return false;
  }
  return value.destinationOrigin === "CONTROLLED_PACK"
    ? value.destinationAuthorityId === value.objectPackId &&
        value.destinationOriginHash === value.profileHash
    : value.destinationOrigin === "WORKSPACE_PROFILE";
}

function isCanonicalWorkspaceId(value: unknown): value is string {
  return (
    typeof value === "string" &&
    /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(value)
  );
}

function isCanonicalUtcMilliseconds(value: unknown): value is string {
  const parsed = typeof value === "string" ? Date.parse(value) : Number.NaN;
  return (
    typeof value === "string" &&
    /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/.test(value) &&
    Number.isFinite(parsed) &&
    new Date(parsed).toISOString() === value
  );
}

function isObjectWorkspaceRecordShape(
  value: unknown,
): value is ObjectWorkspaceRecord {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "schemaVersion",
      "workspaceId",
      "generation",
      "displayName",
      "migrationObjectNameOrScope",
      "creatorAttribution",
      "establishedBy",
      "createdAt",
      "descriptorHash",
    ]) &&
    value.schemaVersion === SCHEMAS.objectWorkspace &&
    isCanonicalWorkspaceId(value.workspaceId) &&
    value.generation === 1 &&
    isBoundedCommandText(value.displayName, 256, false, true) &&
    isBoundedCommandText(value.migrationObjectNameOrScope, 256, false, true) &&
    isBoundedCommandText(value.creatorAttribution, 256, false, true) &&
    ["INITIALIZED", "ADOPTED"].includes(String(value.establishedBy)) &&
    isCanonicalUtcMilliseconds(value.createdAt) &&
    isHash(value.descriptorHash)
  );
}

function isObjectWorkspaceCommandShape(
  value: unknown,
): value is ObjectWorkspaceCommand {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "schemaVersion",
      "expectedStateHash",
      "displayName",
      "migrationObjectNameOrScope",
      "creatorAttribution",
      "idempotencyKey",
    ]) &&
    (value.schemaVersion === OBJECT_WORKSPACE_COMMAND_SCHEMAS.initialize ||
      value.schemaVersion === OBJECT_WORKSPACE_COMMAND_SCHEMAS.adopt) &&
    isHash(value.expectedStateHash) &&
    isBoundedCommandText(value.displayName, 256, false, true) &&
    isBoundedCommandText(value.migrationObjectNameOrScope, 256, false, true) &&
    isBoundedCommandText(value.creatorAttribution, 256, false, true) &&
    isIdempotencyKey(value.idempotencyKey)
  );
}

function isObjectWorkspaceStatusShape(
  value: unknown,
): value is ObjectWorkspaceStatus {
  if (
    !isRecord(value) ||
    !hasExactKeys(value, [
      "schemaVersion",
      "state",
      "stateHash",
      "requiredAction",
      "unavailableReason",
      "pendingCommand",
      "workspace",
    ]) ||
    value.schemaVersion !== SCHEMAS.objectWorkspaceStatus ||
    !["INITIALIZE_REQUIRED", "ADOPT_REQUIRED", "ACTIVE"].includes(
      String(value.state),
    ) ||
    !isHash(value.stateHash)
  ) {
    return false;
  }
  if (value.state === "ACTIVE") {
    return (
      value.requiredAction === null &&
      value.unavailableReason === null &&
      value.pendingCommand === null &&
      isObjectWorkspaceRecordShape(value.workspace)
    );
  }
  const expectedAction =
    value.state === "INITIALIZE_REQUIRED" ? "INITIALIZE" : "ADOPT";
  const expectedSchema =
    expectedAction === "INITIALIZE"
      ? OBJECT_WORKSPACE_COMMAND_SCHEMAS.initialize
      : OBJECT_WORKSPACE_COMMAND_SCHEMAS.adopt;
  return (
    value.requiredAction === expectedAction &&
    isBoundedCommandText(value.unavailableReason, 1024, false, true) &&
    (value.pendingCommand === null ||
      (isObjectWorkspaceCommandShape(value.pendingCommand) &&
        value.pendingCommand.schemaVersion === expectedSchema)) &&
    value.workspace === null
  );
}

function isNullableBoundedText(
  value: unknown,
  maximum = 256,
): value is string | null {
  return value === null || isBoundedCommandText(value, maximum, false, true);
}

function isDestinationFieldState(
  value: unknown,
): value is DestinationFieldState {
  return (
    isText(value) && ["AVAILABLE", "ACTIVE", "NEEDS_REVIEW"].includes(value)
  );
}

function isDestinationDiffState(value: unknown): value is DestinationDiffState {
  return (
    isText(value) &&
    [
      "UNCHANGED",
      "ADDED",
      "REMOVED",
      "TECHNICALLY_CHANGED",
      "MANUAL_POTENTIAL_MATCH",
    ].includes(value)
  );
}

function isDestinationOrigin(value: unknown): value is DestinationOrigin {
  return (
    isText(value) && ["CONTROLLED_PACK", "WORKSPACE_PROFILE"].includes(value)
  );
}

function isDestinationTechnicalIdentity(value: unknown): value is string {
  return (
    typeof value === "string" &&
    value.length >= 3 &&
    value.length <= 768 &&
    !value.startsWith(".") &&
    !value.endsWith(".") &&
    !value.includes("..") &&
    /^[A-Za-z0-9._-]+$/.test(value)
  );
}

function isDestinationProfileId(value: unknown): value is string {
  return (
    typeof value === "string" &&
    value.length >= 3 &&
    value.length <= 128 &&
    /^[a-z0-9][a-z0-9.-]*$/.test(value)
  );
}

function isDestinationVersion(value: unknown): value is string {
  return (
    typeof value === "string" &&
    value.length <= 32 &&
    /^\d+\.\d+\.\d+$/.test(value)
  );
}

function isManualSupplementEvidenceShape(
  value: unknown,
): value is ManualSupplementEvidence {
  return (
    isRecord(value) &&
    hasExactKeys(value, ["supplementId", "actor", "reason"]) &&
    isDecisionIdentity(value.supplementId) &&
    isBoundedCommandText(value.actor, 256, false, true) &&
    isBoundedCommandText(value.reason, 4096, false, true)
  );
}

function isDestinationDraftFieldShape(
  value: unknown,
): value is DestinationDraftField {
  if (
    !isRecord(value) ||
    !hasExactKeys(value, [
      "fieldId",
      "label",
      "logicalType",
      "businessRequired",
      "sensitive",
      "key",
      "state",
      "technicalProperty",
      "manualSupplement",
      "technicalChangeReview",
      "confirmMaxLength",
      "maxLength",
      "diffState",
    ]) ||
    !isDecisionIdentity(value.fieldId) ||
    !isBoundedCommandText(value.label, 512, false, true) ||
    !isDecisionIdentity(value.logicalType) ||
    typeof value.businessRequired !== "boolean" ||
    typeof value.sensitive !== "boolean" ||
    typeof value.key !== "boolean" ||
    !isDestinationFieldState(value.state) ||
    !(
      value.technicalProperty === null ||
      isDestinationTechnicalIdentity(value.technicalProperty)
    ) ||
    !(
      value.manualSupplement === null ||
      isManualSupplementEvidenceShape(value.manualSupplement)
    ) ||
    !(
      value.technicalChangeReview === null ||
      (isRecord(value.technicalChangeReview) &&
        hasExactKeys(value.technicalChangeReview, [
          "decision",
          "actor",
          "reason",
        ]) &&
        value.technicalChangeReview.decision ===
          "ACCEPT_REVISED_TECHNICAL_EVIDENCE" &&
        isBoundedCommandText(
          value.technicalChangeReview.actor,
          256,
          false,
          true,
        ) &&
        isBoundedCommandText(
          value.technicalChangeReview.reason,
          4096,
          false,
          true,
        ))
    ) ||
    typeof value.confirmMaxLength !== "boolean" ||
    !(value.maxLength === null || isCount(value.maxLength)) ||
    !(value.diffState === null || isDestinationDiffState(value.diffState))
  ) {
    return false;
  }
  const technical = value.technicalProperty !== null;
  const manual = value.manualSupplement !== null;
  return (
    (technical || manual) &&
    (!value.confirmMaxLength || value.maxLength !== null) &&
    (value.technicalChangeReview === null ||
      (technical &&
        value.state === "ACTIVE" &&
        value.diffState === "TECHNICALLY_CHANGED"))
  );
}

function isDestinationDraftEntityShape(
  value: unknown,
): value is DestinationDraftEntity {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "entityId",
      "label",
      "root",
      "technicalEntity",
      "fields",
    ]) &&
    isCanonicalDestinationEntityId(value.entityId) &&
    isBoundedCommandText(value.label, 512, false, true) &&
    typeof value.root === "boolean" &&
    (value.technicalEntity === null ||
      isDestinationTechnicalIdentity(value.technicalEntity)) &&
    isBoundedArray(value.fields, 4096, isDestinationDraftFieldShape)
  );
}

function isCanonicalDestinationEntityId(value: unknown): value is string {
  return (
    typeof value === "string" &&
    new TextEncoder().encode(value).byteLength <= 128 &&
    /^[A-Za-z0-9][A-Za-z0-9_:-]*$/.test(value)
  );
}

function isDestinationRelationshipKeyPairShape(
  value: unknown,
): value is DestinationRelationshipKeyPair {
  return (
    isRecord(value) &&
    hasExactKeys(value, ["fromFieldId", "toFieldId"]) &&
    isDecisionIdentity(value.fromFieldId) &&
    isDecisionIdentity(value.toFieldId)
  );
}

function isDestinationDraftRelationshipShape(
  value: unknown,
): value is DestinationDraftRelationship {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "relationshipId",
      "fromEntityId",
      "toEntityId",
      "cardinality",
      "technicalNavigation",
      "keyPairs",
      "confirmed",
    ]) &&
    isDecisionIdentity(value.relationshipId) &&
    isDecisionIdentity(value.fromEntityId) &&
    isDecisionIdentity(value.toEntityId) &&
    ["0..*", "1..*"].includes(String(value.cardinality)) &&
    (value.technicalNavigation === null ||
      isDestinationTechnicalIdentity(value.technicalNavigation)) &&
    isBoundedArray(value.keyPairs, 16, isDestinationRelationshipKeyPairShape) &&
    value.keyPairs.length > 0 &&
    typeof value.confirmed === "boolean"
  );
}

function isDestinationDraftRuleShape(
  value: unknown,
): value is DestinationDraftRule {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "ruleId",
      "severity",
      "kind",
      "target",
      "message",
      "parameters",
    ]) &&
    isDecisionIdentity(value.ruleId) &&
    isBoundedCommandText(value.severity, 64, false, true) &&
    [
      "required",
      "max_length",
      "unique",
      "allowed_values",
      "pattern",
      "conditional_required",
      "reference_lookup",
    ].includes(String(value.kind)) &&
    isBoundedCommandText(value.target, 512, false, true) &&
    isBoundedCommandText(value.message, 1024, false, true) &&
    isJsonValue(value.parameters)
  );
}

function isDestinationCompatibilityAcknowledgementShape(
  value: unknown,
): value is DestinationCompatibilityAcknowledgement {
  return (
    isRecord(value) &&
    hasExactKeys(value, ["findingHash", "actor", "reason"]) &&
    isHash(value.findingHash) &&
    isBoundedCommandText(value.actor, 256, false, true) &&
    isBoundedCommandText(value.reason, 4096, false, true)
  );
}

function isDestinationDraftSnapshotShape(
  value: unknown,
): value is DestinationDraftSnapshot {
  if (
    !isRecord(value) ||
    !hasExactKeys(value, [
      "profileId",
      "title",
      "owner",
      "entities",
      "relationships",
      "rules",
      "compatibilityAcknowledgements",
    ]) ||
    !isDestinationProfileId(value.profileId) ||
    !isBoundedCommandText(value.title, 512, false, true) ||
    !isBoundedCommandText(value.owner, 256, false, true) ||
    !isBoundedArray(value.entities, 128, isDestinationDraftEntityShape) ||
    !isBoundedArray(
      value.relationships,
      512,
      isDestinationDraftRelationshipShape,
    ) ||
    !isBoundedArray(value.rules, 4096, isDestinationDraftRuleShape) ||
    !isBoundedArray(
      value.compatibilityAcknowledgements,
      4096,
      isDestinationCompatibilityAcknowledgementShape,
    )
  ) {
    return false;
  }
  const totalFields = value.entities.reduce(
    (count, entity) => count + entity.fields.length,
    0,
  );
  const entityIds = value.entities.map((entity) => entity.entityId);
  const technicalProperties = value.entities.flatMap((entity) =>
    entity.fields.flatMap((field) =>
      field.technicalProperty === null ? [] : [field.technicalProperty],
    ),
  );
  return (
    value.entities.length > 0 &&
    value.entities.filter((entity) => entity.root).length === 1 &&
    new Set(entityIds).size === entityIds.length &&
    value.entities.every((entity) => {
      const fieldIds = entity.fields.map((field) => field.fieldId);
      return new Set(fieldIds).size === fieldIds.length;
    }) &&
    new Set(technicalProperties).size === technicalProperties.length &&
    new Set(value.relationships.map((item) => item.relationshipId)).size ===
      value.relationships.length &&
    new Set(value.rules.map((item) => item.ruleId)).size ===
      value.rules.length &&
    totalFields > 0 &&
    totalFields <= 4096
  );
}

function isDestinationDraftShape(
  value: unknown,
): value is DestinationDraftView {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "schemaVersion",
      "draftId",
      "catalogueAuthorityId",
      "catalogueHash",
      "generation",
      "draftHash",
      "state",
      "snapshot",
      "freezeBlockers",
      "frozenProfileId",
      "frozenProfileVersion",
      "frozenProfileHash",
      "updatedAt",
    ]) &&
    value.schemaVersion === SCHEMAS.destinationDraft &&
    isDecisionIdentity(value.draftId) &&
    isDecisionIdentity(value.catalogueAuthorityId) &&
    isHash(value.catalogueHash) &&
    isCount(value.generation) &&
    value.generation > 0 &&
    isHash(value.draftHash) &&
    ["EDITABLE", "FROZEN"].includes(String(value.state)) &&
    isDestinationDraftSnapshotShape(value.snapshot) &&
    isBoundedArray(value.freezeBlockers, 4096, (item): item is string =>
      isBoundedCommandText(item, 4096, false, true),
    ) &&
    ((value.state === "EDITABLE" &&
      value.frozenProfileId === null &&
      value.frozenProfileVersion === null &&
      value.frozenProfileHash === null) ||
      (value.state === "FROZEN" &&
        isDecisionIdentity(value.frozenProfileId) &&
        isBoundedCommandText(value.frozenProfileVersion, 128, false, true) &&
        isHash(value.frozenProfileHash))) &&
    isBoundedCommandText(value.updatedAt, 64, false, true)
  );
}

function isDestinationReviewedProfileShape(
  value: unknown,
): value is FrozenDestinationProfileView {
  return isDestinationReviewedProfileEnvelope(
    value,
    isReviewedUnapprovedDestinationProfile,
  );
}

function isDestinationReviewedCandidateShape(
  value: unknown,
): value is FrozenDestinationProfileView {
  return isDestinationReviewedProfileEnvelope(
    value,
    (profile) =>
      isReviewedUnapprovedDestinationProfile(profile) ||
      isApprovedDestinationProfile(profile),
  );
}

function isDestinationReviewedProfileEnvelope(
  value: unknown,
  profileCheck: (profile: unknown) => boolean,
): value is FrozenDestinationProfileView {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "schemaVersion",
      "engagementGeneration",
      "profile",
      "originBindingId",
      "originBindingHash",
      "draftId",
      "draftGeneration",
      "draftHash",
    ]) &&
    value.schemaVersion === SCHEMAS.destinationReviewedProfile &&
    isCount(value.engagementGeneration) &&
    profileCheck(value.profile) &&
    isDecisionIdentity(value.originBindingId) &&
    isHash(value.originBindingHash) &&
    isDecisionIdentity(value.draftId) &&
    isCount(value.draftGeneration) &&
    value.draftGeneration > 0 &&
    isHash(value.draftHash)
  );
}

function isDestinationProfileApprovalShape(
  value: unknown,
): value is ApprovedDestinationProfileView {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "schemaVersion",
      "engagementGeneration",
      "profile",
      "destinationOrigin",
      "destinationAuthorityId",
      "destinationOriginHash",
    ]) &&
    value.schemaVersion === SCHEMAS.destinationProfileApproval &&
    isCount(value.engagementGeneration) &&
    isApprovedDestinationProfile(value.profile) &&
    value.destinationOrigin === "WORKSPACE_PROFILE" &&
    isDecisionIdentity(value.destinationAuthorityId) &&
    isHash(value.destinationOriginHash)
  );
}

function isDestinationArtifactViewShape(
  value: unknown,
): value is DestinationArtifactView {
  if (
    !isRecord(value) ||
    !hasExactKeys(value, [
      "schemaVersion",
      "catalogueAuthorityId",
      "origin",
      "artifactLabel",
      "system",
      "clientOrTenant",
      "release",
      "manualActor",
      "manualReason",
      "snapshotId",
      "snapshotSha256",
      "snapshotBytes",
      "catalogueHash",
      "cataloguePayloadSha256",
      "catalogueFileSha256",
      "parserName",
      "parserVersion",
      "odataGeneration",
      "namespaceCount",
      "namespaces",
      "entityCount",
      "propertyCount",
      "navigationCount",
      "compatibility",
      "createdAt",
    ]) ||
    value.schemaVersion !== SCHEMAS.destinationArtifact ||
    !isDecisionIdentity(value.catalogueAuthorityId) ||
    !["EDMX", "MANUAL"].includes(String(value.origin)) ||
    !isBoundedCommandText(value.artifactLabel, 256, false, true) ||
    !isNullableBoundedText(value.system, 512) ||
    !isNullableBoundedText(value.clientOrTenant, 512) ||
    !isNullableBoundedText(value.release, 512) ||
    !isNullableBoundedText(value.manualActor, 256) ||
    !isNullableBoundedText(value.manualReason, 4096) ||
    !(value.snapshotId === null || isDecisionIdentity(value.snapshotId)) ||
    !(value.snapshotSha256 === null || isHash(value.snapshotSha256)) ||
    !(value.snapshotBytes === null || isCount(value.snapshotBytes)) ||
    !isHash(value.catalogueHash) ||
    !isHash(value.cataloguePayloadSha256) ||
    !(
      value.catalogueFileSha256 === null || isHash(value.catalogueFileSha256)
    ) ||
    !isNullableBoundedText(value.parserName, 256) ||
    !isNullableBoundedText(value.parserVersion, 256) ||
    !(
      value.odataGeneration === null ||
      (isText(value.odataGeneration) &&
        ["V2", "V4"].includes(value.odataGeneration))
    ) ||
    !isCount(value.namespaceCount) ||
    !isBoundedArray(value.namespaces, 4096, (item): item is string =>
      isBoundedCommandText(item, 768, false, true),
    ) ||
    value.namespaces.length !== value.namespaceCount ||
    new Set(value.namespaces).size !== value.namespaces.length ||
    value.namespaces.some(
      (namespace, index, namespaces) =>
        index > 0 && namespaces[index - 1] > namespace,
    ) ||
    !isCount(value.entityCount) ||
    !isCount(value.propertyCount) ||
    !isCount(value.navigationCount) ||
    !isDestinationCompatibilityReportShape(value.compatibility) ||
    !isBoundedCommandText(value.createdAt, 64, false, true)
  ) {
    return false;
  }
  return value.origin === "EDMX"
    ? value.manualActor === null &&
        value.manualReason === null &&
        value.snapshotId !== null &&
        value.snapshotSha256 !== null &&
        value.snapshotBytes !== null &&
        value.catalogueFileSha256 !== null &&
        value.parserName !== null &&
        value.parserVersion !== null &&
        value.odataGeneration !== null
    : value.system === null &&
        value.clientOrTenant === null &&
        value.release === null &&
        value.manualActor !== null &&
        value.manualReason !== null &&
        value.snapshotId === null &&
        value.snapshotSha256 === null &&
        value.snapshotBytes === null &&
        value.catalogueFileSha256 === null &&
        value.parserName === null &&
        value.parserVersion === null &&
        value.odataGeneration === null;
}

function isDestinationCompatibilityFindingShape(
  value: unknown,
): value is DestinationCompatibilityFinding {
  if (
    !isRecord(value) ||
    !hasExactKeys(value, [
      "code",
      "classification",
      "technicalLocation",
      "count",
      "blocking",
      "findingHash",
    ]) ||
    !isBoundedCommandText(value.code, 256, false, true) ||
    !isText(value.classification) ||
    ![
      "SCOPE_NEUTRAL_INVENTORY",
      "NON_STRUCTURAL_WARNING",
      "UNRESOLVED_SELECTED_SCOPE",
      "UNSUPPORTED_IRRELEVANT",
      "SELECTED_SCOPE_WARNING",
      "SELECTED_SCOPE_OPERATOR_RESOLVED",
    ].includes(value.classification) ||
    !isBoundedCommandText(value.technicalLocation, 1024, false, true) ||
    !isCount(value.count) ||
    typeof value.blocking !== "boolean" ||
    !isHash(value.findingHash)
  ) {
    return false;
  }
  return (
    (value.classification === "UNRESOLVED_SELECTED_SCOPE" && value.blocking) ||
    (value.classification !== "UNRESOLVED_SELECTED_SCOPE" && !value.blocking)
  );
}

function isDestinationCompatibilityReportShape(
  value: unknown,
): value is DestinationCompatibilityReport {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "reportHash",
      "scopedCompletenessHash",
      "algorithm",
      "odataGeneration",
      "safelyExtractedCount",
      "unsupportedInventoryCount",
      "unsupportedIrrelevantCount",
      "unresolvedSelectedScopeCount",
      "findings",
    ]) &&
    isHash(value.reportHash) &&
    (value.scopedCompletenessHash === null ||
      isHash(value.scopedCompletenessHash)) &&
    isBoundedCommandText(value.algorithm, 256, false, true) &&
    (value.odataGeneration === null ||
      (isText(value.odataGeneration) &&
        ["V2", "V4"].includes(value.odataGeneration))) &&
    isCount(value.safelyExtractedCount) &&
    isCount(value.unsupportedInventoryCount) &&
    isCount(value.unsupportedIrrelevantCount) &&
    isCount(value.unresolvedSelectedScopeCount) &&
    isBoundedArray(value.findings, 4096, isDestinationCompatibilityFindingShape)
  );
}

function isActiveDestinationViewShape(
  value: unknown,
): value is ActiveDestinationView {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "origin",
      "destinationAuthorityId",
      "destinationOriginHash",
      "profileId",
      "profileVersion",
      "profileHash",
      "profileAuthority",
      "approvalId",
      "approvalHash",
    ]) &&
    isDestinationOrigin(value.origin) &&
    isDecisionIdentity(value.destinationAuthorityId) &&
    isHash(value.destinationOriginHash) &&
    isDestinationProfileId(value.profileId) &&
    isDestinationVersion(value.profileVersion) &&
    isHash(value.profileHash) &&
    ["NONE", "LOCAL_MAPPING_VALIDATION"].includes(
      String(value.profileAuthority),
    ) &&
    ((value.profileAuthority === "NONE" &&
      value.approvalId === null &&
      value.approvalHash === null) ||
      (value.profileAuthority === "LOCAL_MAPPING_VALIDATION" &&
        isDecisionIdentity(value.approvalId) &&
        isHash(value.approvalHash)))
  );
}

function isDestinationWorkshopLimitsShape(
  value: unknown,
): value is DestinationWorkshopLimits {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "maximumEdmxBytes",
      "maximumEntities",
      "maximumFields",
      "maximumRelationships",
      "maximumRules",
      "maximumPageSize",
    ]) &&
    isCount(value.maximumEdmxBytes) &&
    value.maximumEdmxBytes > 0 &&
    isCount(value.maximumEntities) &&
    isCount(value.maximumFields) &&
    isCount(value.maximumRelationships) &&
    isCount(value.maximumRules) &&
    isCount(value.maximumPageSize) &&
    value.maximumPageSize > 0 &&
    value.maximumPageSize <= 100
  );
}

function isEligibleCarryForwardSourceShape(
  value: unknown,
): value is EligibleCarryForwardSourceView {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "mappingId",
      "mappingVersion",
      "mappingHash",
      "mappingApprovalId",
      "mappingApprovalHash",
      "activeJobId",
      "activeSourceHash",
      "validatedJobId",
      "validatedSourceHash",
      "profileId",
      "profileVersion",
      "profileHash",
    ]) &&
    isDecisionIdentity(value.mappingId) &&
    isDestinationVersion(value.mappingVersion) &&
    isHash(value.mappingHash) &&
    isDecisionIdentity(value.mappingApprovalId) &&
    isHash(value.mappingApprovalHash) &&
    isDecisionIdentity(value.activeJobId) &&
    isHash(value.activeSourceHash) &&
    isDecisionIdentity(value.validatedJobId) &&
    isHash(value.validatedSourceHash) &&
    isDestinationProfileId(value.profileId) &&
    isDestinationVersion(value.profileVersion) &&
    isHash(value.profileHash) &&
    (value.activeJobId !== value.validatedJobId ||
      value.activeSourceHash === value.validatedSourceHash)
  );
}

function isDestinationWorkshopShape(value: Record<string, unknown>): boolean {
  return (
    hasExactKeys(value, [
      "schemaVersion",
      "workspaceId",
      "workspaceDescriptorHash",
      "engagementId",
      "engagementGeneration",
      "destinationOrigin",
      "activeDestination",
      "reviewedCandidate",
      "eligibleCarryForwardSource",
      "latestArtifact",
      "draft",
      "allowedOrigins",
      "limits",
    ]) &&
    value.schemaVersion === SCHEMAS.destinationWorkshop &&
    isCanonicalWorkspaceId(value.workspaceId) &&
    isHash(value.workspaceDescriptorHash) &&
    isDecisionIdentity(value.engagementId) &&
    isCount(value.engagementGeneration) &&
    (value.destinationOrigin === null ||
      isDestinationOrigin(value.destinationOrigin)) &&
    (value.activeDestination === null ||
      isActiveDestinationViewShape(value.activeDestination)) &&
    (value.reviewedCandidate === null ||
      isDestinationReviewedCandidateShape(value.reviewedCandidate)) &&
    (value.eligibleCarryForwardSource === null ||
      isEligibleCarryForwardSourceShape(value.eligibleCarryForwardSource)) &&
    (value.latestArtifact === null ||
      isDestinationArtifactViewShape(value.latestArtifact)) &&
    (value.draft === null || isDestinationDraftShape(value.draft)) &&
    isBoundedArray(value.allowedOrigins, 2, isDestinationOrigin) &&
    value.allowedOrigins.length === 2 &&
    new Set(value.allowedOrigins).size === value.allowedOrigins.length &&
    isDestinationWorkshopLimitsShape(value.limits) &&
    (value.activeDestination === null ||
      value.destinationOrigin === value.activeDestination.origin) &&
    destinationWorkshopCarrySourceMatchesActive(value) &&
    destinationWorkshopCandidateMatchesDraft(value)
  );
}

function destinationWorkshopCarrySourceMatchesActive(
  value: Record<string, unknown>,
): boolean {
  const source = value.eligibleCarryForwardSource;
  if (source === null) return true;
  const active = value.activeDestination;
  if (!isRecord(source) || !isRecord(active)) return false;
  return (
    active.profileAuthority === "LOCAL_MAPPING_VALIDATION" &&
    source.profileId === active.profileId &&
    source.profileVersion === active.profileVersion &&
    source.profileHash === active.profileHash
  );
}

function destinationWorkshopCandidateMatchesDraft(
  value: Record<string, unknown>,
): boolean {
  const draft = value.draft;
  const candidate = value.reviewedCandidate;
  if (!isRecord(draft) || draft.state !== "FROZEN") return true;
  if (!isRecord(candidate) || !isRecord(candidate.profile)) return false;
  return (
    candidate.draftId === draft.draftId &&
    candidate.draftGeneration === draft.generation &&
    candidate.draftHash === draft.draftHash &&
    candidate.profile.profile_id === draft.frozenProfileId &&
    candidate.profile.profile_version === draft.frozenProfileVersion &&
    candidate.profile.profile_hash === draft.frozenProfileHash
  );
}

function isDestinationCatalogueEntityShape(
  value: unknown,
): value is DestinationCatalogueEntity {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "technicalEntity",
      "namespace",
      "name",
      "keyProperties",
      "propertyCount",
      "navigationCount",
      "selected",
      "root",
    ]) &&
    isDestinationTechnicalIdentity(value.technicalEntity) &&
    isBoundedCommandText(value.namespace, 512, false, true) &&
    isBoundedCommandText(value.name, 256, false, true) &&
    isBoundedArray(value.keyProperties, 64, (item): item is string =>
      isBoundedCommandText(item, 512, false, true),
    ) &&
    isCount(value.propertyCount) &&
    isCount(value.navigationCount) &&
    typeof value.selected === "boolean" &&
    typeof value.root === "boolean" &&
    (!value.root || value.selected)
  );
}

function isDestinationCatalogueEntityPageShape(
  value: Record<string, unknown>,
): boolean {
  return (
    hasExactKeys(value, [
      "schemaVersion",
      "catalogueAuthorityId",
      "catalogueHash",
      "draftId",
      "draftGeneration",
      "draftHash",
      "items",
      "nextCursor",
    ]) &&
    value.schemaVersion === SCHEMAS.destinationCatalogueEntities &&
    isDecisionIdentity(value.catalogueAuthorityId) &&
    isHash(value.catalogueHash) &&
    ((value.draftId === null &&
      value.draftGeneration === null &&
      value.draftHash === null) ||
      (isDecisionIdentity(value.draftId) &&
        isCount(value.draftGeneration) &&
        isHash(value.draftHash))) &&
    isBoundedArray(value.items, 100, isDestinationCatalogueEntityShape) &&
    isNullableBoundedText(value.nextCursor, 4096)
  );
}

function isDestinationCataloguePropertyShape(
  value: unknown,
): value is DestinationCatalogueProperty {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "technicalProperty",
      "technicalEntity",
      "name",
      "edmType",
      "nullable",
      "maxLength",
      "precision",
      "scale",
      "keyCandidate",
      "requiredCandidate",
      "state",
      "field",
    ]) &&
    isDestinationTechnicalIdentity(value.technicalProperty) &&
    isDestinationTechnicalIdentity(value.technicalEntity) &&
    isBoundedCommandText(value.name, 256, false, true) &&
    isBoundedCommandText(value.edmType, 128, false, true) &&
    (value.nullable === null || typeof value.nullable === "boolean") &&
    isNullableBoundedText(value.maxLength, 64) &&
    isNullableBoundedText(value.precision, 64) &&
    isNullableBoundedText(value.scale, 64) &&
    typeof value.keyCandidate === "boolean" &&
    typeof value.requiredCandidate === "boolean" &&
    isDestinationFieldState(value.state) &&
    (value.field === null || isDestinationDraftFieldShape(value.field))
  );
}

function isDestinationCataloguePropertyPageShape(
  value: Record<string, unknown>,
): boolean {
  return (
    hasExactKeys(value, [
      "schemaVersion",
      "catalogueAuthorityId",
      "catalogueHash",
      "technicalEntity",
      "draftId",
      "draftGeneration",
      "draftHash",
      "items",
      "nextCursor",
    ]) &&
    value.schemaVersion === SCHEMAS.destinationCatalogueProperties &&
    isDecisionIdentity(value.catalogueAuthorityId) &&
    isHash(value.catalogueHash) &&
    isDestinationTechnicalIdentity(value.technicalEntity) &&
    ((value.draftId === null &&
      value.draftGeneration === null &&
      value.draftHash === null) ||
      (isDecisionIdentity(value.draftId) &&
        isCount(value.draftGeneration) &&
        isHash(value.draftHash))) &&
    isBoundedArray(value.items, 100, isDestinationCataloguePropertyShape) &&
    isNullableBoundedText(value.nextCursor, 4096)
  );
}

function isDestinationArtifactReservationShape(
  value: Record<string, unknown>,
): boolean {
  return (
    hasExactKeys(value, [
      "schemaVersion",
      "attemptId",
      "state",
      "artifactLabel",
      "sourceName",
      "maximumBytes",
      "createdAt",
    ]) &&
    value.schemaVersion === SCHEMAS.destinationArtifactReservation &&
    isDecisionIdentity(value.attemptId) &&
    value.state === "RECEIVING" &&
    isBoundedCommandText(value.artifactLabel, 256, false, true) &&
    isBoundedCommandText(value.sourceName, 255, false, true) &&
    isCount(value.maximumBytes) &&
    value.maximumBytes > 0 &&
    isBoundedCommandText(value.createdAt, 64, false, true)
  );
}

function isDestinationArtifactResultShape(
  value: Record<string, unknown>,
): boolean {
  return isDestinationArtifactViewShape(value);
}

function isDestinationRebaseShape(value: Record<string, unknown>): boolean {
  return (
    hasExactKeys(value, [
      "schemaVersion",
      "engagementGeneration",
      "rebaseId",
      "diffHash",
      "counts",
      "draft",
    ]) &&
    value.schemaVersion === SCHEMAS.destinationRebase &&
    isCount(value.engagementGeneration) &&
    isDecisionIdentity(value.rebaseId) &&
    isHash(value.diffHash) &&
    isRecord(value.counts) &&
    Object.values(value.counts).every(isCount) &&
    isDestinationDraftShape(value.draft)
  );
}

function isUnresolvedCarryForwardRelationshipShape(
  value: unknown,
): value is DestinationCarryForwardView["unresolvedRelationships"][number] {
  return (
    isRecord(value) &&
    hasExactKeys(value, ["relationshipId", "reason"]) &&
    isDecisionIdentity(value.relationshipId) &&
    isOneOf(value.reason, [
      "RELATIONSHIP_KEY_BINDING_UNRESOLVED",
      "RELATIONSHIP_CHANGED",
      "RELATIONSHIP_NOT_PRESENT",
    ] as const)
  );
}

function isDestinationCarryForwardShape(
  value: Record<string, unknown>,
): boolean {
  const shape =
    hasExactKeys(value, [
      "schemaVersion",
      "engagementGeneration",
      "carryForwardId",
      "resultHash",
      "clonedBindingCount",
      "unresolvedTargetCount",
      "unresolvedRelationshipCount",
      "unresolvedRelationships",
    ]) &&
    value.schemaVersion === SCHEMAS.destinationCarryForward &&
    isCount(value.engagementGeneration) &&
    isDecisionIdentity(value.carryForwardId) &&
    isHash(value.resultHash) &&
    isCount(value.clonedBindingCount) &&
    isCount(value.unresolvedTargetCount) &&
    isCount(value.unresolvedRelationshipCount) &&
    isBoundedArray(
      value.unresolvedRelationships,
      256,
      isUnresolvedCarryForwardRelationshipShape,
    );
  if (!shape) return false;
  const relationships =
    value.unresolvedRelationships as DestinationCarryForwardView["unresolvedRelationships"];
  return (
    value.unresolvedRelationshipCount === relationships.length &&
    relationships.every(
      (relationship, index) =>
        index === 0 ||
        relationships[index - 1].relationshipId < relationship.relationshipId,
    )
  );
}

function isJourneyShape(value: Record<string, unknown>): boolean {
  if (
    !hasExactKeys(value, [
      "schemaVersion",
      "currentStageId",
      "workspace",
      "binding",
      "stages",
    ]) ||
    !isJourneyStageId(value.currentStageId) ||
    !isObjectWorkspaceStatusShape(value.workspace) ||
    !isJourneyBindingShape(value.binding) ||
    !isBoundedArray(
      value.stages,
      JOURNEY_STAGE_IDS.length,
      isJourneyStageShape,
    ) ||
    value.stages.length !== JOURNEY_STAGE_IDS.length
  ) {
    return false;
  }
  const stages = value.stages;
  return (
    stages.every((stage, index) => stage.id === JOURNEY_STAGE_IDS[index]) &&
    stages.filter((stage) => stage.status === "CURRENT").length === 1 &&
    stages.some(
      (stage) =>
        stage.id === value.currentStageId && stage.status === "CURRENT",
    )
  );
}

function isEngagementContextShape(value: Record<string, unknown>): boolean {
  const capabilities = value.capabilities;
  const exactEnvelope =
    hasExactKeys(value, [
      "schemaVersion",
      "workspaceLabel",
      "engagement",
      "supportedContracts",
      "capabilities",
    ]) &&
    value.schemaVersion === SCHEMAS.engagementContext &&
    isBoundedCommandText(value.workspaceLabel, 256) &&
    (value.engagement === null || isEngagementStateShape(value.engagement)) &&
    isBoundedArray(value.supportedContracts, 64, isText) &&
    isRecord(capabilities) &&
    hasExactKeys(capabilities, [
      "engagementSetup",
      "sourceIntake",
      "textualProfiling",
      "reviewedObjectPackSelection",
      "explicitProfileApproval",
      "mappingPreparation",
      "validationLaunch",
      "governedIssueDecisions",
      "migrationResultApproval",
      "targetWrite",
      "publication",
    ]);
  if (!exactEnvelope) return false;
  const inactive =
    capabilities.engagementSetup === false &&
    capabilities.sourceIntake === false &&
    capabilities.textualProfiling === false &&
    capabilities.reviewedObjectPackSelection === false &&
    capabilities.explicitProfileApproval === false &&
    capabilities.mappingPreparation === false &&
    capabilities.validationLaunch === false &&
    capabilities.governedIssueDecisions === false;
  const active =
    capabilities.engagementSetup === true &&
    typeof capabilities.sourceIntake === "boolean" &&
    capabilities.textualProfiling === true &&
    capabilities.reviewedObjectPackSelection === true &&
    capabilities.explicitProfileApproval === true &&
    capabilities.mappingPreparation === true &&
    capabilities.validationLaunch === true &&
    typeof capabilities.governedIssueDecisions === "boolean";
  return (
    (inactive || active) &&
    capabilities.migrationResultApproval === false &&
    capabilities.targetWrite === false &&
    capabilities.publication === false
  );
}

function isSourceArchiveManifest(
  value: unknown,
): value is SourceArchiveManifest {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "entries",
      "csv_paths",
      "total_compressed_bytes",
      "total_expanded_bytes",
      "maximum_observed_compression_ratio",
    ]) &&
    isBoundedArray(
      value.entries,
      4096,
      (entry): entry is SourceArchiveManifest["entries"][number] =>
        Boolean(
          isRecord(entry) &&
          hasExactKeys(entry, [
            "path",
            "is_directory",
            "is_csv",
            "compressed_size",
            "expanded_size",
            "compression_ratio",
            "crc32",
          ]) &&
          isSafeSourceFile(entry.path) &&
          typeof entry.is_directory === "boolean" &&
          typeof entry.is_csv === "boolean" &&
          isCount(entry.compressed_size) &&
          isCount(entry.expanded_size) &&
          typeof entry.compression_ratio === "number" &&
          Number.isFinite(entry.compression_ratio) &&
          entry.compression_ratio >= 0 &&
          isCount(entry.crc32),
        ),
    ) &&
    isBoundedArray(value.csv_paths, 4096, isSafeSourceFile) &&
    isCount(value.total_compressed_bytes) &&
    isCount(value.total_expanded_bytes) &&
    typeof value.maximum_observed_compression_ratio === "number" &&
    Number.isFinite(value.maximum_observed_compression_ratio) &&
    value.maximum_observed_compression_ratio >= 0
  );
}

function isSourcePreflight(value: unknown): value is SourcePreflight {
  if (
    !isRecord(value) ||
    !hasExactKeys(value, [
      "schemaVersion",
      "preflightId",
      "manifest",
      "state",
      "sourceName",
      "sourceHash",
      "sourceSize",
      "policyHash",
      "manifestHash",
      "errorCode",
      "errorMessage",
      "createdAt",
      "updatedAt",
      "completedAt",
    ])
  ) {
    return false;
  }
  const common =
    value.schemaVersion === SCHEMAS.sourcePreflight &&
    isDecisionIdentity(value.preflightId) &&
    isOneOf(value.state, ["RECEIVING", "READY", "REJECTED", "INTERRUPTED"]) &&
    isBoundedCommandText(value.sourceName, 255, false, true) &&
    (value.sourceHash === null || isHash(value.sourceHash)) &&
    isNullableCount(value.sourceSize) &&
    isHash(value.policyHash) &&
    (value.manifestHash === null || isHash(value.manifestHash)) &&
    (value.manifest === null || isSourceArchiveManifest(value.manifest)) &&
    isNullableText(value.errorCode) &&
    isNullableText(value.errorMessage) &&
    isText(value.createdAt) &&
    isText(value.updatedAt) &&
    isNullableText(value.completedAt);
  if (!common) return false;
  if (value.state === "RECEIVING") {
    return (
      value.sourceHash === null &&
      value.sourceSize === null &&
      value.manifestHash === null &&
      value.manifest === null &&
      value.errorCode === null &&
      value.errorMessage === null &&
      value.completedAt === null
    );
  }
  if (value.state === "REJECTED" || value.state === "INTERRUPTED") {
    return (
      value.errorCode !== null &&
      value.errorMessage !== null &&
      value.manifest === null
    );
  }
  return (
    isHash(value.sourceHash) &&
    isCount(value.sourceSize) &&
    isHash(value.manifestHash) &&
    isSourceArchiveManifest(value.manifest) &&
    value.errorCode === null &&
    value.errorMessage === null &&
    isText(value.completedAt)
  );
}

function isSourcePreflightShape(value: Record<string, unknown>): boolean {
  return isSourcePreflight(value);
}

function isEngagementStateShape(value: unknown): value is EngagementState {
  if (
    !isRecord(value) ||
    !hasExactKeys(value, [
      "schemaVersion",
      "engagementId",
      "displayName",
      "phase",
      "generation",
      "active",
      "profileStage",
      "mappingStage",
      "validationStage",
      "updatedAt",
    ])
  ) {
    return false;
  }
  const active = value.active;
  return (
    value.schemaVersion === SCHEMAS.engagementState &&
    isDecisionIdentity(value.engagementId) &&
    isBoundedCommandText(value.displayName, 256) &&
    isOneOf(value.phase, ENGAGEMENT_PHASES) &&
    isCount(value.generation) &&
    isRecord(active) &&
    hasExactKeys(active, [
      "preflightId",
      "jobId",
      "profilingRunId",
      "objectPackId",
      "destinationOrigin",
      "destinationAuthorityId",
      "destinationOriginHash",
      "profileId",
      "profileVersion",
      "profileHash",
      "mappingId",
      "mappingVersion",
      "mappingHash",
      "validationRunId",
    ]) &&
    [
      "preflightId",
      "jobId",
      "profilingRunId",
      "objectPackId",
      "destinationAuthorityId",
      "profileId",
      "profileVersion",
      "mappingId",
      "mappingVersion",
      "validationRunId",
    ].every((key) => isNullableText(active[key])) &&
    (active.destinationOrigin === null ||
      isDestinationOrigin(active.destinationOrigin)) &&
    (active.destinationOriginHash === null ||
      isHash(active.destinationOriginHash)) &&
    (active.profileHash === null || isHash(active.profileHash)) &&
    (active.mappingHash === null || isHash(active.mappingHash)) &&
    isOneOf(value.profileStage, ["NOT_SELECTED", "REVIEWED", "APPROVED"]) &&
    isEngagementDestinationAuthorityCoherent(active, value.profileStage) &&
    isOneOf(value.mappingStage, ["NOT_PREPARED", "PREPARED", "VALIDATED"]) &&
    isOneOf(value.validationStage, [
      "NOT_LAUNCHED",
      "RUNNING",
      "COMPLETE",
      "FAILED",
      "INTERRUPTED",
      "BOUND_EXCEEDED",
    ]) &&
    isText(value.updatedAt)
  );
}

function isEngagementDestinationAuthorityCoherent(
  active: Record<string, unknown>,
  profileStage: unknown,
): boolean {
  const destination = [
    active.destinationOrigin,
    active.destinationAuthorityId,
    active.destinationOriginHash,
  ];
  const profile = [
    active.objectPackId,
    active.profileId,
    active.profileVersion,
    active.profileHash,
  ];
  const destinationEmpty = destination.every((item) => item === null);
  const destinationComplete = destination.every((item) => item !== null);
  const profileEmpty = profile.every((item) => item === null);
  const profileComplete = profile.every((item) => item !== null);
  if (!(
    (profileStage === "NOT_SELECTED" && destinationEmpty && profileEmpty) ||
    (["REVIEWED", "APPROVED"].includes(String(profileStage)) &&
      destinationComplete &&
      profileComplete)
  )) {
    return false;
  }
  if (destinationEmpty) return true;
  if (active.objectPackId !== active.profileId) return false;
  return active.destinationOrigin === "CONTROLLED_PACK"
    ? active.destinationAuthorityId === active.objectPackId &&
        active.destinationOriginHash === active.profileHash
    : active.destinationOrigin === "WORKSPACE_PROFILE";
}

function isEngagementDetailShape(value: Record<string, unknown>): boolean {
  return (
    hasExactKeys(value, [
      "schemaVersion",
      "state",
      "preflights",
      "availableObjectPackCount",
    ]) &&
    value.schemaVersion === SCHEMAS.engagementDetail &&
    isEngagementStateShape(value.state) &&
    isBoundedArray(value.preflights, 200, isSourcePreflight) &&
    isCount(value.availableObjectPackCount)
  );
}

function isEngagementRecordShape(value: Record<string, unknown>): boolean {
  return (
    hasExactKeys(value, [
      "schemaVersion",
      "engagementId",
      "displayName",
      "createdAt",
      "reopened",
    ]) &&
    value.schemaVersion === SCHEMAS.engagementRecord &&
    isDecisionIdentity(value.engagementId) &&
    isBoundedCommandText(value.displayName, 256) &&
    isText(value.createdAt) &&
    typeof value.reopened === "boolean"
  );
}

function isSourceAcceptanceShape(value: Record<string, unknown>): boolean {
  return (
    hasExactKeys(value, [
      "schemaVersion",
      "engagementId",
      "preflightId",
      "jobId",
      "profilingRunId",
      "sourceHash",
      "profilingResultHash",
      "reusedProfile",
    ]) &&
    value.schemaVersion === SCHEMAS.sourceAcceptance &&
    isDecisionIdentity(value.engagementId) &&
    isDecisionIdentity(value.preflightId) &&
    isDecisionIdentity(value.jobId) &&
    isDecisionIdentity(value.profilingRunId) &&
    isHash(value.sourceHash) &&
    isHash(value.profilingResultHash) &&
    typeof value.reusedProfile === "boolean"
  );
}

function isPreparedMappingShape(value: Record<string, unknown>): boolean {
  return (
    hasExactKeys(value, [
      "schemaVersion",
      "preparationId",
      "mappingHash",
      "document",
      "reusedImmutablePreparation",
      "validation",
    ]) &&
    value.schemaVersion === SCHEMAS.preparedMapping &&
    isDecisionIdentity(value.preparationId) &&
    isHash(value.mappingHash) &&
    isJsonValue(value.document) &&
    typeof value.reusedImmutablePreparation === "boolean" &&
    (value.validation === null || isJsonValue(value.validation))
  );
}

function isValidationLaunchShape(value: Record<string, unknown>): boolean {
  return (
    hasExactKeys(value, [
      "schemaVersion",
      "status",
      "report",
      "reusedAttempt",
    ]) &&
    value.schemaVersion === SCHEMAS.validationLaunch &&
    isJsonValue(value.status) &&
    (value.report === null || isJsonValue(value.report)) &&
    typeof value.reusedAttempt === "boolean"
  );
}

function isTourSummaryShape(value: unknown): value is TourSummary {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "tourId",
      "tourNumber",
      "predecessorTourId",
      "sourceHash",
      "manifestHash",
      "profilingRunId",
      "profileId",
      "profileVersion",
      "profileHash",
      "mappingId",
      "mappingVersion",
      "mappingHash",
      "mappingAuthorityId",
      "mappingAuthorityHash",
      "validationRunId",
      "validationResultHash",
      "tourHash",
      "completedAt",
    ]) &&
    isDecisionIdentity(value.tourId) &&
    isCount(value.tourNumber) &&
    value.tourNumber > 0 &&
    (value.predecessorTourId === null ||
      isDecisionIdentity(value.predecessorTourId)) &&
    isHash(value.sourceHash) &&
    isHash(value.manifestHash) &&
    isDecisionIdentity(value.profilingRunId) &&
    isDecisionIdentity(value.profileId) &&
    isDecisionIdentity(value.profileVersion) &&
    isHash(value.profileHash) &&
    isDecisionIdentity(value.mappingId) &&
    isDecisionIdentity(value.mappingVersion) &&
    isHash(value.mappingHash) &&
    isDecisionIdentity(value.mappingAuthorityId) &&
    isHash(value.mappingAuthorityHash) &&
    isDecisionIdentity(value.validationRunId) &&
    isHash(value.validationResultHash) &&
    isHash(value.tourHash) &&
    isText(value.completedAt)
  );
}

function isRevisedTourAttemptShape(
  value: unknown,
): value is RevisedTourAttempt {
  if (
    !isRecord(value) ||
    !hasExactKeys(value, [
      "tourAttemptId",
      "tourNumber",
      "baselineTourId",
      "state",
      "generation",
      "preflightId",
      "sourceHash",
      "manifestHash",
      "jobId",
      "profilingRunId",
      "structureHash",
      "reuseAuthorityId",
      "reuseAuthorityHash",
      "validationRunId",
      "errorCode",
      "errorMessage",
      "actor",
      "reason",
      "createdAt",
      "updatedAt",
    ])
  ) {
    return false;
  }
  const common =
    isDecisionIdentity(value.tourAttemptId) &&
    isCount(value.tourNumber) &&
    value.tourNumber > 1 &&
    isDecisionIdentity(value.baselineTourId) &&
    isOneOf(value.state, [
      "STARTED",
      "PREFLIGHT_RECEIVING",
      "PREFLIGHT_READY",
      "SOURCE_ACCEPTED",
      "READY_TO_VALIDATE",
      "VALIDATING",
      "COMPLETE",
      "INCOMPATIBLE",
      "FAILED",
    ]) &&
    isCount(value.generation) &&
    (value.preflightId === null || isDecisionIdentity(value.preflightId)) &&
    (value.sourceHash === null || isHash(value.sourceHash)) &&
    (value.manifestHash === null || isHash(value.manifestHash)) &&
    (value.jobId === null || isDecisionIdentity(value.jobId)) &&
    (value.profilingRunId === null ||
      isDecisionIdentity(value.profilingRunId)) &&
    (value.structureHash === null || isHash(value.structureHash)) &&
    (value.reuseAuthorityId === null ||
      isDecisionIdentity(value.reuseAuthorityId)) &&
    (value.reuseAuthorityHash === null || isHash(value.reuseAuthorityHash)) &&
    (value.validationRunId === null ||
      isDecisionIdentity(value.validationRunId)) &&
    isNullableText(value.errorCode) &&
    isNullableText(value.errorMessage) &&
    (value.errorCode === null) === (value.errorMessage === null) &&
    isBoundedCommandText(value.actor, 256, false, true) &&
    isBoundedCommandText(value.reason, 4096, false, true) &&
    isText(value.createdAt) &&
    isText(value.updatedAt);
  if (!common) return false;

  const noAcceptedAuthority =
    value.jobId === null &&
    value.profilingRunId === null &&
    value.structureHash === null &&
    value.reuseAuthorityId === null &&
    value.reuseAuthorityHash === null &&
    value.validationRunId === null;
  if (value.state === "STARTED") {
    return (
      value.preflightId === null &&
      value.sourceHash === null &&
      value.manifestHash === null &&
      noAcceptedAuthority &&
      value.errorCode === null
    );
  }
  if (value.state === "PREFLIGHT_RECEIVING") {
    return (
      value.preflightId !== null &&
      value.sourceHash === null &&
      value.manifestHash === null &&
      noAcceptedAuthority &&
      value.errorCode === null
    );
  }
  if (value.state === "PREFLIGHT_READY") {
    return (
      value.preflightId !== null &&
      value.sourceHash !== null &&
      value.manifestHash !== null &&
      noAcceptedAuthority &&
      value.errorCode === null
    );
  }
  const acceptedSource =
    value.preflightId !== null &&
    value.sourceHash !== null &&
    value.manifestHash !== null;
  const verifiedReuse =
    acceptedSource &&
    value.jobId !== null &&
    value.profilingRunId !== null &&
    value.structureHash !== null &&
    value.reuseAuthorityId !== null &&
    value.reuseAuthorityHash !== null;
  if (value.state === "READY_TO_VALIDATE") {
    return (
      verifiedReuse &&
      value.validationRunId === null &&
      value.errorCode === null
    );
  }
  if (value.state === "VALIDATING") {
    return (
      verifiedReuse &&
      value.validationRunId !== null &&
      value.errorCode === null
    );
  }
  if (value.state === "INCOMPATIBLE") {
    return (
      acceptedSource &&
      value.jobId !== null &&
      value.profilingRunId !== null &&
      value.structureHash !== null &&
      value.reuseAuthorityId === null &&
      value.reuseAuthorityHash === null &&
      value.validationRunId === null &&
      value.errorCode === "MAPPING_REQUIRED"
    );
  }
  if (value.state === "FAILED") {
    return value.errorCode !== null;
  }
  return acceptedSource;
}

function isMappingReuseAuthorityShape(
  value: unknown,
): value is MappingReuseAuthority {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "reuseAuthorityId",
      "reuseAuthorityHash",
      "baselineTourId",
      "candidateTourAttemptId",
      "baselineSourceHash",
      "mappingOriginSourceHash",
      "candidateSourceHash",
      "profileId",
      "profileVersion",
      "profileHash",
      "profileApprovalId",
      "profileApprovalHash",
      "mappingId",
      "mappingVersion",
      "mappingHash",
      "mappingAuthorityId",
      "mappingAuthorityHash",
      "mappingApprovalId",
      "mappingApprovalHash",
      "structureHash",
      "createdAt",
    ]) &&
    [
      "reuseAuthorityId",
      "baselineTourId",
      "candidateTourAttemptId",
      "profileId",
      "profileVersion",
      "profileApprovalId",
      "mappingId",
      "mappingVersion",
      "mappingAuthorityId",
    ].every((key) => isDecisionIdentity(value[key])) &&
    [
      "reuseAuthorityHash",
      "baselineSourceHash",
      "mappingOriginSourceHash",
      "candidateSourceHash",
      "profileHash",
      "profileApprovalHash",
      "mappingHash",
      "mappingAuthorityHash",
      "structureHash",
    ].every((key) => isHash(value[key])) &&
    (value.mappingApprovalId === null ||
      isDecisionIdentity(value.mappingApprovalId)) &&
    (value.mappingApprovalHash === null || isHash(value.mappingApprovalHash)) &&
    (value.mappingApprovalId === null) ===
      (value.mappingApprovalHash === null) &&
    isText(value.createdAt)
  );
}

function isComparedFindingShape(value: unknown): value is ComparedFinding {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "findingId",
      "category",
      "fingerprintHash",
      "occurrence",
      "severity",
      "ruleId",
      "issueCode",
      "displayMessage",
      "entityId",
      "fieldId",
      "relationshipId",
      "relationshipRole",
      "baselineIssueIds",
      "baselineIssueHashes",
      "candidateIssueIds",
      "candidateIssueHashes",
      "identityBasis",
      "identityBasisExplanation",
    ]) &&
    isDecisionIdentity(value.findingId) &&
    isOneOf(value.category, ["CLOSED", "REMAINING", "NEW"]) &&
    isHash(value.fingerprintHash) &&
    isCount(value.occurrence) &&
    isBoundedCommandText(value.severity, 32) &&
    isDecisionIdentity(value.ruleId) &&
    isDecisionIdentity(value.issueCode) &&
    isBoundedCommandText(value.displayMessage, 4096) &&
    isDecisionIdentity(value.entityId) &&
    isBoundedCommandText(value.fieldId, 256) &&
    (value.relationshipId === null ||
      isDecisionIdentity(value.relationshipId)) &&
    (value.relationshipRole === null ||
      isBoundedCommandText(value.relationshipRole, 64)) &&
    isBoundedArray(value.baselineIssueIds, 200, isDecisionIdentity) &&
    isBoundedArray(value.baselineIssueHashes, 200, isHash) &&
    isBoundedArray(value.candidateIssueIds, 200, isDecisionIdentity) &&
    isBoundedArray(value.candidateIssueHashes, 200, isHash) &&
    isOneOf(value.identityBasis, [
      "DECLARED_KEY",
      "FULL_ROW_FALLBACK",
      "UNIQUE_SCOPE",
    ]) &&
    isText(value.identityBasisExplanation) &&
    value.identityBasisExplanation ===
      COMPARED_FINDING_IDENTITY_EXPLANATIONS[value.identityBasis] &&
    value.baselineIssueIds.length === value.baselineIssueHashes.length &&
    value.candidateIssueIds.length === value.candidateIssueHashes.length
  );
}

function isComparedValidationShape(
  value: unknown,
): value is ComparedValidation {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "schemaVersion",
      "comparisonId",
      "baselineTourId",
      "candidateTourId",
      "baselineValidationRunId",
      "candidateValidationRunId",
      "baselineResultHash",
      "candidateResultHash",
      "algorithmVersion",
      "closedCount",
      "remainingCount",
      "newCount",
      "inputHash",
      "comparisonHash",
      "actor",
      "reason",
      "createdAt",
      "findings",
    ]) &&
    value.schemaVersion === SCHEMAS.comparedValidation &&
    [
      "comparisonId",
      "baselineTourId",
      "candidateTourId",
      "baselineValidationRunId",
      "candidateValidationRunId",
    ].every((key) => isDecisionIdentity(value[key])) &&
    value.baselineTourId !== value.candidateTourId &&
    value.baselineValidationRunId !== value.candidateValidationRunId &&
    isHash(value.baselineResultHash) &&
    isHash(value.candidateResultHash) &&
    isBoundedCommandText(value.algorithmVersion, 256) &&
    isCount(value.closedCount) &&
    isCount(value.remainingCount) &&
    isCount(value.newCount) &&
    isHash(value.inputHash) &&
    isHash(value.comparisonHash) &&
    isBoundedCommandText(value.actor, 256, false, true) &&
    isBoundedCommandText(value.reason, 4096, false, true) &&
    isText(value.createdAt) &&
    isBoundedArray(value.findings, 200, isComparedFindingShape) &&
    value.findings.filter((finding) => finding.category === "CLOSED").length ===
      value.closedCount &&
    value.findings.filter((finding) => finding.category === "REMAINING")
      .length === value.remainingCount &&
    value.findings.filter((finding) => finding.category === "NEW").length ===
      value.newCount
  );
}

function isTourChainShape(value: Record<string, unknown>): boolean {
  const activeTour = value.activeTour;
  const pending = value.pendingAttempt;
  const comparison = value.latestComparison;
  const shape =
    hasExactKeys(value, [
      "schemaVersion",
      "engagementGeneration",
      "activeTour",
      "pendingAttempt",
      "latestComparison",
      "tours",
      "comparisons",
    ]) &&
    value.schemaVersion === SCHEMAS.tourChain &&
    isCount(value.engagementGeneration) &&
    (activeTour === null || isTourSummaryShape(activeTour)) &&
    (pending === null || isRevisedTourAttemptShape(pending)) &&
    (comparison === null || isComparedValidationShape(comparison)) &&
    isBoundedArray(value.tours, 100, isTourSummaryShape) &&
    isBoundedArray(value.comparisons, 99, isComparedValidationShape) &&
    (comparison === null ||
      (activeTour !== null &&
        comparison.candidateTourId === activeTour.tourId &&
        comparison.candidateValidationRunId === activeTour.validationRunId)) &&
    (pending === null ||
      (activeTour !== null && pending.baselineTourId === activeTour.tourId));
  if (!shape) return false;
  const tours = value.tours as TourSummary[];
  const comparisons = value.comparisons as ComparedValidation[];
  if (
    tours.some(
      (tour, index) =>
        tour.tourNumber !== index + 1 ||
        (index === 0
          ? tour.predecessorTourId !== null
          : tour.predecessorTourId !== tours[index - 1].tourId),
    ) ||
    new Set(tours.map((tour) => tour.tourId)).size !== tours.length ||
    new Set(comparisons.map((item) => item.comparisonId)).size !==
      comparisons.length
  ) {
    return false;
  }
  if (activeTour === null) {
    if (tours.length !== 0 || comparisons.length !== 0) return false;
  } else if (tours.at(-1)?.tourId !== activeTour.tourId) {
    return false;
  }
  const tourNumberById = new Map(
    tours.map((tour) => [tour.tourId, tour.tourNumber]),
  );
  if (
    comparisons.some((item, index) => {
      const baselineNumber = tourNumberById.get(item.baselineTourId);
      const candidateNumber = tourNumberById.get(item.candidateTourId);
      return (
        baselineNumber === undefined ||
        candidateNumber !== baselineNumber + 1 ||
        candidateNumber !== index + 2
      );
    })
  ) {
    return false;
  }
  return comparison === null
    ? comparisons.length === 0
    : comparisons.at(-1)?.comparisonId === comparison.comparisonId;
}

function isRemediationKeyColumnShape(
  value: unknown,
): value is RemediationKeyColumn {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "entityId",
      "sourceFile",
      "sourceColumnOrdinal",
      "sourceColumn",
    ]) &&
    isDecisionIdentity(value.entityId) &&
    isSafeSourceFile(value.sourceFile) &&
    isCount(value.sourceColumnOrdinal) &&
    isBoundedCommandText(value.sourceColumn, 256, false, true)
  );
}

function isRemediationOptionColumnShape(
  value: unknown,
): value is RemediationPackOptions["files"][number]["columns"][number] {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "sourceColumnOrdinal",
      "sourceColumn",
      "suggested",
      "selected",
    ]) &&
    isCount(value.sourceColumnOrdinal) &&
    isBoundedCommandText(value.sourceColumn, 256, false, true) &&
    typeof value.suggested === "boolean" &&
    value.selected === false
  );
}

function isRemediationOptionFileShape(
  value: unknown,
): value is RemediationPackOptions["files"][number] {
  if (
    !isRecord(value) ||
    !hasExactKeys(value, ["entityId", "sourceFile", "columns"]) ||
    !isDecisionIdentity(value.entityId) ||
    !isSafeSourceFile(value.sourceFile) ||
    !isBoundedArray(value.columns, 4096, isRemediationOptionColumnShape) ||
    value.columns.length === 0
  ) {
    return false;
  }
  const columns = value.columns;
  return (
    new Set(
      columns.map(
        (column) => `${column.sourceColumnOrdinal}:${column.sourceColumn}`,
      ),
    ).size === columns.length &&
    columns.every(
      (column, index) =>
        index === 0 ||
        columns[index - 1].sourceColumnOrdinal < column.sourceColumnOrdinal,
    )
  );
}

function isRemediationPackOptionsShape(
  value: Record<string, unknown>,
): boolean {
  if (
    !hasExactKeys(value, [
      "schemaVersion",
      "engagementId",
      "engagementGeneration",
      "workspaceId",
      "workspaceDescriptorHash",
      "jobId",
      "sourceHash",
      "manifestHash",
      "profilingRunId",
      "profilingResultHash",
      "profileId",
      "profileVersion",
      "profileHash",
      "profileApprovalId",
      "profileApprovalHash",
      "mappingId",
      "mappingVersion",
      "mappingHash",
      "mappingAuthorityId",
      "mappingAuthorityHash",
      "validationRunId",
      "validationResultHash",
      "issueCount",
      "optionsHash",
      "files",
    ]) ||
    value.schemaVersion !== SCHEMAS.remediationPackOptions ||
    !isCount(value.engagementGeneration) ||
    !isCount(value.issueCount) ||
    value.issueCount === 0 ||
    !isBoundedArray(value.files, 64, isRemediationOptionFileShape) ||
    value.files.length === 0
  ) {
    return false;
  }
  const identities = [
    "engagementId",
    "workspaceId",
    "jobId",
    "profilingRunId",
    "profileId",
    "profileVersion",
    "profileApprovalId",
    "mappingId",
    "mappingVersion",
    "mappingAuthorityId",
    "validationRunId",
  ];
  const hashes = [
    "workspaceDescriptorHash",
    "sourceHash",
    "manifestHash",
    "profilingResultHash",
    "profileHash",
    "profileApprovalHash",
    "mappingHash",
    "mappingAuthorityHash",
    "validationResultHash",
    "optionsHash",
  ];
  const files = value.files;
  return (
    identities.every((key) => isDecisionIdentity(value[key])) &&
    hashes.every((key) => isHash(value[key])) &&
    new Set(files.map((file) => `${file.entityId}:${file.sourceFile}`)).size ===
      files.length
  );
}

function isRemediationPackViewShape(
  value: unknown,
): value is RemediationPackView {
  if (
    !isRecord(value) ||
    !hasExactKeys(value, [
      "packId",
      "state",
      "engagementId",
      "validationRunId",
      "sourceHash",
      "selectedKeyColumns",
      "includeOffendingValues",
      "actor",
      "reason",
      "issueCount",
      "memberCount",
      "archiveSizeBytes",
      "archiveSha256",
      "membershipHash",
      "authorityHash",
      "downloadToken",
      "createdAt",
      "completedAt",
    ]) ||
    value.state !== "COMPLETE" ||
    !isBoundedArray(value.selectedKeyColumns, 4096, isRemediationKeyColumnShape)
  ) {
    return false;
  }
  const columns = value.selectedKeyColumns;
  return (
    ["packId", "engagementId", "validationRunId"].every((key) =>
      isDecisionIdentity(value[key]),
    ) &&
    [
      "sourceHash",
      "archiveSha256",
      "membershipHash",
      "authorityHash",
      "downloadToken",
    ].every((key) => isHash(value[key])) &&
    value.downloadToken === value.archiveSha256 &&
    columns.length > 0 &&
    new Set(
      columns.map(
        (column) =>
          `${column.entityId}:${column.sourceFile}:${column.sourceColumnOrdinal}:${column.sourceColumn}`,
      ),
    ).size === columns.length &&
    typeof value.includeOffendingValues === "boolean" &&
    isBoundedCommandText(value.actor, 256, false, true) &&
    isBoundedCommandText(value.reason, 4096, false, true) &&
    isCount(value.issueCount) &&
    isCount(value.memberCount) &&
    value.memberCount > 0 &&
    isCount(value.archiveSizeBytes) &&
    value.archiveSizeBytes > 0 &&
    isText(value.createdAt) &&
    isText(value.completedAt)
  );
}

function isRemediationPackHistoryShape(
  value: Record<string, unknown>,
): boolean {
  return (
    hasExactKeys(value, [
      "schemaVersion",
      "totalCount",
      "returnedCount",
      "truncated",
      "items",
    ]) &&
    value.schemaVersion === SCHEMAS.remediationPackHistory &&
    isCount(value.totalCount) &&
    isCount(value.returnedCount) &&
    typeof value.truncated === "boolean" &&
    isBoundedArray(value.items, 200, isRemediationPackViewShape) &&
    value.returnedCount === value.items.length &&
    value.totalCount >= value.returnedCount &&
    value.truncated === value.returnedCount < value.totalCount &&
    new Set(value.items.map((item) => item.packId)).size === value.items.length
  );
}

function isRemediationPackCommandResultShape(
  value: Record<string, unknown>,
): boolean {
  return (
    hasExactKeys(value, ["schemaVersion", "pack", "reusedAttempt"]) &&
    value.schemaVersion === SCHEMAS.remediationPack &&
    isRemediationPackViewShape(value.pack) &&
    typeof value.reusedAttempt === "boolean"
  );
}

function isAcceptRevisedSourceResultShape(
  value: unknown,
): value is AcceptRevisedSourceResult {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "schemaVersion",
      "engagementGeneration",
      "tourAttempt",
      "reuseAuthority",
      "profilingResultHash",
      "reusedProfile",
      "idempotentReplay",
    ]) &&
    value.schemaVersion === ENGAGEMENT_COMMAND_SCHEMAS.acceptRevisedSource &&
    isCount(value.engagementGeneration) &&
    isRevisedTourAttemptShape(value.tourAttempt) &&
    isMappingReuseAuthorityShape(value.reuseAuthority) &&
    value.tourAttempt.tourAttemptId ===
      value.reuseAuthority.candidateTourAttemptId &&
    value.tourAttempt.reuseAuthorityId ===
      value.reuseAuthority.reuseAuthorityId &&
    value.tourAttempt.reuseAuthorityHash ===
      value.reuseAuthority.reuseAuthorityHash &&
    isHash(value.profilingResultHash) &&
    typeof value.reusedProfile === "boolean" &&
    typeof value.idempotentReplay === "boolean"
  );
}

function isLaunchRevisedValidationResultShape(
  value: unknown,
): value is LaunchRevisedValidationResult {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "schemaVersion",
      "engagementGeneration",
      "activeTour",
      "comparison",
      "idempotentReplay",
    ]) &&
    value.schemaVersion ===
      ENGAGEMENT_COMMAND_SCHEMAS.launchRevisedValidation &&
    isCount(value.engagementGeneration) &&
    isTourSummaryShape(value.activeTour) &&
    isComparedValidationShape(value.comparison) &&
    value.comparison.candidateTourId === value.activeTour.tourId &&
    value.comparison.candidateValidationRunId ===
      value.activeTour.validationRunId &&
    typeof value.idempotentReplay === "boolean"
  );
}

function isReferenceTableShape(value: unknown): value is ReferenceTableCommand {
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, ["entityId", "tableId", "version", "values"]) ||
    !["tableId", "version", "values"].every((key) =>
      Object.hasOwn(value, key),
    ) ||
    (value.entityId !== undefined && !isDecisionIdentity(value.entityId)) ||
    !isDecisionIdentity(value.tableId) ||
    !isBoundedCommandText(value.version, 128) ||
    !isRecord(value.values)
  ) {
    return false;
  }
  const entries = Object.entries(value.values);
  return (
    entries.length <= 1_000 &&
    entries.every(
      ([source, target]) =>
        source.length > 0 &&
        source.length <= 1024 &&
        !hasControlCharacters(source) &&
        isBoundedCommandText(target, 4096, false, true),
    )
  );
}

function isObjectPackShape(value: unknown): value is ObjectPack {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "objectPackId",
      "version",
      "title",
      "description",
      "scope",
      "rootEntityId",
      "mappingMode",
      "limitations",
      "mappingSuggestions",
      "referenceTables",
    ]) &&
    isDecisionIdentity(value.objectPackId) &&
    isBoundedCommandText(value.version, 128) &&
    isBoundedCommandText(value.title, 256) &&
    isBoundedCommandText(value.description, 2048) &&
    isBoundedCommandText(value.scope, 2048) &&
    isDecisionIdentity(value.rootEntityId) &&
    isOneOf(value.mappingMode, [
      "CONTROLLED_SUGGESTIONS",
      "OPERATOR_GOVERNED",
    ]) &&
    isBoundedArray(value.limitations, 100, (item): item is string =>
      isBoundedCommandText(item, 2048),
    ) &&
    isBoundedArray(
      value.mappingSuggestions,
      8192,
      (suggestion): suggestion is ObjectPack["mappingSuggestions"][number] =>
        Boolean(
          isRecord(suggestion) &&
          hasOnlyKeys(suggestion, [
            "suggestionId",
            "targetEntityId",
            "targetFieldId",
            "sourceFile",
            "sourceColumn",
            "sourceColumnOrdinal",
            "kind",
            "constant",
            "lookupTableId",
          ]) &&
          [
            "suggestionId",
            "targetFieldId",
            "sourceFile",
            "sourceColumn",
            "sourceColumnOrdinal",
            "kind",
            "constant",
            "lookupTableId",
          ].every((key) => Object.hasOwn(suggestion, key)) &&
          isDecisionIdentity(suggestion.suggestionId) &&
          (suggestion.targetEntityId === undefined ||
            isDecisionIdentity(suggestion.targetEntityId)) &&
          isDecisionIdentity(suggestion.targetFieldId) &&
          isNullableText(suggestion.sourceFile) &&
          isNullableText(suggestion.sourceColumn) &&
          isNullableCount(suggestion.sourceColumnOrdinal) &&
          isOneOf(suggestion.kind, MAPPING_OPERATIONS) &&
          isNullableText(suggestion.constant) &&
          isNullableText(suggestion.lookupTableId),
        ),
    ) &&
    isBoundedArray(value.referenceTables, 256, isReferenceTableShape)
  );
}

function isObjectPackListShape(value: Record<string, unknown>): boolean {
  return (
    hasExactKeys(value, ["schemaVersion", "items"]) &&
    value.schemaVersion === SCHEMAS.objectPacks &&
    isBoundedArray(value.items, 20, isObjectPackShape)
  );
}

function isJsonValue(value: unknown, depth = 0): value is JsonValue {
  if (depth > 20) return false;
  if (
    value === null ||
    typeof value === "boolean" ||
    (typeof value === "number" && Number.isFinite(value)) ||
    (typeof value === "string" && value.length <= 65_536)
  ) {
    return true;
  }
  if (Array.isArray(value)) {
    return (
      value.length <= 4096 &&
      value.every((item) => isJsonValue(item, depth + 1))
    );
  }
  if (isRecord(value)) {
    const entries = Object.entries(value);
    return (
      entries.length <= 4096 &&
      entries.every(
        ([key, item]) => key.length <= 1024 && isJsonValue(item, depth + 1),
      )
    );
  }
  return false;
}

function isSourceProfileRulesShape(value: Record<string, unknown>): boolean {
  return (
    hasExactKeys(value, [
      "schemaVersion",
      "jobId",
      "profilingRunId",
      "storedSampleValues",
      "sourceColumns",
      "profileFields",
      "rules",
    ]) &&
    value.schemaVersion === SCHEMAS.sourceProfileRules &&
    isDecisionIdentity(value.jobId) &&
    isDecisionIdentity(value.profilingRunId) &&
    value.storedSampleValues === false &&
    isBoundedArray(
      value.sourceColumns,
      8192,
      (item): item is SourceProfileRules["sourceColumns"][number] =>
        Boolean(
          isRecord(item) &&
          hasExactKeys(item, [
            "sourceFile",
            "sourceColumn",
            "sourceColumnOrdinal",
          ]) &&
          isSafeSourceFile(item.sourceFile) &&
          isBoundedCommandText(item.sourceColumn, 1024) &&
          isCount(item.sourceColumnOrdinal) &&
          item.sourceColumnOrdinal <= 8192,
        ),
    ) &&
    isBoundedArray(
      value.profileFields,
      8192,
      (field): field is SourceProfileRules["profileFields"][number] =>
        Boolean(
          isRecord(field) &&
          hasExactKeys(field, [
            "entityId",
            "fieldId",
            "label",
            "logicalType",
            "required",
            "sensitive",
          ]) &&
          isDecisionIdentity(field.entityId) &&
          isDecisionIdentity(field.fieldId) &&
          isBoundedCommandText(field.label, 1024) &&
          isOneOf(field.logicalType, [
            "text",
            "code",
            "integer",
            "decimal",
            "boolean",
            "date",
            "datetime",
            "identifier",
            "country_code",
            "currency",
            "email",
            "phone",
          ]) &&
          typeof field.required === "boolean" &&
          typeof field.sensitive === "boolean",
        ),
    ) &&
    isBoundedArray(
      value.rules,
      8192,
      (rule): rule is SourceProfileRules["rules"][number] =>
        Boolean(
          isRecord(rule) &&
          hasExactKeys(rule, [
            "ruleId",
            "severity",
            "kind",
            "target",
            "message",
            "parameters",
          ]) &&
          isDecisionIdentity(rule.ruleId) &&
          isOneOf(rule.severity, ["information", "warning", "error"]) &&
          isOneOf(rule.kind, [
            "required",
            "max_length",
            "allowed_values",
            "pattern",
            "unique",
            "conditional_required",
            "reference_lookup",
          ]) &&
          isBoundedCommandText(rule.target, 1024) &&
          isBoundedCommandText(rule.message, 4096) &&
          isJsonValue(rule.parameters),
        ),
    )
  );
}

function isSourceEntityAssignmentShape(
  value: unknown,
): value is SourceEntityAssignmentCommand {
  return Boolean(
    isRecord(value) &&
    hasExactKeys(value, ["sourceFile", "entityId"]) &&
    isSafeSourceFile(value.sourceFile) &&
    isDecisionIdentity(value.entityId),
  );
}

function isMappingBindingCommandShape(
  value: unknown,
): value is MappingBindingCommand {
  if (
    !isRecord(value) ||
    !hasOnlyKeys(value, [
      "bindingId",
      "targetEntityId",
      "targetFieldId",
      "sourceFile",
      "sourceColumn",
      "sourceColumnOrdinal",
      "kind",
      "constant",
      "lookupTableId",
    ]) ||
    !["bindingId", "targetEntityId", "targetFieldId", "kind"].every((key) =>
      Object.hasOwn(value, key),
    ) ||
    !isDecisionIdentity(value.bindingId) ||
    !isDecisionIdentity(value.targetEntityId) ||
    !isDecisionIdentity(value.targetFieldId) ||
    !isOneOf(value.kind, MAPPING_OPERATIONS)
  ) {
    return false;
  }
  const sourceMembers = [
    value.sourceFile,
    value.sourceColumn,
    value.sourceColumnOrdinal,
  ];
  const sourceAbsent = sourceMembers.every((member) => member === undefined);
  const sourcePresent =
    isSafeSourceFile(value.sourceFile) &&
    isBoundedCommandText(value.sourceColumn, 1024) &&
    isCount(value.sourceColumnOrdinal);
  return (
    (sourceAbsent || sourcePresent) &&
    (value.constant === undefined ||
      isBoundedCommandText(value.constant, 4096, true)) &&
    (value.lookupTableId === undefined ||
      isDecisionIdentity(value.lookupTableId))
  );
}

function isRelationshipFieldPairShape(
  value: unknown,
): value is RelationshipFieldPairCommand {
  return Boolean(
    isRecord(value) &&
    hasExactKeys(value, ["fromFieldId", "toFieldId"]) &&
    isDecisionIdentity(value.fromFieldId) &&
    isDecisionIdentity(value.toFieldId),
  );
}

function isMappingRelationshipBindingShape(
  value: unknown,
): value is MappingRelationshipBindingCommand {
  return Boolean(
    isRecord(value) &&
    hasExactKeys(value, [
      "relationshipId",
      "fromEntityId",
      "toEntityId",
      "cardinality",
      "keyPairs",
    ]) &&
    isDecisionIdentity(value.relationshipId) &&
    isDecisionIdentity(value.fromEntityId) &&
    isDecisionIdentity(value.toEntityId) &&
    isOneOf(value.cardinality, ["0..*", "1..*"]) &&
    isBoundedArray(value.keyPairs, 32, isRelationshipFieldPairShape),
  );
}

function isMappingDraftSnapshotShape(
  value: unknown,
): value is MappingDraftSnapshot {
  return Boolean(
    isRecord(value) &&
    hasOnlyKeys(value, [
      "sourceEntityAssignments",
      "bindings",
      "relationshipBindings",
      "referenceTables",
      "author",
    ]) &&
    [
      "sourceEntityAssignments",
      "bindings",
      "relationshipBindings",
      "referenceTables",
    ].every((key) => Object.hasOwn(value, key)) &&
    isBoundedArray(
      value.sourceEntityAssignments,
      32,
      isSourceEntityAssignmentShape,
    ) &&
    isBoundedArray(value.bindings, 512, isMappingBindingCommandShape) &&
    isBoundedArray(
      value.relationshipBindings,
      256,
      isMappingRelationshipBindingShape,
    ) &&
    isBoundedArray(value.referenceTables, 32, isReferenceTableShape) &&
    (value.author === undefined ||
      isBoundedCommandText(value.author, 256, false, true)),
  );
}

function isMappingDraftViewShape(value: unknown): value is MappingDraftView {
  return Boolean(
    isRecord(value) &&
    hasExactKeys(value, [
      "schemaVersion",
      "draftId",
      "draftGeneration",
      "draftHash",
      "state",
      "snapshot",
      "submittedMappingId",
      "submittedMappingVersion",
      "submittedMappingHash",
      "updatedAt",
    ]) &&
    value.schemaVersion === SCHEMAS.mappingDraft &&
    isDecisionIdentity(value.draftId) &&
    isCount(value.draftGeneration) &&
    value.draftGeneration > 0 &&
    isHash(value.draftHash) &&
    isOneOf(value.state, ["EDITABLE", "SUBMITTED", "APPROVED"]) &&
    isMappingDraftSnapshotShape(value.snapshot) &&
    isNullableText(value.submittedMappingId) &&
    isNullableText(value.submittedMappingVersion) &&
    (value.submittedMappingHash === null ||
      isHash(value.submittedMappingHash)) &&
    isText(value.updatedAt),
  );
}

function isMappingWorkspaceShape(value: Record<string, unknown>): boolean {
  return (
    hasExactKeys(value, [
      "schemaVersion",
      "engagementGeneration",
      "jobId",
      "sourceHash",
      "profilingRunId",
      "destinationOrigin",
      "objectPackId",
      "objectPackVersion",
      "profileId",
      "profileVersion",
      "profileHash",
      "mappingMode",
      "sourceFiles",
      "entities",
      "relationships",
      "allowedOperations",
      "controlledReferenceTables",
      "referenceTableObligations",
      "limits",
      "revisionDraftScope",
      "draft",
    ]) &&
    value.schemaVersion === SCHEMAS.mappingWorkspace &&
    isCount(value.engagementGeneration) &&
    isDecisionIdentity(value.jobId) &&
    isHash(value.sourceHash) &&
    isDecisionIdentity(value.profilingRunId) &&
    isOneOf(value.destinationOrigin, [
      "CONTROLLED_PACK",
      "WORKSPACE_PROFILE",
    ]) &&
    isDecisionIdentity(value.objectPackId) &&
    isDecisionIdentity(value.objectPackVersion) &&
    isDecisionIdentity(value.profileId) &&
    isDecisionIdentity(value.profileVersion) &&
    isHash(value.profileHash) &&
    value.mappingMode === "OPERATOR_GOVERNED" &&
    isBoundedArray(value.sourceFiles, 32, (file): file is unknown =>
      Boolean(
        isRecord(file) &&
        hasExactKeys(file, ["sourceFile", "columns"]) &&
        isSafeSourceFile(file.sourceFile) &&
        isBoundedArray(file.columns, 8192, (column): column is unknown =>
          Boolean(
            isRecord(column) &&
            hasExactKeys(column, [
              "sourceFile",
              "sourceColumn",
              "sourceColumnOrdinal",
            ]) &&
            column.sourceFile === file.sourceFile &&
            isSafeSourceFile(column.sourceFile) &&
            isBoundedCommandText(column.sourceColumn, 1024) &&
            isCount(column.sourceColumnOrdinal),
          ),
        ),
      ),
    ) &&
    isBoundedArray(value.entities, 32, (entity): entity is unknown =>
      Boolean(
        isRecord(entity) &&
        hasExactKeys(entity, ["entityId", "root", "key", "fields"]) &&
        isDecisionIdentity(entity.entityId) &&
        typeof entity.root === "boolean" &&
        isBoundedArray(entity.key, 64, isDecisionIdentity) &&
        isBoundedArray(entity.fields, 8192, (field): field is unknown =>
          Boolean(
            isRecord(field) &&
            hasExactKeys(field, [
              "entityId",
              "fieldId",
              "label",
              "logicalType",
              "required",
              "sensitive",
            ]) &&
            field.entityId === entity.entityId &&
            isDecisionIdentity(field.fieldId) &&
            isBoundedCommandText(field.label, 1024) &&
            isBoundedCommandText(field.logicalType, 64) &&
            typeof field.required === "boolean" &&
            typeof field.sensitive === "boolean",
          ),
        ),
      ),
    ) &&
    isBoundedArray(
      value.relationships,
      64,
      isMappingRelationshipBindingShape,
    ) &&
    isBoundedArray(
      value.allowedOperations,
      MAPPING_OPERATIONS.length,
      (operation): operation is MappingOperation =>
        isOneOf(operation, MAPPING_OPERATIONS),
    ) &&
    isBoundedArray(
      value.controlledReferenceTables,
      32,
      isReferenceTableShape,
    ) &&
    isBoundedArray(
      value.referenceTableObligations,
      32,
      (obligation): obligation is unknown =>
        Boolean(
          isRecord(obligation) &&
          hasExactKeys(obligation, [
            "entityId",
            "tableId",
            "tableVersion",
            "ruleIds",
            "targetFields",
          ]) &&
          isDecisionIdentity(obligation.entityId) &&
          isDecisionIdentity(obligation.tableId) &&
          isDecisionIdentity(obligation.tableVersion) &&
          isBoundedArray(obligation.ruleIds, 4096, isDecisionIdentity) &&
          isBoundedArray(obligation.targetFields, 4096, isDecisionIdentity),
        ),
    ) &&
    isRecord(value.limits) &&
    hasExactKeys(value.limits, [
      "maximumSourceFiles",
      "maximumBindings",
      "maximumRelationships",
      "maximumReferenceTables",
      "maximumReferenceEntries",
      "maximumReferenceBytes",
      "maximumReferenceKeyBytes",
      "maximumReferenceValueBytes",
    ]) &&
    Object.values(value.limits).every(isCount) &&
    (value.revisionDraftScope === null ||
      Boolean(
        isRecord(value.revisionDraftScope) &&
        hasExactKeys(value.revisionDraftScope, [
          "tourAttemptId",
          "tourNumber",
          "baselineTourId",
          "profilingRunId",
          "submissionAvailable",
          "unavailableReason",
        ]) &&
        isDecisionIdentity(value.revisionDraftScope.tourAttemptId) &&
        isCount(value.revisionDraftScope.tourNumber) &&
        value.revisionDraftScope.tourNumber >= 2 &&
        isDecisionIdentity(value.revisionDraftScope.baselineTourId) &&
        isDecisionIdentity(value.revisionDraftScope.profilingRunId) &&
        value.revisionDraftScope.profilingRunId === value.profilingRunId &&
        value.revisionDraftScope.submissionAvailable === false &&
        isBoundedCommandText(
          value.revisionDraftScope.unavailableReason,
          1024,
          false,
          true,
        ),
      )) &&
    (value.draft === null || isMappingDraftViewShape(value.draft))
  );
}

function isMappingContractCoverageStatus(
  value: unknown,
): value is MappingContractCoverageStatus {
  return isOneOf(value, ["COMPLETE", "UNRESOLVED", "WARNING", "BLOCKER"]);
}

function isMappingContractCoverageShape(
  value: Record<string, unknown>,
): boolean {
  if (
    !hasExactKeys(value, [
      "schemaVersion",
      "authority",
      "overallStatus",
      "summary",
      "fields",
      "sourceColumns",
      "rules",
      "tables",
      "relationships",
      "coverageHash",
    ]) ||
    value.schemaVersion !== SCHEMAS.mappingContractCoverage ||
    !isMappingContractCoverageStatus(value.overallStatus) ||
    !isHash(value.coverageHash) ||
    !isRecord(value.authority) ||
    !hasExactKeys(value.authority, [
      "workspaceId",
      "workspaceDescriptorHash",
      "engagementId",
      "engagementGeneration",
      "jobId",
      "sourceHash",
      "profilingRunId",
      "destinationOrigin",
      "profileId",
      "profileVersion",
      "profileHash",
      "profileApprovalId",
      "profileApprovalHash",
      "draftId",
      "draftGeneration",
      "draftHash",
      "draftState",
      "mappingId",
      "mappingVersion",
      "mappingHash",
      "mappingApprovalId",
      "mappingApprovalHash",
    ]) ||
    !isDecisionIdentity(value.authority.workspaceId) ||
    !isHash(value.authority.workspaceDescriptorHash) ||
    !isDecisionIdentity(value.authority.engagementId) ||
    !isCount(value.authority.engagementGeneration) ||
    !isDecisionIdentity(value.authority.jobId) ||
    !isHash(value.authority.sourceHash) ||
    !isDecisionIdentity(value.authority.profilingRunId) ||
    !isOneOf(value.authority.destinationOrigin, [
      "CONTROLLED_PACK",
      "WORKSPACE_PROFILE",
    ]) ||
    !isDecisionIdentity(value.authority.profileId) ||
    !isDecisionIdentity(value.authority.profileVersion) ||
    !isHash(value.authority.profileHash) ||
    !isDecisionIdentity(value.authority.profileApprovalId) ||
    !isHash(value.authority.profileApprovalHash) ||
    !isNullableText(value.authority.draftId) ||
    !isNullableCount(value.authority.draftGeneration) ||
    !(
      value.authority.draftHash === null || isHash(value.authority.draftHash)
    ) ||
    !(
      value.authority.draftState === null ||
      isOneOf(value.authority.draftState, ["EDITABLE", "SUBMITTED", "APPROVED"])
    ) ||
    !isNullableText(value.authority.mappingId) ||
    !isNullableText(value.authority.mappingVersion) ||
    !(
      value.authority.mappingHash === null ||
      isHash(value.authority.mappingHash)
    ) ||
    !isNullableText(value.authority.mappingApprovalId) ||
    !(
      value.authority.mappingApprovalHash === null ||
      isHash(value.authority.mappingApprovalHash)
    ) ||
    !isRecord(value.summary) ||
    !hasExactKeys(value.summary, [
      "complete",
      "unresolved",
      "warnings",
      "blockers",
    ]) ||
    !Object.values(value.summary).every(isCount)
  ) {
    return false;
  }
  const statusAndReasons = (item: Record<string, unknown>): boolean =>
    isMappingContractCoverageStatus(item.status) &&
    isBoundedArray(item.reasons, 32, (reason) =>
      isBoundedCommandText(reason, 4096, false, true),
    );
  return (
    isBoundedArray(value.fields, 8192, (field): field is unknown =>
      Boolean(
        isRecord(field) &&
        hasExactKeys(field, [
          "entityId",
          "fieldId",
          "bindingId",
          "operation",
          "sourceFile",
          "sourceColumn",
          "sourceColumnOrdinal",
          "fixedValueConfigured",
          "lookupTableId",
          "lookupTableVersion",
          "ruleIds",
          "ruleKinds",
          "status",
          "reasons",
        ]) &&
        isDecisionIdentity(field.entityId) &&
        isDecisionIdentity(field.fieldId) &&
        isNullableText(field.bindingId) &&
        (field.operation === null ||
          isOneOf(field.operation, MAPPING_OPERATIONS)) &&
        isNullableText(field.sourceFile) &&
        isNullableText(field.sourceColumn) &&
        isNullableCount(field.sourceColumnOrdinal) &&
        typeof field.fixedValueConfigured === "boolean" &&
        isNullableText(field.lookupTableId) &&
        isNullableText(field.lookupTableVersion) &&
        isBoundedArray(field.ruleIds, 4096, isDecisionIdentity) &&
        isBoundedArray(field.ruleKinds, 4096, (kind) =>
          isBoundedCommandText(kind, 64, false, true),
        ) &&
        statusAndReasons(field),
      ),
    ) &&
    isBoundedArray(value.sourceColumns, 131072, (column): column is unknown =>
      Boolean(
        isRecord(column) &&
        hasExactKeys(column, [
          "sourceFile",
          "sourceColumn",
          "sourceColumnOrdinal",
          "status",
          "reason",
        ]) &&
        isSafeSourceFile(column.sourceFile) &&
        isBoundedCommandText(column.sourceColumn, 1024) &&
        isCount(column.sourceColumnOrdinal) &&
        isMappingContractCoverageStatus(column.status) &&
        isBoundedCommandText(column.reason, 4096, false, true),
      ),
    ) &&
    isBoundedArray(value.rules, 4096, (rule): rule is unknown =>
      Boolean(
        isRecord(rule) &&
        hasExactKeys(rule, [
          "ruleId",
          "kind",
          "entityId",
          "fieldId",
          "referenceTableId",
          "referenceTableVersion",
          "status",
          "reasons",
        ]) &&
        isDecisionIdentity(rule.ruleId) &&
        isBoundedCommandText(rule.kind, 64, false, true) &&
        isDecisionIdentity(rule.entityId) &&
        isDecisionIdentity(rule.fieldId) &&
        isNullableText(rule.referenceTableId) &&
        isNullableText(rule.referenceTableVersion) &&
        statusAndReasons(rule),
      ),
    ) &&
    isBoundedArray(value.tables, 32, (table): table is unknown =>
      Boolean(
        isRecord(table) &&
        hasExactKeys(table, [
          "entityId",
          "tableId",
          "tableVersion",
          "entryCount",
          "roles",
          "status",
          "reasons",
        ]) &&
        isDecisionIdentity(table.entityId) &&
        isDecisionIdentity(table.tableId) &&
        isDecisionIdentity(table.tableVersion) &&
        isCount(table.entryCount) &&
        isBoundedArray(table.roles, 2, (role) =>
          isOneOf(role, ["TRANSFORM_LOOKUP", "REFERENCE_DOMAIN"]),
        ) &&
        statusAndReasons(table),
      ),
    ) &&
    isBoundedArray(
      value.relationships,
      256,
      (relationship): relationship is unknown =>
        Boolean(
          isRecord(relationship) &&
          hasExactKeys(relationship, [
            "relationshipId",
            "fromEntityId",
            "toEntityId",
            "cardinality",
            "keyPairs",
            "status",
            "reasons",
          ]) &&
          isDecisionIdentity(relationship.relationshipId) &&
          isDecisionIdentity(relationship.fromEntityId) &&
          isDecisionIdentity(relationship.toEntityId) &&
          isOneOf(relationship.cardinality, ["0..*", "1..*"]) &&
          isBoundedArray(
            relationship.keyPairs,
            32,
            isRelationshipFieldPairShape,
          ) &&
          statusAndReasons(relationship),
        ),
    )
  );
}

function isSavedMappingDraftShape(value: Record<string, unknown>): boolean {
  return (
    hasExactKeys(value, [
      "schemaVersion",
      "engagementGeneration",
      "draft",
      "idempotentReplay",
    ]) &&
    value.schemaVersion === SCHEMAS.mappingDraft &&
    isCount(value.engagementGeneration) &&
    isMappingDraftViewShape(value.draft) &&
    typeof value.idempotentReplay === "boolean"
  );
}

function isSubmittedMappingShape(value: Record<string, unknown>): boolean {
  return (
    hasExactKeys(value, [
      "schemaVersion",
      "engagementGeneration",
      "draft",
      "preparationId",
      "mappingHash",
      "coverage",
      "idempotentReplay",
    ]) &&
    value.schemaVersion === SCHEMAS.submittedMapping &&
    isCount(value.engagementGeneration) &&
    isMappingDraftViewShape(value.draft) &&
    isDecisionIdentity(value.preparationId) &&
    isHash(value.mappingHash) &&
    isJsonValue(value.coverage) &&
    typeof value.idempotentReplay === "boolean"
  );
}

function isMappingApprovalShape(value: Record<string, unknown>): boolean {
  return (
    hasExactKeys(value, [
      "schemaVersion",
      "engagementGeneration",
      "approvalId",
      "approvalHash",
      "mappingId",
      "mappingVersion",
      "mappingHash",
      "actor",
      "reason",
      "approvedAt",
      "idempotentReplay",
    ]) &&
    value.schemaVersion === SCHEMAS.mappingApproval &&
    isCount(value.engagementGeneration) &&
    isDecisionIdentity(value.approvalId) &&
    isHash(value.approvalHash) &&
    isDecisionIdentity(value.mappingId) &&
    isDecisionIdentity(value.mappingVersion) &&
    isHash(value.mappingHash) &&
    isBoundedCommandText(value.actor, 256) &&
    isBoundedCommandText(value.reason, 4096) &&
    isText(value.approvedAt) &&
    typeof value.idempotentReplay === "boolean"
  );
}

function isProfileVersionViewShape(value: unknown): boolean {
  if (
    !isRecord(value) ||
    !hasExactKeys(value, [
      "schema_version",
      "profile_id",
      "profile_version",
      "profile_hash",
      "schema_hash",
      "validation_hash",
      "declared_status",
      "lifecycle_status",
      "authority",
      "can_start_new_runs",
      "registered_at",
      "approval",
      "lifecycle_events",
    ])
  ) {
    return false;
  }
  const approval = value.approval;
  return (
    isCount(value.schema_version) &&
    isDecisionIdentity(value.profile_id) &&
    isBoundedCommandText(value.profile_version, 128) &&
    isHash(value.profile_hash) &&
    isHash(value.schema_hash) &&
    isHash(value.validation_hash) &&
    isOneOf(value.declared_status, [
      "GENERATED_DRAFT",
      "REVIEWED_DRAFT",
      "APPROVED",
      "DEPRECATED",
      "RETIRED",
    ]) &&
    isOneOf(value.lifecycle_status, [
      "GENERATED_DRAFT",
      "REVIEWED_DRAFT",
      "APPROVED",
      "DEPRECATED",
      "RETIRED",
    ]) &&
    isOneOf(value.authority, ["NONE", "LOCAL_MAPPING_VALIDATION"]) &&
    typeof value.can_start_new_runs === "boolean" &&
    isText(value.registered_at) &&
    (approval === null ||
      (isRecord(approval) &&
        hasExactKeys(approval, [
          "approval_id",
          "approver",
          "reason",
          "acknowledged",
          "approved_at",
          "approval_hash",
        ]) &&
        isDecisionIdentity(approval.approval_id) &&
        isBoundedCommandText(approval.approver, 256) &&
        isBoundedCommandText(approval.reason, 4096) &&
        typeof approval.acknowledged === "boolean" &&
        isText(approval.approved_at) &&
        isHash(approval.approval_hash))) &&
    isBoundedArray(value.lifecycle_events, 4096, (event): event is unknown =>
      Boolean(
        isRecord(event) &&
        hasExactKeys(event, [
          "sequence",
          "from_state",
          "to_state",
          "actor",
          "reason",
          "previous_event_hash",
          "event_hash",
          "occurred_at",
        ]) &&
        isCount(event.sequence) &&
        isNullableText(event.from_state) &&
        isOneOf(event.to_state, [
          "GENERATED_DRAFT",
          "REVIEWED_DRAFT",
          "APPROVED",
          "DEPRECATED",
          "RETIRED",
        ]) &&
        isBoundedCommandText(event.actor, 256) &&
        isBoundedCommandText(event.reason, 4096) &&
        (event.previous_event_hash === null ||
          isHash(event.previous_event_hash)) &&
        isHash(event.event_hash) &&
        isText(event.occurred_at),
      ),
    )
  );
}

function isReviewedUnapprovedDestinationProfile(
  value: unknown,
): value is DestinationProfileVersionView {
  return (
    isProfileVersionViewShape(value) &&
    isRecord(value) &&
    value.declared_status === "REVIEWED_DRAFT" &&
    value.lifecycle_status === "REVIEWED_DRAFT" &&
    value.authority === "NONE" &&
    value.can_start_new_runs === false &&
    value.approval === null
  );
}

function isApprovedDestinationProfile(
  value: unknown,
): value is DestinationProfileVersionView {
  return (
    isProfileVersionViewShape(value) &&
    isRecord(value) &&
    value.declared_status === "REVIEWED_DRAFT" &&
    value.lifecycle_status === "APPROVED" &&
    value.authority === "LOCAL_MAPPING_VALIDATION" &&
    value.can_start_new_runs === true &&
    isRecord(value.approval) &&
    value.approval.acknowledged === true
  );
}

export function decodeSourcePreflightReservation(
  value: unknown,
): SourcePreflightDocument {
  if (
    !isRecord(value) ||
    !hasExactKeys(value, ["preflight"]) ||
    !isSourcePreflight(value.preflight)
  ) {
    throw new ContractError(
      "INVALID_RESPONSE",
      "The source reservation response does not match its closed contract.",
    );
  }
  return value as unknown as SourcePreflightDocument;
}

export function decodeEngagementCommandResult(
  command: EngagementStateCommand,
  value: unknown,
): unknown {
  if (!isRecord(value)) {
    throw new ContractError(
      "INVALID_RESPONSE",
      "The local command response is not a JSON object.",
    );
  }
  switch (command.schemaVersion) {
    case ENGAGEMENT_COMMAND_SCHEMAS.create:
      return decodeContract(value, SCHEMAS.engagementRecord);
    case ENGAGEMENT_COMMAND_SCHEMAS.acceptSource:
      return decodeContract(value, SCHEMAS.sourceAcceptance);
    case ENGAGEMENT_COMMAND_SCHEMAS.selectObjectPack:
      if (
        hasExactKeys(value, ["objectPack", "profile"]) &&
        isObjectPackShape(value.objectPack) &&
        isProfileVersionViewShape(value.profile)
      ) {
        return value;
      }
      break;
    case ENGAGEMENT_COMMAND_SCHEMAS.approveObjectPackProfile:
      if (isProfileVersionViewShape(value)) return value;
      break;
    case ENGAGEMENT_COMMAND_SCHEMAS.prepareMapping:
      return decodeContract(value, SCHEMAS.preparedMapping);
    case ENGAGEMENT_COMMAND_SCHEMAS.saveMappingDraft:
      return decodeContract(value, SCHEMAS.mappingDraft);
    case ENGAGEMENT_COMMAND_SCHEMAS.submitMappingDraft:
      return decodeContract(value, SCHEMAS.submittedMapping);
    case ENGAGEMENT_COMMAND_SCHEMAS.approveMappingVersion:
      return decodeContract(value, SCHEMAS.mappingApproval);
    case ENGAGEMENT_COMMAND_SCHEMAS.launchValidation:
      return decodeContract(value, SCHEMAS.validationLaunch);
  }
  throw new ContractError(
    "INVALID_RESPONSE",
    "The local command response does not match its closed result contract.",
  );
}

function isJobShape(value: Record<string, unknown>): boolean {
  const data = value.data;
  if (!isRecord(data) || !isText(value.canonical_hash)) return false;
  const source = data.accepted_source;
  const timestamps = data.timestamps;
  const counts = data.counts;
  const reconciliation = data.reconciliation;
  return (
    isText(data.job_id) &&
    isText(data.state) &&
    (source === null ||
      (isRecord(source) &&
        isText(source.source_name) &&
        isText(source.source_hash) &&
        isCount(source.source_size) &&
        isText(source.stored_archive))) &&
    isRecord(timestamps) &&
    isText(timestamps.created_at) &&
    isText(timestamps.updated_at) &&
    isNullableText(timestamps.completed_at) &&
    isRecord(counts) &&
    isCount(counts.accepted_file_count) &&
    isCount(counts.accepted_row_count) &&
    isCount(counts.accepted_field_count) &&
    isCount(counts.pending_row_count) &&
    isCount(counts.profiling_run_count) &&
    isRecord(reconciliation) &&
    isCount(reconciliation.source_records) &&
    isCount(reconciliation.stored_records) &&
    isCount(reconciliation.pending_records) &&
    typeof reconciliation.closed === "boolean" &&
    isText(data.integrity_status) &&
    (data.selected_governance === null ||
      isSelectedGovernance(data.selected_governance)) &&
    isBoundedArray(
      data.warnings,
      200,
      (warning): warning is JobSummary["data"]["warnings"][number] =>
        Boolean(
          isRecord(warning) && isText(warning.code) && isCount(warning.count),
        ),
    )
  );
}

function isFileShape(value: Record<string, unknown>): boolean {
  const scope = value.scope;
  return (
    isRecord(scope) &&
    isText(scope.job_id) &&
    isText(scope.source_hash) &&
    isBoundedArray(
      value.items,
      200,
      (item): item is FileManifestPage["items"][number] =>
        Boolean(
          isRecord(item) &&
          isText(item.file_id) &&
          isText(item.path) &&
          isCount(item.source_ordinal) &&
          isCount(item.compressed_bytes) &&
          isCount(item.expanded_bytes) &&
          isCount(item.row_count) &&
          isCount(item.column_count) &&
          isText(item.ingestion_state) &&
          isText(item.profiling_state),
        ),
    ) &&
    isPageInfo(value.page) &&
    isText(value.canonical_hash)
  );
}

function isProfilingShape(value: Record<string, unknown>): boolean {
  const scope = value.scope;
  return (
    isRecord(scope) &&
    isText(scope.run_id) &&
    isText(scope.job_id) &&
    isText(scope.result_hash) &&
    typeof value.include_samples === "boolean" &&
    isBoundedArray(
      value.items,
      200,
      (item): item is ProfilingSummaryPage["items"][number] => {
        if (!isRecord(item)) return false;
        const samples = item.samples;
        return (
          isText(item.path) &&
          isCount(item.column_ordinal) &&
          isText(item.column_name) &&
          isCount(item.empty_count) &&
          isCount(item.nonempty_count) &&
          isNullableCount(item.min_decoded_length) &&
          isNullableCount(item.max_decoded_length) &&
          isCount(item.oversized_count) &&
          isText(item.distinct_status) &&
          isNullableCount(item.distinct_count) &&
          isCount(item.sample_count) &&
          (samples === undefined ||
            isBoundedArray(
              samples,
              200,
              (
                sample,
              ): sample is NonNullable<
                ProfilingSummaryPage["items"][number]["samples"]
              >[number] =>
                Boolean(
                  isRecord(sample) &&
                  isText(sample.sha256) &&
                  isCount(sample.utf8_bytes) &&
                  isCount(sample.decoded_length) &&
                  (sample.value === undefined || isText(sample.value)),
                ),
            ))
        );
      },
    ) &&
    isPageInfo(value.page) &&
    isText(value.canonical_hash)
  );
}

function isProfileShape(value: Record<string, unknown>): boolean {
  const data = value.data;
  return (
    isRecord(data) &&
    isText(data.profile_id) &&
    isText(data.profile_version) &&
    isText(data.profile_hash) &&
    isText(data.schema_hash) &&
    isRecord(data.document) &&
    isText(data.declared_status) &&
    isText(data.validation_hash) &&
    isText(data.registered_at) &&
    isText(data.lifecycle_state) &&
    isBoundedArray(
      data.approvals,
      4096,
      (approval): approval is ProfileView["data"]["approvals"][number] =>
        Boolean(
          isRecord(approval) &&
          isText(approval.approval_id) &&
          isText(approval.approver) &&
          isText(approval.reason) &&
          typeof approval.acknowledged === "boolean" &&
          isText(approval.approved_at) &&
          isText(approval.approval_hash),
        ),
    ) &&
    isBoundedArray(
      data.lifecycle_events,
      4096,
      (event): event is ProfileView["data"]["lifecycle_events"][number] =>
        Boolean(
          isRecord(event) &&
          isCount(event.sequence) &&
          isNullableText(event.from_state) &&
          isText(event.to_state) &&
          isText(event.actor) &&
          isText(event.reason) &&
          isNullableText(event.previous_event_hash) &&
          isText(event.event_hash) &&
          isText(event.occurred_at),
        ),
    ) &&
    isText(value.canonical_hash)
  );
}

function isMappingShape(value: Record<string, unknown>): boolean {
  const scope = value.scope;
  const mapping = value.mapping;
  return (
    isRecord(scope) &&
    isText(scope.mapping_id) &&
    isText(scope.mapping_version) &&
    isText(scope.mapping_hash) &&
    isRecord(mapping) &&
    [
      "mapping_id",
      "mapping_version",
      "mapping_hash",
      "profile_id",
      "profile_version",
      "profile_hash",
      "source_hash",
      "validated_job_id",
    ].every((key) => isText(mapping[key])) &&
    isBoundedArray(
      value.items,
      200,
      (item): item is MappingCoveragePage["items"][number] =>
        Boolean(
          isRecord(item) &&
          isText(item.kind) &&
          isNullableText(item.mapping_kind) &&
          isNullableText(item.source_file) &&
          isNullableCount(item.source_column_ordinal) &&
          isNullableText(item.source_column_name) &&
          isNullableText(item.entity_id) &&
          isNullableText(item.field_id) &&
          isNullableText(item.binding_id) &&
          isNullableText(item.issue_code) &&
          isNullableText(item.detail),
        ),
    ) &&
    isPageInfo(value.page) &&
    isText(value.canonical_hash)
  );
}

function isIssueGroup(value: unknown): boolean {
  return (
    isRecord(value) &&
    hasExactKeys(value, ["items", "truncated"]) &&
    isBoundedArray(
      value.items,
      200,
      (item): item is { key: string; count: number } =>
        Boolean(
          isRecord(item) &&
          hasExactKeys(item, ["key", "count"]) &&
          isText(item.key) &&
          isCount(item.count),
        ),
    ) &&
    typeof value.truncated === "boolean"
  );
}

function isValidationShape(value: Record<string, unknown>): boolean {
  const data = value.data;
  if (!isRecord(data) || !isText(value.canonical_hash)) return false;
  const timestamps = data.timestamps;
  const groups = data.issue_breakdowns;
  return (
    [
      "run_id",
      "identity_hash",
      "job_id",
      "source_hash",
      "profile_id",
      "profile_version",
      "profile_hash",
      "mapping_id",
      "mapping_version",
      "mapping_hash",
      "rule_set_hash",
      "engine_version",
      "policy_hash",
      "state",
    ].every((key) => isText(data[key])) &&
    isNullableText(data.result_hash) &&
    isCount(data.issue_count) &&
    isCount(data.rows_processed) &&
    isNullableText(data.error_code) &&
    isNullableText(data.error_message) &&
    isRecord(timestamps) &&
    isText(timestamps.created_at) &&
    isText(timestamps.updated_at) &&
    isNullableText(timestamps.completed_at) &&
    isRecord(groups) &&
    ["by_severity", "by_rule", "by_entity", "by_field", "by_source_file"].every(
      (key) => isIssueGroup(groups[key]),
    )
  );
}

function isValidationIdentity(value: unknown): boolean {
  return (
    isRecord(value) &&
    [
      "run_id",
      "identity_hash",
      "job_id",
      "source_hash",
      "profile_id",
      "profile_version",
      "profile_hash",
      "mapping_id",
      "mapping_version",
      "mapping_hash",
      "state",
    ].every((key) => isText(value[key])) &&
    isNullableText(value.result_hash)
  );
}

function isIssuesShape(value: Record<string, unknown>): boolean {
  const filters = value.filters;
  return (
    isValidationIdentity(value.scope) &&
    isRecord(filters) &&
    ["severity", "rule_id", "entity_id", "field_id", "source_file"].every(
      (key) => isNullableText(filters[key]),
    ) &&
    isNullableCount(filters.source_ordinal) &&
    isBoundedArray(
      value.items,
      200,
      (item): item is IssueListPage["items"][number] =>
        Boolean(
          isRecord(item) &&
          [
            "issue_id",
            "severity",
            "rule_id",
            "issue_code",
            "display_message",
            "entity_id",
            "field_id",
            "source_file",
            "issue_hash",
          ].every((key) => isText(item[key])) &&
          isCount(item.source_ordinal) &&
          isCount(item.line_start) &&
          isCount(item.line_end),
        ),
    ) &&
    isPageInfo(value.page) &&
    isText(value.canonical_hash)
  );
}

function isRowShape(value: Record<string, unknown>): boolean {
  const data = value.data;
  if (!isRecord(data) || !isText(value.canonical_hash)) return false;
  return (
    ["run_id", "job_id", "source_hash", "source_file"].every((key) =>
      isText(data[key]),
    ) &&
    isCount(data.source_ordinal) &&
    isCount(data.line_start) &&
    isCount(data.line_end) &&
    isBoundedArray(
      data.source_values,
      4096,
      (item): item is RowDetail["data"]["source_values"][number] =>
        Boolean(
          isRecord(item) &&
          isCount(item.column_ordinal) &&
          isText(item.column_name) &&
          isText(item.value),
        ),
    ) &&
    isBoundedArray(
      data.canonical_projection,
      4096,
      (item): item is RowDetail["data"]["canonical_projection"][number] => {
        if (!isRecord(item) || !isRecord(item.lineage)) return false;
        return (
          isText(item.entity_id) &&
          isText(item.field_id) &&
          isNullableText(item.value) &&
          isText(item.lineage.kind) &&
          isText(item.lineage.binding_id) &&
          isNullableCount(item.lineage.source_column_ordinal) &&
          isNullableText(item.lineage.source_column_name) &&
          isNullableText(item.lineage.lookup_table_id)
        );
      },
    ) &&
    isBoundedArray(
      data.issues,
      200,
      (item): item is RowDetail["data"]["issues"][number] =>
        Boolean(
          isRecord(item) &&
          [
            "issue_id",
            "severity",
            "rule_id",
            "issue_code",
            "display_message",
            "entity_id",
            "field_id",
            "issue_hash",
          ].every((key) => isText(item[key])),
        ),
    )
  );
}

function isOneOf<const T extends readonly string[]>(
  value: unknown,
  allowed: T,
): value is T[number] {
  return typeof value === "string" && allowed.includes(value);
}

function isHash(value: unknown): value is string {
  return typeof value === "string" && /^[a-f0-9]{64}$/.test(value);
}

function isDecisionSummary(value: unknown): value is DecisionSummary {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "decision_id",
      "disposition",
      "practitioner_identifier",
      "applied_at",
      "reason_summary",
      "correction_overlay_id",
      "reevaluation_id",
      "reevaluation_state",
      "generation",
      "supersession_count",
      "lifecycle_outcome",
      "integrity_status",
      "active",
    ]) &&
    isText(value.decision_id) &&
    isOneOf(value.disposition, DECISION_DISPOSITIONS) &&
    isText(value.practitioner_identifier) &&
    isText(value.applied_at) &&
    isText(value.reason_summary) &&
    isNullableText(value.correction_overlay_id) &&
    isNullableText(value.reevaluation_id) &&
    isOneOf(value.reevaluation_state, REEVALUATION_STATES) &&
    isCount(value.generation) &&
    isCount(value.supersession_count) &&
    isOneOf(value.lifecycle_outcome, DECISION_OUTCOMES) &&
    isOneOf(value.integrity_status, DECISION_INTEGRITY_STATUSES) &&
    typeof value.active === "boolean"
  );
}

function isCanonicalLineage(value: unknown): value is CanonicalLineage {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "kind",
      "binding_id",
      "source_column_ordinal",
      "source_column_name",
      "lookup_table_id",
    ]) &&
    isOneOf(value.kind, CANONICAL_LINEAGE_KINDS) &&
    isText(value.binding_id) &&
    isNullableCount(value.source_column_ordinal) &&
    (value.source_column_ordinal === null ||
      value.source_column_ordinal <= 4095) &&
    isNullableText(value.source_column_name) &&
    isNullableText(value.lookup_table_id)
  );
}

function isDecisionAttemptSummary(
  value: unknown,
): value is DecisionAttemptSummary {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "decision_attempt_id",
      "disposition",
      "correction_overlay_id",
      "reevaluation_id",
      "attempt_state",
      "reevaluation_state",
      "lifecycle_outcome",
      "integrity_status",
      "active",
    ]) &&
    isText(value.decision_attempt_id) &&
    isOneOf(value.disposition, DECISION_DISPOSITIONS) &&
    isNullableText(value.correction_overlay_id) &&
    isNullableText(value.reevaluation_id) &&
    isOneOf(value.attempt_state, DECISION_ATTEMPT_STATES) &&
    isOneOf(value.reevaluation_state, REEVALUATION_STATES) &&
    isOneOf(value.lifecycle_outcome, DECISION_OUTCOMES) &&
    isOneOf(value.integrity_status, DECISION_INTEGRITY_STATUSES) &&
    typeof value.active === "boolean" &&
    (!value.active || value.lifecycle_outcome === "EFFECTIVE")
  );
}

function isDecisionQueueShape(value: Record<string, unknown>): boolean {
  const scope = value.scope;
  const filters = value.filters;
  return (
    hasExactKeys(value, [
      "schema_version",
      "scope",
      "filters",
      "items",
      "page",
      "canonical_hash",
    ]) &&
    isRecord(scope) &&
    hasExactKeys(scope, ["job_id", "validation_run_id"]) &&
    isText(scope.job_id) &&
    isText(scope.validation_run_id) &&
    isRecord(filters) &&
    hasExactKeys(filters, [
      "disposition",
      "severity",
      "entity_id",
      "field_id",
      "rule_id",
      "source_file",
      "reevaluation_state",
    ]) &&
    (filters.disposition === null ||
      filters.disposition === "UNRESOLVED" ||
      isOneOf(filters.disposition, DECISION_DISPOSITIONS)) &&
    ["severity", "entity_id", "field_id", "rule_id", "source_file"].every(
      (key) => isNullableText(filters[key]),
    ) &&
    (filters.reevaluation_state === null ||
      isOneOf(filters.reevaluation_state, REEVALUATION_STATES)) &&
    isBoundedArray(
      value.items,
      200,
      (item): item is DecisionQueuePage["items"][number] =>
        Boolean(
          isRecord(item) &&
          hasExactKeys(item, [
            "issue_id",
            "severity",
            "rule_id",
            "issue_code",
            "display_message",
            "entity_id",
            "field_id",
            "source_file",
            "source_ordinal",
            "active_decision",
            "latest_attempt",
          ]) &&
          [
            "issue_id",
            "severity",
            "rule_id",
            "issue_code",
            "display_message",
            "entity_id",
            "field_id",
            "source_file",
          ].every((key) => isText(item[key])) &&
          isCount(item.source_ordinal) &&
          (item.active_decision === null ||
            isDecisionSummary(item.active_decision)) &&
          (item.latest_attempt === null ||
            isDecisionAttemptSummary(item.latest_attempt)),
        ),
    ) &&
    isPageInfo(value.page) &&
    isHash(value.canonical_hash)
  );
}

function isDecisionCountsShape(value: Record<string, unknown>): boolean {
  const scope = value.scope;
  const groups = value.groups;
  const groupKeys = [
    "by_disposition",
    "by_severity",
    "by_entity",
    "by_field",
    "by_rule",
    "by_source_file",
    "by_reevaluation_state",
  ];
  return (
    hasExactKeys(value, [
      "schema_version",
      "scope",
      "groups",
      "canonical_hash",
    ]) &&
    isRecord(scope) &&
    hasExactKeys(scope, ["job_id", "validation_run_id"]) &&
    isText(scope.job_id) &&
    isText(scope.validation_run_id) &&
    isRecord(groups) &&
    hasExactKeys(groups, groupKeys) &&
    groupKeys.every((key) => isIssueGroup(groups[key])) &&
    isHash(value.canonical_hash)
  );
}

function isDecisionIssueShape(value: Record<string, unknown>): boolean {
  const data = value.data;
  if (!isRecord(data) || !isHash(value.canonical_hash)) return false;
  const row = data.accepted_row;
  const field = data.field_context;
  const concurrency = data.concurrency;
  if (!isRecord(row) || !isRecord(field) || !isRecord(concurrency))
    return false;
  const constraints = field.constraints;
  return (
    hasExactKeys(value, ["schema_version", "data", "canonical_hash"]) &&
    hasExactKeys(data, [
      "job_id",
      "validation_run_id",
      "issue_id",
      "issue_hash",
      "severity",
      "rule_id",
      "issue_code",
      "display_message",
      "entity_id",
      "field_id",
      "accepted_row",
      "field_context",
      "active_decision",
      "latest_attempt",
      "decision_history_count",
      "concurrency",
      "authority_notice",
    ]) &&
    hasExactKeys(row, [
      "source_file",
      "source_ordinal",
      "line_start",
      "line_end",
    ]) &&
    hasExactKeys(field, [
      "received_source_value",
      "original_canonical_value",
      "original_lineage",
      "active_overlay_value",
      "effective_canonical_value",
      "effective_value_hash",
      "active_overlay_id",
      "correction_eligible",
      "allowed_operations",
      "constraints",
    ]) &&
    [
      "job_id",
      "validation_run_id",
      "issue_id",
      "issue_hash",
      "severity",
      "rule_id",
      "issue_code",
      "display_message",
      "entity_id",
      "field_id",
      "authority_notice",
    ].every((key) => isText(data[key])) &&
    isText(row.source_file) &&
    isCount(row.source_ordinal) &&
    isCount(row.line_start) &&
    isCount(row.line_end) &&
    isNullableText(field.received_source_value) &&
    isNullableText(field.original_canonical_value) &&
    isCanonicalLineage(field.original_lineage) &&
    isNullableText(field.active_overlay_value) &&
    isNullableText(field.effective_canonical_value) &&
    isHash(field.effective_value_hash) &&
    isNullableText(field.active_overlay_id) &&
    typeof field.correction_eligible === "boolean" &&
    isBoundedArray(field.allowed_operations, 3, (operation) =>
      isOneOf(operation, CORRECTION_OPERATIONS),
    ) &&
    isRecord(constraints) &&
    hasExactKeys(constraints, [
      "maximum_characters",
      "allow_empty",
      "allow_null",
    ]) &&
    isNullableCount(constraints.maximum_characters) &&
    typeof constraints.allow_empty === "boolean" &&
    typeof constraints.allow_null === "boolean" &&
    (data.active_decision === null ||
      isDecisionSummary(data.active_decision)) &&
    (data.latest_attempt === null ||
      isDecisionAttemptSummary(data.latest_attempt)) &&
    isCount(data.decision_history_count) &&
    hasExactKeys(concurrency, [
      "expected_active_decision_id",
      "expected_active_generation",
      "expected_effective_value_hash",
    ]) &&
    isNullableText(concurrency.expected_active_decision_id) &&
    isCount(concurrency.expected_active_generation) &&
    isHash(concurrency.expected_effective_value_hash)
  );
}

function isDecisionHistoryShape(value: Record<string, unknown>): boolean {
  const scope = value.scope;
  return (
    hasExactKeys(value, [
      "schema_version",
      "scope",
      "items",
      "page",
      "canonical_hash",
    ]) &&
    isRecord(scope) &&
    hasExactKeys(scope, ["job_id", "validation_run_id", "issue_id"]) &&
    ["job_id", "validation_run_id", "issue_id"].every((key) =>
      isText(scope[key]),
    ) &&
    isBoundedArray(
      value.items,
      200,
      (item): item is DecisionHistoryPage["items"][number] =>
        Boolean(
          isRecord(item) &&
          hasExactKeys(item, [
            "decision_id",
            "disposition",
            "practitioner_identifier",
            "applied_at",
            "reason",
            "evidence_reference",
            "prior_decision_id",
            "generation",
            "correction_overlay_id",
            "reevaluation_id",
            "canonical_hash",
            "lifecycle_outcome",
            "active",
          ]) &&
          isText(item.decision_id) &&
          isOneOf(item.disposition, DECISION_DISPOSITIONS) &&
          isText(item.practitioner_identifier) &&
          isText(item.applied_at) &&
          isText(item.reason) &&
          isNullableText(item.evidence_reference) &&
          isNullableText(item.prior_decision_id) &&
          isCount(item.generation) &&
          isNullableText(item.correction_overlay_id) &&
          isNullableText(item.reevaluation_id) &&
          isHash(item.canonical_hash) &&
          isOneOf(item.lifecycle_outcome, DECISION_OUTCOMES) &&
          typeof item.active === "boolean",
        ),
    ) &&
    isPageInfo(value.page) &&
    isHash(value.canonical_hash)
  );
}

function isCorrectionShape(value: Record<string, unknown>): boolean {
  const data = value.data;
  if (!isRecord(data)) return false;
  const commonShape =
    hasExactKeys(value, ["schema_version", "data", "canonical_hash"]) &&
    hasExactKeys(data, [
      "overlay_id",
      "decision_attempt_id",
      "decision_id",
      "job_id",
      "validation_run_id",
      "issue_id",
      "source_file",
      "source_ordinal",
      "entity_id",
      "field_id",
      "profile_id",
      "profile_version",
      "profile_hash",
      "mapping_id",
      "mapping_version",
      "mapping_hash",
      "operation",
      "original_canonical_value",
      "original_lineage",
      "prior_effective_value",
      "overlay_value",
      "effective_canonical_value",
      "effective_value_hash",
      "practitioner_identifier",
      "reason",
      "attempted_at",
      "applied_at",
      "active",
      "superseded_by_overlay_id",
      "reevaluation_id",
      "attempt_state",
      "reevaluation_state",
      "lifecycle_outcome",
      "error_code",
      "error_message",
      "integrity_status",
    ]) &&
    [
      "overlay_id",
      "decision_attempt_id",
      "job_id",
      "validation_run_id",
      "issue_id",
      "source_file",
      "entity_id",
      "field_id",
      "profile_id",
      "profile_version",
      "profile_hash",
      "mapping_id",
      "mapping_version",
      "mapping_hash",
      "practitioner_identifier",
      "reason",
      "attempted_at",
    ].every((key) => isText(data[key])) &&
    isNullableText(data.decision_id) &&
    isCount(data.source_ordinal) &&
    data.source_ordinal > 0 &&
    isOneOf(data.operation, CORRECTION_OPERATIONS) &&
    isNullableText(data.original_canonical_value) &&
    isCanonicalLineage(data.original_lineage) &&
    isNullableText(data.prior_effective_value) &&
    isNullableText(data.overlay_value) &&
    isNullableText(data.effective_canonical_value) &&
    (data.effective_value_hash === null || isHash(data.effective_value_hash)) &&
    isNullableText(data.applied_at) &&
    typeof data.active === "boolean" &&
    isNullableText(data.superseded_by_overlay_id) &&
    isNullableText(data.reevaluation_id) &&
    isOneOf(data.attempt_state, DECISION_ATTEMPT_STATES) &&
    isOneOf(data.reevaluation_state, REEVALUATION_STATES) &&
    isOneOf(data.lifecycle_outcome, DECISION_OUTCOMES) &&
    isNullableText(data.error_code) &&
    isNullableText(data.error_message) &&
    isOneOf(data.integrity_status, DECISION_INTEGRITY_STATUSES) &&
    isHash(value.canonical_hash);
  if (!commonShape) return false;
  const validOverlayValue =
    (data.operation === "REPLACE_EFFECTIVE_TEXT" &&
      isText(data.overlay_value) &&
      data.overlay_value.length > 0) ||
    (data.operation === "SET_EFFECTIVE_EMPTY" && data.overlay_value === "") ||
    (data.operation === "SET_EFFECTIVE_NULL" && data.overlay_value === null);
  if (!validOverlayValue) return false;
  if (data.decision_id === null) {
    return (
      data.effective_canonical_value === null &&
      data.effective_value_hash === null &&
      data.applied_at === null &&
      data.active === false &&
      [
        "PENDING_REEVALUATION",
        "FAILED",
        "INTERRUPTED",
        "STALE_REJECTED",
        "INTEGRITY_BLOCKED",
      ].includes(data.lifecycle_outcome as DecisionLifecycleOutcome)
    );
  }
  return (
    isHash(data.effective_value_hash) &&
    isText(data.applied_at) &&
    isText(data.reevaluation_id) &&
    data.reevaluation_state === "COMPLETE" &&
    (data.lifecycle_outcome === "EFFECTIVE" ||
      data.lifecycle_outcome === "SUPERSEDED") &&
    data.error_code === null &&
    data.error_message === null
  );
}

function isDecisionResultShape(value: Record<string, unknown>): boolean {
  const data = value.data;
  if (!isRecord(data)) return false;
  const common =
    hasExactKeys(value, ["schema_version", "data", "canonical_hash"]) &&
    hasExactKeys(data, [
      "decision_id",
      "disposition",
      "lifecycle_outcome",
      "active",
      "idempotent_replay",
      "correction_overlay_id",
      "reevaluation_id",
      "reevaluation_state",
      "original_issue_present",
      "residual_issue_count",
      "new_issue_count",
      "integrity_status",
    ]) &&
    isText(data.decision_id) &&
    isOneOf(data.disposition, DECISION_DISPOSITIONS) &&
    data.lifecycle_outcome === "EFFECTIVE" &&
    typeof data.active === "boolean" &&
    typeof data.idempotent_replay === "boolean" &&
    isNullableText(data.correction_overlay_id) &&
    isNullableText(data.reevaluation_id) &&
    (data.reevaluation_state === "NOT_REQUIRED" ||
      data.reevaluation_state === "COMPLETE") &&
    typeof data.original_issue_present === "boolean" &&
    isCount(data.residual_issue_count) &&
    isCount(data.new_issue_count) &&
    data.integrity_status === "VERIFIED" &&
    isHash(value.canonical_hash);
  if (!common) return false;
  return data.disposition === "CORRECTED"
    ? isText(data.correction_overlay_id) &&
        isText(data.reevaluation_id) &&
        data.reevaluation_state === "COMPLETE"
    : data.correction_overlay_id === null &&
        data.reevaluation_id === null &&
        data.reevaluation_state === "NOT_REQUIRED";
}

function isCategoryCounts(
  value: unknown,
): value is ReconciliationCategoryCount[] {
  return (
    Array.isArray(value) &&
    value.length === RECONCILIATION_CATEGORIES.length &&
    value.every((item, index) => {
      const expected = RECONCILIATION_CATEGORIES[index];
      return (
        isRecord(item) &&
        hasExactKeys(item, ["category", "row_count"]) &&
        item.category === expected &&
        isCount(item.row_count)
      );
    })
  );
}

function isResultSnapshotSummary(
  value: unknown,
): value is ResultReconciliationSnapshotSummary {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "snapshot_id",
      "state",
      "job_id",
      "accepted_row_count",
      "governing_input_hash",
      "snapshot_hash",
      "current",
      "counts",
      "warning_codes",
      "blocker_codes",
      "completed_at",
    ]) &&
    isDecisionIdentity(value.snapshot_id) &&
    isOneOf(value.state, RECONCILIATION_ATTEMPT_STATES) &&
    isDecisionIdentity(value.job_id) &&
    isCount(value.accepted_row_count) &&
    isHash(value.governing_input_hash) &&
    (value.snapshot_hash === null || isHash(value.snapshot_hash)) &&
    typeof value.current === "boolean" &&
    isCategoryCounts(value.counts) &&
    isUniqueResultCodes(value.warning_codes) &&
    isUniqueResultCodes(value.blocker_codes) &&
    (value.completed_at === null ||
      isBoundedCommandText(value.completed_at, 128)) &&
    (value.state === "COMPLETE"
      ? isHash(value.snapshot_hash) && value.completed_at !== null
      : value.snapshot_hash === null)
  );
}

function isResultSnapshotListShape(value: Record<string, unknown>): boolean {
  const scope = value.scope;
  return (
    hasExactKeys(value, [
      "schema_version",
      "scope",
      "items",
      "page",
      "canonical_hash",
    ]) &&
    isRecord(scope) &&
    hasExactKeys(scope, ["job_id"]) &&
    isDecisionIdentity(scope.job_id) &&
    isBoundedArray(value.items, 200, isResultSnapshotSummary) &&
    isPageInfo(value.page) &&
    isHash(value.canonical_hash)
  );
}

function isResultSnapshotShape(value: Record<string, unknown>): boolean {
  return (
    hasExactKeys(value, ["schema_version", "data", "canonical_hash"]) &&
    isResultSnapshotSummary(value.data) &&
    isHash(value.canonical_hash)
  );
}

function isResultRowsShape(value: Record<string, unknown>): boolean {
  const scope = value.scope;
  const filters = value.filters;
  return (
    hasExactKeys(value, [
      "schema_version",
      "scope",
      "filters",
      "items",
      "page",
      "canonical_hash",
    ]) &&
    isRecord(scope) &&
    hasExactKeys(scope, ["job_id", "snapshot_id", "snapshot_hash"]) &&
    isDecisionIdentity(scope.job_id) &&
    isDecisionIdentity(scope.snapshot_id) &&
    isHash(scope.snapshot_hash) &&
    isRecord(filters) &&
    hasExactKeys(filters, [
      "category",
      "source_file",
      "source_ordinal",
      "entity_id",
      "issue_id",
      "decision_id",
    ]) &&
    (filters.category === null ||
      isOneOf(filters.category, RECONCILIATION_CATEGORIES)) &&
    (filters.source_ordinal === null ||
      (isCount(filters.source_ordinal) && filters.source_ordinal > 0)) &&
    ["source_file", "entity_id", "issue_id", "decision_id"].every((key) =>
      isNullableText(filters[key]),
    ) &&
    isBoundedArray(
      value.items,
      200,
      (item): item is ResultReconciliationRowList["items"][number] =>
        Boolean(
          isRecord(item) &&
          hasExactKeys(item, [
            "source_file",
            "source_ordinal",
            "line_start",
            "line_end",
            "entity_id",
            "category",
            "explanation_codes",
            "lineage_hashes",
          ]) &&
          isSafeSourceFile(item.source_file) &&
          isCount(item.source_ordinal) &&
          item.source_ordinal > 0 &&
          isCount(item.line_start) &&
          item.line_start > 0 &&
          isCount(item.line_end) &&
          item.line_end >= item.line_start &&
          isDecisionIdentity(item.entity_id) &&
          isOneOf(item.category, RECONCILIATION_CATEGORIES) &&
          isUniqueResultCodes(item.explanation_codes) &&
          isBoundedArray(item.lineage_hashes, 200, isHash),
        ),
    ) &&
    isPageInfo(value.page) &&
    isHash(value.canonical_hash)
  );
}

const RESULT_LINEAGE_REFERENCE_KINDS = [
  "VALIDATION_RUN",
  "VALIDATION_ISSUE",
  "ISSUE_DECISION",
  "CORRECTION_OVERLAY",
  "CORRECTION_EFFECTIVATION",
  "REEVALUATION_RUN",
] as const;

function isResultRowDetailShape(value: Record<string, unknown>): boolean {
  const scope = value.scope;
  const data = value.data;
  return (
    hasExactKeys(value, [
      "schema_version",
      "scope",
      "data",
      "canonical_hash",
    ]) &&
    isRecord(scope) &&
    hasExactKeys(scope, ["job_id", "snapshot_id", "snapshot_hash"]) &&
    isDecisionIdentity(scope.job_id) &&
    isDecisionIdentity(scope.snapshot_id) &&
    isHash(scope.snapshot_hash) &&
    isRecord(data) &&
    hasExactKeys(data, [
      "source_file",
      "source_ordinal",
      "line_start",
      "line_end",
      "row_identity_hash",
      "entity_id",
      "category",
      "explanation_codes",
      "lineage_references",
      "classification_hash",
    ]) &&
    isSafeSourceFile(data.source_file) &&
    isCount(data.source_ordinal) &&
    data.source_ordinal > 0 &&
    isCount(data.line_start) &&
    data.line_start > 0 &&
    isCount(data.line_end) &&
    data.line_end >= data.line_start &&
    isHash(data.row_identity_hash) &&
    isDecisionIdentity(data.entity_id) &&
    isOneOf(data.category, RECONCILIATION_CATEGORIES) &&
    isUniqueResultCodes(data.explanation_codes) &&
    isBoundedArray(
      data.lineage_references,
      200,
      (reference): reference is ResultReconciliationLineageReference =>
        Boolean(
          isRecord(reference) &&
          hasExactKeys(reference, [
            "reference_kind",
            "reference_id",
            "reference_hash",
          ]) &&
          isOneOf(reference.reference_kind, RESULT_LINEAGE_REFERENCE_KINDS) &&
          isDecisionIdentity(reference.reference_id) &&
          isHash(reference.reference_hash),
        ),
    ) &&
    isHash(data.classification_hash) &&
    isHash(value.canonical_hash)
  );
}

function isResultCandidateSummary(
  value: unknown,
): value is ResultCandidateSummary {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "candidate_id",
      "snapshot_id",
      "snapshot_hash",
      "candidate_hash",
      "current",
      "eligible",
      "included_count",
      "excluded_count",
      "blocked_count",
      "warning_codes",
      "required_acknowledgments",
      "creator",
      "preparation_reason",
      "created_at",
    ]) &&
    isDecisionIdentity(value.candidate_id) &&
    isDecisionIdentity(value.snapshot_id) &&
    isHash(value.snapshot_hash) &&
    isHash(value.candidate_hash) &&
    typeof value.current === "boolean" &&
    typeof value.eligible === "boolean" &&
    isCount(value.included_count) &&
    isCount(value.excluded_count) &&
    isCount(value.blocked_count) &&
    isUniqueResultCodes(value.warning_codes) &&
    isUniqueResultCodes(value.required_acknowledgments) &&
    isBoundedCommandText(value.creator, 256, false, true) &&
    isBoundedCommandText(value.preparation_reason, 4096) &&
    isBoundedCommandText(value.created_at, 128)
  );
}

function isResultCandidateListShape(value: Record<string, unknown>): boolean {
  const scope = value.scope;
  return (
    hasExactKeys(value, [
      "schema_version",
      "scope",
      "items",
      "page",
      "canonical_hash",
    ]) &&
    isRecord(scope) &&
    hasExactKeys(scope, ["job_id"]) &&
    isDecisionIdentity(scope.job_id) &&
    isBoundedArray(value.items, 200, isResultCandidateSummary) &&
    isPageInfo(value.page) &&
    isHash(value.canonical_hash)
  );
}

function isResultEligibilityShape(value: Record<string, unknown>): boolean {
  const data = value.data;
  return (
    hasExactKeys(value, ["schema_version", "data", "canonical_hash"]) &&
    isRecord(data) &&
    hasExactKeys(data, [
      "candidate_id",
      "candidate_hash",
      "snapshot_id",
      "snapshot_hash",
      "eligible",
      "current",
      "reason_codes",
      "required_acknowledgments",
      "expected_active_action_id",
      "expected_generation",
    ]) &&
    isDecisionIdentity(data.candidate_id) &&
    isHash(data.candidate_hash) &&
    isDecisionIdentity(data.snapshot_id) &&
    isHash(data.snapshot_hash) &&
    typeof data.eligible === "boolean" &&
    typeof data.current === "boolean" &&
    isUniqueResultCodes(data.reason_codes) &&
    isUniqueResultCodes(data.required_acknowledgments) &&
    (data.expected_active_action_id === null ||
      isDecisionIdentity(data.expected_active_action_id)) &&
    isCount(data.expected_generation) &&
    isHash(value.canonical_hash)
  );
}

function isResultActionSummary(value: unknown): value is ResultActionSummary {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "action_id",
      "action_kind",
      "action_hash",
      "candidate_id",
      "candidate_hash",
      "actor",
      "reason",
      "generation",
      "predecessor_action_id",
      "occurred_at",
    ]) &&
    isDecisionIdentity(value.action_id) &&
    isOneOf(value.action_kind, RESULT_ACTION_KINDS) &&
    isHash(value.action_hash) &&
    isDecisionIdentity(value.candidate_id) &&
    isHash(value.candidate_hash) &&
    isBoundedCommandText(value.actor, 256, false, true) &&
    isBoundedCommandText(value.reason, 4096) &&
    isCount(value.generation) &&
    (value.predecessor_action_id === null ||
      isDecisionIdentity(value.predecessor_action_id)) &&
    isBoundedCommandText(value.occurred_at, 128)
  );
}

function isResultActionHistoryShape(value: Record<string, unknown>): boolean {
  const scope = value.scope;
  return (
    hasExactKeys(value, [
      "schema_version",
      "scope",
      "items",
      "page",
      "canonical_hash",
    ]) &&
    isRecord(scope) &&
    hasExactKeys(scope, ["job_id", "candidate_id"]) &&
    isDecisionIdentity(scope.job_id) &&
    (scope.candidate_id === null || isDecisionIdentity(scope.candidate_id)) &&
    isBoundedArray(value.items, 200, isResultActionSummary) &&
    isPageInfo(value.page) &&
    isHash(value.canonical_hash)
  );
}

function isResultReconciliationCommandShape(
  value: Record<string, unknown>,
): boolean {
  const data = value.data;
  return (
    hasExactKeys(value, ["schema_version", "data", "canonical_hash"]) &&
    isRecord(data) &&
    hasExactKeys(data, [
      "snapshot_id",
      "snapshot_hash",
      "state",
      "current",
      "accepted_row_count",
      "blocked_count",
      "idempotent_replay",
    ]) &&
    isDecisionIdentity(data.snapshot_id) &&
    isHash(data.snapshot_hash) &&
    data.state === "COMPLETE" &&
    typeof data.current === "boolean" &&
    isCount(data.accepted_row_count) &&
    isCount(data.blocked_count) &&
    typeof data.idempotent_replay === "boolean" &&
    isHash(value.canonical_hash)
  );
}

function isResultCandidateCommandShape(
  value: Record<string, unknown>,
): boolean {
  const data = value.data;
  return (
    hasExactKeys(value, ["schema_version", "data", "canonical_hash"]) &&
    isRecord(data) &&
    hasExactKeys(data, [
      "candidate_id",
      "candidate_hash",
      "snapshot_id",
      "snapshot_hash",
      "current",
      "eligible",
      "generation",
      "idempotent_replay",
    ]) &&
    isDecisionIdentity(data.candidate_id) &&
    isHash(data.candidate_hash) &&
    isDecisionIdentity(data.snapshot_id) &&
    isHash(data.snapshot_hash) &&
    typeof data.current === "boolean" &&
    typeof data.eligible === "boolean" &&
    isCount(data.generation) &&
    typeof data.idempotent_replay === "boolean" &&
    isHash(value.canonical_hash)
  );
}

function isResultActionCommandShape(value: Record<string, unknown>): boolean {
  const data = value.data;
  return (
    hasExactKeys(value, ["schema_version", "data", "canonical_hash"]) &&
    isRecord(data) &&
    hasExactKeys(data, [
      "action_id",
      "action_kind",
      "action_hash",
      "candidate_id",
      "candidate_hash",
      "generation",
      "active_approval",
      "idempotent_replay",
    ]) &&
    isDecisionIdentity(data.action_id) &&
    isOneOf(data.action_kind, RESULT_ACTION_KINDS) &&
    isHash(data.action_hash) &&
    isDecisionIdentity(data.candidate_id) &&
    isHash(data.candidate_hash) &&
    isCount(data.generation) &&
    typeof data.active_approval === "boolean" &&
    typeof data.idempotent_replay === "boolean" &&
    isHash(value.canonical_hash)
  );
}

function isReconciliationShape(value: Record<string, unknown>): boolean {
  const scope = value.scope;
  const aggregate = value.aggregate;
  return (
    isRecord(scope) &&
    isText(scope.job_id) &&
    isText(scope.source_hash) &&
    isNullableText(scope.validation_run_id) &&
    isRecord(aggregate) &&
    isCount(aggregate.source_records) &&
    isCount(aggregate.stored_records) &&
    isCount(aggregate.pending_records) &&
    typeof aggregate.closed === "boolean" &&
    isNullableCount(aggregate.validation_rows_processed) &&
    isNullableCount(aggregate.validation_issue_count) &&
    isBoundedArray(
      value.items,
      200,
      (item): item is ReconciliationSummary["items"][number] =>
        Boolean(
          isRecord(item) &&
          isText(item.path) &&
          isCount(item.source_records) &&
          isCount(item.stored_records) &&
          isCount(item.pending_records) &&
          typeof item.closed === "boolean" &&
          isNullableCount(item.validation_issue_count),
        ),
    ) &&
    isPageInfo(value.page) &&
    isText(value.canonical_hash)
  );
}

function isOperationsShape(value: Record<string, unknown>): boolean {
  const scope = value.scope;
  return (
    isRecord(scope) &&
    isBoundedArray(scope.states, 20, isText) &&
    isBoundedArray(
      value.items,
      200,
      (item): item is OperationalCleanupPage["items"][number] =>
        Boolean(
          isRecord(item) &&
          isText(item.cleanup_id) &&
          isText(item.namespace) &&
          isNullableText(item.job_id) &&
          isText(item.state) &&
          isCount(item.cleanup_attempts) &&
          isNullableText(item.last_error_code) &&
          typeof item.diagnostic_present === "boolean" &&
          isText(item.created_at) &&
          isText(item.updated_at) &&
          isNullableText(item.quarantined_at) &&
          isNullableText(item.cleaned_at),
        ),
    ) &&
    isPageInfo(value.page) &&
    isText(value.canonical_hash)
  );
}

function isPublicationEligibilityShape(
  value: Record<string, unknown>,
): boolean {
  return (
    hasExactKeys(value, [
      "schema_version",
      "is_eligible",
      "job_id",
      "candidate_id",
      "candidate_hash",
      "approval_id",
      "approval_hash",
      "total_accepted_rows",
      "included_rows",
      "excluded_rows",
      "blocked_rows",
      "blockers",
      "warnings",
      "required_acknowledgments",
      "evaluated_at",
    ]) &&
    typeof value.is_eligible === "boolean" &&
    isText(value.job_id) &&
    isNullableText(value.candidate_id) &&
    (value.candidate_hash === null || isHash(value.candidate_hash)) &&
    isNullableText(value.approval_id) &&
    (value.approval_hash === null || isHash(value.approval_hash)) &&
    isCount(value.total_accepted_rows) &&
    isCount(value.included_rows) &&
    isCount(value.excluded_rows) &&
    isCount(value.blocked_rows) &&
    isBoundedArray(value.blockers, 100, isText) &&
    isBoundedArray(value.warnings, 100, isText) &&
    isBoundedArray(value.required_acknowledgments, 100, isText) &&
    isText(value.evaluated_at)
  );
}

function isPublicationSummaryItem(
  value: unknown,
): value is PublicationSummaryItem {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "publication_id",
      "job_id",
      "candidate_id",
      "approval_id",
      "status",
      "practitioner",
      "publication_reason",
      "semantic_publication_hash",
      "physical_archive_digest",
      "package_size_bytes",
      "archive_relative_path",
      "created_at",
      "completed_at",
    ]) &&
    isText(value.publication_id) &&
    isText(value.job_id) &&
    isText(value.candidate_id) &&
    isText(value.approval_id) &&
    isText(value.status) &&
    isText(value.practitioner) &&
    isText(value.publication_reason) &&
    (value.semantic_publication_hash === null ||
      isHash(value.semantic_publication_hash)) &&
    (value.physical_archive_digest === null ||
      isHash(value.physical_archive_digest)) &&
    isNullableCount(value.package_size_bytes) &&
    isNullableText(value.archive_relative_path) &&
    isText(value.created_at) &&
    isNullableText(value.completed_at)
  );
}

function isPublicationListShape(value: Record<string, unknown>): boolean {
  return (
    hasExactKeys(value, ["schema_version", "job_id", "total_count", "items"]) &&
    isText(value.job_id) &&
    isCount(value.total_count) &&
    isBoundedArray(value.items, 1000, isPublicationSummaryItem) &&
    value.total_count === value.items.length
  );
}

function isEngagementReadinessPackageHistoryShape(
  value: Record<string, unknown>,
): boolean {
  if (
    !hasExactKeys(value, [
      "schema_version",
      "engagement_id",
      "total_count",
      "returned_count",
      "truncated",
      "items",
    ]) ||
    value.schema_version !== SCHEMAS.engagementReadinessPackages ||
    !isDecisionIdentity(value.engagement_id) ||
    !isCount(value.total_count) ||
    !isCount(value.returned_count) ||
    typeof value.truncated !== "boolean" ||
    !isBoundedArray(
      value.items,
      200,
      (item): item is PublicationSummaryItem => {
        return (
          isPublicationSummaryItem(item) &&
          item.status === "COMPLETE" &&
          isHash(item.semantic_publication_hash) &&
          isHash(item.physical_archive_digest) &&
          isCount(item.package_size_bytes) &&
          isText(item.archive_relative_path) &&
          isText(item.completed_at)
        );
      },
    )
  ) {
    return false;
  }
  const items = value.items;
  return (
    value.returned_count === items.length &&
    value.total_count >= items.length &&
    value.truncated === value.total_count > items.length &&
    new Set(items.map((item) => item.publication_id)).size === items.length
  );
}

function isManifestMember(value: unknown): value is ManifestMember {
  if (!isRecord(value)) return false;
  const keys = Object.keys(value);
  const requiredKeys = ["filename", "media_type", "byte_size", "sha256_digest"];
  return (
    requiredKeys.every((key) => keys.includes(key)) &&
    keys.every((key) => [...requiredKeys, "row_count"].includes(key)) &&
    isText(value.filename) &&
    isText(value.media_type) &&
    (value.row_count === undefined || isCount(value.row_count)) &&
    isCount(value.byte_size) &&
    isHash(value.sha256_digest)
  );
}

function isPublicationManifest(value: unknown): value is PublicationManifest {
  if (!isRecord(value)) return false;
  const textKeys = [
    "publication_id",
    "publication_version",
    "contract_version",
    "application_version",
    "engine_version",
    "created_at",
    "practitioner",
    "publication_reason",
    "disclaimer",
    "source_package_digest",
    "profile_identity",
    "profile_version",
    "profile_digest",
    "mapping_digest",
    "validation_digest",
    "reconciliation_snapshot_hash",
    "candidate_id",
    "candidate_hash",
    "approval_id",
    "approval_hash",
    "semantic_publication_hash",
  ] as const;
  const optionalTextKeys = [
    "mapping_identity",
    "mapping_version",
    "validation_run_id",
    "reconciliation_snapshot_id",
    "approval_actor",
    "approval_reason",
    "approval_occurred_at",
  ] as const;
  const countKeys = [
    "included_valid_count",
    "included_corrected_count",
    "included_waived_count",
    "included_accepted_as_is_count",
    "excluded_count",
    "blocked_unresolved_count",
    "blocked_incomplete_count",
    "blocked_conflict_count",
    "total_accepted_rows",
  ] as const;
  const optionalCountKeys = ["governing_decision_count"] as const;
  const allowedKeys = [
    ...textKeys,
    ...optionalTextKeys,
    ...countKeys,
    ...optionalCountKeys,
    "member_inventory",
  ];
  return (
    [...textKeys, ...countKeys, "member_inventory"].every((key) =>
      Object.hasOwn(value, key),
    ) &&
    Object.keys(value).every((key) => allowedKeys.includes(key as never)) &&
    textKeys.every((key) => isText(value[key])) &&
    optionalTextKeys.every(
      (key) => value[key] === undefined || isText(value[key]),
    ) &&
    [
      "source_package_digest",
      "profile_digest",
      "mapping_digest",
      "validation_digest",
      "reconciliation_snapshot_hash",
      "candidate_hash",
      "approval_hash",
      "semantic_publication_hash",
    ].every((key) => isHash(value[key])) &&
    countKeys.every((key) => isCount(value[key])) &&
    optionalCountKeys.every(
      (key) => value[key] === undefined || isCount(value[key]),
    ) &&
    isBoundedArray(value.member_inventory, 32, isManifestMember)
  );
}

function isPublicationDetailShape(value: Record<string, unknown>): boolean {
  if (!(
    hasExactKeys(value, [
      "schema_version",
      "publication_id",
      "job_id",
      "candidate_id",
      "candidate_hash",
      "approval_id",
      "approval_hash",
      "contract_version",
      "status",
      "practitioner",
      "publication_reason",
      "semantic_publication_hash",
      "physical_archive_digest",
      "package_size_bytes",
      "member_count",
      "included_row_count",
      "excluded_row_count",
      "archive_relative_path",
      "manifest",
      "created_at",
      "completed_at",
    ]) &&
    isText(value.publication_id) &&
    isText(value.job_id) &&
    isText(value.candidate_id) &&
    isHash(value.candidate_hash) &&
    isText(value.approval_id) &&
    isHash(value.approval_hash) &&
    value.contract_version === "migration-stream/readiness-package/v1" &&
    isText(value.status) &&
    isText(value.practitioner) &&
    isText(value.publication_reason) &&
    isHash(value.semantic_publication_hash) &&
    isHash(value.physical_archive_digest) &&
    isCount(value.package_size_bytes) &&
    isCount(value.member_count) &&
    isCount(value.included_row_count) &&
    isCount(value.excluded_row_count) &&
    isText(value.archive_relative_path) &&
    isPublicationManifest(value.manifest) &&
    isText(value.created_at) &&
    isText(value.completed_at)
  )) {
    return false;
  }
  const manifest = value.manifest;
  const included =
    manifest.included_valid_count +
    manifest.included_corrected_count +
    manifest.included_waived_count +
    manifest.included_accepted_as_is_count;
  const blocked =
    manifest.blocked_unresolved_count +
    manifest.blocked_incomplete_count +
    manifest.blocked_conflict_count;
  return (
    value.publication_id === manifest.publication_id &&
    value.contract_version === manifest.contract_version &&
    value.candidate_id === manifest.candidate_id &&
    value.candidate_hash === manifest.candidate_hash &&
    value.approval_id === manifest.approval_id &&
    value.approval_hash === manifest.approval_hash &&
    value.practitioner === manifest.practitioner &&
    value.publication_reason === manifest.publication_reason &&
    value.semantic_publication_hash === manifest.semantic_publication_hash &&
    value.created_at === manifest.created_at &&
    value.included_row_count === included &&
    value.excluded_row_count === manifest.excluded_count &&
    manifest.total_accepted_rows ===
      included + manifest.excluded_count + blocked &&
    value.member_count === manifest.member_inventory.length &&
    new Set(
      manifest.member_inventory.map(
        (member: ManifestMember) => member.filename,
      ),
    ).size === manifest.member_inventory.length
  );
}

function hasRequiredContractShape(
  value: Record<string, unknown>,
  schema: SchemaName,
): boolean {
  switch (schema) {
    case SCHEMAS.context:
    case SCHEMAS.governedContext:
    case SCHEMAS.resultApprovalContext:
    case SCHEMAS.publicationContext:
      return isContextShape(value);
    case SCHEMAS.engagementContext:
      return isEngagementContextShape(value);
    case SCHEMAS.engagementRecord:
      return isEngagementRecordShape(value);
    case SCHEMAS.engagementState:
      return isEngagementStateShape(value);
    case SCHEMAS.engagementDetail:
      return isEngagementDetailShape(value);
    case SCHEMAS.sourcePreflight:
      return isSourcePreflightShape(value);
    case SCHEMAS.sourceAcceptance:
      return isSourceAcceptanceShape(value);
    case SCHEMAS.objectPacks:
      return isObjectPackListShape(value);
    case SCHEMAS.sourceProfileRules:
      return isSourceProfileRulesShape(value);
    case SCHEMAS.mappingWorkspace:
      return isMappingWorkspaceShape(value);
    case SCHEMAS.mappingContractCoverage:
      return isMappingContractCoverageShape(value);
    case SCHEMAS.mappingDraft:
      return isSavedMappingDraftShape(value);
    case SCHEMAS.submittedMapping:
      return isSubmittedMappingShape(value);
    case SCHEMAS.mappingApproval:
      return isMappingApprovalShape(value);
    case SCHEMAS.preparedMapping:
      return isPreparedMappingShape(value);
    case SCHEMAS.validationLaunch:
      return isValidationLaunchShape(value);
    case SCHEMAS.tourChain:
      return isTourChainShape(value);
    case SCHEMAS.comparedValidation:
      return isComparedValidationShape(value);
    case SCHEMAS.remediationPackOptions:
      return isRemediationPackOptionsShape(value);
    case SCHEMAS.remediationPackHistory:
      return isRemediationPackHistoryShape(value);
    case SCHEMAS.remediationPack:
      return isRemediationPackCommandResultShape(value);
    case SCHEMAS.journey:
      return isJourneyShape(value);
    case SCHEMAS.objectWorkspace:
      return isObjectWorkspaceRecordShape(value);
    case SCHEMAS.objectWorkspaceStatus:
      return isObjectWorkspaceStatusShape(value);
    case SCHEMAS.destinationWorkshop:
      return isDestinationWorkshopShape(value);
    case SCHEMAS.destinationCatalogueEntities:
      return isDestinationCatalogueEntityPageShape(value);
    case SCHEMAS.destinationCatalogueProperties:
      return isDestinationCataloguePropertyPageShape(value);
    case SCHEMAS.destinationArtifactReservation:
      return isDestinationArtifactReservationShape(value);
    case SCHEMAS.destinationArtifact:
      return isDestinationArtifactResultShape(value);
    case SCHEMAS.destinationDraft:
      return isDestinationDraftShape(value);
    case SCHEMAS.destinationReviewedProfile:
      return isDestinationReviewedProfileShape(value);
    case SCHEMAS.destinationProfileApproval:
      return isDestinationProfileApprovalShape(value);
    case SCHEMAS.destinationRebase:
      return isDestinationRebaseShape(value);
    case SCHEMAS.destinationCarryForward:
      return isDestinationCarryForwardShape(value);
    case SCHEMAS.job:
      return isJobShape(value);
    case SCHEMAS.files:
      return isFileShape(value);
    case SCHEMAS.profiling:
      return isProfilingShape(value);
    case SCHEMAS.profile:
      return isProfileShape(value);
    case SCHEMAS.mapping:
      return isMappingShape(value);
    case SCHEMAS.validation:
      return isValidationShape(value);
    case SCHEMAS.issues:
      return isIssuesShape(value);
    case SCHEMAS.row:
      return isRowShape(value);
    case SCHEMAS.reconciliation:
      return isReconciliationShape(value);
    case SCHEMAS.operations:
      return isOperationsShape(value);
    case SCHEMAS.decisionQueue:
      return isDecisionQueueShape(value);
    case SCHEMAS.decisionCounts:
      return isDecisionCountsShape(value);
    case SCHEMAS.decisionIssue:
      return isDecisionIssueShape(value);
    case SCHEMAS.decisionHistory:
      return isDecisionHistoryShape(value);
    case SCHEMAS.correction:
      return isCorrectionShape(value);
    case SCHEMAS.decisionResult:
    case SCHEMAS.decisionCommandResult:
      return isDecisionResultShape(value);
    case SCHEMAS.resultReconciliationSnapshots:
      return isResultSnapshotListShape(value);
    case SCHEMAS.resultReconciliationSnapshot:
      return isResultSnapshotShape(value);
    case SCHEMAS.resultReconciliationRows:
      return isResultRowsShape(value);
    case SCHEMAS.resultReconciliationRow:
      return isResultRowDetailShape(value);
    case SCHEMAS.resultCandidates:
      return isResultCandidateListShape(value);
    case SCHEMAS.resultEligibility:
      return isResultEligibilityShape(value);
    case SCHEMAS.resultActions:
      return isResultActionHistoryShape(value);
    case SCHEMAS.resultReconciliationCommand:
      return isResultReconciliationCommandShape(value);
    case SCHEMAS.resultCandidateCommand:
      return isResultCandidateCommandShape(value);
    case SCHEMAS.resultActionCommand:
      return isResultActionCommandShape(value);
    case SCHEMAS.publicationEligibility:
      return isPublicationEligibilityShape(value);
    case SCHEMAS.publicationList:
      return isPublicationListShape(value);
    case SCHEMAS.engagementReadinessPackages:
      return isEngagementReadinessPackageHistoryShape(value);
    case SCHEMAS.publicationDetail:
      return isPublicationDetailShape(value);
    case SCHEMAS.error:
      return false;
  }
}

export function decodeContract<T>(value: unknown, schema: SchemaName): T {
  if (!isRecord(value)) {
    throw new ContractError(
      "INVALID_RESPONSE",
      "The server response is not a JSON object.",
    );
  }
  const received =
    schema.startsWith("migration-stream/engagement/") ||
    schema.startsWith("migration-stream/object-workspace/") ||
    schema === SCHEMAS.journey
      ? value.schemaVersion
      : value.schema_version;
  if (received !== schema) {
    throw new ContractError(
      "SCHEMA_MISMATCH",
      `Expected ${schema}; received ${typeof received === "string" ? received : "no schema identifier"}.`,
    );
  }
  if (!hasRequiredContractShape(value, schema)) {
    throw new ContractError(
      "INVALID_RESPONSE",
      "The server response does not contain the required contract shape.",
    );
  }
  return value as T;
}

function hasOnlyKeys(
  value: Record<string, unknown>,
  allowed: readonly string[],
): boolean {
  return Object.keys(value).every((key) => allowed.includes(key));
}

function hasExactKeys(
  value: Record<string, unknown>,
  expected: readonly string[],
): boolean {
  return (
    Object.keys(value).length === expected.length &&
    expected.every((key) => Object.hasOwn(value, key))
  );
}

function isBoundedCommandText(
  value: unknown,
  maximum: number,
  allowEmpty = false,
  exactTrim = false,
): value is string {
  return (
    typeof value === "string" &&
    new TextEncoder().encode(value).byteLength <= maximum &&
    (allowEmpty || value.trim().length > 0) &&
    (!exactTrim || value.trim() === value) &&
    !hasControlCharacters(value)
  );
}

function hasControlCharacters(value: string): boolean {
  for (const character of value) {
    const code = character.codePointAt(0) ?? 0;
    if (code <= 0x1f || (code >= 0x7f && code <= 0x9f)) return true;
  }
  return false;
}

function isDecisionIdentity(value: unknown): value is string {
  return (
    typeof value === "string" &&
    value.length <= 256 &&
    /^[A-Za-z0-9][A-Za-z0-9._:-]*$/.test(value)
  );
}

function isSafeSourceFile(value: unknown): value is string {
  if (
    !isBoundedCommandText(value, 1024) ||
    value.startsWith("/") ||
    value.includes("\\")
  ) {
    return false;
  }
  return value.split("/").every((segment) => {
    if (
      segment.length === 0 ||
      segment === "." ||
      segment === ".." ||
      segment.trim() !== segment
    ) {
      return false;
    }
    const extension = segment.includes(".")
      ? segment.slice(segment.lastIndexOf(".") + 1).toLowerCase()
      : "";
    return extension !== "part" && extension !== "tombstone";
  });
}

export function validateDecisionCommand(value: unknown): DecisionCommand {
  if (!isRecord(value)) {
    throw new ContractError(
      "INVALID_DECISION_COMMAND",
      "The decision command is not a supported object.",
    );
  }
  const commonKeys = [
    "schema_version",
    "validation_run_id",
    "issue_hash",
    "practitioner_identifier",
    "reason",
    "evidence_reference",
    "expected_active_decision_id",
    "expected_active_generation",
    "acknowledge_not_migration_result_approval",
  ];
  const validHead =
    isCount(value.expected_active_generation) &&
    ((value.expected_active_generation === 0 &&
      value.expected_active_decision_id === null) ||
      (value.expected_active_generation > 0 &&
        isDecisionIdentity(value.expected_active_decision_id)));
  const common =
    isDecisionIdentity(value.validation_run_id) &&
    isHash(value.issue_hash) &&
    isBoundedCommandText(value.practitioner_identifier, 256, false, true) &&
    isBoundedCommandText(value.reason, 4096) &&
    (value.evidence_reference === null ||
      isBoundedCommandText(value.evidence_reference, 2048)) &&
    validHead &&
    value.acknowledge_not_migration_result_approval === true;
  if (!common) {
    throw new ContractError(
      "INVALID_DECISION_COMMAND",
      "The decision command is incomplete or exceeds its safe bounds.",
    );
  }

  if (value.schema_version === COMMAND_SCHEMAS.corrected) {
    const correction = value.correction;
    const validCorrection =
      isRecord(correction) &&
      hasOnlyKeys(correction, [
        "source_file",
        "source_ordinal",
        "entity_id",
        "field_id",
        "operation",
        "value",
        "expected_effective_value_hash",
      ]) &&
      isSafeSourceFile(correction.source_file) &&
      isCount(correction.source_ordinal) &&
      correction.source_ordinal > 0 &&
      isDecisionIdentity(correction.entity_id) &&
      isDecisionIdentity(correction.field_id) &&
      isOneOf(correction.operation, CORRECTION_OPERATIONS) &&
      isHash(correction.expected_effective_value_hash) &&
      ((correction.operation === "REPLACE_EFFECTIVE_TEXT" &&
        isBoundedCommandText(correction.value, 65_536)) ||
        (correction.operation === "SET_EFFECTIVE_EMPTY" &&
          correction.value === "") ||
        (correction.operation === "SET_EFFECTIVE_NULL" &&
          correction.value === null));
    if (validCorrection && hasOnlyKeys(value, [...commonKeys, "correction"])) {
      return value as unknown as CorrectIssueCommand;
    }
  } else if (
    value.schema_version === COMMAND_SCHEMAS.waived &&
    value.acknowledge_waiver_limitations === true &&
    hasOnlyKeys(value, [...commonKeys, "acknowledge_waiver_limitations"])
  ) {
    return value as unknown as WaiveIssueCommand;
  } else if (
    value.schema_version === COMMAND_SCHEMAS.acceptedAsIs &&
    isHash(value.expected_effective_value_hash) &&
    value.acknowledge_validation_truth_unchanged === true &&
    hasOnlyKeys(value, [
      ...commonKeys,
      "expected_effective_value_hash",
      "acknowledge_validation_truth_unchanged",
    ])
  ) {
    return value as unknown as AcceptIssueAsIsCommand;
  } else if (
    value.schema_version === COMMAND_SCHEMAS.excluded &&
    isSafeSourceFile(value.source_file) &&
    isCount(value.source_ordinal) &&
    value.source_ordinal > 0 &&
    value.acknowledge_row_scope === true &&
    hasOnlyKeys(value, [
      ...commonKeys,
      "source_file",
      "source_ordinal",
      "acknowledge_row_scope",
    ])
  ) {
    return value as unknown as ExcludeIssueRowCommand;
  }
  throw new ContractError(
    "UNSUPPORTED_DECISION_ACTION",
    "The decision action or payload is not in the closed supported vocabulary.",
  );
}

function isResultCode(value: unknown): value is string {
  return (
    typeof value === "string" &&
    value.length <= 256 &&
    /^[A-Z][A-Z0-9_:-]*$/.test(value)
  );
}

function isUniqueResultCodes(value: unknown): value is string[] {
  return (
    isBoundedArray(value, 200, isResultCode) &&
    new Set(value).size === value.length
  );
}

function isValidResultActionHead(value: Record<string, unknown>): boolean {
  return (
    isCount(value.expected_generation) &&
    ((value.expected_generation === 0 &&
      value.expected_active_action_id === null) ||
      (value.expected_generation > 0 &&
        isDecisionIdentity(value.expected_active_action_id)))
  );
}

export function validateResultCommand(value: unknown): ResultCommand {
  if (!isRecord(value)) {
    throw new ContractError(
      "INVALID_RESULT_COMMAND",
      "The migration-result command is not a supported object.",
    );
  }
  if (value.schema_version === RESULT_COMMAND_SCHEMAS.calculate) {
    if (
      hasExactKeys(value, ["schema_version", "actor", "reason"]) &&
      isBoundedCommandText(value.actor, 256, false, true) &&
      isBoundedCommandText(value.reason, 4096)
    ) {
      return value as unknown as CalculateReconciliationCommand;
    }
  } else if (value.schema_version === RESULT_COMMAND_SCHEMAS.createCandidate) {
    if (
      hasExactKeys(value, [
        "schema_version",
        "snapshot_id",
        "snapshot_hash",
        "creator",
        "preparation_reason",
      ]) &&
      isDecisionIdentity(value.snapshot_id) &&
      isHash(value.snapshot_hash) &&
      isBoundedCommandText(value.creator, 256, false, true) &&
      isBoundedCommandText(value.preparation_reason, 4096)
    ) {
      return value as unknown as CreateResultCandidateCommand;
    }
  } else {
    const baseKeys = [
      "schema_version",
      "candidate_id",
      "candidate_hash",
      "actor",
      "reason",
      "warning_acknowledgments",
      "acknowledge_local_only_boundary",
      "expected_active_action_id",
      "expected_generation",
    ];
    const base =
      isDecisionIdentity(value.candidate_id) &&
      isHash(value.candidate_hash) &&
      isBoundedCommandText(value.actor, 256, false, true) &&
      isBoundedCommandText(value.reason, 4096) &&
      isUniqueResultCodes(value.warning_acknowledgments) &&
      value.acknowledge_local_only_boundary === true &&
      isValidResultActionHead(value);
    if (
      base &&
      (value.schema_version === RESULT_COMMAND_SCHEMAS.approve ||
        value.schema_version === RESULT_COMMAND_SCHEMAS.reject ||
        value.schema_version === RESULT_COMMAND_SCHEMAS.revoke) &&
      hasExactKeys(value, baseKeys)
    ) {
      return value as unknown as
        | ApproveResultCandidateCommand
        | RejectResultCandidateCommand
        | RevokeResultApprovalCommand;
    }
    if (
      base &&
      value.schema_version === RESULT_COMMAND_SCHEMAS.supersede &&
      hasExactKeys(value, [
        ...baseKeys,
        "replacement_candidate_id",
        "replacement_candidate_hash",
      ]) &&
      isDecisionIdentity(value.replacement_candidate_id) &&
      value.replacement_candidate_id !== value.candidate_id &&
      isHash(value.replacement_candidate_hash)
    ) {
      return value as unknown as SupersedeResultApprovalCommand;
    }
  }
  throw new ContractError(
    "INVALID_RESULT_COMMAND",
    "The migration-result command is incomplete, outside the closed action vocabulary, or exceeds its safe bounds.",
  );
}

export function resultCommandResponseSchema(
  command: ResultCommand,
):
  | typeof SCHEMAS.resultReconciliationCommand
  | typeof SCHEMAS.resultCandidateCommand
  | typeof SCHEMAS.resultActionCommand {
  if (command.schema_version === RESULT_COMMAND_SCHEMAS.calculate) {
    return SCHEMAS.resultReconciliationCommand;
  }
  if (command.schema_version === RESULT_COMMAND_SCHEMAS.createCandidate) {
    return SCHEMAS.resultCandidateCommand;
  }
  return SCHEMAS.resultActionCommand;
}

function isIdempotencyKey(value: unknown): value is string {
  return (
    typeof value === "string" &&
    value.length >= 16 &&
    value.length <= 128 &&
    /^[A-Za-z0-9][A-Za-z0-9._:-]*$/.test(value)
  );
}

export function validateReserveSourcePreflightCommand(
  value: unknown,
): ReserveSourcePreflightCommand {
  if (
    isRecord(value) &&
    hasExactKeys(value, ["schemaVersion", "sourceName", "idempotencyKey"]) &&
    value.schemaVersion === ENGAGEMENT_COMMAND_SCHEMAS.reserveSourcePreflight &&
    isBoundedCommandText(value.sourceName, 255, false, true) &&
    value.sourceName.toLowerCase().endsWith(".zip") &&
    isIdempotencyKey(value.idempotencyKey)
  ) {
    return value as unknown as ReserveSourcePreflightCommand;
  }
  throw new ContractError(
    "INVALID_ENGAGEMENT_COMMAND",
    "The source preflight reservation is incomplete or outside its safe bounds.",
  );
}

export function validateObjectWorkspaceCommand(
  value: unknown,
): ObjectWorkspaceCommand {
  if (isObjectWorkspaceCommandShape(value)) {
    return value;
  }
  throw new ContractError(
    "INVALID_OBJECT_WORKSPACE_COMMAND",
    "The object workspace command is incomplete or outside its safe bounds.",
  );
}

function isManualPropertyDefinitionShape(
  value: unknown,
): value is ManualPropertyDefinition {
  return (
    isRecord(value) &&
    hasExactKeys(value, [
      "name",
      "edmType",
      "nullable",
      "maxLength",
      "keyCandidate",
    ]) &&
    isDecisionIdentity(value.name) &&
    isBoundedCommandText(value.edmType, 256, false, true) &&
    typeof value.nullable === "boolean" &&
    (value.maxLength === null ||
      (isCount(value.maxLength) &&
        value.maxLength > 0 &&
        value.maxLength <= 1_000_000)) &&
    typeof value.keyCandidate === "boolean"
  );
}

function isManualEntityDefinitionShape(
  value: unknown,
): value is ManualEntityDefinition {
  return (
    isRecord(value) &&
    hasExactKeys(value, ["namespace", "name", "properties"]) &&
    isDecisionIdentity(value.namespace) &&
    isDecisionIdentity(value.name) &&
    isBoundedArray(value.properties, 4096, isManualPropertyDefinitionShape) &&
    value.properties.length > 0 &&
    new Set(value.properties.map((property) => property.name)).size ===
      value.properties.length
  );
}

function isManualCatalogueEntitiesShape(
  value: unknown,
): value is ManualEntityDefinition[] {
  if (!isBoundedArray(value, 128, isManualEntityDefinitionShape)) {
    return false;
  }
  const technicalEntities = value.map(
    (entity) => `${entity.namespace}.${entity.name}`,
  );
  const totalProperties = value.reduce(
    (count, entity) => count + entity.properties.length,
    0,
  );
  return (
    value.length > 0 &&
    new Set(technicalEntities).size === technicalEntities.length &&
    totalProperties <= 4096
  );
}

export function validateDestinationCommand(value: unknown): DestinationCommand {
  if (
    !isRecord(value) ||
    !isIdempotencyKey(value.idempotencyKey) ||
    !isCount(value.expectedEngagementGeneration)
  ) {
    throw new ContractError(
      "INVALID_DESTINATION_COMMAND",
      "The destination command is incomplete or lacks exact generation and retry authority.",
    );
  }
  if (
    value.schemaVersion === DESTINATION_COMMAND_SCHEMAS.reserveEdmx &&
    hasExactKeys(value, [
      "schemaVersion",
      "expectedEngagementGeneration",
      "expectedWorkspaceId",
      "expectedWorkspaceDescriptorHash",
      "sourceName",
      "artifactLabel",
      "system",
      "clientOrTenant",
      "release",
      "idempotencyKey",
    ]) &&
    isCanonicalWorkspaceId(value.expectedWorkspaceId) &&
    isHash(value.expectedWorkspaceDescriptorHash) &&
    isBoundedCommandText(value.sourceName, 255, false, true) &&
    value.sourceName.toLowerCase().endsWith(".edmx") &&
    isBoundedCommandText(value.artifactLabel, 256, false, true) &&
    isNullableBoundedText(value.system, 512) &&
    isNullableBoundedText(value.clientOrTenant, 512) &&
    isNullableBoundedText(value.release, 512)
  ) {
    return value as unknown as ReserveEdmxCommand;
  }
  if (
    value.schemaVersion === DESTINATION_COMMAND_SCHEMAS.createManualCatalogue &&
    hasExactKeys(value, [
      "schemaVersion",
      "expectedEngagementGeneration",
      "artifactLabel",
      "actor",
      "reason",
      "entities",
      "idempotencyKey",
    ]) &&
    isBoundedCommandText(value.artifactLabel, 256, false, true) &&
    isBoundedCommandText(value.actor, 256, false, true) &&
    isBoundedCommandText(value.reason, 4096, false, true) &&
    isManualCatalogueEntitiesShape(value.entities)
  ) {
    return value as unknown as CreateManualCatalogueCommand;
  }
  if (
    value.schemaVersion === DESTINATION_COMMAND_SCHEMAS.startDraft &&
    hasExactKeys(value, [
      "schemaVersion",
      "expectedEngagementGeneration",
      "catalogueAuthorityId",
      "expectedCatalogueHash",
      "rootTechnicalEntity",
      "childTechnicalEntities",
      "profileId",
      "title",
      "owner",
      "actor",
      "reason",
      "idempotencyKey",
    ]) &&
    isDecisionIdentity(value.catalogueAuthorityId) &&
    isHash(value.expectedCatalogueHash) &&
    isDestinationTechnicalIdentity(value.rootTechnicalEntity) &&
    isBoundedArray(value.childTechnicalEntities, 127, (item): item is string =>
      isDestinationTechnicalIdentity(item),
    ) &&
    new Set(value.childTechnicalEntities).size ===
      value.childTechnicalEntities.length &&
    !value.childTechnicalEntities.includes(value.rootTechnicalEntity) &&
    isDestinationProfileId(value.profileId) &&
    isBoundedCommandText(value.title, 512, false, true) &&
    isBoundedCommandText(value.owner, 256, false, true) &&
    isBoundedCommandText(value.actor, 256, false, true) &&
    isBoundedCommandText(value.reason, 4096, false, true)
  ) {
    return value as unknown as StartDestinationDraftCommand;
  }
  if (
    value.schemaVersion === DESTINATION_COMMAND_SCHEMAS.saveDraft &&
    hasExactKeys(value, [
      "schemaVersion",
      "expectedEngagementGeneration",
      "draftId",
      "expectedDraftGeneration",
      "expectedDraftHash",
      "snapshot",
      "actor",
      "reason",
      "idempotencyKey",
    ]) &&
    isDecisionIdentity(value.draftId) &&
    isCount(value.expectedDraftGeneration) &&
    value.expectedDraftGeneration > 0 &&
    isHash(value.expectedDraftHash) &&
    isDestinationDraftSnapshotShape(value.snapshot) &&
    isBoundedCommandText(value.actor, 256, false, true) &&
    isBoundedCommandText(value.reason, 4096, false, true)
  ) {
    return value as unknown as SaveDestinationDraftCommand;
  }
  if (
    value.schemaVersion === DESTINATION_COMMAND_SCHEMAS.rebaseDraft &&
    hasExactKeys(value, [
      "schemaVersion",
      "expectedEngagementGeneration",
      "draftId",
      "expectedDraftGeneration",
      "expectedDraftHash",
      "targetCatalogueAuthorityId",
      "expectedTargetCatalogueHash",
      "actor",
      "reason",
      "idempotencyKey",
    ]) &&
    isDecisionIdentity(value.draftId) &&
    isCount(value.expectedDraftGeneration) &&
    value.expectedDraftGeneration > 0 &&
    isHash(value.expectedDraftHash) &&
    isDecisionIdentity(value.targetCatalogueAuthorityId) &&
    isHash(value.expectedTargetCatalogueHash) &&
    isBoundedCommandText(value.actor, 256, false, true) &&
    isBoundedCommandText(value.reason, 4096, false, true)
  ) {
    return value as unknown as RebaseDestinationDraftCommand;
  }
  if (
    value.schemaVersion === DESTINATION_COMMAND_SCHEMAS.freezeDraft &&
    hasExactKeys(value, [
      "schemaVersion",
      "expectedEngagementGeneration",
      "draftId",
      "expectedDraftGeneration",
      "expectedDraftHash",
      "profileVersion",
      "acknowledgement",
      "actor",
      "reason",
      "idempotencyKey",
    ]) &&
    isDecisionIdentity(value.draftId) &&
    isCount(value.expectedDraftGeneration) &&
    value.expectedDraftGeneration > 0 &&
    isHash(value.expectedDraftHash) &&
    isDestinationVersion(value.profileVersion) &&
    value.acknowledgement === DESTINATION_ACKNOWLEDGEMENTS.freeze &&
    isBoundedCommandText(value.actor, 256, false, true) &&
    isBoundedCommandText(value.reason, 4096, false, true)
  ) {
    return value as unknown as FreezeDestinationDraftCommand;
  }
  if (
    value.schemaVersion === DESTINATION_COMMAND_SCHEMAS.approveProfile &&
    hasExactKeys(value, [
      "schemaVersion",
      "expectedEngagementGeneration",
      "profileId",
      "profileVersion",
      "profileHash",
      "originBindingHash",
      "actor",
      "reason",
      "authorityAcknowledgement",
      "idempotencyKey",
    ]) &&
    isDestinationProfileId(value.profileId) &&
    isDestinationVersion(value.profileVersion) &&
    isHash(value.profileHash) &&
    isHash(value.originBindingHash) &&
    isBoundedCommandText(value.actor, 256, false, true) &&
    isBoundedCommandText(value.reason, 4096, false, true) &&
    value.authorityAcknowledgement ===
      DESTINATION_ACKNOWLEDGEMENTS.localMappingValidation
  ) {
    return value as unknown as ApproveDestinationProfileCommand;
  }
  if (
    value.schemaVersion === DESTINATION_COMMAND_SCHEMAS.carryForwardMapping &&
    hasExactKeys(value, [
      "schemaVersion",
      "expectedEngagementGeneration",
      "sourceMappingId",
      "sourceMappingVersion",
      "sourceMappingHash",
      "targetProfileId",
      "targetProfileVersion",
      "targetProfileHash",
      "actor",
      "reason",
      "idempotencyKey",
    ]) &&
    isDecisionIdentity(value.sourceMappingId) &&
    isDestinationVersion(value.sourceMappingVersion) &&
    isHash(value.sourceMappingHash) &&
    isDestinationProfileId(value.targetProfileId) &&
    isDestinationVersion(value.targetProfileVersion) &&
    isHash(value.targetProfileHash) &&
    isBoundedCommandText(value.actor, 256, false, true) &&
    isBoundedCommandText(value.reason, 4096, false, true)
  ) {
    return value as unknown as CarryForwardMappingCommand;
  }
  throw new ContractError(
    "INVALID_DESTINATION_COMMAND",
    "The destination command is incomplete or outside its safe bounds.",
  );
}

export function validateEngagementStateCommand(
  value: unknown,
): EngagementStateCommand {
  if (!isRecord(value) || !isIdempotencyKey(value.idempotencyKey)) {
    throw new ContractError(
      "INVALID_ENGAGEMENT_COMMAND",
      "The local engagement command is incomplete or has no stable retry identity.",
    );
  }
  if (
    value.schemaVersion === ENGAGEMENT_COMMAND_SCHEMAS.create &&
    hasExactKeys(value, ["schemaVersion", "displayName", "idempotencyKey"]) &&
    isBoundedCommandText(value.displayName, 256, false, true)
  ) {
    return value as unknown as CreateEngagementCommand;
  }
  if (
    value.schemaVersion === ENGAGEMENT_COMMAND_SCHEMAS.acceptSource &&
    hasExactKeys(value, [
      "schemaVersion",
      "preflightId",
      "expectedSourceHash",
      "expectedManifestHash",
      "idempotencyKey",
      "acknowledgement",
    ]) &&
    isDecisionIdentity(value.preflightId) &&
    isHash(value.expectedSourceHash) &&
    isHash(value.expectedManifestHash) &&
    value.acknowledgement === "migration-stream/source-acceptance/v1"
  ) {
    return value as unknown as AcceptSourceCommand;
  }
  if (
    value.schemaVersion === ENGAGEMENT_COMMAND_SCHEMAS.selectObjectPack &&
    hasExactKeys(value, ["schemaVersion", "objectPackId", "idempotencyKey"]) &&
    isDecisionIdentity(value.objectPackId)
  ) {
    return value as unknown as SelectObjectPackCommand;
  }
  if (
    value.schemaVersion ===
      ENGAGEMENT_COMMAND_SCHEMAS.approveObjectPackProfile &&
    hasExactKeys(value, [
      "schemaVersion",
      "actor",
      "reason",
      "authorityAcknowledgement",
      "idempotencyKey",
    ]) &&
    isBoundedCommandText(value.actor, 256, false, true) &&
    isBoundedCommandText(value.reason, 4096) &&
    value.authorityAcknowledgement ===
      "migration-stream/local-mapping-validation-authority/v1"
  ) {
    return value as unknown as ApproveObjectPackProfileCommand;
  }
  if (
    value.schemaVersion === ENGAGEMENT_COMMAND_SCHEMAS.prepareMapping &&
    hasExactKeys(value, [
      "schemaVersion",
      "idempotencyKey",
      "mappingId",
      "version",
      "selectedSuggestionIds",
      "author",
      "reviewer",
      "acknowledgement",
    ]) &&
    isDecisionIdentity(value.mappingId) &&
    isDecisionIdentity(value.version) &&
    isBoundedArray(value.selectedSuggestionIds, 8192, isDecisionIdentity) &&
    new Set(value.selectedSuggestionIds).size ===
      value.selectedSuggestionIds.length &&
    (value.author === null ||
      isBoundedCommandText(value.author, 256, false, true)) &&
    (value.reviewer === null ||
      isBoundedCommandText(value.reviewer, 256, false, true)) &&
    value.acknowledgement ===
      "migration-stream/local-assisted-mapping-reviewed-suggestions-no-target-compatibility/v1"
  ) {
    return value as unknown as PrepareMappingCommand;
  }
  if (
    value.schemaVersion === ENGAGEMENT_COMMAND_SCHEMAS.saveMappingDraft &&
    hasExactKeys(value, [
      "schemaVersion",
      "idempotencyKey",
      "expectedEngagementGeneration",
      "expectedDraftId",
      "expectedDraftGeneration",
      "expectedDraftHash",
      "snapshot",
    ]) &&
    isCount(value.expectedEngagementGeneration) &&
    ((value.expectedDraftId === null &&
      value.expectedDraftGeneration === null &&
      value.expectedDraftHash === null) ||
      (isDecisionIdentity(value.expectedDraftId) &&
        isCount(value.expectedDraftGeneration) &&
        value.expectedDraftGeneration > 0 &&
        isHash(value.expectedDraftHash))) &&
    isMappingDraftSnapshotShape(value.snapshot)
  ) {
    return value as unknown as SaveMappingDraftCommand;
  }
  if (
    value.schemaVersion === ENGAGEMENT_COMMAND_SCHEMAS.submitMappingDraft &&
    hasExactKeys(value, [
      "schemaVersion",
      "idempotencyKey",
      "expectedEngagementGeneration",
      "draftId",
      "expectedDraftGeneration",
      "expectedDraftHash",
      "mappingId",
      "version",
      "acknowledgement",
    ]) &&
    isCount(value.expectedEngagementGeneration) &&
    isDecisionIdentity(value.draftId) &&
    isCount(value.expectedDraftGeneration) &&
    value.expectedDraftGeneration > 0 &&
    isHash(value.expectedDraftHash) &&
    isDecisionIdentity(value.mappingId) &&
    isDecisionIdentity(value.version) &&
    value.acknowledgement ===
      "migration-stream/operator-governed-mapping-reviewed-and-frozen/v1"
  ) {
    return value as unknown as SubmitMappingDraftCommand;
  }
  if (
    value.schemaVersion === ENGAGEMENT_COMMAND_SCHEMAS.approveMappingVersion &&
    hasExactKeys(value, [
      "schemaVersion",
      "idempotencyKey",
      "expectedEngagementGeneration",
      "draftId",
      "expectedDraftGeneration",
      "expectedDraftHash",
      "expectedMappingId",
      "expectedMappingVersion",
      "expectedMappingHash",
      "actor",
      "reason",
      "acknowledgement",
    ]) &&
    isCount(value.expectedEngagementGeneration) &&
    isDecisionIdentity(value.draftId) &&
    isCount(value.expectedDraftGeneration) &&
    value.expectedDraftGeneration > 0 &&
    isHash(value.expectedDraftHash) &&
    isDecisionIdentity(value.expectedMappingId) &&
    isDecisionIdentity(value.expectedMappingVersion) &&
    isHash(value.expectedMappingHash) &&
    isBoundedCommandText(value.actor, 256, false, true) &&
    isBoundedCommandText(value.reason, 4096, false, true) &&
    value.acknowledgement ===
      "migration-stream/local-mapping-version-approval/v1"
  ) {
    return value as unknown as ApproveMappingVersionCommand;
  }
  if (
    value.schemaVersion === ENGAGEMENT_COMMAND_SCHEMAS.launchValidation &&
    hasExactKeys(value, [
      "schemaVersion",
      "idempotencyKey",
      "expectedEngagementGeneration",
      "expectedJobId",
      "expectedMappingId",
      "expectedMappingVersion",
      "expectedMappingHash",
    ]) &&
    isCount(value.expectedEngagementGeneration) &&
    isDecisionIdentity(value.expectedJobId) &&
    isDecisionIdentity(value.expectedMappingId) &&
    isDecisionIdentity(value.expectedMappingVersion) &&
    isHash(value.expectedMappingHash)
  ) {
    return value as unknown as LaunchValidationCommand;
  }
  throw new ContractError(
    "INVALID_ENGAGEMENT_COMMAND",
    "The local engagement command is incomplete, outside the closed vocabulary, or exceeds its safe bounds.",
  );
}

export function validateRevisedTourCommand(value: unknown): RevisedTourCommand {
  if (
    !isRecord(value) ||
    !isIdempotencyKey(value.idempotencyKey) ||
    !isCount(value.expectedEngagementGeneration)
  ) {
    throw new ContractError(
      "INVALID_REVISED_TOUR_COMMAND",
      "The revised-validation command is incomplete or has no stable generation and retry identity.",
    );
  }
  if (
    value.schemaVersion === ENGAGEMENT_COMMAND_SCHEMAS.startRevisedTour &&
    hasExactKeys(value, [
      "schemaVersion",
      "idempotencyKey",
      "expectedEngagementGeneration",
      "actor",
      "reason",
      "acknowledgement",
    ]) &&
    isBoundedCommandText(value.actor, 256, false, true) &&
    isBoundedCommandText(value.reason, 4096, false, true) &&
    value.acknowledgement === REVISED_SOURCE_ACKNOWLEDGEMENT
  ) {
    return value as unknown as StartRevisedTourCommand;
  }
  if (
    value.schemaVersion ===
      ENGAGEMENT_COMMAND_SCHEMAS.reserveRevisedSourcePreflight &&
    hasExactKeys(value, [
      "schemaVersion",
      "idempotencyKey",
      "expectedEngagementGeneration",
      "tourAttemptId",
      "sourceName",
    ]) &&
    isDecisionIdentity(value.tourAttemptId) &&
    isBoundedCommandText(value.sourceName, 255, false, true) &&
    value.sourceName.toLowerCase().endsWith(".zip")
  ) {
    return value as unknown as ReserveRevisedSourcePreflightCommand;
  }
  if (
    value.schemaVersion === ENGAGEMENT_COMMAND_SCHEMAS.acceptRevisedSource &&
    hasExactKeys(value, [
      "schemaVersion",
      "idempotencyKey",
      "expectedEngagementGeneration",
      "tourAttemptId",
      "preflightId",
      "expectedSourceHash",
      "expectedManifestHash",
      "actor",
      "reason",
      "acknowledgement",
    ]) &&
    isDecisionIdentity(value.tourAttemptId) &&
    isDecisionIdentity(value.preflightId) &&
    isHash(value.expectedSourceHash) &&
    isHash(value.expectedManifestHash) &&
    isBoundedCommandText(value.actor, 256, false, true) &&
    isBoundedCommandText(value.reason, 4096, false, true) &&
    value.acknowledgement === REVISED_SOURCE_ACKNOWLEDGEMENT
  ) {
    return value as unknown as AcceptRevisedSourceCommand;
  }
  if (
    value.schemaVersion ===
      ENGAGEMENT_COMMAND_SCHEMAS.launchRevisedValidation &&
    hasExactKeys(value, [
      "schemaVersion",
      "idempotencyKey",
      "expectedEngagementGeneration",
      "tourAttemptId",
      "expectedReuseAuthorityId",
      "expectedReuseAuthorityHash",
      "actor",
      "reason",
      "acknowledgement",
    ]) &&
    isDecisionIdentity(value.tourAttemptId) &&
    isDecisionIdentity(value.expectedReuseAuthorityId) &&
    isHash(value.expectedReuseAuthorityHash) &&
    isBoundedCommandText(value.actor, 256, false, true) &&
    isBoundedCommandText(value.reason, 4096, false, true) &&
    value.acknowledgement === REVISED_SOURCE_ACKNOWLEDGEMENT
  ) {
    return value as unknown as LaunchRevisedValidationCommand;
  }
  throw new ContractError(
    "INVALID_REVISED_TOUR_COMMAND",
    "The revised-validation command is outside the closed local contract.",
  );
}

export function validateCreateRemediationPackCommand(
  value: unknown,
): CreateRemediationPackCommand {
  if (
    !isRecord(value) ||
    value.schemaVersion !== ENGAGEMENT_COMMAND_SCHEMAS.createRemediationPack ||
    !hasExactKeys(value, [
      "schemaVersion",
      "expectedEngagementGeneration",
      "expectedValidationRunId",
      "expectedOptionsHash",
      "selectedKeyColumns",
      "actor",
      "reason",
      "localDataExportAcknowledgement",
      "includeOffendingValues",
      "offendingValuesAcknowledgement",
      "idempotencyKey",
    ]) ||
    !isCount(value.expectedEngagementGeneration) ||
    !isDecisionIdentity(value.expectedValidationRunId) ||
    !isHash(value.expectedOptionsHash) ||
    !isBoundedArray(
      value.selectedKeyColumns,
      4096,
      isRemediationKeyColumnShape,
    ) ||
    value.selectedKeyColumns.length === 0 ||
    !isBoundedCommandText(value.actor, 256, false, true) ||
    !isBoundedCommandText(value.reason, 4096, false, true) ||
    value.localDataExportAcknowledgement !==
      REMEDIATION_LOCAL_EXPORT_ACKNOWLEDGEMENT ||
    typeof value.includeOffendingValues !== "boolean" ||
    !isIdempotencyKey(value.idempotencyKey)
  ) {
    throw new ContractError(
      "INVALID_REMEDIATION_PACK_COMMAND",
      "The remediation-pack command is incomplete or outside the bounded local export contract.",
    );
  }
  if (
    (value.includeOffendingValues &&
      value.offendingValuesAcknowledgement !==
        REMEDIATION_OFFENDING_VALUES_ACKNOWLEDGEMENT) ||
    (!value.includeOffendingValues &&
      value.offendingValuesAcknowledgement !== null)
  ) {
    throw new ContractError(
      "INVALID_REMEDIATION_PACK_COMMAND",
      "Offending-value inclusion requires its separate exact local export acknowledgement.",
    );
  }
  const selected = value.selectedKeyColumns;
  if (
    new Set(
      selected.map(
        (column) =>
          `${column.entityId}:${column.sourceFile}:${column.sourceColumnOrdinal}:${column.sourceColumn}`,
      ),
    ).size !== selected.length
  ) {
    throw new ContractError(
      "INVALID_REMEDIATION_PACK_COMMAND",
      "Selected remediation key columns must be unique exact source-column identities.",
    );
  }
  return value as unknown as CreateRemediationPackCommand;
}

export function decodeRevisedTourCommandResult(
  command: RevisedTourCommand,
  value: unknown,
):
  | TourChain
  | SourcePreflightDocument
  | AcceptRevisedSourceResult
  | LaunchRevisedValidationResult {
  switch (command.schemaVersion) {
    case ENGAGEMENT_COMMAND_SCHEMAS.startRevisedTour:
      return decodeContract<TourChain>(value, SCHEMAS.tourChain);
    case ENGAGEMENT_COMMAND_SCHEMAS.reserveRevisedSourcePreflight:
      return decodeSourcePreflightReservation(value);
    case ENGAGEMENT_COMMAND_SCHEMAS.acceptRevisedSource:
      if (isAcceptRevisedSourceResultShape(value)) return value;
      break;
    case ENGAGEMENT_COMMAND_SCHEMAS.launchRevisedValidation:
      if (isLaunchRevisedValidationResultShape(value)) return value;
      break;
  }
  throw new ContractError(
    "INVALID_RESPONSE",
    "The revised-validation result does not match its closed local contract.",
  );
}

export function decodeApplicationContext(value: unknown): ApplicationContext {
  const schema = isRecord(value)
    ? (value.schemaVersion ?? value.schema_version)
    : null;
  if (schema === SCHEMAS.engagementContext) {
    try {
      return decodeContract<EngagementContext>(
        value,
        SCHEMAS.engagementContext,
      );
    } catch (error) {
      if (error instanceof ContractError && error.code === "INVALID_RESPONSE") {
        throw new ContractError(
          "INVALID_CONTEXT",
          "The engagement startup context is incomplete or grants unsafe authority.",
        );
      }
      throw error;
    }
  }
  return decodeContext(value);
}

export function isEngagementContext(
  context: ApplicationContext,
): context is EngagementContext {
  return (
    "schemaVersion" in context &&
    context.schemaVersion === SCHEMAS.engagementContext
  );
}

export function decodeContext(value: unknown): WorkbenchContext {
  try {
    const schema = isRecord(value) ? value.schema_version : null;
    if (schema === SCHEMAS.context) {
      return decodeContract<ReadOnlyWorkbenchContext>(value, SCHEMAS.context);
    }
    if (schema === SCHEMAS.governedContext) {
      return decodeContract<GovernedWorkbenchContext>(
        value,
        SCHEMAS.governedContext,
      );
    }
    if (schema === SCHEMAS.resultApprovalContext) {
      return decodeContract<ResultApprovalWorkbenchContext>(
        value,
        SCHEMAS.resultApprovalContext,
      );
    }
    if (schema === SCHEMAS.publicationContext) {
      return decodeContract<PublicationWorkbenchContext>(
        value,
        SCHEMAS.publicationContext,
      );
    }
    throw new ContractError(
      "SCHEMA_MISMATCH",
      "The startup context does not use a supported capability contract.",
    );
  } catch (error) {
    if (error instanceof ContractError && error.code === "INVALID_RESPONSE") {
      throw new ContractError(
        "INVALID_CONTEXT",
        "The startup context is incomplete, has unsafe capabilities, or is not explicitly read-only.",
      );
    }
    throw error;
  }
}

export function decodeHttpError(value: unknown): HttpErrorEnvelope | null {
  if (!isRecord(value) || value.schema_version !== SCHEMAS.error) return null;
  const error = value.error;
  if (
    value.ok !== false ||
    !isRecord(error) ||
    typeof error.code !== "string" ||
    typeof error.message !== "string" ||
    typeof error.request_id !== "string" ||
    typeof error.retryable !== "boolean"
  ) {
    return null;
  }
  return value as unknown as HttpErrorEnvelope;
}
