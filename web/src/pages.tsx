import { type FormEvent, type ReactNode } from "react";
import { apiQuery, useRead } from "./api";
import {
  SCHEMAS,
  type FileManifestPage,
  type IssueListPage,
  type JobSummary,
  type MappingCoveragePage,
  type OperationalCleanupPage,
  type ProfileView,
  type ProfilingSummaryPage,
  type ReconciliationSummary,
  type RowDetail,
  type ValidationSummary,
} from "./contracts";
import { AppLink, navigate } from "./router";
import { RemediationPackPanel } from "./remediation-packs";
import {
  Card,
  DefinitionGrid,
  EmptyState,
  Hash,
  PageHeader,
  Pagination,
  ReadBoundary,
  Status,
  StructuredValue,
  TableShell,
  Value,
} from "./ui";

function displayNumber(value: number | null): string {
  return value === null ? "Not available" : value.toLocaleString("en-US");
}

function pageRequest(location: URL): {
  pageSize: number;
  cursor: string | null;
} {
  const parsed = Number(location.searchParams.get("page_size") ?? "50");
  const pageSize =
    Number.isInteger(parsed) && parsed >= 1 && parsed <= 200 ? parsed : 50;
  return { pageSize, cursor: location.searchParams.get("cursor") };
}

function formText(form: FormData, key: string, fallback = ""): string {
  const value = form.get(key);
  return typeof value === "string" ? value : fallback;
}

function pageHref(
  pathname: string,
  location: URL,
  cursor: string | null,
): string | null {
  if (!cursor) return null;
  const query = new URLSearchParams(location.searchParams);
  query.set("cursor", cursor);
  return `${pathname}?${query.toString()}`;
}

function firstPageHref(
  pathname: string,
  location: URL,
  cursorNames: string[] = ["cursor"],
): string | null {
  if (!cursorNames.some((name) => location.searchParams.has(name))) return null;
  const query = new URLSearchParams(location.searchParams);
  cursorNames.forEach((name) => query.delete(name));
  const encoded = query.toString();
  return encoded ? `${pathname}?${encoded}` : pathname;
}

function PageSizeControl({
  location,
  clearParams = ["cursor"],
}: {
  location: URL;
  clearParams?: string[];
}) {
  const { pageSize } = pageRequest(location);
  function apply(event: FormEvent<HTMLFormElement>): void {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const value = formText(form, "page_size", "50");
    const query = new URLSearchParams(location.searchParams);
    query.set("page_size", value);
    clearParams.forEach((name) => query.delete(name));
    navigate(`${location.pathname}?${query.toString()}`);
  }
  return (
    <form className="inline-control" onSubmit={apply}>
      <label htmlFor="page-size">Page size</label>
      <input
        id="page-size"
        name="page_size"
        type="number"
        min="1"
        max="200"
        defaultValue={pageSize}
      />
      <button type="submit" className="button button--secondary">
        Apply
      </button>
    </form>
  );
}

export function OverviewPage() {
  const read = useRead<JobSummary>("/api/v1/job", SCHEMAS.job);
  return (
    <>
      <PageHeader
        eyebrow="Selected job"
        title="Overview"
        description="Authoritative job, reconciliation, integrity, governance, and operational evidence."
      />
      <ReadBoundary {...read}>
        {(response) => {
          const job = response.data;
          return (
            <div className="dashboard-grid">
              <Card title="Job state" className="card--wide">
                <DefinitionGrid
                  items={[
                    {
                      label: "Job identity",
                      value: <Value>{job.job_id}</Value>,
                    },
                    { label: "Lifecycle", value: <Status value={job.state} /> },
                    {
                      label: "Integrity",
                      value: <Status value={job.integrity_status} />,
                    },
                    { label: "Created", value: job.timestamps.created_at },
                    { label: "Updated", value: job.timestamps.updated_at },
                    {
                      label: "Completed",
                      value: job.timestamps.completed_at ?? "Not completed",
                    },
                  ]}
                />
              </Card>
              <Card title="Accepted source">
                {job.accepted_source ? (
                  <DefinitionGrid
                    items={[
                      {
                        label: "Package",
                        value: job.accepted_source.source_name,
                      },
                      {
                        label: "Stored identity",
                        value: job.accepted_source.stored_archive,
                      },
                      {
                        label: "Bytes",
                        value: displayNumber(job.accepted_source.source_size),
                      },
                      {
                        label: "SHA-256",
                        value: <Hash>{job.accepted_source.source_hash}</Hash>,
                      },
                    ]}
                  />
                ) : (
                  <EmptyState>
                    No accepted source package is reported.
                  </EmptyState>
                )}
              </Card>
              <Card title="Accepted evidence counts">
                <div className="metric-grid">
                  <Metric
                    label="Files"
                    value={job.counts.accepted_file_count}
                  />
                  <Metric label="Rows" value={job.counts.accepted_row_count} />
                  <Metric
                    label="Fields"
                    value={job.counts.accepted_field_count}
                  />
                  <Metric
                    label="Pending"
                    value={job.counts.pending_row_count}
                  />
                  <Metric
                    label="Profile runs"
                    value={job.counts.profiling_run_count}
                  />
                </div>
              </Card>
              <Card title="Reconciliation">
                <DefinitionGrid
                  items={[
                    {
                      label: "Closure",
                      value: <Status value={job.reconciliation.closed} />,
                    },
                    {
                      label: "Source records",
                      value: displayNumber(job.reconciliation.source_records),
                    },
                    {
                      label: "Stored records",
                      value: displayNumber(job.reconciliation.stored_records),
                    },
                    {
                      label: "Pending records",
                      value: displayNumber(job.reconciliation.pending_records),
                    },
                  ]}
                />
              </Card>
              <Card title="Selected governance" className="card--wide">
                {job.selected_governance ? (
                  <DefinitionGrid
                    items={[
                      {
                        label: "Profile",
                        value: `${job.selected_governance.profile_id} · ${job.selected_governance.profile_version}`,
                      },
                      {
                        label: "Profile hash",
                        value: (
                          <Hash>{job.selected_governance.profile_hash}</Hash>
                        ),
                      },
                      {
                        label: "Mapping",
                        value: `${job.selected_governance.mapping_id} · ${job.selected_governance.mapping_version}`,
                      },
                      {
                        label: "Mapping hash",
                        value: (
                          <Hash>{job.selected_governance.mapping_hash}</Hash>
                        ),
                      },
                      {
                        label: "Validation run",
                        value: job.selected_governance.validation_run_id,
                      },
                      {
                        label: "Validation state",
                        value: (
                          <Status
                            value={job.selected_governance.validation_state}
                          />
                        ),
                      },
                    ]}
                  />
                ) : (
                  <EmptyState>No governance selection is reported.</EmptyState>
                )}
              </Card>
              <Card title="Operational warnings">
                {job.warnings.length ? (
                  <ul className="warning-list">
                    {job.warnings.map((warning) => (
                      <li key={warning.code}>
                        <strong>{warning.code}</strong>
                        <span>{displayNumber(warning.count)}</span>
                      </li>
                    ))}
                  </ul>
                ) : (
                  <EmptyState>No operational warning is reported.</EmptyState>
                )}
              </Card>
              <p className="contract-hash card--wide">
                Response evidence <Hash>{response.canonical_hash}</Hash>
              </p>
            </div>
          );
        }}
      </ReadBoundary>
    </>
  );
}

function Metric({ label, value }: { label: string; value: number }) {
  return (
    <div className="metric">
      <strong>{displayNumber(value)}</strong>
      <span>{label}</span>
    </div>
  );
}

export function FilesPage({ location }: { location: URL }) {
  const { pageSize, cursor } = pageRequest(location);
  const reconciliationCursor = location.searchParams.get(
    "reconciliation_cursor",
  );
  const read = useRead<FileManifestPage>(
    apiQuery("/api/v1/files", { page_size: pageSize, cursor }),
    SCHEMAS.files,
  );
  const reconciliation = useRead<ReconciliationSummary>(
    apiQuery("/api/v1/reconciliation", {
      page_size: pageSize,
      cursor: reconciliationCursor,
    }),
    SCHEMAS.reconciliation,
  );
  return (
    <>
      <PageHeader
        eyebrow="Accepted source only"
        title="Files"
        description="Accepted CSV identities and server-reported ingestion, profiling, and count evidence."
        action={
          <PageSizeControl
            location={location}
            clearParams={["cursor", "reconciliation_cursor"]}
          />
        }
      />
      <ReadBoundary {...read}>
        {(response) => (
          <ReadBoundary {...reconciliation}>
            {(reconciliationResponse) => {
              const reconciliationByPath = new Map(
                reconciliationResponse.items.map((item) => [item.path, item]),
              );
              const nextQuery = new URLSearchParams(location.searchParams);
              if (response.page.next_cursor) {
                nextQuery.set("cursor", response.page.next_cursor);
              } else {
                nextQuery.delete("cursor");
              }
              if (reconciliationResponse.page.next_cursor) {
                nextQuery.set(
                  "reconciliation_cursor",
                  reconciliationResponse.page.next_cursor,
                );
              } else {
                nextQuery.delete("reconciliation_cursor");
              }
              const cursorsAligned =
                Boolean(response.page.next_cursor) ===
                Boolean(reconciliationResponse.page.next_cursor);
              const nextHref =
                cursorsAligned && response.page.next_cursor
                  ? `/files?${nextQuery.toString()}`
                  : null;
              return response.items.length ? (
                <>
                  <TableShell label="Accepted files">
                    <table>
                      <thead>
                        <tr>
                          <th scope="col">Archive path</th>
                          <th scope="col">Ordinal</th>
                          <th scope="col">Rows</th>
                          <th scope="col">Columns</th>
                          <th scope="col">Compressed</th>
                          <th scope="col">Expanded</th>
                          <th scope="col">Ingestion</th>
                          <th scope="col">Profiling</th>
                          <th scope="col">Reconciliation</th>
                        </tr>
                      </thead>
                      <tbody>
                        {response.items.map((item) => {
                          const closure = reconciliationByPath.get(item.path);
                          return (
                            <tr key={item.file_id}>
                              <th scope="row">
                                <Value>{item.path}</Value>
                                <small>{item.file_id}</small>
                              </th>
                              <td>{item.source_ordinal}</td>
                              <td>{displayNumber(item.row_count)}</td>
                              <td>{displayNumber(item.column_count)}</td>
                              <td>{displayNumber(item.compressed_bytes)}</td>
                              <td>{displayNumber(item.expanded_bytes)}</td>
                              <td>
                                <Status value={item.ingestion_state} />
                              </td>
                              <td>
                                <Status value={item.profiling_state} />
                              </td>
                              <td>
                                {closure ? (
                                  <>
                                    <Status value={closure.closed} />
                                    <small>
                                      source {closure.source_records} · stored{" "}
                                      {closure.stored_records} · pending{" "}
                                      {closure.pending_records}
                                    </small>
                                  </>
                                ) : (
                                  <span className="muted">
                                    Not reported on paired server page
                                  </span>
                                )}
                              </td>
                            </tr>
                          );
                        })}
                      </tbody>
                    </table>
                  </TableShell>
                  {!cursorsAligned ? (
                    <div className="notice" role="status">
                      Paired server pagination is incomplete; no further paired
                      page is available.
                    </div>
                  ) : null}
                  <Pagination
                    page={response.page}
                    firstHref={firstPageHref("/files", location, [
                      "cursor",
                      "reconciliation_cursor",
                    ])}
                    nextHref={nextHref}
                  />
                </>
              ) : (
                <EmptyState>No accepted file exists on this page.</EmptyState>
              );
            }}
          </ReadBoundary>
        )}
      </ReadBoundary>
    </>
  );
}

export function ProfilingPage({ location }: { location: URL }) {
  const { pageSize, cursor } = pageRequest(location);
  const includeSamples =
    location.searchParams.get("include_samples") === "true";
  const read = useRead<ProfilingSummaryPage>(
    apiQuery("/api/v1/profiling", {
      include_samples: includeSamples,
      page_size: pageSize,
      cursor,
    }),
    SCHEMAS.profiling,
  );
  function toggleSamples(): void {
    const query = new URLSearchParams(location.searchParams);
    query.set("include_samples", includeSamples ? "false" : "true");
    query.delete("cursor");
    navigate(`/profiling?${query.toString()}`);
  }
  return (
    <>
      <PageHeader
        eyebrow="Bounded column evidence"
        title="Profiling"
        description="Textual statistics and server-provided distinct classifications. Samples require an explicit request."
        action={
          <div className="action-stack">
            <button type="button" className="button" onClick={toggleSamples}>
              {includeSamples ? "Hide samples" : "Request samples"}
            </button>
            <PageSizeControl location={location} />
          </div>
        }
      />
      <div className="notice" role="note">
        Samples are{" "}
        <strong>
          {includeSamples ? "explicitly requested" : "not requested"}
        </strong>
        .
      </div>
      <ReadBoundary {...read}>
        {(response) =>
          response.items.length ? (
            <>
              <div className="card-list">
                {response.items.map((item) => (
                  <Card
                    key={`${item.path}:${item.column_ordinal}`}
                    eyebrow={`${item.path} · column ${item.column_ordinal}`}
                    title={item.column_name}
                  >
                    <DefinitionGrid
                      items={[
                        {
                          label: "Empty",
                          value: displayNumber(item.empty_count),
                        },
                        {
                          label: "Non-empty",
                          value: displayNumber(item.nonempty_count),
                        },
                        {
                          label: "Decoded length",
                          value: `${displayNumber(item.min_decoded_length)} – ${displayNumber(item.max_decoded_length)}`,
                        },
                        {
                          label: "Oversized",
                          value: displayNumber(item.oversized_count),
                        },
                        {
                          label: "Distinct status",
                          value: <Status value={item.distinct_status} />,
                        },
                        {
                          label: "Distinct count",
                          value: displayNumber(item.distinct_count),
                        },
                        {
                          label: "Sample count",
                          value: displayNumber(item.sample_count),
                        },
                      ]}
                    />
                    {response.include_samples ? (
                      item.samples?.length ? (
                        <div className="sample-panel">
                          <h3>Requested samples</h3>
                          <ul>
                            {item.samples.map((sample) => (
                              <li key={sample.sha256}>
                                <Value>
                                  {sample.value ?? "Value absent or redacted"}
                                </Value>
                                <small>
                                  {sample.utf8_bytes} bytes · decoded length{" "}
                                  {sample.decoded_length} · {sample.sha256}
                                </small>
                              </li>
                            ))}
                          </ul>
                        </div>
                      ) : (
                        <p className="muted">
                          Samples are absent, redacted, or not computed.
                        </p>
                      )
                    ) : null}
                  </Card>
                ))}
              </div>
              <Pagination
                page={response.page}
                firstHref={firstPageHref("/profiling", location)}
                nextHref={pageHref(
                  "/profiling",
                  location,
                  response.page.next_cursor,
                )}
              />
            </>
          ) : (
            <EmptyState>No profiling evidence exists on this page.</EmptyState>
          )
        }
      </ReadBoundary>
    </>
  );
}

export function ProfilePage() {
  const read = useRead<ProfileView>("/api/v1/profile", SCHEMAS.profile);
  return (
    <>
      <PageHeader
        eyebrow="Immutable governance evidence"
        title="Profile"
        description="Technical provenance is separated from local lifecycle and approval authority. No approval action is available."
      />
      <ReadBoundary {...read}>
        {(response) => (
          <div className="dashboard-grid">
            <Card title="Technical provenance">
              <DefinitionGrid
                items={[
                  {
                    label: "Profile",
                    value: `${response.data.profile_id} · ${response.data.profile_version}`,
                  },
                  {
                    label: "Profile hash",
                    value: <Hash>{response.data.profile_hash}</Hash>,
                  },
                  {
                    label: "Schema hash",
                    value: <Hash>{response.data.schema_hash}</Hash>,
                  },
                  {
                    label: "Validation hash",
                    value: <Hash>{response.data.validation_hash}</Hash>,
                  },
                  { label: "Registered", value: response.data.registered_at },
                ]}
              />
            </Card>
            <Card title="Local authority">
              <DefinitionGrid
                items={[
                  {
                    label: "Declared status",
                    value: <Status value={response.data.declared_status} />,
                  },
                  {
                    label: "Lifecycle state",
                    value: <Status value={response.data.lifecycle_state} />,
                  },
                  {
                    label: "Approval records",
                    value: response.data.approvals.length,
                  },
                  {
                    label: "Lifecycle events",
                    value: response.data.lifecycle_events.length,
                  },
                ]}
              />
            </Card>
            <Card title="Approval evidence" className="card--wide">
              {response.data.approvals.length ? (
                <TableShell label="Profile approvals">
                  <table>
                    <thead>
                      <tr>
                        <th scope="col">Approval</th>
                        <th scope="col">Approver</th>
                        <th scope="col">Reason</th>
                        <th scope="col">Acknowledged</th>
                        <th scope="col">At</th>
                      </tr>
                    </thead>
                    <tbody>
                      {response.data.approvals.map((approval) => (
                        <tr key={approval.approval_id}>
                          <th scope="row">
                            <Value>{approval.approval_id}</Value>
                          </th>
                          <td>
                            <Value>{approval.approver}</Value>
                          </td>
                          <td>
                            <Value>{approval.reason}</Value>
                          </td>
                          <td>
                            <Status value={approval.acknowledged} />
                          </td>
                          <td>{approval.approved_at}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </TableShell>
              ) : (
                <EmptyState>No approval evidence is reported.</EmptyState>
              )}
            </Card>
            <Card title="Lifecycle evidence" className="card--wide">
              {response.data.lifecycle_events.length ? (
                <ol className="timeline">
                  {response.data.lifecycle_events.map((event) => (
                    <li key={event.event_hash}>
                      <Status value={event.to_state} />
                      <strong>Sequence {event.sequence}</strong>
                      <p>
                        <Value>{event.reason}</Value>
                      </p>
                      <small>
                        {event.actor} · {event.occurred_at}
                      </small>
                    </li>
                  ))}
                </ol>
              ) : (
                <EmptyState>No lifecycle event is reported.</EmptyState>
              )}
            </Card>
            <Card
              title="Canonical profile document"
              className="card--wide structured-document"
            >
              <StructuredValue value={response.data.document} />
            </Card>
          </div>
        )}
      </ReadBoundary>
    </>
  );
}

export function MappingPage({ location }: { location: URL }) {
  const { pageSize, cursor } = pageRequest(location);
  const read = useRead<MappingCoveragePage>(
    apiQuery("/api/v1/mapping", { page_size: pageSize, cursor }),
    SCHEMAS.mapping,
  );
  return (
    <>
      <PageHeader
        eyebrow="Read-only lineage"
        title="Mapping"
        description="Server-reported bindings, uncovered source and canonical fields, and coverage exceptions."
        action={<PageSizeControl location={location} />}
      />
      <ReadBoundary {...read}>
        {(response) => (
          <>
            <Card title="Mapping identity">
              <DefinitionGrid
                items={[
                  {
                    label: "Mapping",
                    value: `${response.mapping.mapping_id} · ${response.mapping.mapping_version}`,
                  },
                  {
                    label: "Mapping hash",
                    value: <Hash>{response.mapping.mapping_hash}</Hash>,
                  },
                  {
                    label: "Profile",
                    value: `${response.mapping.profile_id} · ${response.mapping.profile_version}`,
                  },
                  {
                    label: "Profile hash",
                    value: <Hash>{response.mapping.profile_hash}</Hash>,
                  },
                  {
                    label: "Source hash",
                    value: <Hash>{response.mapping.source_hash}</Hash>,
                  },
                ]}
              />
            </Card>
            {response.items.length ? (
              <TableShell label="Mapping coverage">
                <table>
                  <thead>
                    <tr>
                      <th scope="col">Coverage category</th>
                      <th scope="col">Lineage kind</th>
                      <th scope="col">Source</th>
                      <th scope="col">Canonical target</th>
                      <th scope="col">Binding</th>
                      <th scope="col">Issue</th>
                      <th scope="col">Detail</th>
                    </tr>
                  </thead>
                  <tbody>
                    {response.items.map((item, index) => (
                      <tr
                        key={`${item.kind}:${item.binding_id ?? item.issue_code ?? index}`}
                      >
                        <th scope="row">
                          <Status value={item.kind} />
                        </th>
                        <td>{item.mapping_kind ?? "—"}</td>
                        <td>
                          <Value>
                            {item.source_file
                              ? `${item.source_file} · ${item.source_column_name ?? item.source_column_ordinal ?? ""}`
                              : "—"}
                          </Value>
                        </td>
                        <td>
                          <Value>
                            {item.entity_id
                              ? `${item.entity_id}.${item.field_id ?? ""}`
                              : "—"}
                          </Value>
                        </td>
                        <td>
                          <Value>{item.binding_id ?? "—"}</Value>
                        </td>
                        <td>{item.issue_code ?? "—"}</td>
                        <td>
                          <Value>{item.detail ?? "—"}</Value>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </TableShell>
            ) : (
              <EmptyState>
                No mapping coverage item exists on this page.
              </EmptyState>
            )}
            <Pagination
              page={response.page}
              firstHref={firstPageHref("/mapping", location)}
              nextHref={pageHref(
                "/mapping",
                location,
                response.page.next_cursor,
              )}
            />
          </>
        )}
      </ReadBoundary>
    </>
  );
}

export function ValidationPage({
  engagementBound = false,
}: {
  engagementBound?: boolean;
}) {
  const read = useRead<ValidationSummary>(
    "/api/v1/validation",
    SCHEMAS.validation,
  );
  return (
    <>
      <PageHeader
        eyebrow="Canonical validation evidence"
        title="Validation"
        description="The server-reported lifecycle and result evidence is displayed without migration-result approval."
      />
      <ReadBoundary {...read}>
        {(response) => (
          <div className="dashboard-grid">
            <Card title="Run identity" className="card--wide">
              <DefinitionGrid
                items={[
                  { label: "Run", value: response.data.run_id },
                  {
                    label: "State",
                    value: <Status value={response.data.state} />,
                  },
                  {
                    label: "Identity hash",
                    value: <Hash>{response.data.identity_hash}</Hash>,
                  },
                  {
                    label: "Result hash",
                    value: <Hash>{response.data.result_hash}</Hash>,
                  },
                  {
                    label: "Rows processed",
                    value: displayNumber(response.data.rows_processed),
                  },
                  {
                    label: "Issues",
                    value: displayNumber(response.data.issue_count),
                  },
                ]}
              />
              {response.data.state !== "COMPLETE" ? (
                <div className="notice notice--attention" role="status">
                  This run is <strong>{response.data.state}</strong>. It is not
                  displayed as complete.
                </div>
              ) : null}
              {response.data.error_code || response.data.error_message ? (
                <div className="notice notice--critical" role="alert">
                  <strong>
                    {response.data.error_code ?? "VALIDATION_ERROR"}
                  </strong>
                  <p>
                    <Value>
                      {response.data.error_message ??
                        "No safe error message was provided."}
                    </Value>
                  </p>
                </div>
              ) : null}
            </Card>
            <Card title="Authority inputs">
              <DefinitionGrid
                items={[
                  { label: "Job", value: response.data.job_id },
                  {
                    label: "Profile",
                    value: `${response.data.profile_id} · ${response.data.profile_version}`,
                  },
                  {
                    label: "Profile hash",
                    value: <Hash>{response.data.profile_hash}</Hash>,
                  },
                  {
                    label: "Mapping",
                    value: `${response.data.mapping_id} · ${response.data.mapping_version}`,
                  },
                  {
                    label: "Mapping hash",
                    value: <Hash>{response.data.mapping_hash}</Hash>,
                  },
                ]}
              />
            </Card>
            <Card title="Execution evidence">
              <DefinitionGrid
                items={[
                  {
                    label: "Rule set",
                    value: <Hash>{response.data.rule_set_hash}</Hash>,
                  },
                  {
                    label: "Policy",
                    value: <Hash>{response.data.policy_hash}</Hash>,
                  },
                  { label: "Engine", value: response.data.engine_version },
                  {
                    label: "Created",
                    value: response.data.timestamps.created_at,
                  },
                  {
                    label: "Updated",
                    value: response.data.timestamps.updated_at,
                  },
                  {
                    label: "Completed",
                    value:
                      response.data.timestamps.completed_at ?? "Not completed",
                  },
                ]}
              />
            </Card>
            <Card title="Issue breakdowns" className="card--wide">
              <div className="breakdown-grid">
                {Object.entries(response.data.issue_breakdowns).map(
                  ([label, group]) => (
                    <section key={label}>
                      <h3>
                        {label.replace("by_", "By ").replaceAll("_", " ")}
                      </h3>
                      {group.items.length ? (
                        <ul>
                          {group.items.map((item) => (
                            <li key={item.key}>
                              <Value>{item.key}</Value>
                              <strong>{item.count}</strong>
                            </li>
                          ))}
                        </ul>
                      ) : (
                        <p className="muted">No grouped issues</p>
                      )}
                      {group.truncated ? (
                        <p className="supporting">
                          Server-reported group is truncated.
                        </p>
                      ) : null}
                    </section>
                  ),
                )}
              </div>
            </Card>
            {response.data.state === "COMPLETE" && engagementBound ? (
              <Card title="Refresh this source" className="card--wide">
                <p>
                  Accept another immutable source revision in this engagement,
                  replay exact approved authority, and compare durable findings.
                </p>
                <AppLink
                  className="button button--secondary"
                  href="/comparison#start-source-refresh"
                >
                  Start source refresh
                </AppLink>
              </Card>
            ) : null}
          </div>
        )}
      </ReadBoundary>
    </>
  );
}

const ISSUE_FILTERS = [
  ["severity", "Severity"],
  ["rule_id", "Rule"],
  ["entity_id", "Entity"],
  ["field_id", "Field"],
  ["source_file", "Accepted file"],
  ["source_ordinal", "Source row"],
] as const;

export function IssuesPage({
  location,
  decisionWritesEnabled = false,
  engagementBound = false,
}: {
  location: URL;
  decisionWritesEnabled?: boolean;
  engagementBound?: boolean;
}) {
  const { pageSize, cursor } = pageRequest(location);
  const values = Object.fromEntries(
    ISSUE_FILTERS.map(([key]) => [key, location.searchParams.get(key)]),
  );
  const read = useRead<IssueListPage>(
    apiQuery("/api/v1/issues", { page_size: pageSize, cursor, ...values }),
    SCHEMAS.issues,
  );
  function filter(event: FormEvent<HTMLFormElement>): void {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const query = new URLSearchParams();
    const requestedPageSize = Number(form.get("page_size"));
    if (
      !Number.isInteger(requestedPageSize) ||
      requestedPageSize < 1 ||
      requestedPageSize > 200
    ) {
      return;
    }
    query.set("page_size", String(requestedPageSize));
    for (const [key] of ISSUE_FILTERS) {
      const value = formText(form, key).trim();
      if (value) query.set(key, value);
    }
    navigate(`/issues?${query.toString()}`);
  }
  return (
    <>
      <PageHeader
        eyebrow="Bounded issue evidence"
        title="Issues"
        description="Equality-only filters and authoritative opaque cursor pagination. Broad issue pages contain no received source values."
      />
      {engagementBound ? (
        <>
          <div className="action-stack">
            <AppLink
              className="button button--secondary"
              href="/comparison#start-source-refresh"
            >
              Start source refresh
            </AppLink>
          </div>
          <RemediationPackPanel />
        </>
      ) : null}
      <form
        className="filter-panel"
        aria-label="Issue equality filters"
        onSubmit={filter}
      >
        {ISSUE_FILTERS.map(([key, label]) => (
          <label key={key}>
            {label}
            {key === "severity" ? (
              <select name={key} defaultValue={values[key] ?? ""}>
                <option value="">All severities</option>
                <option value="ERROR">Error</option>
                <option value="WARNING">Warning</option>
                <option value="INFORMATION">Information</option>
              </select>
            ) : (
              <input
                name={key}
                defaultValue={values[key] ?? ""}
                inputMode={key === "source_ordinal" ? "numeric" : undefined}
              />
            )}
          </label>
        ))}
        <label>
          Page size
          <input
            name="page_size"
            type="number"
            min="1"
            max="200"
            defaultValue={pageSize}
          />
        </label>
        <div className="filter-actions">
          <button className="button" type="submit">
            Apply filters
          </button>
          <AppLink
            className="button button--secondary"
            href={`/issues?page_size=${pageSize}`}
          >
            Clear
          </AppLink>
        </div>
      </form>
      <ReadBoundary {...read}>
        {(response) =>
          response.items.length ? (
            <>
              <TableShell label="Validation issues">
                <table>
                  <thead>
                    <tr>
                      <th scope="col">Severity</th>
                      <th scope="col">Code and message</th>
                      <th scope="col">Rule</th>
                      <th scope="col">Canonical field</th>
                      <th scope="col">Accepted row</th>
                      <th scope="col">Detail</th>
                    </tr>
                  </thead>
                  <tbody>
                    {response.items.map((item) => {
                      const rowHref = `/rows/${item.source_ordinal}?source_file=${encodeURIComponent(item.source_file)}`;
                      return (
                        <tr key={item.issue_id}>
                          <td>
                            <Status value={item.severity} />
                          </td>
                          <th scope="row">
                            <div className="issue-summary">
                              <strong>{item.issue_code}</strong>
                              <Value>{item.display_message}</Value>
                            </div>
                          </th>
                          <td>
                            <Value>{item.rule_id}</Value>
                          </td>
                          <td>
                            <Value>
                              {item.entity_id}.{item.field_id}
                            </Value>
                          </td>
                          <td>
                            <Value>
                              {item.source_file} · {item.source_ordinal}
                            </Value>
                            <small>
                              lines {item.line_start}–{item.line_end}
                            </small>
                          </td>
                          <td>
                            <div className="table-actions">
                              <AppLink href={rowHref}>Open exact row</AppLink>
                              {decisionWritesEnabled ? (
                                <AppLink
                                  href={`/issues/${encodeURIComponent(item.issue_id)}`}
                                >
                                  Open governed detail
                                </AppLink>
                              ) : null}
                            </div>
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </TableShell>
              <Pagination
                page={response.page}
                firstHref={firstPageHref("/issues", location)}
                nextHref={pageHref(
                  "/issues",
                  location,
                  response.page.next_cursor,
                )}
              />
            </>
          ) : (
            <EmptyState>
              No issue matches this bounded page and filter set.
            </EmptyState>
          )
        }
      </ReadBoundary>
    </>
  );
}

export function RowDetailPage({
  location,
  ordinal,
  governedDecisionsEnabled,
}: {
  location: URL;
  ordinal: number;
  governedDecisionsEnabled: boolean;
}) {
  const sourceFile = location.searchParams.get("source_file") ?? "";
  if (!sourceFile || !Number.isSafeInteger(ordinal) || ordinal <= 0) {
    return (
      <>
        <PageHeader
          eyebrow="Bounded contextual read"
          title="Row Detail"
          description="One accepted file and ordinal must be explicitly selected from Issues."
        />
        <div className="state-panel state-panel--error" role="alert">
          <div>
            <strong>Invalid row selection</strong>
            <p>Return to Issues and choose one accepted row.</p>
            <AppLink href="/issues">Back to Issues</AppLink>
          </div>
        </div>
      </>
    );
  }
  return (
    <RowDetailRead
      sourceFile={sourceFile}
      ordinal={ordinal}
      governedDecisionsEnabled={governedDecisionsEnabled}
    />
  );
}

function RowDetailRead({
  sourceFile,
  ordinal,
  governedDecisionsEnabled,
}: {
  sourceFile: string;
  ordinal: number;
  governedDecisionsEnabled: boolean;
}) {
  const read = useRead<RowDetail>(
    apiQuery(`/api/v1/rows/${ordinal}`, { source_file: sourceFile }),
    SCHEMAS.row,
  );
  return (
    <>
      <PageHeader
        eyebrow="Explicit bounded read"
        title="Row Detail"
        description="One immutable received row is visually separated from its canonical projection and lineage."
        action={
          <AppLink className="button button--secondary" href="/issues">
            Back to Issues
          </AppLink>
        }
      />
      <ReadBoundary {...read}>
        {(response) => (
          <>
            <Card title="Row identity">
              <DefinitionGrid
                items={[
                  {
                    label: "Accepted file",
                    value: <Value>{response.data.source_file}</Value>,
                  },
                  {
                    label: "Source ordinal",
                    value: response.data.source_ordinal,
                  },
                  {
                    label: "Lines",
                    value: `${response.data.line_start}–${response.data.line_end}`,
                  },
                  { label: "Validation run", value: response.data.run_id },
                  { label: "Job", value: response.data.job_id },
                ]}
              />
            </Card>
            <div className="row-evidence-grid">
              <Card
                eyebrow="Source evidence"
                title="Immutable received text"
                className="source-evidence"
              >
                {response.data.source_values.length ? (
                  <TableShell label="Immutable received source values">
                    <table>
                      <thead>
                        <tr>
                          <th scope="col">Column</th>
                          <th scope="col">Received text</th>
                        </tr>
                      </thead>
                      <tbody>
                        {response.data.source_values.map((item) => (
                          <tr key={item.column_ordinal}>
                            <th scope="row">
                              {item.column_ordinal} ·{" "}
                              <Value>{item.column_name}</Value>
                            </th>
                            <td>
                              <Value>{item.value}</Value>
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </TableShell>
                ) : (
                  <EmptyState>
                    No received value is reported for this exact row.
                  </EmptyState>
                )}
              </Card>
              <Card
                eyebrow="Canonical evidence"
                title="Projection and lineage"
                className="canonical-evidence"
              >
                {response.data.canonical_projection.length ? (
                  <TableShell label="Canonical projection and lineage">
                    <table>
                      <thead>
                        <tr>
                          <th scope="col">Canonical field</th>
                          <th scope="col">Projected text</th>
                          <th scope="col">Lineage</th>
                        </tr>
                      </thead>
                      <tbody>
                        {response.data.canonical_projection.map((item) => (
                          <tr key={`${item.entity_id}:${item.field_id}`}>
                            <th scope="row">
                              <Value>
                                {item.entity_id}.{item.field_id}
                              </Value>
                            </th>
                            <td>
                              <Value>{item.value ?? "Not projected"}</Value>
                            </td>
                            <td>
                              <Status value={item.lineage.kind} />
                              <small>
                                {item.lineage.binding_id}
                                {item.lineage.source_column_name
                                  ? ` · ${item.lineage.source_column_name}`
                                  : ""}
                                {item.lineage.lookup_table_id
                                  ? ` · lookup ${item.lineage.lookup_table_id}`
                                  : ""}
                              </small>
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </TableShell>
                ) : (
                  <EmptyState>No canonical projection is reported.</EmptyState>
                )}
              </Card>
            </div>
            <Card title={`Associated issues (${response.data.issues.length})`}>
              {response.data.issues.length ? (
                <ul className="issue-cards">
                  {response.data.issues.map((issue) => (
                    <li key={issue.issue_id}>
                      <Status value={issue.severity} />
                      <strong>{issue.issue_code}</strong>
                      <Value>{issue.display_message}</Value>
                      <small>
                        {issue.rule_id} · {issue.entity_id}.{issue.field_id}
                        {governedDecisionsEnabled ? (
                          <>
                            {" · "}
                            <AppLink
                              href={`/issues/${encodeURIComponent(issue.issue_id)}`}
                            >
                              Open governed context
                            </AppLink>
                          </>
                        ) : null}
                      </small>
                    </li>
                  ))}
                </ul>
              ) : (
                <EmptyState>
                  No issue is associated with this exact row.
                </EmptyState>
              )}
            </Card>
          </>
        )}
      </ReadBoundary>
    </>
  );
}

export function ReconciliationPage({ location }: { location: URL }) {
  const { pageSize, cursor } = pageRequest(location);
  const read = useRead<ReconciliationSummary>(
    apiQuery("/api/v1/reconciliation", { page_size: pageSize, cursor }),
    SCHEMAS.reconciliation,
  );
  return (
    <>
      <PageHeader
        eyebrow="Contractual closure evidence"
        title="Reconciliation"
        description="Accepted source, stored, pending, and validation coverage counts without publication or result approval."
        action={<PageSizeControl location={location} />}
      />
      <ReadBoundary {...read}>
        {(response) => (
          <>
            <Card title="Aggregate reconciliation">
              <DefinitionGrid
                items={[
                  {
                    label: "Closure",
                    value: <Status value={response.aggregate.closed} />,
                  },
                  {
                    label: "Source records",
                    value: displayNumber(response.aggregate.source_records),
                  },
                  {
                    label: "Stored records",
                    value: displayNumber(response.aggregate.stored_records),
                  },
                  {
                    label: "Pending records",
                    value: displayNumber(response.aggregate.pending_records),
                  },
                  {
                    label: "Validation rows",
                    value: displayNumber(
                      response.aggregate.validation_rows_processed,
                    ),
                  },
                  {
                    label: "Validation issues",
                    value: displayNumber(
                      response.aggregate.validation_issue_count,
                    ),
                  },
                ]}
              />
            </Card>
            {response.items.length ? (
              <TableShell label="Per-file reconciliation">
                <table>
                  <thead>
                    <tr>
                      <th scope="col">Accepted file</th>
                      <th scope="col">Source</th>
                      <th scope="col">Stored</th>
                      <th scope="col">Pending</th>
                      <th scope="col">Closure</th>
                      <th scope="col">Validation issues</th>
                    </tr>
                  </thead>
                  <tbody>
                    {response.items.map((item) => (
                      <tr key={item.path}>
                        <th scope="row">
                          <Value>{item.path}</Value>
                        </th>
                        <td>{item.source_records}</td>
                        <td>{item.stored_records}</td>
                        <td>{item.pending_records}</td>
                        <td>
                          <Status value={item.closed} />
                        </td>
                        <td>{displayNumber(item.validation_issue_count)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </TableShell>
            ) : (
              <EmptyState>
                No per-file reconciliation evidence exists on this page.
              </EmptyState>
            )}
            <Pagination
              page={response.page}
              firstHref={firstPageHref("/reconciliation", location)}
              nextHref={pageHref(
                "/reconciliation",
                location,
                response.page.next_cursor,
              )}
            />
          </>
        )}
      </ReadBoundary>
    </>
  );
}

export function OperationsPage({ location }: { location: URL }) {
  const { pageSize, cursor } = pageRequest(location);
  const namespace = location.searchParams.get("namespace");
  const state = location.searchParams.get("state");
  const read = useRead<OperationalCleanupPage>(
    apiQuery("/api/v1/operations", {
      page_size: pageSize,
      cursor,
      namespace,
      state,
    }),
    SCHEMAS.operations,
  );
  function filter(event: FormEvent<HTMLFormElement>): void {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const query = new URLSearchParams();
    query.set("page_size", String(pageSize));
    for (const key of ["namespace", "state"] as const) {
      const value = formText(form, key).trim();
      if (value) query.set(key, value);
    }
    navigate(`/operations?${query.toString()}`);
  }
  return (
    <>
      <PageHeader
        eyebrow="Operational evidence only"
        title="Operations"
        description="Bounded cleanup warning evidence without tombstone names, paths, contents, diagnostics, or recovery actions."
      />
      <form
        className="filter-panel filter-panel--compact"
        aria-label="Cleanup equality filters"
        onSubmit={filter}
      >
        <label>
          Namespace
          <select name="namespace" defaultValue={namespace ?? ""}>
            <option value="">All namespaces</option>
            <option value="packages">Packages</option>
            <option value="metadata">Metadata</option>
            <option value="catalogues">Catalogues</option>
          </select>
        </label>
        <label>
          State
          <select name="state" defaultValue={state ?? ""}>
            <option value="">All states</option>
            <option value="PLANNED">Planned</option>
            <option value="QUARANTINED">Quarantined</option>
            <option value="CLEANUP_PENDING">Cleanup pending</option>
            <option value="CLEANED">Cleaned</option>
            <option value="CONFLICT">Conflict</option>
          </select>
        </label>
        <div className="filter-actions">
          <button className="button" type="submit">
            Apply filters
          </button>
          <AppLink className="button button--secondary" href="/operations">
            Clear
          </AppLink>
        </div>
      </form>
      <ReadBoundary {...read}>
        {(response) =>
          response.items.length ? (
            <>
              <div className="card-list">
                {response.items.map((item) => (
                  <Card
                    key={item.cleanup_id}
                    eyebrow={item.namespace}
                    title={item.state}
                  >
                    <DefinitionGrid
                      items={[
                        {
                          label: "State",
                          value: <Status value={item.state} />,
                        },
                        {
                          label: "Cleanup attempts",
                          value: item.cleanup_attempts,
                        },
                        {
                          label: "Last safe error code",
                          value: item.last_error_code ?? "None reported",
                        },
                        {
                          label: "Diagnostic retained",
                          value: <Status value={item.diagnostic_present} />,
                        },
                        { label: "Created", value: item.created_at },
                        { label: "Updated", value: item.updated_at },
                        {
                          label: "Quarantined",
                          value: item.quarantined_at ?? "Not reported",
                        },
                        {
                          label: "Cleaned",
                          value: item.cleaned_at ?? "Not reported",
                        },
                      ]}
                    />
                    <p className="supporting">
                      Operational evidence does not change job, source, profile,
                      mapping, validation, or result authority.
                    </p>
                  </Card>
                ))}
              </div>
              <Pagination
                page={response.page}
                firstHref={firstPageHref("/operations", location)}
                nextHref={pageHref(
                  "/operations",
                  location,
                  response.page.next_cursor,
                )}
              />
            </>
          ) : (
            <EmptyState>
              No cleanup warning matches this bounded page and filter set.
            </EmptyState>
          )
        }
      </ReadBoundary>
    </>
  );
}

export function NotFoundPage(): ReactNode {
  return (
    <>
      <PageHeader
        eyebrow="Unknown workbench route"
        title="Page not found"
        description="The requested local read-only view does not exist."
      />
      <Card>
        <AppLink href="/">Return to Overview</AppLink>
      </Card>
    </>
  );
}
