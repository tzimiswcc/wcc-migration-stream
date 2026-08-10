import type { FormEvent, ReactNode } from "react";
import { useState } from "react";
import { submitPublicationCommand, useRead } from "./api";
import {
  COMMAND_SCHEMAS,
  SCHEMAS,
  ContractError,
  type PublicationEligibility,
  type PublicationListPage,
  type PublicationRecord,
  type PublicationRequest,
  type PublicationSummaryItem,
} from "./contracts";
import {
  Card,
  DefinitionGrid,
  EmptyState,
  Hash,
  PageHeader,
  ReadBoundary,
  Status,
  TableShell,
  Value,
} from "./ui";
import { AppLink } from "./router";

const READINESS_BOUNDARY =
  "I understand this creates local Migration Stream readiness evidence only. It does not transmit data, authorize or perform a target load, establish target acceptance or compatibility, provide an authenticated signature, or grant production approval.";

function generateIdempotencyKey(): string {
  return `publication-${crypto.randomUUID()}`;
}

function publicationEvidenceHref(publicationId: string): string {
  return `/publication/${encodeURIComponent(publicationId)}`;
}

function publicationDetailApiPath(publicationId: string): string {
  return `/api/v1/publications/${encodeURIComponent(publicationId)}`;
}

function publicationDownloadPath(publicationId: string): string {
  return `${publicationDetailApiPath(publicationId)}/download`;
}

export function PublicationWorkbenchPage(): ReactNode {
  const eligibilityRead = useRead<PublicationEligibility>(
    "/api/v1/publications/eligibility",
    SCHEMAS.publicationEligibility,
  );
  const listRead = useRead<PublicationListPage>(
    "/api/v1/publications",
    SCHEMAS.publicationList,
  );

  return (
    <div>
      <PageHeader
        eyebrow="Local readiness evidence"
        title="Readiness Packages"
        description="Generate and inspect deterministic, tamper-evident local migration-readiness packages."
      />
      <div className="notice readiness-package-boundary" role="note">
        <strong>Local package boundary</strong>
        <p>
          Migration Stream stores each generated package as an immutable
          technical Publication record. Generating a local package does not
          transmit it, load a target, establish target compatibility, or
          constitute target acceptance.
        </p>
      </div>
      <nav className="section-index" aria-label="Readiness Package sections">
        <strong>On this page</strong>
        <ul>
          <li>
            <a href="#package-eligibility">Eligibility</a>
          </li>
          <li>
            <a href="#package-generation">Generate package</a>
          </li>
          <li>
            <a href="#package-history">Package history</a>
          </li>
        </ul>
      </nav>
      <ReadBoundary<PublicationEligibility>
        state={eligibilityRead.state}
        retry={eligibilityRead.retry}
      >
        {(eligibility) => (
          <PublicationView
            eligibility={eligibility}
            listState={listRead.state}
            onRefreshList={listRead.retry}
          />
        )}
      </ReadBoundary>
    </div>
  );
}

export function PublicationCapabilityUnavailablePage(): ReactNode {
  return (
    <div>
      <PageHeader
        eyebrow="Capability unavailable"
        title="Readiness Packages are disabled"
        description="This workbench process does not advertise governed local readiness-package generation."
      />
      <EmptyState title="Local package generation unavailable">
        Restart with an explicitly package-generation-enabled workbench
        configuration to inspect eligibility and local package history.
      </EmptyState>
    </div>
  );
}

export function PublicationEvidencePage({
  publicationId,
}: {
  publicationId: string;
}): ReactNode {
  const detailRead = useRead<PublicationRecord>(
    publicationDetailApiPath(publicationId),
    SCHEMAS.publicationDetail,
  );

  return (
    <div>
      <nav className="breadcrumbs" aria-label="Breadcrumb">
        <AppLink href="/publication">Readiness Packages</AppLink>
        <span aria-hidden="true">/</span>
        <span aria-current="page">Package Evidence</span>
      </nav>
      <PageHeader
        eyebrow="Recorded local readiness evidence"
        title="Readiness Package Evidence"
        description="Inspect the immutable Publication record and its recorded manifest evidence."
      />
      <div className="notice readiness-package-boundary" role="note">
        <strong>Local-only, non-authority boundary</strong>
        <p>
          This is local Migration Stream readiness evidence only. It does not
          transmit data, authorize or execute a target load, establish target
          compatibility or acceptance, provide an authenticated signature, or
          grant production approval.
        </p>
      </div>
      <ReadBoundary<PublicationRecord>
        state={detailRead.state}
        retry={detailRead.retry}
      >
        {(record) => <PublicationEvidenceView record={record} />}
      </ReadBoundary>
    </div>
  );
}

export function PublicationEvidenceView({
  record,
}: {
  record: PublicationRecord;
}): ReactNode {
  const categoryCounts = [
    [
      "INCLUDED_VALID",
      "Included without a blocking issue",
      record.manifest.included_valid_count,
    ],
    [
      "INCLUDED_CORRECTED",
      "Included with a governed correction",
      record.manifest.included_corrected_count,
    ],
    [
      "INCLUDED_WAIVED",
      "Included under a governed waiver",
      record.manifest.included_waived_count,
    ],
    [
      "INCLUDED_ACCEPTED_AS_IS",
      "Included by an accept-as-is decision",
      record.manifest.included_accepted_as_is_count,
    ],
    [
      "EXCLUDED",
      "Excluded by a governed decision",
      record.manifest.excluded_count,
    ],
    [
      "BLOCKED_UNRESOLVED",
      "Blocked by unresolved issues",
      record.manifest.blocked_unresolved_count,
    ],
    [
      "BLOCKED_INCOMPLETE",
      "Blocked by incomplete evidence",
      record.manifest.blocked_incomplete_count,
    ],
    [
      "BLOCKED_CONFLICT",
      "Blocked by conflicting evidence",
      record.manifest.blocked_conflict_count,
    ],
  ] as const;
  const blockedRows =
    record.manifest.blocked_unresolved_count +
    record.manifest.blocked_incomplete_count +
    record.manifest.blocked_conflict_count;

  return (
    <div className="readiness-package-stack">
      <Card title="Local readiness outcome">
        <p className="readiness-outcome-statement">
          All {record.manifest.total_accepted_rows} accepted source rows are
          accounted for: <strong>{record.included_row_count} included</strong>{" "}
          in the local readiness extract,{" "}
          <strong>{record.excluded_row_count} excluded</strong> under governed
          decision evidence, and <strong>{blockedRows} blocked</strong>.
        </p>
        <DefinitionGrid
          items={[
            {
              label: "Package-generation status",
              value: <Status value={record.status} />,
            },
            {
              label: "Locally included rows",
              value: <Value>{record.included_row_count}</Value>,
            },
            {
              label: "Governed exclusions",
              value: <Value>{record.excluded_row_count}</Value>,
            },
            {
              label: "Blocking rows",
              value: <Value>{blockedRows}</Value>,
            },
          ]}
        />
        <p className="supporting">
          <strong>COMPLETE</strong> means this local package finished
          generation. It does not establish target compatibility or acceptance
          and does not authorize a load.
        </p>
      </Card>

      <Card title="Governed decisions in this package">
        <DefinitionGrid
          items={[
            {
              label: "Consequential decisions",
              value: (
                <Value>
                  {record.manifest.governing_decision_count ?? "Not declared"}
                </Value>
              ),
            },
            {
              label: "Accepted as-is",
              value: (
                <Value>{record.manifest.included_accepted_as_is_count}</Value>
              ),
            },
            {
              label: "Corrected",
              value: <Value>{record.manifest.included_corrected_count}</Value>,
            },
            {
              label: "Waived",
              value: <Value>{record.manifest.included_waived_count}</Value>,
            },
            {
              label: "Excluded",
              value: <Value>{record.manifest.excluded_count}</Value>,
            },
          ]}
        />
        <p className="supporting">
          {record.manifest.governing_decision_count === undefined
            ? "This older manifest does not declare its governing decision count."
            : "Exact row ordinals, issue and decision identities, evidence hashes, practitioners, and reasons are sealed in issue-decision-evidence.json; excluded-row linkage is repeated in exclusion-evidence.json."}
        </p>
      </Card>

      <Card title="Local approval and package generation">
        <DefinitionGrid
          items={[
            {
              label: "Local approval actor",
              value: (
                <Value>
                  {record.manifest.approval_actor ?? "Not declared"}
                </Value>
              ),
            },
            {
              label: "Local approval reason",
              value: (
                <Value>
                  {record.manifest.approval_reason ?? "Not declared"}
                </Value>
              ),
            },
            {
              label: "Local approval occurred at",
              value: (
                <Value>
                  {record.manifest.approval_occurred_at ?? "Not declared"}
                </Value>
              ),
            },
            {
              label: "Package generated by",
              value: <Value>{record.practitioner}</Value>,
            },
            {
              label: "Package-generation reason",
              value: <Value>{record.publication_reason}</Value>,
            },
            {
              label: "Package completed at",
              value: <Value>{record.completed_at}</Value>,
            },
          ]}
        />
        <div className="notice readiness-package-message" role="note">
          <strong>Recorded package boundary</strong>
          <p>
            <Value>{record.manifest.disclaimer}</Value>
          </p>
        </div>
      </Card>

      <Card title="Publication record">
        <DefinitionGrid
          items={[
            {
              label: "Publication ID",
              value: <Value>{record.publication_id}</Value>,
            },
            {
              label: "Status",
              value: <Status value={record.status} />,
            },
            {
              label: "Job ID",
              value: <Value>{record.job_id}</Value>,
            },
            {
              label: "Package contract",
              value: <Value>{record.contract_version}</Value>,
            },
            {
              label: "Publication version",
              value: <Value>{record.manifest.publication_version}</Value>,
            },
            {
              label: "Application / engine",
              value: (
                <Value>
                  {record.manifest.application_version} /{" "}
                  {record.manifest.engine_version}
                </Value>
              ),
            },
          ]}
        />
      </Card>

      <Card title="Recorded provenance and hash chain">
        <p className="supporting">
          These values are recorded by the package manifest. This view does not
          perform a new integrity verification or infer target compatibility.
        </p>
        <ol
          className="provenance-chain"
          aria-label="Recorded package provenance"
        >
          <li>
            <strong>Source package</strong>
            <Hash>{record.manifest.source_package_digest}</Hash>
          </li>
          <li>
            <strong>
              Profile {record.manifest.profile_identity} ·{" "}
              {record.manifest.profile_version}
            </strong>
            <Hash>{record.manifest.profile_digest}</Hash>
          </li>
          <li>
            <strong>
              Mapping{" "}
              {record.manifest.mapping_identity
                ? `${record.manifest.mapping_identity} · ${
                    record.manifest.mapping_version ?? "version not declared"
                  }`
                : "identity not declared"}
            </strong>
            <Hash>{record.manifest.mapping_digest}</Hash>
          </li>
          <li>
            <strong>
              Validation run{" "}
              {record.manifest.validation_run_id ?? "identity not declared"}
            </strong>
            <Hash>{record.manifest.validation_digest}</Hash>
          </li>
          <li>
            <strong>
              Reconciliation snapshot{" "}
              {record.manifest.reconciliation_snapshot_id ??
                "identity not declared"}
            </strong>
            <Hash>{record.manifest.reconciliation_snapshot_hash}</Hash>
          </li>
          <li>
            <strong>Candidate {record.manifest.candidate_id}</strong>
            <Hash>{record.manifest.candidate_hash}</Hash>
          </li>
          <li>
            <strong>Local approval {record.manifest.approval_id}</strong>
            <Hash>{record.manifest.approval_hash}</Hash>
          </li>
        </ol>
      </Card>

      <Card title="Eight-category row accounting">
        <div
          className="category-count-grid"
          aria-label="Eight recorded row categories"
        >
          {categoryCounts.map(([category, description, count]) => (
            <div className="category-count" key={category}>
              <strong>{count}</strong>
              <span>{category}</span>
              <small>{description}</small>
            </div>
          ))}
        </div>
        <DefinitionGrid
          items={[
            {
              label: "Manifest total accepted rows",
              value: <Value>{record.manifest.total_accepted_rows}</Value>,
            },
            {
              label: "Publication included rows",
              value: <Value>{record.included_row_count}</Value>,
            },
            {
              label: "Publication excluded rows",
              value: <Value>{record.excluded_row_count}</Value>,
            },
          ]}
        />
      </Card>

      <Card title="Recorded package digests">
        <DefinitionGrid
          items={[
            {
              label: "Semantic publication hash",
              value: <Hash>{record.semantic_publication_hash}</Hash>,
            },
            {
              label: "Manifest semantic publication hash",
              value: <Hash>{record.manifest.semantic_publication_hash}</Hash>,
            },
            {
              label: "Physical archive digest",
              value: <Hash>{record.physical_archive_digest}</Hash>,
            },
            {
              label: "Package size (bytes)",
              value: <Value>{record.package_size_bytes}</Value>,
            },
            {
              label: "Inventoried content members",
              value: <Value>{record.member_count}</Value>,
            },
            {
              label: "Total ZIP entries (content + manifest)",
              value: <Value>{record.member_count + 1}</Value>,
            },
            {
              label: "Local archive path",
              value: <Value>{record.archive_relative_path}</Value>,
            },
          ]}
        />
      </Card>

      <Card title="Manifest member inventory">
        <p className="supporting">
          The manifest inventories {record.member_count} content members.{" "}
          <Value>manifest.json</Value> is the additional self-describing ZIP
          entry and does not recursively inventory itself.
        </p>
        {record.manifest.member_inventory.length === 0 ? (
          <EmptyState title="No manifest members recorded">
            This Publication record contains an empty member inventory.
          </EmptyState>
        ) : (
          <TableShell label="Recorded manifest member inventory">
            <table className="table">
              <thead>
                <tr>
                  <th scope="col">Filename</th>
                  <th scope="col">Media type</th>
                  <th scope="col">Rows</th>
                  <th scope="col">Bytes</th>
                  <th scope="col">SHA-256 digest</th>
                </tr>
              </thead>
              <tbody>
                {record.manifest.member_inventory.map((member) => (
                  <tr key={member.filename}>
                    <th scope="row">
                      <Value>{member.filename}</Value>
                    </th>
                    <td>
                      <Value>{member.media_type}</Value>
                    </td>
                    <td>
                      <Value>{member.row_count ?? "Not declared"}</Value>
                    </td>
                    <td>
                      <Value>{member.byte_size}</Value>
                    </td>
                    <td>
                      <Hash>{member.sha256_digest}</Hash>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </TableShell>
        )}
      </Card>

      <nav
        className="action-stack readiness-package-actions"
        aria-label="Readiness package evidence actions"
      >
        <a
          href={publicationDownloadPath(record.publication_id)}
          download={`wp007-readiness-${record.job_id}-${record.publication_id}.zip`}
          className="button"
        >
          Download package ZIP
        </a>
        <AppLink href="/publication" className="button button--secondary">
          Return to Readiness Packages
        </AppLink>
      </nav>
    </div>
  );
}

interface PublicationViewProps {
  eligibility: PublicationEligibility;
  listState: ReturnType<typeof useRead<PublicationListPage>>["state"];
  onRefreshList: () => void;
}

export function PublicationView({
  eligibility,
  listState,
  onRefreshList,
}: PublicationViewProps): ReactNode {
  const [practitioner, setPractitioner] = useState("");
  const [reason, setReason] = useState("");
  const [acknowledgedBoundary, setAcknowledgedBoundary] = useState(false);
  const [acknowledgedRequirements, setAcknowledgedRequirements] = useState<
    Record<string, boolean>
  >({});
  const [commandIdentity, setCommandIdentity] = useState(
    generateIdempotencyKey,
  );

  const [submissionState, setSubmissionState] = useState<
    | { status: "idle" }
    | { status: "submitting" }
    | { status: "success"; record: PublicationRecord }
    | { status: "error"; error: ContractError }
  >({ status: "idle" });

  const allRequirementsAcknowledged =
    eligibility.required_acknowledgments.every(
      (acknowledgment) => acknowledgedRequirements[acknowledgment] === true,
    );

  const canSubmit =
    eligibility.is_eligible &&
    Boolean(eligibility.candidate_id) &&
    Boolean(eligibility.approval_id) &&
    practitioner.trim().length > 0 &&
    reason.trim().length > 0 &&
    acknowledgedBoundary &&
    allRequirementsAcknowledged &&
    submissionState.status !== "submitting";

  const resetPendingCommand = () => {
    setCommandIdentity(generateIdempotencyKey());
    if (submissionState.status === "error") {
      setSubmissionState({ status: "idle" });
    }
  };

  const handleToggleRequirement = (acknowledgment: string) => {
    resetPendingCommand();
    setAcknowledgedRequirements((prev) => ({
      ...prev,
      [acknowledgment]: !prev[acknowledgment],
    }));
  };

  const handleSubmit = async (e: FormEvent) => {
    e.preventDefault();
    if (
      !canSubmit ||
      !eligibility.candidate_id ||
      !eligibility.approval_id ||
      !eligibility.candidate_hash ||
      !eligibility.approval_hash
    ) {
      return;
    }

    setSubmissionState({ status: "submitting" });

    const request: PublicationRequest = {
      schema_version: COMMAND_SCHEMAS.publish,
      job_id: eligibility.job_id,
      candidate_id: eligibility.candidate_id,
      approval_id: eligibility.approval_id,
      practitioner: practitioner.trim(),
      publication_reason: reason.trim(),
      acknowledgments: Object.keys(acknowledgedRequirements).filter(
        (key) => acknowledgedRequirements[key],
      ),
      idempotency_key: commandIdentity,
    };

    try {
      const record = await submitPublicationCommand(
        "/api/v1/publications",
        request,
      );
      setSubmissionState({ status: "success", record });
      setReason("");
      setCommandIdentity(generateIdempotencyKey());
      onRefreshList();
    } catch (err: unknown) {
      const error =
        err instanceof ContractError
          ? err
          : new ContractError(
              "PUBLICATION_FAILED",
              "Local package generation request failed.",
            );
      setSubmissionState({ status: "error", error });
    }
  };

  return (
    <div className="readiness-package-stack">
      {/* 1. Eligibility Status Card */}
      <div id="package-eligibility" className="section-anchor" />
      <Card title="Package Generation Eligibility">
        <p className="supporting">
          <strong>ELIGIBLE</strong> means the selected job has one current,
          active local approval with closed row accounting and no package
          blockers. It means only that local package generation is allowed; it
          is not target-readiness or load authority.
        </p>
        <DefinitionGrid
          items={[
            {
              label: "Status",
              value: (
                <Status
                  value={eligibility.is_eligible ? "ELIGIBLE" : "INELIGIBLE"}
                />
              ),
            },
            {
              label: "Job ID",
              value: <Value>{eligibility.job_id}</Value>,
            },
            {
              label: "Active Candidate",
              value: eligibility.candidate_id ? (
                <div>
                  <div>
                    <Value>{eligibility.candidate_id}</Value>
                  </div>
                  <Hash>{eligibility.candidate_hash}</Hash>
                </div>
              ) : (
                <Value>None (No active approved result candidate)</Value>
              ),
            },
            {
              label: "Active Approval",
              value: eligibility.approval_id ? (
                <div>
                  <div>
                    <Value>{eligibility.approval_id}</Value>
                  </div>
                  <Hash>{eligibility.approval_hash}</Hash>
                </div>
              ) : (
                <Value>None</Value>
              ),
            },
          ]}
        />

        {eligibility.blockers.length > 0 && (
          <div
            className="notice notice--critical readiness-package-message"
            role="note"
          >
            <h3>Package Generation Blockers ({eligibility.blockers.length})</h3>
            <ul>
              {eligibility.blockers.map((blocker, idx) => (
                <li key={idx}>
                  <Value>{blocker}</Value>
                </li>
              ))}
            </ul>
          </div>
        )}

        {eligibility.warnings.length > 0 && (
          <div
            className="notice notice--attention readiness-package-message"
            role="note"
          >
            <h3>Validation Warnings ({eligibility.warnings.length})</h3>
            <ul>
              {eligibility.warnings.map((warning, idx) => (
                <li key={idx}>
                  <Value>{warning}</Value>
                </li>
              ))}
            </ul>
          </div>
        )}
      </Card>

      {/* 2. Controlled Publication Form */}
      <div id="package-generation" className="section-anchor" />
      <Card title="Generate Local Readiness Package">
        <form
          onSubmit={(e) => {
            void handleSubmit(e);
          }}
          className="decision-form"
        >
          <label htmlFor="practitioner-input">
            Practitioner identifier (local attribution) *
            <input
              id="practitioner-input"
              type="text"
              value={practitioner}
              onChange={(e) => {
                setPractitioner(e.target.value);
                resetPendingCommand();
              }}
              placeholder="e.g. synthetic-practitioner-01"
              required
            />
            <small>
              Recorded as local attribution only; this is not authentication or
              external authority.
            </small>
          </label>

          <label htmlFor="reason-input">
            Local package-generation reason *
            <textarea
              id="reason-input"
              value={reason}
              onChange={(e) => {
                setReason(e.target.value);
                resetPendingCommand();
              }}
              placeholder="Provide the local package-generation reason and review context..."
              rows={3}
              required
            />
          </label>

          {/* Server-required acknowledgments */}
          {eligibility.required_acknowledgments.length > 0 && (
            <fieldset className="acknowledgment-fieldset">
              <legend>Required Package Acknowledgments *</legend>
              {eligibility.required_acknowledgments.map((acknowledgment) => (
                <label key={acknowledgment} className="acknowledgment">
                  <input
                    type="checkbox"
                    checked={Boolean(acknowledgedRequirements[acknowledgment])}
                    onChange={() => handleToggleRequirement(acknowledgment)}
                  />
                  <span>{acknowledgment}</span>
                </label>
              ))}
            </fieldset>
          )}

          {/* Readiness Boundary Acknowledgment */}
          <div className="authority-acknowledgment">
            <label className="acknowledgment">
              <input
                id="boundary-checkbox"
                type="checkbox"
                checked={acknowledgedBoundary}
                onChange={(e) => {
                  setAcknowledgedBoundary(e.target.checked);
                  resetPendingCommand();
                }}
              />
              <span>
                <strong>Non-Authority Boundary Acknowledgment:</strong>{" "}
                {READINESS_BOUNDARY}
              </span>
            </label>
          </div>

          <div>
            <button
              id="publish-button"
              type="submit"
              disabled={!canSubmit}
              className="button"
            >
              {submissionState.status === "submitting"
                ? "Generating Local Package..."
                : "Generate Local Readiness Package"}
            </button>
          </div>

          {submissionState.status === "error" && (
            <div className="notice notice--critical" role="alert">
              <strong>
                Local package generation failed: [{submissionState.error.code}]
              </strong>{" "}
              {submissionState.error.message}
            </div>
          )}

          {submissionState.status === "success" && (
            <div className="notice" role="status" aria-live="polite">
              <strong>Local readiness package created</strong>
              <p>
                Publication ID:{" "}
                <code>{submissionState.record.publication_id}</code>
              </p>
              <p>
                Semantic Digest:{" "}
                <Hash>{submissionState.record.semantic_publication_hash}</Hash>
              </p>
              <p>
                Physical Digest:{" "}
                <Hash>{submissionState.record.physical_archive_digest}</Hash>
              </p>
              <p>
                <AppLink
                  href={publicationEvidenceHref(
                    submissionState.record.publication_id,
                  )}
                >
                  Inspect package evidence
                </AppLink>
              </p>
            </div>
          )}
        </form>
      </Card>

      {/* 3. Publication History List & Download */}
      <div id="package-history" className="section-anchor" />
      <Card title="Local Package History">
        <ReadBoundary<PublicationListPage>
          state={listState}
          retry={onRefreshList}
        >
          {(listPage) => (
            <div>
              {listPage.items.length === 0 ? (
                <EmptyState title="No local packages generated">
                  Generated local readiness packages for this job will appear
                  here.
                </EmptyState>
              ) : (
                <TableShell label="Local readiness package history">
                  <table className="table">
                    <thead>
                      <tr>
                        <th>Publication ID</th>
                        <th>Practitioner</th>
                        <th>Status</th>
                        <th>Size</th>
                        <th>Semantic Hash</th>
                        <th>Created At</th>
                        <th>Action</th>
                      </tr>
                    </thead>
                    <tbody>
                      {listPage.items.map((pub: PublicationSummaryItem) => (
                        <tr key={pub.publication_id}>
                          <td>
                            <Value>{pub.publication_id}</Value>
                          </td>
                          <td>
                            <Value>{pub.practitioner}</Value>
                          </td>
                          <td>
                            <Status value={pub.status} />
                          </td>
                          <td>
                            <Value>
                              {pub.package_size_bytes === null
                                ? "Pending"
                                : `${(pub.package_size_bytes / 1024).toFixed(1)} KB`}
                            </Value>
                          </td>
                          <td>
                            <Hash>
                              {pub.semantic_publication_hash ?? "Pending"}
                            </Hash>
                          </td>
                          <td>
                            <Value>{pub.created_at}</Value>
                          </td>
                          <td>
                            <div className="table-actions">
                              <AppLink
                                href={publicationEvidenceHref(
                                  pub.publication_id,
                                )}
                              >
                                View evidence
                              </AppLink>
                              <a
                                href={publicationDownloadPath(
                                  pub.publication_id,
                                )}
                                download={`wp007-readiness-${pub.job_id}-${pub.publication_id}.zip`}
                                className="button button--secondary"
                              >
                                Download ZIP
                              </a>
                            </div>
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </TableShell>
              )}
            </div>
          )}
        </ReadBoundary>
      </Card>
    </div>
  );
}
