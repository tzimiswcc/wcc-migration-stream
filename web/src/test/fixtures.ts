import { COMMAND_SCHEMAS, SCHEMAS } from "../contracts";
import type {
  ObjectWorkspaceRecord,
  ObjectWorkspaceStatus,
} from "../contracts";

const hash = (character: string) => character.repeat(64);
const workbenchSchemas = Object.values(SCHEMAS).filter(
  (schema) => !schema.startsWith("migration-stream/engagement/"),
);

export const contextFixture = {
  schema_version: SCHEMAS.context,
  read_only: true,
  workspace_label: "Synthetic readiness workspace",
  job_id: "job-synthetic-004",
  profiling_run_id: "profiling-synthetic-004",
  selected_governance: {
    profile_id: "synthetic.object",
    profile_version: "1.0.0",
    profile_hash: hash("a"),
    mapping_id: "mapping-synthetic-004",
    mapping_version: "1",
    mapping_hash: hash("b"),
    validation_run_id: "validation-synthetic-004",
    validation_state: "COMPLETE",
  },
  supported_contracts: Object.values(SCHEMAS).filter((schema) =>
    schema.startsWith("migration-stream/read/"),
  ),
};

export const objectWorkspaceRecordFixture = {
  schemaVersion: SCHEMAS.objectWorkspace,
  workspaceId: "d795cba1-2a9c-4c1e-9343-984875fa38c2",
  generation: 1,
  displayName: "Synthetic readiness workspace",
  migrationObjectNameOrScope: "Synthetic object readiness",
  creatorAttribution: "Synthetic local operator",
  establishedBy: "INITIALIZED",
  createdAt: "2026-07-29T08:00:00.000Z",
  descriptorHash:
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
} satisfies ObjectWorkspaceRecord;

export const activeObjectWorkspaceFixture = {
  schemaVersion: SCHEMAS.objectWorkspaceStatus,
  state: "ACTIVE",
  stateHash: hash("e"),
  requiredAction: null,
  unavailableReason: null,
  pendingCommand: null,
  workspace: objectWorkspaceRecordFixture,
} satisfies ObjectWorkspaceStatus;

export const initializeRequiredObjectWorkspaceFixture = {
  schemaVersion: SCHEMAS.objectWorkspaceStatus,
  state: "INITIALIZE_REQUIRED",
  stateHash: hash("f"),
  requiredAction: "INITIALIZE",
  unavailableReason:
    "Initialize this workspace before accepting source evidence.",
  pendingCommand: null,
  workspace: null,
} satisfies ObjectWorkspaceStatus;

export const adoptRequiredObjectWorkspaceFixture = {
  schemaVersion: SCHEMAS.objectWorkspaceStatus,
  state: "ADOPT_REQUIRED",
  stateHash: hash("0"),
  requiredAction: "ADOPT",
  unavailableReason: "Adopt this existing local evidence before continuing.",
  pendingCommand: null,
  workspace: null,
} satisfies ObjectWorkspaceStatus;

export const journeyFixture = {
  schemaVersion: SCHEMAS.journey,
  currentStageId: "ISSUES",
  workspace: activeObjectWorkspaceFixture,
  binding: {
    engagementId: null,
    engagementGeneration: null,
    jobId: "job-synthetic-004",
    sourceHash: hash("1"),
    manifestHash: hash("2"),
    profilingRunId: "profiling-synthetic-004",
    objectPackId: "synthetic.object",
    destinationOrigin: "CONTROLLED_PACK",
    destinationAuthorityId: "synthetic.object",
    destinationOriginHash: hash("a"),
    profileId: "synthetic.object",
    profileVersion: "1.0.0",
    profileHash: hash("a"),
    mappingId: "mapping-synthetic-004",
    mappingVersion: "1",
    mappingHash: hash("b"),
    validationRunId: "validation-synthetic-004",
    validationResultHash: hash("c"),
    activeTourId: null,
    activeTourNumber: null,
    activeTourHash: null,
    comparisonId: null,
    comparisonHash: null,
    entityCount: 1,
  },
  stages: [
    {
      id: "SETUP",
      label: "Setup / Workspace",
      status: "COMPLETED",
      available: true,
      href: "/",
      unavailableReason: null,
    },
    {
      id: "DESTINATION",
      label: "Destination",
      status: "COMPLETED",
      available: true,
      href: "/profile",
      unavailableReason: null,
    },
    {
      id: "SOURCE",
      label: "Source",
      status: "COMPLETED",
      available: true,
      href: "/files",
      unavailableReason: null,
    },
    {
      id: "MAPPING",
      label: "Mapping",
      status: "COMPLETED",
      available: true,
      href: "/mapping",
      unavailableReason: null,
    },
    {
      id: "VALIDATE",
      label: "Validate",
      status: "COMPLETED",
      available: true,
      href: "/validation",
      unavailableReason: null,
    },
    {
      id: "COMPARE",
      label: "Compare",
      status: "LOCKED",
      available: false,
      href: null,
      unavailableReason:
        "Compared validation is available after a compatible revised source is validated.",
    },
    {
      id: "ISSUES",
      label: "Issues",
      status: "CURRENT",
      available: true,
      href: "/issues",
      unavailableReason: null,
    },
    {
      id: "DECISIONS",
      label: "Decisions",
      status: "LOCKED",
      available: false,
      href: null,
      unavailableReason:
        "Governed issue decisions are unavailable in this read-only process.",
    },
    {
      id: "RECONCILIATION",
      label: "Reconciliation",
      status: "AVAILABLE",
      available: true,
      href: "/reconciliation",
      unavailableReason: null,
    },
    {
      id: "PACKAGE",
      label: "Package",
      status: "LOCKED",
      available: false,
      href: null,
      unavailableReason:
        "A readiness package requires eligible approved local result evidence.",
    },
  ],
};

export const governedContextFixture = {
  ...contextFixture,
  schema_version: SCHEMAS.governedContext,
  read_only: false,
  capabilities: {
    governed_issue_decisions: true,
    correction_overlays: true,
    authenticated_practitioner_identity: false,
    source_mutation: false,
    original_projection_mutation: false,
    profile_mutation: false,
    mapping_mutation: false,
    migration_result_approval: false,
    target_write: false,
    publication: false,
  },
  supported_contracts: workbenchSchemas,
};

export const publicationContextFixture = {
  ...governedContextFixture,
  schema_version: SCHEMAS.publicationContext,
  capabilities: {
    ...governedContextFixture.capabilities,
    migration_result_approval: true,
    publication: true,
  },
  supported_contracts: [
    ...workbenchSchemas,
    COMMAND_SCHEMAS.publish,
    "migration-stream/readiness-package/v1",
  ],
};

export const engagementStateFixture = {
  schemaVersion: SCHEMAS.engagementState,
  engagementId: "engagement-tour-001",
  displayName: "Tour 1 migration readiness",
  phase: "AWAITING_SOURCE",
  generation: 1,
  active: {
    preflightId: null,
    jobId: null,
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
  },
  profileStage: "NOT_SELECTED",
  mappingStage: "NOT_PREPARED",
  validationStage: "NOT_LAUNCHED",
  updatedAt: "2026-07-28T10:00:00Z",
};

export const engagementContextFixture = {
  schemaVersion: SCHEMAS.engagementContext,
  workspaceLabel: "Tour 1 local workspace",
  engagement: null,
  supportedContracts: [
    SCHEMAS.engagementContext,
    SCHEMAS.engagementDetail,
    SCHEMAS.sourcePreflight,
    SCHEMAS.objectPacks,
    SCHEMAS.sourceProfileRules,
    SCHEMAS.profiling,
    SCHEMAS.mapping,
    SCHEMAS.validation,
  ],
  capabilities: {
    engagementSetup: true,
    sourceIntake: true,
    textualProfiling: true,
    reviewedObjectPackSelection: true,
    explicitProfileApproval: true,
    mappingPreparation: true,
    validationLaunch: true,
    governedIssueDecisions: false,
    migrationResultApproval: false,
    targetWrite: false,
    publication: false,
  },
};

export const inactiveEngagementContextFixture = {
  ...engagementContextFixture,
  workspaceLabel: "Object workspace identity pending",
  capabilities: {
    engagementSetup: false,
    sourceIntake: false,
    textualProfiling: false,
    reviewedObjectPackSelection: false,
    explicitProfileApproval: false,
    mappingPreparation: false,
    validationLaunch: false,
    governedIssueDecisions: false,
    migrationResultApproval: false,
    targetWrite: false,
    publication: false,
  },
};

export const receivingPreflightFixture = {
  schemaVersion: SCHEMAS.sourcePreflight,
  preflightId: "preflight-tour-001",
  state: "RECEIVING",
  sourceName: "tour-1-source.zip",
  sourceHash: null,
  sourceSize: null,
  policyHash: hash("1"),
  manifestHash: null,
  manifest: null,
  errorCode: null,
  errorMessage: null,
  createdAt: "2026-07-28T10:01:00Z",
  updatedAt: "2026-07-28T10:01:00Z",
  completedAt: null,
};

export const readyPreflightFixture = {
  ...receivingPreflightFixture,
  state: "READY",
  sourceHash: hash("2"),
  sourceSize: 128,
  manifestHash: hash("3"),
  manifest: {
    entries: [
      {
        path: "objects/tour.csv",
        is_directory: false,
        is_csv: true,
        compressed_size: 64,
        expanded_size: 128,
        compression_ratio: 2,
        crc32: 123456,
      },
    ],
    csv_paths: ["objects/tour.csv"],
    total_compressed_bytes: 64,
    total_expanded_bytes: 128,
    maximum_observed_compression_ratio: 2,
  },
  updatedAt: "2026-07-28T10:02:00Z",
  completedAt: "2026-07-28T10:02:00Z",
};

export const engagementDetailFixture = {
  schemaVersion: SCHEMAS.engagementDetail,
  state: engagementStateFixture,
  preflights: [],
  availableObjectPackCount: 1,
};

export const engagementObjectPackFixture = {
  objectPackId: "tour-1.profile",
  version: "1.0.0",
  title: "Tour 1 reviewed object pack",
  description:
    "Controlled synthetic proof material for the presenter showcase.",
  scope: "Synthetic source-to-canonical readiness proof only",
  rootEntityId: "TourObject",
  mappingMode: "CONTROLLED_SUGGESTIONS",
  limitations: [
    "No production business semantics.",
    "No target-system compatibility or load claim.",
  ],
  mappingSuggestions: [
    {
      suggestionId: "suggestion.object-key",
      targetFieldId: "object_key",
      sourceFile: "objects/tour.csv",
      sourceColumn: "object_key",
      sourceColumnOrdinal: 0,
      kind: "direct",
      constant: null,
      lookupTableId: null,
    },
    {
      suggestionId: "suggestion.classification",
      targetFieldId: "classification",
      sourceFile: null,
      sourceColumn: null,
      sourceColumnOrdinal: null,
      kind: "constant",
      constant: "SYNTHETIC",
      lookupTableId: null,
    },
  ],
  referenceTables: [
    {
      tableId: "tour-codes",
      version: "1.0.0",
      values: { A: "Active" },
    },
  ],
};

export const engagementObjectPacksFixture = {
  schemaVersion: SCHEMAS.objectPacks,
  items: [engagementObjectPackFixture],
};

export const engagementProfileVersionFixture = {
  schema_version: 1,
  profile_id: "tour-1.profile",
  profile_version: "1.0.0",
  profile_hash: hash("4"),
  schema_hash: hash("5"),
  validation_hash: hash("6"),
  declared_status: "REVIEWED_DRAFT",
  lifecycle_status: "REVIEWED_DRAFT",
  authority: "NONE",
  can_start_new_runs: false,
  registered_at: "2026-07-28T10:03:00Z",
  approval: null,
  lifecycle_events: [
    {
      sequence: 1,
      from_state: null,
      to_state: "REVIEWED_DRAFT",
      actor: "controlled-object-pack",
      reason: "Registered reviewed proof material without approval.",
      previous_event_hash: null,
      event_hash: hash("7"),
      occurred_at: "2026-07-28T10:03:00Z",
    },
  ],
};

export const sourceProfileRulesFixture = {
  schemaVersion: SCHEMAS.sourceProfileRules,
  jobId: "job-tour-001",
  profilingRunId: "profiling-tour-001",
  storedSampleValues: false,
  sourceColumns: [
    {
      sourceFile: "objects/tour.csv",
      sourceColumn: "object_key",
      sourceColumnOrdinal: 0,
    },
  ],
  profileFields: [
    {
      entityId: "TourObject",
      fieldId: "object_key",
      label: "Object key",
      logicalType: "identifier",
      required: true,
      sensitive: false,
    },
    {
      entityId: "TourObject",
      fieldId: "classification",
      label: "Classification",
      logicalType: "code",
      required: false,
      sensitive: false,
    },
  ],
  rules: [
    {
      ruleId: "required.object-key",
      severity: "error",
      kind: "required",
      target: "TourObject.object_key",
      message: "Object key is required.",
      parameters: {},
    },
  ],
};

export const sampleFreeProfilingFixture = {
  schema_version: SCHEMAS.profiling,
  scope: {
    run_id: "profiling-tour-001",
    job_id: "job-tour-001",
    result_hash: hash("8"),
  },
  include_samples: false,
  items: [
    {
      path: "objects/tour.csv",
      column_ordinal: 0,
      column_name: "object_key",
      empty_count: 0,
      nonempty_count: 2,
      min_decoded_length: 6,
      max_decoded_length: 8,
      oversized_count: 0,
      distinct_status: "EXACT",
      distinct_count: 2,
      sample_count: 0,
    },
  ],
  page: { page_size: 200, returned: 1, next_cursor: null },
  canonical_hash: hash("b"),
};

export const engagementProfiledStateFixture = {
  ...engagementStateFixture,
  phase: "AWAITING_OBJECT_PACK",
  generation: 4,
  active: {
    ...engagementStateFixture.active,
    preflightId: readyPreflightFixture.preflightId,
    jobId: "job-tour-001",
    profilingRunId: "profiling-tour-001",
  },
  updatedAt: "2026-07-28T10:04:00Z",
};

export const engagementProfiledDetailFixture = {
  ...engagementDetailFixture,
  state: engagementProfiledStateFixture,
  preflights: [readyPreflightFixture],
};

export const engagementSelectedStateFixture = {
  ...engagementProfiledStateFixture,
  phase: "AWAITING_PROFILE_APPROVAL",
  generation: 5,
  active: {
    ...engagementProfiledStateFixture.active,
    objectPackId: engagementObjectPackFixture.objectPackId,
    destinationOrigin: "CONTROLLED_PACK",
    destinationAuthorityId: engagementObjectPackFixture.objectPackId,
    destinationOriginHash: engagementProfileVersionFixture.profile_hash,
    profileId: engagementProfileVersionFixture.profile_id,
    profileVersion: engagementProfileVersionFixture.profile_version,
    profileHash: engagementProfileVersionFixture.profile_hash,
  },
  profileStage: "REVIEWED",
  updatedAt: "2026-07-28T10:05:00Z",
};

export const engagementSelectedDetailFixture = {
  ...engagementProfiledDetailFixture,
  state: engagementSelectedStateFixture,
};

export const engagementApprovedStateFixture = {
  ...engagementSelectedStateFixture,
  phase: "AWAITING_MAPPING",
  generation: 6,
  profileStage: "APPROVED",
  updatedAt: "2026-07-28T10:06:00Z",
};

export const engagementApprovedDetailFixture = {
  ...engagementSelectedDetailFixture,
  state: engagementApprovedStateFixture,
};

export const engagementMappingReadyStateFixture = {
  ...engagementApprovedStateFixture,
  phase: "READY_TO_VALIDATE",
  generation: 7,
  active: {
    ...engagementApprovedStateFixture.active,
    mappingId: "engagement.mapping",
    mappingVersion: "1.0.0",
    mappingHash: hash("9"),
  },
  mappingStage: "VALIDATED",
  updatedAt: "2026-07-28T10:07:00Z",
};

export const engagementMappingReadyDetailFixture = {
  ...engagementApprovedDetailFixture,
  state: engagementMappingReadyStateFixture,
};

export const engagementValidationCompleteStateFixture = {
  ...engagementMappingReadyStateFixture,
  phase: "FIRST_ISSUES_READY",
  generation: 8,
  active: {
    ...engagementMappingReadyStateFixture.active,
    validationRunId: "validation-tour-001",
  },
  validationStage: "COMPLETE",
  updatedAt: "2026-07-28T10:08:00Z",
};

export const engagementValidationCompleteDetailFixture = {
  ...engagementMappingReadyDetailFixture,
  state: engagementValidationCompleteStateFixture,
};

export const engagementMappingFixture = {
  schema_version: SCHEMAS.mapping,
  scope: {
    mapping_id: "engagement.mapping",
    mapping_version: "1.0.0",
    mapping_hash: hash("9"),
  },
  mapping: {
    mapping_id: "engagement.mapping",
    mapping_version: "1.0.0",
    mapping_hash: hash("9"),
    profile_id: engagementProfileVersionFixture.profile_id,
    profile_version: engagementProfileVersionFixture.profile_version,
    profile_hash: engagementProfileVersionFixture.profile_hash,
    source_hash: readyPreflightFixture.sourceHash,
    validated_job_id: "job-tour-001",
  },
  items: [
    {
      kind: "MAPPED",
      mapping_kind: "DIRECT",
      source_file: "objects/tour.csv",
      source_column_ordinal: 0,
      source_column_name: "object_key",
      entity_id: "TourObject",
      field_id: "object_key",
      binding_id: "binding-tour-object-key",
      issue_code: null,
      detail: null,
    },
  ],
  page: { page_size: 200, returned: 1, next_cursor: null },
  canonical_hash: hash("a"),
};

export const engagementValidationFixture = {
  schema_version: SCHEMAS.validation,
  data: {
    run_id: "validation-tour-001",
    identity_hash: hash("c"),
    job_id: "job-tour-001",
    source_hash: readyPreflightFixture.sourceHash,
    profile_id: engagementProfileVersionFixture.profile_id,
    profile_version: engagementProfileVersionFixture.profile_version,
    profile_hash: engagementProfileVersionFixture.profile_hash,
    mapping_id: "engagement.mapping",
    mapping_version: "1.0.0",
    mapping_hash: hash("9"),
    rule_set_hash: hash("d"),
    engine_version: "migration-stream-validation/v1",
    policy_hash: hash("e"),
    state: "COMPLETE",
    result_hash: hash("f"),
    issue_count: 2,
    rows_processed: 2,
    error_code: null,
    error_message: null,
    timestamps: {
      created_at: "2026-07-28T10:07:00Z",
      updated_at: "2026-07-28T10:08:00Z",
      completed_at: "2026-07-28T10:08:00Z",
    },
    issue_breakdowns: {
      by_severity: {
        items: [{ key: "ERROR", count: 2 }],
        truncated: false,
      },
      by_rule: { items: [], truncated: false },
      by_entity: { items: [], truncated: false },
      by_field: { items: [], truncated: false },
      by_source_file: { items: [], truncated: false },
    },
  },
  canonical_hash: hash("0"),
};

export const jobFixture = {
  schema_version: SCHEMAS.job,
  data: {
    job_id: "job-synthetic-004",
    state: "COMPLETE",
    accepted_source: {
      source_name: "synthetic-readiness.zip",
      source_hash: hash("1"),
      source_size: 4096,
      stored_archive: "accepted/source-package.zip",
    },
    timestamps: {
      created_at: "2026-07-20T10:00:00Z",
      updated_at: "2026-07-20T10:02:00Z",
      completed_at: "2026-07-20T10:02:00Z",
    },
    counts: {
      accepted_file_count: 2,
      accepted_row_count: 4,
      accepted_field_count: 12,
      pending_row_count: 0,
      profiling_run_count: 1,
    },
    reconciliation: {
      source_records: 4,
      stored_records: 4,
      pending_records: 0,
      closed: true,
    },
    integrity_status: "NOT_CHECKED",
    selected_governance: contextFixture.selected_governance,
    warnings: [{ code: "CLEANUP_PENDING", count: 1 }],
  },
  canonical_hash: hash("2"),
};

export const filesFixture = {
  schema_version: SCHEMAS.files,
  scope: { job_id: "job-synthetic-004", source_hash: hash("1") },
  items: [
    {
      file_id: "accepted-file-001",
      path: "objects/alpha.csv",
      source_ordinal: 1,
      compressed_bytes: 240,
      expanded_bytes: 920,
      row_count: 2,
      column_count: 6,
      ingestion_state: "COMPLETE",
      profiling_state: "COMPLETE",
    },
  ],
  page: { page_size: 1, returned: 1, next_cursor: "files-next-opaque" },
  canonical_hash: hash("3"),
};

export const profilingFixture = {
  schema_version: SCHEMAS.profiling,
  scope: {
    run_id: "profiling-synthetic-004",
    job_id: "job-synthetic-004",
    result_hash: hash("4"),
  },
  include_samples: false,
  items: [
    {
      path: "objects/alpha.csv",
      column_ordinal: 1,
      column_name: "object_key",
      empty_count: 0,
      nonempty_count: 2,
      min_decoded_length: 6,
      max_decoded_length: 6,
      oversized_count: 0,
      distinct_status: "EXACT",
      distinct_count: 2,
      sample_count: 1,
    },
  ],
  page: { page_size: 50, returned: 1, next_cursor: null },
  canonical_hash: hash("5"),
};

export const profilingSamplesFixture = {
  ...profilingFixture,
  include_samples: true,
  items: profilingFixture.items.map((item) => ({
    ...item,
    samples: [
      {
        sha256: hash("6"),
        utf8_bytes: 6,
        decoded_length: 6,
        value: "000123",
      },
    ],
  })),
};

export const profileFixture = {
  schema_version: SCHEMAS.profile,
  data: {
    profile_id: "synthetic.object",
    profile_version: "1.0.0",
    profile_hash: hash("a"),
    schema_hash: hash("7"),
    document: {
      profileId: "synthetic.object",
      status: "APPROVED",
      entities: [
        {
          entityId: "SyntheticObject",
          keys: ["object_key"],
          fields: [{ fieldId: "object_key", type: "TEXT", required: true }],
          relationships: [],
          rules: [{ ruleId: "required.object_key" }],
        },
      ],
      compatibilityDeclarations: [{ context: "synthetic-only" }],
    },
    declared_status: "APPROVED",
    validation_hash: hash("8"),
    registered_at: "2026-07-20T10:03:00Z",
    lifecycle_state: "APPROVED",
    approvals: [
      {
        approval_id: "approval-synthetic-001",
        approver: "synthetic-owner",
        reason: "Synthetic readiness fixture",
        acknowledged: true,
        approved_at: "2026-07-20T10:03:00Z",
        approval_hash: hash("9"),
      },
    ],
    lifecycle_events: [
      {
        sequence: 1,
        from_state: "GENERATED_DRAFT",
        to_state: "APPROVED",
        actor: "synthetic-owner",
        reason: "Synthetic readiness fixture",
        previous_event_hash: null,
        event_hash: hash("a"),
        occurred_at: "2026-07-20T10:03:00Z",
      },
    ],
  },
  canonical_hash: hash("b"),
};

export const mappingFixture = {
  schema_version: SCHEMAS.mapping,
  scope: {
    mapping_id: "mapping-synthetic-004",
    mapping_version: "1",
    mapping_hash: hash("b"),
  },
  mapping: {
    mapping_id: "mapping-synthetic-004",
    mapping_version: "1",
    mapping_hash: hash("b"),
    profile_id: "synthetic.object",
    profile_version: "1.0.0",
    profile_hash: hash("a"),
    source_hash: hash("1"),
    validated_job_id: "job-synthetic-004",
  },
  items: [
    {
      kind: "MAPPED",
      mapping_kind: "DIRECT",
      source_file: "objects/alpha.csv",
      source_column_ordinal: 1,
      source_column_name: "object_key",
      entity_id: "SyntheticObject",
      field_id: "object_key",
      binding_id: "binding-001",
      issue_code: null,
      detail: null,
    },
    {
      kind: "UNCOVERED_REQUIRED_FIELD",
      mapping_kind: null,
      source_file: null,
      source_column_ordinal: null,
      source_column_name: null,
      entity_id: "SyntheticObject",
      field_id: "classification",
      binding_id: null,
      issue_code: "REQUIRED_FIELD_UNCOVERED",
      detail: "No accepted binding",
    },
  ],
  page: { page_size: 50, returned: 2, next_cursor: null },
  canonical_hash: hash("c"),
};

const emptyBreakdown = { items: [], truncated: false };
export const validationFixture = {
  schema_version: SCHEMAS.validation,
  data: {
    run_id: "validation-synthetic-004",
    identity_hash: hash("d"),
    job_id: "job-synthetic-004",
    source_hash: hash("1"),
    profile_id: "synthetic.object",
    profile_version: "1.0.0",
    profile_hash: hash("a"),
    mapping_id: "mapping-synthetic-004",
    mapping_version: "1",
    mapping_hash: hash("b"),
    rule_set_hash: hash("e"),
    engine_version: "migration-stream-validation/v1",
    policy_hash: hash("f"),
    state: "COMPLETE",
    result_hash: hash("0"),
    issue_count: 3,
    rows_processed: 4,
    error_code: null,
    error_message: null,
    timestamps: {
      created_at: "2026-07-20T10:04:00Z",
      updated_at: "2026-07-20T10:05:00Z",
      completed_at: "2026-07-20T10:05:00Z",
    },
    issue_breakdowns: {
      by_severity: {
        items: [
          { key: "ERROR", count: 1 },
          { key: "WARNING", count: 1 },
          { key: "INFORMATION", count: 1 },
        ],
        truncated: false,
      },
      by_rule: emptyBreakdown,
      by_entity: emptyBreakdown,
      by_field: emptyBreakdown,
      by_source_file: emptyBreakdown,
    },
  },
  canonical_hash: hash("1"),
};

export const issuesFixture = {
  schema_version: SCHEMAS.issues,
  scope: {
    run_id: "validation-synthetic-004",
    identity_hash: hash("d"),
    job_id: "job-synthetic-004",
    source_hash: hash("1"),
    profile_id: "synthetic.object",
    profile_version: "1.0.0",
    profile_hash: hash("a"),
    mapping_id: "mapping-synthetic-004",
    mapping_version: "1",
    mapping_hash: hash("b"),
    state: "COMPLETE",
    result_hash: hash("0"),
  },
  filters: {
    severity: null,
    rule_id: null,
    entity_id: null,
    field_id: null,
    source_file: null,
    source_ordinal: null,
  },
  items: [
    {
      issue_id: "issue-synthetic-001",
      severity: "ERROR",
      rule_id: "required.object_key",
      issue_code: "REQUIRED_TEXT_EMPTY",
      display_message: "Required canonical text is absent.",
      entity_id: "SyntheticObject",
      field_id: "object_key",
      source_file: "objects/alpha.csv",
      source_ordinal: 2,
      line_start: 3,
      line_end: 3,
      issue_hash: hash("2"),
    },
  ],
  page: { page_size: 25, returned: 1, next_cursor: "issues-next-opaque" },
  canonical_hash: hash("3"),
};

export const rowFixture = {
  schema_version: SCHEMAS.row,
  data: {
    run_id: "validation-synthetic-004",
    job_id: "job-synthetic-004",
    source_hash: hash("1"),
    source_file: "objects/alpha.csv",
    source_ordinal: 2,
    line_start: 3,
    line_end: 3,
    source_values: [
      { column_ordinal: 1, column_name: "object_key", value: "000123" },
      { column_ordinal: 2, column_name: "numeric_text", value: "1e10" },
      { column_ordinal: 3, column_name: "date_text", value: "2026-07-22" },
      { column_ordinal: 4, column_name: "unicode_text", value: "Ωμέγα" },
      {
        column_ordinal: 5,
        column_name: "markup_text",
        value: '<script>alert("not executed")</script>',
      },
    ],
    canonical_projection: [
      {
        entity_id: "SyntheticObject",
        field_id: "object_key",
        value: "000123",
        lineage: {
          kind: "DIRECT",
          binding_id: "binding-001",
          source_column_ordinal: 1,
          source_column_name: "object_key",
          lookup_table_id: null,
        },
      },
      {
        entity_id: "SyntheticObject",
        field_id: "classification",
        value: "STATIC",
        lineage: {
          kind: "CONSTANT",
          binding_id: "binding-002",
          source_column_ordinal: null,
          source_column_name: null,
          lookup_table_id: null,
        },
      },
    ],
    issues: [
      {
        issue_id: "issue-synthetic-001",
        severity: "ERROR",
        rule_id: "required.object_key",
        issue_code: "REQUIRED_TEXT_EMPTY",
        display_message: "Required canonical text is absent.",
        entity_id: "SyntheticObject",
        field_id: "object_key",
        issue_hash: hash("2"),
      },
    ],
  },
  canonical_hash: hash("4"),
};

export const reconciliationFixture = {
  schema_version: SCHEMAS.reconciliation,
  scope: {
    job_id: "job-synthetic-004",
    source_hash: hash("1"),
    validation_run_id: "validation-synthetic-004",
  },
  aggregate: {
    source_records: 4,
    stored_records: 4,
    pending_records: 0,
    closed: true,
    validation_rows_processed: 4,
    validation_issue_count: 3,
  },
  items: [
    {
      path: "objects/alpha.csv",
      source_records: 2,
      stored_records: 2,
      pending_records: 0,
      closed: true,
      validation_issue_count: 1,
    },
  ],
  page: {
    page_size: 1,
    returned: 1,
    next_cursor: "reconciliation-next-opaque",
  },
  canonical_hash: hash("5"),
};

export const operationsFixture = {
  schema_version: SCHEMAS.operations,
  scope: { states: ["CLEANUP_PENDING"] },
  items: [
    {
      cleanup_id: "internal-cleanup-001",
      namespace: "packages",
      job_id: "job-synthetic-004",
      state: "CLEANUP_PENDING",
      cleanup_attempts: 2,
      last_error_code: "DELETE_DEFERRED",
      diagnostic_present: true,
      created_at: "2026-07-20T09:50:00Z",
      updated_at: "2026-07-20T10:06:00Z",
      quarantined_at: "2026-07-20T09:51:00Z",
      cleaned_at: null,
    },
  ],
  page: { page_size: 50, returned: 1, next_cursor: null },
  canonical_hash: hash("6"),
};

export const decisionSummaryFixture = {
  decision_id: "decision-synthetic-002",
  disposition: "CORRECTED",
  practitioner_identifier: "practitioner-synthetic",
  applied_at: "2026-07-22T12:30:00Z",
  reason_summary: "Corrected an exact synthetic text field.",
  correction_overlay_id: "overlay-synthetic-002",
  reevaluation_id: "reevaluation-synthetic-002",
  reevaluation_state: "COMPLETE",
  generation: 2,
  supersession_count: 1,
  lifecycle_outcome: "EFFECTIVE",
  integrity_status: "VERIFIED",
  active: true,
};

export const decisionAttemptSummaryFixture = {
  decision_attempt_id: "decision-attempt-synthetic-002",
  disposition: "CORRECTED",
  correction_overlay_id: "overlay-synthetic-002",
  reevaluation_id: "reevaluation-synthetic-002",
  attempt_state: "EFFECTIVE",
  reevaluation_state: "COMPLETE",
  lifecycle_outcome: "EFFECTIVE",
  integrity_status: "VERIFIED",
  active: true,
};

export const interruptedDecisionAttemptFixture = {
  decision_attempt_id: "decision-attempt-synthetic-interrupted",
  disposition: "CORRECTED",
  correction_overlay_id: "overlay-synthetic-interrupted",
  reevaluation_id: "reevaluation-synthetic-interrupted",
  attempt_state: "REEVALUATION_INTERRUPTED",
  reevaluation_state: "INTERRUPTED",
  lifecycle_outcome: "INTERRUPTED",
  integrity_status: "VERIFIED",
  active: false,
};

export const decisionQueueFixture = {
  schema_version: SCHEMAS.decisionQueue,
  scope: {
    job_id: "job-synthetic-004",
    validation_run_id: "validation-synthetic-004",
  },
  filters: {
    disposition: null,
    severity: null,
    entity_id: null,
    field_id: null,
    rule_id: null,
    source_file: null,
    reevaluation_state: null,
  },
  items: [
    {
      issue_id: "issue-synthetic-001",
      severity: "ERROR",
      rule_id: "required.object_key",
      issue_code: "REQUIRED_TEXT_EMPTY",
      display_message: "Required canonical text is absent.",
      entity_id: "SyntheticObject",
      field_id: "object_key",
      source_file: "objects/alpha.csv",
      source_ordinal: 2,
      active_decision: decisionSummaryFixture,
      latest_attempt: decisionAttemptSummaryFixture,
    },
  ],
  page: { page_size: 50, returned: 1, next_cursor: "decision-next-opaque" },
  canonical_hash: hash("7"),
};

export const decisionCountsFixture = {
  schema_version: SCHEMAS.decisionCounts,
  scope: {
    job_id: "job-synthetic-004",
    validation_run_id: "validation-synthetic-004",
  },
  groups: {
    by_disposition: {
      items: [{ key: "CORRECTED", count: 1 }],
      truncated: false,
    },
    by_severity: { items: [{ key: "ERROR", count: 1 }], truncated: false },
    by_entity: emptyBreakdown,
    by_field: emptyBreakdown,
    by_rule: emptyBreakdown,
    by_source_file: emptyBreakdown,
    by_reevaluation_state: {
      items: [{ key: "COMPLETE", count: 1 }],
      truncated: false,
    },
  },
  canonical_hash: hash("8"),
};

export const decisionIssueFixture = {
  schema_version: SCHEMAS.decisionIssue,
  data: {
    job_id: "job-synthetic-004",
    validation_run_id: "validation-synthetic-004",
    issue_id: "issue-synthetic-001",
    issue_hash: hash("2"),
    severity: "ERROR",
    rule_id: "required.object_key",
    issue_code: "REQUIRED_TEXT_EMPTY",
    display_message: '<script>alert("not executed")</script>',
    entity_id: "SyntheticObject",
    field_id: "object_key",
    accepted_row: {
      source_file: "objects/alpha.csv",
      source_ordinal: 2,
      line_start: 3,
      line_end: 3,
    },
    field_context: {
      received_source_value: '<img src=x onerror="alert(1)">',
      original_canonical_value: "000123",
      original_lineage: {
        kind: "DIRECT",
        binding_id: "binding-001",
        source_column_ordinal: 1,
        source_column_name: "object_key",
        lookup_table_id: null,
      },
      active_overlay_value: "SYNTHETIC-123",
      effective_canonical_value: "SYNTHETIC-123",
      effective_value_hash: hash("e"),
      active_overlay_id: "overlay-synthetic-002",
      correction_eligible: true,
      allowed_operations: [
        "REPLACE_EFFECTIVE_TEXT",
        "SET_EFFECTIVE_EMPTY",
        "SET_EFFECTIVE_NULL",
      ],
      constraints: {
        maximum_characters: 64,
        allow_empty: true,
        allow_null: true,
      },
    },
    active_decision: decisionSummaryFixture,
    latest_attempt: decisionAttemptSummaryFixture,
    decision_history_count: 2,
    concurrency: {
      expected_active_decision_id: "decision-synthetic-002",
      expected_active_generation: 2,
      expected_effective_value_hash: hash("e"),
    },
    authority_notice:
      "This decision is practitioner evidence, not migration-result approval.",
  },
  canonical_hash: hash("9"),
};

export const decisionHistoryFixture = {
  schema_version: SCHEMAS.decisionHistory,
  scope: {
    job_id: "job-synthetic-004",
    validation_run_id: "validation-synthetic-004",
    issue_id: "issue-synthetic-001",
  },
  items: [
    {
      decision_id: "decision-synthetic-002",
      disposition: "CORRECTED",
      practitioner_identifier: "practitioner-synthetic",
      applied_at: "2026-07-22T12:30:00Z",
      reason: "Corrected an exact synthetic text field.",
      evidence_reference: "synthetic-evidence-002",
      prior_decision_id: "decision-synthetic-001",
      generation: 2,
      correction_overlay_id: "overlay-synthetic-002",
      reevaluation_id: "reevaluation-synthetic-002",
      canonical_hash: hash("a"),
      lifecycle_outcome: "EFFECTIVE",
      active: true,
    },
    {
      decision_id: "decision-synthetic-001",
      disposition: "WAIVED",
      practitioner_identifier: "practitioner-synthetic",
      applied_at: "2026-07-22T11:30:00Z",
      reason: "Earlier synthetic disposition.",
      evidence_reference: null,
      prior_decision_id: null,
      generation: 1,
      correction_overlay_id: null,
      reevaluation_id: null,
      canonical_hash: hash("b"),
      lifecycle_outcome: "SUPERSEDED",
      active: false,
    },
  ],
  page: { page_size: 50, returned: 2, next_cursor: null },
  canonical_hash: hash("c"),
};

export const correctionFixture = {
  schema_version: SCHEMAS.correction,
  data: {
    overlay_id: "overlay-synthetic-002",
    decision_attempt_id: "decision-attempt-synthetic-002",
    decision_id: "decision-synthetic-002",
    job_id: "job-synthetic-004",
    validation_run_id: "validation-synthetic-004",
    issue_id: "issue-synthetic-001",
    source_file: "objects/alpha.csv",
    source_ordinal: 2,
    entity_id: "SyntheticObject",
    field_id: "object_key",
    profile_id: "synthetic.object",
    profile_version: "1.0.0",
    profile_hash: hash("a"),
    mapping_id: "mapping-synthetic-004",
    mapping_version: "1",
    mapping_hash: hash("b"),
    operation: "REPLACE_EFFECTIVE_TEXT",
    original_canonical_value: "000123",
    original_lineage: {
      kind: "DIRECT",
      binding_id: "binding-001",
      source_column_ordinal: 1,
      source_column_name: "object_key",
      lookup_table_id: null,
    },
    prior_effective_value: "000123",
    overlay_value: "SYNTHETIC-123",
    effective_canonical_value: "SYNTHETIC-123",
    effective_value_hash: hash("e"),
    practitioner_identifier: "practitioner-synthetic",
    reason: "Corrected an exact synthetic text field.",
    attempted_at: "2026-07-22T12:29:59Z",
    applied_at: "2026-07-22T12:30:00Z",
    active: true,
    superseded_by_overlay_id: null,
    reevaluation_id: "reevaluation-synthetic-002",
    attempt_state: "EFFECTIVE",
    reevaluation_state: "COMPLETE",
    lifecycle_outcome: "EFFECTIVE",
    error_code: null,
    error_message: null,
    integrity_status: "VERIFIED",
  },
  canonical_hash: hash("d"),
};

export const decisionResultFixture = {
  schema_version: SCHEMAS.decisionResult,
  data: {
    decision_id: "decision-synthetic-002",
    disposition: "CORRECTED",
    lifecycle_outcome: "EFFECTIVE",
    active: true,
    idempotent_replay: false,
    correction_overlay_id: "overlay-synthetic-002",
    reevaluation_id: "reevaluation-synthetic-002",
    reevaluation_state: "COMPLETE",
    original_issue_present: false,
    residual_issue_count: 0,
    new_issue_count: 0,
    integrity_status: "VERIFIED",
  },
  canonical_hash: hash("e"),
};

export const decisionCommandResultFixture = {
  ...decisionResultFixture,
  schema_version: SCHEMAS.decisionCommandResult,
};

export const interruptedDecisionQueueFixture = {
  ...decisionQueueFixture,
  items: [
    {
      ...decisionQueueFixture.items[0],
      active_decision: null,
      latest_attempt: interruptedDecisionAttemptFixture,
    },
  ],
  canonical_hash: hash("f"),
};

export const interruptedCorrectionFixture = {
  ...correctionFixture,
  data: {
    ...correctionFixture.data,
    overlay_id: "overlay-synthetic-interrupted",
    decision_attempt_id: "decision-attempt-synthetic-interrupted",
    decision_id: null,
    effective_canonical_value: null,
    effective_value_hash: null,
    attempted_at: "2026-07-22T12:35:00Z",
    applied_at: null,
    active: false,
    reevaluation_id: "reevaluation-synthetic-interrupted",
    attempt_state: "REEVALUATION_INTERRUPTED",
    reevaluation_state: "INTERRUPTED",
    lifecycle_outcome: "INTERRUPTED",
    error_code: "VALIDATION_CANCELLED",
    error_message: "Bounded synthetic re-evaluation was interrupted.",
  },
  canonical_hash: hash("0"),
};

export const routeFixtures: Record<string, unknown> = {
  "/api/v1/context": contextFixture,
  "/api/v1/journey": journeyFixture,
  "/api/v1/job": jobFixture,
  "/api/v1/files": filesFixture,
  "/api/v1/profiling": profilingFixture,
  "/api/v1/profile": profileFixture,
  "/api/v1/mapping": mappingFixture,
  "/api/v1/validation": validationFixture,
  "/api/v1/issues": issuesFixture,
  "/api/v1/reconciliation": reconciliationFixture,
  "/api/v1/operations": operationsFixture,
  "/api/v1/decision-queue": decisionQueueFixture,
};

export const staleCursorError = {
  schema_version: SCHEMAS.error,
  ok: false,
  error: {
    code: "STALE_CURSOR",
    message: "The cursor no longer matches the selected evidence scope.",
    request_id: "request-synthetic-stale",
    retryable: false,
  },
};
