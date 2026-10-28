#![allow(
    clippy::missing_errors_doc,
    clippy::too_many_lines,
    clippy::manual_let_else
)]

//! WP-007 Publication Service Implementation.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;
use csv::{Terminator, WriterBuilder};
use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

use crate::error::{KernelError, Result};
use crate::publication::{
    MAX_PACKAGE_SIZE_BYTES, MEMBER_APPROVAL_EVIDENCE, MEMBER_EFFECTIVE_ROWS_CSV,
    MEMBER_EFFECTIVE_ROWS_JSON, MEMBER_EXCLUSION_EVIDENCE, MEMBER_ISSUE_DECISION_EVIDENCE,
    MEMBER_MANIFEST, MEMBER_PROVENANCE, MEMBER_RECONCILIATION_EVIDENCE, ManifestMember,
    PUBLICATION_COMMAND_SCHEMA, PUBLICATION_CONTRACT_VERSION, PUBLICATION_DETAIL_SCHEMA,
    PUBLICATION_NON_AUTHORITY_DISCLAIMER, PublicationEligibility, PublicationManifest,
    PublicationRecord, PublicationRequest, PublicationStatus,
};
use crate::publication_read_model::get_publication_eligibility_from_conn;

/// Publication Service managing workspace package creation and state.
#[derive(Clone, Debug)]
pub struct PublicationService {
    workspace: PathBuf,
}

impl PublicationService {
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
        }
    }

    pub fn publish(&self, request: &PublicationRequest) -> Result<PublicationRecord> {
        if request.schema_version != PUBLICATION_COMMAND_SCHEMA {
            return Err(KernelError::Reconciliation(format!(
                "Publication command contract mismatch: expected {PUBLICATION_COMMAND_SCHEMA}"
            )));
        }
        let canonical_request_json =
            crate::canonical::canonical_json_string(&serde_json::to_value(request)?)?;
        let request_hash = sha256_hex(canonical_request_json.as_bytes());

        // Hold the verified workspace writer identity from before the first
        // package byte through immutable record registration.
        let writer = crate::state::WorkspaceWriter::open(&self.workspace)?;
        let connection = writer.connection();

        // 1. Return only an exact idempotent replay before re-evaluating mutable heads.
        let existing_record = get_existing_idempotent_publication(
            connection,
            &request.job_id,
            &request.idempotency_key,
            &request_hash,
        )?;
        if let Some(record) = existing_record {
            return Ok(record);
        }

        // 2. Verify eligibility
        let eligibility = get_publication_eligibility_from_conn(connection, &request.job_id)?;
        if !eligibility.is_eligible {
            return Err(KernelError::Reconciliation(format!(
                "Job {} is not eligible for publication: {}",
                request.job_id,
                eligibility.blockers.join("; ")
            )));
        }

        let candidate_id = eligibility.candidate_id.as_deref().ok_or_else(|| {
            KernelError::Reconciliation(
                "Missing candidate identity in eligibility evaluation".to_owned(),
            )
        })?;
        let candidate_hash = eligibility.candidate_hash.as_deref().ok_or_else(|| {
            KernelError::Reconciliation(
                "Missing candidate hash in eligibility evaluation".to_owned(),
            )
        })?;
        let approval_id = eligibility.approval_id.as_deref().ok_or_else(|| {
            KernelError::Reconciliation(
                "Missing approval identity in eligibility evaluation".to_owned(),
            )
        })?;
        let approval_hash = eligibility.approval_hash.as_deref().ok_or_else(|| {
            KernelError::Reconciliation(
                "Missing approval hash in eligibility evaluation".to_owned(),
            )
        })?;

        if candidate_id != request.candidate_id || approval_id != request.approval_id {
            return Err(KernelError::Reconciliation(
                "Publication request candidate or approval ID does not match active eligible head"
                    .to_owned(),
            ));
        }

        if eligibility
            .required_acknowledgments
            .iter()
            .any(|required| !request.acknowledgments.contains(required))
        {
            return Err(KernelError::Reconciliation(
                "Publication request is missing a required non-authority acknowledgment".to_owned(),
            ));
        }

        let pub_id = format!("pub_{}", uuid::Uuid::new_v4().simple());
        let created_at = Utc::now().to_rfc3339();

        // 3. Prepare workspace directories
        let base_workspace = self.workspace.join("publications");
        let staging_dir = base_workspace.join("staging").join(&pub_id);
        let completed_dir = base_workspace.join("completed").join(&pub_id);

        fs::create_dir_all(&staging_dir).map_err(|e| io_err(&staging_dir, e))?;
        fs::create_dir_all(&completed_dir).map_err(|e| io_err(&completed_dir, e))?;

        // 4. Generate package member contents
        let governance = publication_governance(connection, &request.job_id, candidate_id)?;
        let (members_data, manifest) = generate_package_contents(
            connection,
            &pub_id,
            &request.job_id,
            candidate_id,
            candidate_hash,
            approval_id,
            approval_hash,
            &request.practitioner,
            &request.publication_reason,
            &created_at,
            &eligibility,
            &governance,
        )?;

        // 5. Build ZIP container deterministically
        let safe_zip_filename = format!("wp007-readiness-{}-{}.zip", request.job_id, pub_id);
        let staging_zip_path = staging_dir.join(&safe_zip_filename);
        let completed_zip_path = completed_dir.join(&safe_zip_filename);

        let (package_size_bytes, physical_archive_digest) =
            write_deterministic_zip(&staging_zip_path, &members_data)?;

        if package_size_bytes > MAX_PACKAGE_SIZE_BYTES {
            let _ = fs::remove_dir_all(&staging_dir);
            let _ = fs::remove_dir_all(&completed_dir);
            return Err(KernelError::Reconciliation(format!(
                "Publication package size ({package_size_bytes} bytes) exceeds maximum allowed bound ({MAX_PACKAGE_SIZE_BYTES} bytes)"
            )));
        }

        // 6. Atomic finalization: move zip to completed directory
        fs::rename(&staging_zip_path, &completed_zip_path)
            .map_err(|e| io_err(&staging_zip_path, e))?;
        let _ = fs::remove_dir_all(&staging_dir);

        let archive_relative_path = format!("publications/completed/{pub_id}/{safe_zip_filename}");

        // 7. Persist publication attempt and record in database via WorkspaceWriter
        let record = PublicationRecord {
            schema_version: PUBLICATION_DETAIL_SCHEMA.to_owned(),
            publication_id: pub_id.clone(),
            job_id: request.job_id.clone(),
            candidate_id: candidate_id.to_owned(),
            candidate_hash: candidate_hash.to_owned(),
            approval_id: approval_id.to_owned(),
            approval_hash: approval_hash.to_owned(),
            contract_version: PUBLICATION_CONTRACT_VERSION.to_owned(),
            status: PublicationStatus::Complete,
            practitioner: request.practitioner.clone(),
            publication_reason: request.publication_reason.clone(),
            semantic_publication_hash: manifest.semantic_publication_hash.clone(),
            physical_archive_digest,
            package_size_bytes,
            member_count: manifest.member_inventory.len(),
            included_row_count: eligibility.included_rows,
            excluded_row_count: eligibility.excluded_rows,
            archive_relative_path,
            manifest,
            created_at: created_at.clone(),
            completed_at: created_at,
        };

        persist_publication_record(
            connection,
            &record,
            &request.idempotency_key,
            &request_hash,
            &canonical_request_json,
        )?;

        Ok(record)
    }
}

struct MemberContent {
    filename: &'static str,
    bytes: Vec<u8>,
    #[allow(dead_code)]
    row_count: Option<usize>,
    #[allow(dead_code)]
    sha256: String,
}

struct PublicationGovernance {
    source_package_digest: String,
    profile_id: String,
    profile_version: String,
    profile_hash: String,
    mapping_id: String,
    mapping_version: String,
    mapping_hash: String,
    validation_run_id: String,
    validation_result_hash: String,
    reconciliation_snapshot_id: String,
    reconciliation_snapshot_hash: String,
    policy_identity: String,
    policy_version: String,
    policy_hash: String,
    policy_activation_id: String,
    policy_activation_hash: String,
    engine_version: String,
    canonicalization_version: String,
}

impl PublicationGovernance {
    fn validate(&self) -> Result<()> {
        for (label, value) in [
            ("profile ID", self.profile_id.as_str()),
            ("profile version", self.profile_version.as_str()),
            ("mapping ID", self.mapping_id.as_str()),
            ("mapping version", self.mapping_version.as_str()),
            ("validation run ID", self.validation_run_id.as_str()),
            (
                "reconciliation snapshot ID",
                self.reconciliation_snapshot_id.as_str(),
            ),
            ("policy identity", self.policy_identity.as_str()),
            ("policy version", self.policy_version.as_str()),
            ("policy activation ID", self.policy_activation_id.as_str()),
            ("engine version", self.engine_version.as_str()),
            (
                "canonicalization version",
                self.canonicalization_version.as_str(),
            ),
        ] {
            validate_governing_identifier(label, value)?;
        }
        for (label, value) in [
            ("source package digest", self.source_package_digest.as_str()),
            ("profile digest", self.profile_hash.as_str()),
            ("mapping digest", self.mapping_hash.as_str()),
            (
                "validation result digest",
                self.validation_result_hash.as_str(),
            ),
            (
                "reconciliation snapshot digest",
                self.reconciliation_snapshot_hash.as_str(),
            ),
            ("policy digest", self.policy_hash.as_str()),
            (
                "policy activation digest",
                self.policy_activation_hash.as_str(),
            ),
        ] {
            validate_governing_digest(label, value)?;
        }
        Ok(())
    }
}

type PublicationRowCategories = BTreeMap<(String, u64), String>;

#[derive(Default)]
struct PublicationCategoryCounts {
    included_valid: usize,
    included_corrected: usize,
    included_waived: usize,
    included_accepted_as_is: usize,
    excluded: usize,
    blocked_unresolved: usize,
    blocked_incomplete: usize,
    blocked_conflict: usize,
}

impl PublicationCategoryCounts {
    fn included(&self) -> usize {
        self.included_valid
            + self.included_corrected
            + self.included_waived
            + self.included_accepted_as_is
    }

    fn blocked(&self) -> usize {
        self.blocked_unresolved + self.blocked_incomplete + self.blocked_conflict
    }

    fn total(&self) -> usize {
        self.included() + self.excluded + self.blocked()
    }
}

#[derive(Serialize)]
struct EffectivePublicationRow {
    source_file: String,
    source_ordinal: u64,
    line_start: u64,
    line_end: u64,
    entity_id: String,
    category: String,
    canonical_values: BTreeMap<String, Option<String>>,
    lineage: BTreeMap<String, Value>,
}

#[derive(Clone, Serialize)]
struct PublicationDecisionEvidence {
    source_file: String,
    source_ordinal: u64,
    line_start: u64,
    line_end: u64,
    entity_id: String,
    field_id: String,
    issue_id: String,
    issue_hash: String,
    severity: String,
    rule_id: String,
    issue_code: String,
    decision_id: String,
    decision_hash: String,
    disposition: String,
    generation: u64,
    practitioner: String,
    reason: String,
    evidence_reference: Option<String>,
    committed_at: String,
    correction_overlay_id: Option<String>,
    correction_overlay_hash: Option<String>,
    correction_effectivation_id: Option<String>,
    correction_effectivation_hash: Option<String>,
    effective_value_hash: Option<String>,
    reevaluation_id: Option<String>,
    reevaluation_hash: Option<String>,
}

#[derive(Serialize)]
struct ExcludedRowEvidence {
    source_file: String,
    source_ordinal: u64,
    line_start: u64,
    line_end: u64,
    entity_id: String,
    row_identity_hash: String,
    classification_hash: String,
    explanation_codes: Vec<String>,
    exclusion_decisions: Vec<PublicationDecisionEvidence>,
}

struct PublicationApprovalEvidence {
    approval_id: String,
    approval_hash: String,
    action_kind: String,
    actor: String,
    reason: String,
    generation: u64,
    warning_acknowledgments: Vec<String>,
    warning_acknowledgments_hash: String,
    authority_boundary_acknowledged: bool,
    occurred_at: String,
    candidate_creator: String,
    candidate_preparation_reason: String,
    candidate_required_acknowledgments: Vec<String>,
    candidate_required_acknowledgments_hash: String,
}

fn publication_governance(
    connection: &Connection,
    job_id: &str,
    candidate_id: &str,
) -> Result<PublicationGovernance> {
    connection
        .query_row(
            "SELECT s.accepted_source_hash, s.profile_id, s.profile_version,
                    s.profile_hash, s.mapping_id, s.mapping_version, s.mapping_hash,
                    s.validation_run_id, s.validation_result_hash,
                    s.snapshot_id, s.snapshot_hash,
                    s.policy_identity, s.policy_version, s.policy_hash,
                    s.policy_activation_id, s.policy_activation_hash,
                    s.engine_version, s.canonicalization_version
             FROM migration_result_candidates c
             JOIN reconciliation_snapshots s
               ON s.snapshot_id = c.snapshot_id
              AND s.snapshot_hash = c.snapshot_hash
              AND s.job_id = c.job_id
             WHERE c.job_id = ? AND c.candidate_id = ?",
            [job_id, candidate_id],
            |row| {
                Ok(PublicationGovernance {
                    source_package_digest: row.get(0)?,
                    profile_id: row.get(1)?,
                    profile_version: row.get(2)?,
                    profile_hash: row.get(3)?,
                    mapping_id: row.get(4)?,
                    mapping_version: row.get(5)?,
                    mapping_hash: row.get(6)?,
                    validation_run_id: row.get(7)?,
                    validation_result_hash: row.get(8)?,
                    reconciliation_snapshot_id: row.get(9)?,
                    reconciliation_snapshot_hash: row.get(10)?,
                    policy_identity: row.get(11)?,
                    policy_version: row.get(12)?,
                    policy_hash: row.get(13)?,
                    policy_activation_id: row.get(14)?,
                    policy_activation_hash: row.get(15)?,
                    engine_version: row.get(16)?,
                    canonicalization_version: row.get(17)?,
                })
            },
        )
        .map_err(KernelError::Sqlite)
}

fn publication_row_categories(
    connection: &Connection,
    job_id: &str,
    snapshot_id: &str,
) -> Result<(PublicationRowCategories, PublicationCategoryCounts)> {
    let mut statement = connection.prepare(
        "SELECT source_file, source_ordinal, category
         FROM reconciliation_row_classifications
         WHERE job_id = ?1 AND snapshot_id = ?2
         ORDER BY source_file, source_ordinal",
    )?;
    let rows = statement.query_map(params![job_id, snapshot_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut categories = BTreeMap::new();
    let mut counts = PublicationCategoryCounts::default();
    for row in rows {
        let (source_file, source_ordinal, category) = row?;
        let source_ordinal = u64::try_from(source_ordinal).map_err(|_| {
            KernelError::Reconciliation(
                "publication snapshot contains a negative source ordinal".to_owned(),
            )
        })?;
        match category.as_str() {
            "INCLUDED_VALID" => counts.included_valid += 1,
            "INCLUDED_CORRECTED" => counts.included_corrected += 1,
            "INCLUDED_WAIVED" => counts.included_waived += 1,
            "INCLUDED_ACCEPTED_AS_IS" => counts.included_accepted_as_is += 1,
            "EXCLUDED" => counts.excluded += 1,
            "BLOCKED_UNRESOLVED" => counts.blocked_unresolved += 1,
            "BLOCKED_INCOMPLETE" => counts.blocked_incomplete += 1,
            "BLOCKED_CONFLICT" => counts.blocked_conflict += 1,
            _ => {
                return Err(KernelError::Reconciliation(format!(
                    "publication snapshot contains unsupported category {category}"
                )));
            }
        }
        if categories
            .insert((source_file, source_ordinal), category)
            .is_some()
        {
            return Err(KernelError::Reconciliation(
                "publication snapshot contains a duplicate row identity".to_owned(),
            ));
        }
    }
    Ok((categories, counts))
}

fn publication_decision_evidence(
    connection: &Connection,
    job_id: &str,
    snapshot_id: &str,
) -> Result<Vec<PublicationDecisionEvidence>> {
    struct EvidenceRow {
        source_file: String,
        source_ordinal: i64,
        line_start: i64,
        line_end: i64,
        entity_id: String,
        field_id: String,
        issue_id: String,
        issue_hash: String,
        severity: String,
        rule_id: String,
        issue_code: String,
        decision_id: String,
        decision_hash: String,
        disposition: String,
        generation: i64,
        practitioner: String,
        reason: String,
        evidence_reference: Option<String>,
        committed_at: String,
        correction_overlay_id: Option<String>,
        correction_overlay_hash: Option<String>,
        correction_effectivation_id: Option<String>,
        correction_effectivation_hash: Option<String>,
        effective_value_hash: Option<String>,
        reevaluation_id: Option<String>,
        reevaluation_hash: Option<String>,
    }

    let mut statement = connection.prepare(
        "SELECT c.source_file, c.source_ordinal, c.line_start, c.line_end,
                c.entity_id, vi.field_id, d.issue_id, vi.issue_hash,
                vi.severity, vi.rule_id, vi.issue_code,
                d.decision_id, d.decision_hash, d.disposition, d.generation,
                d.practitioner_id, d.reason, d.evidence_reference, d.committed_at,
                d.correction_overlay_id, o.overlay_hash,
                ce.effectivation_id, ce.effectivation_hash, ce.effective_value_hash,
                d.reevaluation_id, r.terminal_hash
         FROM reconciliation_row_lineage_references reference
         JOIN reconciliation_row_classifications c
           ON c.snapshot_id = reference.snapshot_id
          AND c.source_file = reference.source_file
          AND c.source_ordinal = reference.source_ordinal
         JOIN issue_decisions d
           ON d.decision_id = reference.reference_id
          AND d.decision_hash = reference.reference_hash
          AND d.job_id = c.job_id
         JOIN validation_issues vi
           ON vi.run_id = d.validation_run_id
          AND vi.issue_id = d.issue_id
          AND vi.source_file = c.source_file
          AND vi.source_ordinal = c.source_ordinal
         LEFT JOIN correction_overlays o
           ON o.overlay_id = d.correction_overlay_id
         LEFT JOIN correction_effectivations ce
           ON ce.decision_id = d.decision_id
         LEFT JOIN reevaluation_runs r
           ON r.reevaluation_id = d.reevaluation_id
         WHERE reference.snapshot_id = ?1
           AND c.job_id = ?2
           AND reference.reference_kind = 'ISSUE_DECISION'
         ORDER BY c.source_file, c.source_ordinal, d.issue_id, d.decision_id",
    )?;
    let rows = statement.query_map(params![snapshot_id, job_id], |row| {
        Ok(EvidenceRow {
            source_file: row.get(0)?,
            source_ordinal: row.get(1)?,
            line_start: row.get(2)?,
            line_end: row.get(3)?,
            entity_id: row.get(4)?,
            field_id: row.get(5)?,
            issue_id: row.get(6)?,
            issue_hash: row.get(7)?,
            severity: row.get(8)?,
            rule_id: row.get(9)?,
            issue_code: row.get(10)?,
            decision_id: row.get(11)?,
            decision_hash: row.get(12)?,
            disposition: row.get(13)?,
            generation: row.get(14)?,
            practitioner: row.get(15)?,
            reason: row.get(16)?,
            evidence_reference: row.get(17)?,
            committed_at: row.get(18)?,
            correction_overlay_id: row.get(19)?,
            correction_overlay_hash: row.get(20)?,
            correction_effectivation_id: row.get(21)?,
            correction_effectivation_hash: row.get(22)?,
            effective_value_hash: row.get(23)?,
            reevaluation_id: row.get(24)?,
            reevaluation_hash: row.get(25)?,
        })
    })?;
    rows.map(|row| {
        let row = row?;
        Ok(PublicationDecisionEvidence {
            source_file: row.source_file,
            source_ordinal: positive_sql_u64(row.source_ordinal, "decision source ordinal")?,
            line_start: positive_sql_u64(row.line_start, "decision line start")?,
            line_end: positive_sql_u64(row.line_end, "decision line end")?,
            entity_id: row.entity_id,
            field_id: row.field_id,
            issue_id: row.issue_id,
            issue_hash: row.issue_hash,
            severity: row.severity,
            rule_id: row.rule_id,
            issue_code: row.issue_code,
            decision_id: row.decision_id,
            decision_hash: row.decision_hash,
            disposition: row.disposition,
            generation: positive_sql_u64(row.generation, "decision generation")?,
            practitioner: row.practitioner,
            reason: row.reason,
            evidence_reference: row.evidence_reference,
            committed_at: row.committed_at,
            correction_overlay_id: row.correction_overlay_id,
            correction_overlay_hash: row.correction_overlay_hash,
            correction_effectivation_id: row.correction_effectivation_id,
            correction_effectivation_hash: row.correction_effectivation_hash,
            effective_value_hash: row.effective_value_hash,
            reevaluation_id: row.reevaluation_id,
            reevaluation_hash: row.reevaluation_hash,
        })
    })
    .collect()
}

fn publication_exclusion_evidence(
    connection: &Connection,
    job_id: &str,
    snapshot_id: &str,
    decisions: &[PublicationDecisionEvidence],
    expected_excluded_rows: usize,
) -> Result<Vec<ExcludedRowEvidence>> {
    struct ExcludedRow {
        source_file: String,
        source_ordinal: i64,
        line_start: i64,
        line_end: i64,
        entity_id: String,
        row_identity_hash: String,
        classification_hash: String,
        explanation_codes_json: String,
    }

    let mut statement = connection.prepare(
        "SELECT source_file, source_ordinal, line_start, line_end, entity_id,
                row_identity_hash, classification_hash, explanation_codes_json
         FROM reconciliation_row_classifications
         WHERE snapshot_id = ?1 AND job_id = ?2 AND category = 'EXCLUDED'
         ORDER BY source_file, source_ordinal",
    )?;
    let rows = statement.query_map(params![snapshot_id, job_id], |row| {
        Ok(ExcludedRow {
            source_file: row.get(0)?,
            source_ordinal: row.get(1)?,
            line_start: row.get(2)?,
            line_end: row.get(3)?,
            entity_id: row.get(4)?,
            row_identity_hash: row.get(5)?,
            classification_hash: row.get(6)?,
            explanation_codes_json: row.get(7)?,
        })
    })?;
    let mut evidence = Vec::new();
    for row in rows {
        let row = row?;
        let source_ordinal = positive_sql_u64(row.source_ordinal, "excluded source ordinal")?;
        let exclusion_decisions = decisions
            .iter()
            .filter(|decision| {
                decision.source_file == row.source_file
                    && decision.source_ordinal == source_ordinal
                    && decision.disposition == "EXCLUDED"
            })
            .cloned()
            .collect::<Vec<_>>();
        if exclusion_decisions.is_empty() {
            return Err(KernelError::Reconciliation(
                "excluded publication row has no exact governing exclusion decision".to_owned(),
            ));
        }
        let explanation_codes = serde_json::from_str::<Vec<String>>(&row.explanation_codes_json)
            .map_err(KernelError::Json)?;
        evidence.push(ExcludedRowEvidence {
            source_file: row.source_file,
            source_ordinal,
            line_start: positive_sql_u64(row.line_start, "excluded line start")?,
            line_end: positive_sql_u64(row.line_end, "excluded line end")?,
            entity_id: row.entity_id,
            row_identity_hash: row.row_identity_hash,
            classification_hash: row.classification_hash,
            explanation_codes,
            exclusion_decisions,
        });
    }
    if evidence.len() != expected_excluded_rows {
        return Err(KernelError::Reconciliation(
            "excluded publication evidence does not close to eligibility counts".to_owned(),
        ));
    }
    Ok(evidence)
}

fn publication_approval_evidence(
    connection: &Connection,
    job_id: &str,
    candidate_id: &str,
    candidate_hash: &str,
    approval_id: &str,
    approval_hash: &str,
) -> Result<PublicationApprovalEvidence> {
    type ApprovalRow = (
        String,
        String,
        String,
        String,
        String,
        i64,
        String,
        String,
        i64,
        String,
        String,
        String,
        String,
        String,
    );
    let row: ApprovalRow = connection.query_row(
        "SELECT a.action_id, a.action_hash, a.action_kind, a.actor, a.reason,
                a.generation, a.warning_acknowledgments_json,
                a.warning_acknowledgments_hash,
                a.authority_boundary_acknowledged, a.occurred_at,
                c.creator, c.preparation_reason,
                c.required_acknowledgments_json, c.required_acknowledgments_hash
         FROM migration_result_actions a
         JOIN migration_result_candidates c
           ON c.candidate_id = a.candidate_id
          AND c.candidate_hash = a.candidate_hash
          AND c.job_id = a.job_id
         WHERE a.job_id = ?1
           AND a.candidate_id = ?2
           AND a.candidate_hash = ?3
           AND a.action_id = ?4
           AND a.action_hash = ?5",
        params![
            job_id,
            candidate_id,
            candidate_hash,
            approval_id,
            approval_hash
        ],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
                row.get(10)?,
                row.get(11)?,
                row.get(12)?,
                row.get(13)?,
            ))
        },
    )?;
    if row.2 != "APPROVED" || row.8 != 1 {
        return Err(KernelError::Reconciliation(
            "publication approval evidence is not an acknowledged active local approval".to_owned(),
        ));
    }
    Ok(PublicationApprovalEvidence {
        approval_id: row.0,
        approval_hash: row.1,
        action_kind: row.2,
        actor: row.3,
        reason: row.4,
        generation: positive_sql_u64(row.5, "approval generation")?,
        warning_acknowledgments: serde_json::from_str(&row.6).map_err(KernelError::Json)?,
        warning_acknowledgments_hash: row.7,
        authority_boundary_acknowledged: true,
        occurred_at: row.9,
        candidate_creator: row.10,
        candidate_preparation_reason: row.11,
        candidate_required_acknowledgments: serde_json::from_str(&row.12)
            .map_err(KernelError::Json)?,
        candidate_required_acknowledgments_hash: row.13,
    })
}

#[allow(clippy::too_many_arguments)]
fn generate_package_contents(
    connection: &Connection,
    pub_id: &str,
    job_id: &str,
    candidate_id: &str,
    candidate_hash: &str,
    approval_id: &str,
    approval_hash: &str,
    practitioner: &str,
    reason: &str,
    created_at: &str,
    eligibility: &PublicationEligibility,
    governance: &PublicationGovernance,
) -> Result<(Vec<MemberContent>, PublicationManifest)> {
    governance.validate()?;
    let (row_categories, category_counts) =
        publication_row_categories(connection, job_id, &governance.reconciliation_snapshot_id)?;
    if category_counts.total() != eligibility.total_accepted_rows
        || category_counts.included() != eligibility.included_rows
        || category_counts.excluded != eligibility.excluded_rows
        || category_counts.blocked() != eligibility.blocked_rows
    {
        return Err(KernelError::Reconciliation(
            "publication snapshot categories differ from eligibility evidence".to_owned(),
        ));
    }
    let decision_evidence =
        publication_decision_evidence(connection, job_id, &governance.reconciliation_snapshot_id)?;
    let exclusion_evidence = publication_exclusion_evidence(
        connection,
        job_id,
        &governance.reconciliation_snapshot_id,
        &decision_evidence,
        eligibility.excluded_rows,
    )?;
    let approval_evidence = publication_approval_evidence(
        connection,
        job_id,
        candidate_id,
        candidate_hash,
        approval_id,
        approval_hash,
    )?;
    let included_identities = row_categories
        .iter()
        .filter(|(_, category)| category.starts_with("INCLUDED_"))
        .map(|(identity, _)| identity.clone())
        .collect::<BTreeSet<_>>();
    let projected_rows = crate::validation::project_effective_rows_for_publication(
        connection,
        job_id,
        &governance.validation_run_id,
        &governance.mapping_id,
        &governance.mapping_version,
        &governance.mapping_hash,
        &included_identities,
    )?;
    let mut effective_rows = Vec::with_capacity(projected_rows.len());
    for row in projected_rows {
        let category = row_categories
            .get(&(row.source_file.clone(), row.source_ordinal))
            .ok_or_else(|| {
                KernelError::Reconciliation(
                    "publication projection is absent from the governing snapshot".to_owned(),
                )
            })?
            .clone();
        effective_rows.push(EffectivePublicationRow {
            source_file: row.source_file,
            source_ordinal: row.source_ordinal,
            line_start: row.line_start,
            line_end: row.line_end,
            entity_id: row.entity_id,
            category,
            canonical_values: row.canonical_values,
            lineage: row.lineage,
        });
    }

    // 1. Provenance
    let provenance_json = serde_json::json!({
        "contract_version": PUBLICATION_CONTRACT_VERSION,
        "application_name": "migration-stream",
        "engine_version": "0.1.0",
        "schema_version": crate::state::SCHEMA_VERSION,
        "created_at": created_at,
        "source_package_digest": governance.source_package_digest,
        "profile_id": governance.profile_id,
        "profile_version": governance.profile_version,
        "profile_hash": governance.profile_hash,
        "mapping_id": governance.mapping_id,
        "mapping_version": governance.mapping_version,
        "mapping_hash": governance.mapping_hash,
        "validation_run_id": governance.validation_run_id,
        "validation_result_hash": governance.validation_result_hash,
        "reconciliation_policy_id": governance.policy_identity,
        "reconciliation_policy_version": governance.policy_version,
        "reconciliation_policy_hash": governance.policy_hash,
        "policy_activation_id": governance.policy_activation_id,
        "policy_activation_hash": governance.policy_activation_hash,
        "reconciliation_engine_version": governance.engine_version,
        "canonicalization_version": governance.canonicalization_version,
    });
    let provenance_bytes = format_canonical_json(&provenance_json)?;
    let provenance_sha = sha256_hex(&provenance_bytes);

    // 2. Reconciliation Evidence
    let recon_json = serde_json::json!({
        "job_id": job_id,
        "total_accepted_rows": eligibility.total_accepted_rows,
        "included_rows": eligibility.included_rows,
        "excluded_rows": eligibility.excluded_rows,
        "blocked_rows": eligibility.blocked_rows,
        "closure_status": "COMPLETE",
        "readiness_closure": "ZERO_BLOCKERS",
        "category_counts": {
            "INCLUDED_VALID": category_counts.included_valid,
            "INCLUDED_CORRECTED": category_counts.included_corrected,
            "INCLUDED_WAIVED": category_counts.included_waived,
            "INCLUDED_ACCEPTED_AS_IS": category_counts.included_accepted_as_is,
            "EXCLUDED": category_counts.excluded,
            "BLOCKED_UNRESOLVED": category_counts.blocked_unresolved,
            "BLOCKED_INCOMPLETE": category_counts.blocked_incomplete,
            "BLOCKED_CONFLICT": category_counts.blocked_conflict,
        },
        "readiness_summary": {
            "outcome": "LOCAL_READINESS_RESULT_APPROVED",
            "statement": format!(
                "All {} accepted source rows are accounted for: {} included in the local readiness extract, {} excluded under governed decision evidence, and {} blocked.",
                eligibility.total_accepted_rows,
                eligibility.included_rows,
                eligibility.excluded_rows,
                eligibility.blocked_rows,
            ),
            "remaining_non_blocking_warning_codes":
                approval_evidence.candidate_required_acknowledgments,
            "local_only": true,
        },
        "reconciliation_snapshot_id": governance.reconciliation_snapshot_id,
        "reconciliation_snapshot_hash": governance.reconciliation_snapshot_hash,
        "policy_identity": governance.policy_identity,
        "policy_version": governance.policy_version,
        "policy_hash": governance.policy_hash,
        "policy_activation_id": governance.policy_activation_id,
        "policy_activation_hash": governance.policy_activation_hash,
    });
    let recon_bytes = format_canonical_json(&recon_json)?;
    let recon_sha = sha256_hex(&recon_bytes);

    // 3. Approval Evidence
    let approval_json = serde_json::json!({
        "approval_id": approval_evidence.approval_id,
        "approval_hash": approval_evidence.approval_hash,
        "action_kind": approval_evidence.action_kind,
        "actor": approval_evidence.actor,
        "approval_reason": approval_evidence.reason,
        "generation": approval_evidence.generation,
        "warning_acknowledgments": approval_evidence.warning_acknowledgments,
        "warning_acknowledgments_hash": approval_evidence.warning_acknowledgments_hash,
        "authority_boundary_acknowledged":
            approval_evidence.authority_boundary_acknowledged,
        "occurred_at": approval_evidence.occurred_at,
        "candidate_id": candidate_id,
        "candidate_hash": candidate_hash,
        "candidate_creator": approval_evidence.candidate_creator,
        "candidate_preparation_reason": approval_evidence.candidate_preparation_reason,
        "candidate_required_acknowledgments":
            approval_evidence.candidate_required_acknowledgments,
        "candidate_required_acknowledgments_hash":
            approval_evidence.candidate_required_acknowledgments_hash,
        "package_generation": {
            "practitioner": practitioner,
            "reason": reason,
        },
        "disclaimer": PUBLICATION_NON_AUTHORITY_DISCLAIMER,
    });
    let approval_bytes = format_canonical_json(&approval_json)?;
    let approval_sha = sha256_hex(&approval_bytes);

    // 4. Issue & Decision Evidence
    let issue_json = serde_json::json!({
        "job_id": job_id,
        "candidate_id": candidate_id,
        "reconciliation_snapshot_id": governance.reconciliation_snapshot_id,
        "reconciliation_snapshot_hash": governance.reconciliation_snapshot_hash,
        "decision_count": decision_evidence.len(),
        "decisions": decision_evidence,
    });
    let issue_bytes = format_canonical_json(&issue_json)?;
    let issue_sha = sha256_hex(&issue_bytes);

    // 5. Exclusion Evidence
    let exclusion_json = serde_json::json!({
        "job_id": job_id,
        "reconciliation_snapshot_id": governance.reconciliation_snapshot_id,
        "reconciliation_snapshot_hash": governance.reconciliation_snapshot_hash,
        "excluded_count": exclusion_evidence.len(),
        "excluded_rows": exclusion_evidence,
    });
    let exclusion_bytes = format_canonical_json(&exclusion_json)?;
    let exclusion_sha = sha256_hex(&exclusion_bytes);

    // 6. Effective Rows JSON & CSV
    let effective_rows_json_val = serde_json::json!({
        "job_id": job_id,
        "candidate_id": candidate_id,
        "effective_row_count": eligibility.included_rows,
        "rows": effective_rows
    });
    let effective_rows_json_bytes = format_canonical_json(&effective_rows_json_val)?;
    let effective_rows_json_sha = sha256_hex(&effective_rows_json_bytes);

    let mut effective_rows_csv = WriterBuilder::new()
        .terminator(Terminator::Any(b'\n'))
        .from_writer(Vec::new());
    effective_rows_csv
        .write_record([
            "source_file",
            "source_ordinal",
            "line_start",
            "line_end",
            "entity_id",
            "category",
            "canonical_values_json",
        ])
        .map_err(publication_csv_error)?;
    for row in &effective_rows {
        let canonical_values_json =
            crate::canonical::canonical_json_string(&serde_json::to_value(&row.canonical_values)?)?;
        effective_rows_csv
            .write_record([
                row.source_file.clone(),
                row.source_ordinal.to_string(),
                row.line_start.to_string(),
                row.line_end.to_string(),
                row.entity_id.clone(),
                row.category.clone(),
                canonical_values_json,
            ])
            .map_err(publication_csv_error)?;
    }
    let effective_rows_csv_bytes = effective_rows_csv
        .into_inner()
        .map_err(|error| publication_csv_error(error.into_error()))?;
    let effective_rows_csv_sha = sha256_hex(&effective_rows_csv_bytes);

    // Compute semantic hash over canonical member filenames and contents
    let mut semantic_hasher = Sha256::new();
    semantic_hasher.update(MEMBER_APPROVAL_EVIDENCE.as_bytes());
    semantic_hasher.update(&approval_bytes);
    semantic_hasher.update(MEMBER_EFFECTIVE_ROWS_CSV.as_bytes());
    semantic_hasher.update(&effective_rows_csv_bytes);
    semantic_hasher.update(MEMBER_EFFECTIVE_ROWS_JSON.as_bytes());
    semantic_hasher.update(&effective_rows_json_bytes);
    semantic_hasher.update(MEMBER_EXCLUSION_EVIDENCE.as_bytes());
    semantic_hasher.update(&exclusion_bytes);
    semantic_hasher.update(MEMBER_ISSUE_DECISION_EVIDENCE.as_bytes());
    semantic_hasher.update(&issue_bytes);
    semantic_hasher.update(MEMBER_PROVENANCE.as_bytes());
    semantic_hasher.update(&provenance_bytes);
    semantic_hasher.update(MEMBER_RECONCILIATION_EVIDENCE.as_bytes());
    semantic_hasher.update(&recon_bytes);

    let semantic_publication_hash = sha256_hex(&semantic_hasher.finalize());

    // 7. Manifest
    let inventory = vec![
        ManifestMember {
            filename: MEMBER_APPROVAL_EVIDENCE.to_owned(),
            media_type: "application/json".to_owned(),
            row_count: None,
            byte_size: approval_bytes.len() as u64,
            sha256_digest: approval_sha.clone(),
        },
        ManifestMember {
            filename: MEMBER_EFFECTIVE_ROWS_CSV.to_owned(),
            media_type: "text/csv".to_owned(),
            row_count: Some(eligibility.included_rows),
            byte_size: effective_rows_csv_bytes.len() as u64,
            sha256_digest: effective_rows_csv_sha.clone(),
        },
        ManifestMember {
            filename: MEMBER_EFFECTIVE_ROWS_JSON.to_owned(),
            media_type: "application/json".to_owned(),
            row_count: Some(eligibility.included_rows),
            byte_size: effective_rows_json_bytes.len() as u64,
            sha256_digest: effective_rows_json_sha.clone(),
        },
        ManifestMember {
            filename: MEMBER_EXCLUSION_EVIDENCE.to_owned(),
            media_type: "application/json".to_owned(),
            row_count: Some(eligibility.excluded_rows),
            byte_size: exclusion_bytes.len() as u64,
            sha256_digest: exclusion_sha.clone(),
        },
        ManifestMember {
            filename: MEMBER_ISSUE_DECISION_EVIDENCE.to_owned(),
            media_type: "application/json".to_owned(),
            row_count: None,
            byte_size: issue_bytes.len() as u64,
            sha256_digest: issue_sha.clone(),
        },
        ManifestMember {
            filename: MEMBER_PROVENANCE.to_owned(),
            media_type: "application/json".to_owned(),
            row_count: None,
            byte_size: provenance_bytes.len() as u64,
            sha256_digest: provenance_sha.clone(),
        },
        ManifestMember {
            filename: MEMBER_RECONCILIATION_EVIDENCE.to_owned(),
            media_type: "application/json".to_owned(),
            row_count: None,
            byte_size: recon_bytes.len() as u64,
            sha256_digest: recon_sha.clone(),
        },
    ];

    let manifest = PublicationManifest {
        publication_id: pub_id.to_owned(),
        publication_version: "1.0.0".to_owned(),
        contract_version: PUBLICATION_CONTRACT_VERSION.to_owned(),
        application_version: "0.1.0".to_owned(),
        engine_version: "0.1.0".to_owned(),
        created_at: created_at.to_owned(),
        practitioner: practitioner.to_owned(),
        publication_reason: reason.to_owned(),
        disclaimer: PUBLICATION_NON_AUTHORITY_DISCLAIMER.to_owned(),
        source_package_digest: governance.source_package_digest.clone(),
        profile_identity: governance.profile_id.clone(),
        profile_version: governance.profile_version.clone(),
        profile_digest: governance.profile_hash.clone(),
        mapping_identity: Some(governance.mapping_id.clone()),
        mapping_version: Some(governance.mapping_version.clone()),
        mapping_digest: governance.mapping_hash.clone(),
        validation_run_id: Some(governance.validation_run_id.clone()),
        validation_digest: governance.validation_result_hash.clone(),
        reconciliation_snapshot_id: Some(governance.reconciliation_snapshot_id.clone()),
        reconciliation_snapshot_hash: governance.reconciliation_snapshot_hash.clone(),
        candidate_id: candidate_id.to_owned(),
        candidate_hash: candidate_hash.to_owned(),
        approval_id: approval_id.to_owned(),
        approval_hash: approval_hash.to_owned(),
        approval_actor: Some(approval_evidence.actor.clone()),
        approval_reason: Some(approval_evidence.reason.clone()),
        approval_occurred_at: Some(approval_evidence.occurred_at.clone()),
        governing_decision_count: Some(decision_evidence.len()),
        included_valid_count: category_counts.included_valid,
        included_corrected_count: category_counts.included_corrected,
        included_waived_count: category_counts.included_waived,
        included_accepted_as_is_count: category_counts.included_accepted_as_is,
        excluded_count: category_counts.excluded,
        blocked_unresolved_count: category_counts.blocked_unresolved,
        blocked_incomplete_count: category_counts.blocked_incomplete,
        blocked_conflict_count: category_counts.blocked_conflict,
        total_accepted_rows: eligibility.total_accepted_rows,
        semantic_publication_hash,
        member_inventory: inventory,
    };

    let manifest_bytes = format_canonical_json(&manifest)?;
    let manifest_sha = sha256_hex(&manifest_bytes);

    let mut members_data = vec![
        MemberContent {
            filename: MEMBER_MANIFEST,
            bytes: manifest_bytes,
            row_count: None,
            sha256: manifest_sha,
        },
        MemberContent {
            filename: MEMBER_APPROVAL_EVIDENCE,
            bytes: approval_bytes,
            row_count: None,
            sha256: approval_sha,
        },
        MemberContent {
            filename: MEMBER_EFFECTIVE_ROWS_CSV,
            bytes: effective_rows_csv_bytes,
            row_count: Some(eligibility.included_rows),
            sha256: effective_rows_csv_sha,
        },
        MemberContent {
            filename: MEMBER_EFFECTIVE_ROWS_JSON,
            bytes: effective_rows_json_bytes,
            row_count: Some(eligibility.included_rows),
            sha256: effective_rows_json_sha,
        },
        MemberContent {
            filename: MEMBER_EXCLUSION_EVIDENCE,
            bytes: exclusion_bytes,
            row_count: Some(eligibility.excluded_rows),
            sha256: exclusion_sha,
        },
        MemberContent {
            filename: MEMBER_ISSUE_DECISION_EVIDENCE,
            bytes: issue_bytes,
            row_count: None,
            sha256: issue_sha,
        },
        MemberContent {
            filename: MEMBER_PROVENANCE,
            bytes: provenance_bytes,
            row_count: None,
            sha256: provenance_sha,
        },
        MemberContent {
            filename: MEMBER_RECONCILIATION_EVIDENCE,
            bytes: recon_bytes,
            row_count: None,
            sha256: recon_sha,
        },
    ];

    // Ensure member ordering is strictly deterministic
    members_data.sort_by_key(|m| m.filename);

    Ok((members_data, manifest))
}

fn publication_csv_error(error: impl std::fmt::Display) -> KernelError {
    KernelError::Reconciliation(format!(
        "publication effective-row CSV generation failed: {error}"
    ))
}

fn positive_sql_u64(value: i64, label: &str) -> Result<u64> {
    let value = u64::try_from(value)
        .map_err(|_| KernelError::Reconciliation(format!("publication {label} is negative")))?;
    if value == 0 {
        return Err(KernelError::Reconciliation(format!(
            "publication {label} must be positive"
        )));
    }
    Ok(value)
}

fn validate_governing_identifier(label: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 256
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':' | b'/')
        })
    {
        return Err(KernelError::Reconciliation(format!(
            "publication {label} is not a bounded safe identifier"
        )));
    }
    Ok(())
}

fn validate_governing_digest(label: &str, value: &str) -> Result<()> {
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const ZERO_SHA256: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || matches!(value, EMPTY_SHA256 | ZERO_SHA256)
    {
        return Err(KernelError::Reconciliation(format!(
            "publication {label} is not a non-placeholder lowercase SHA-256"
        )));
    }
    Ok(())
}

fn write_deterministic_zip(output_path: &Path, members: &[MemberContent]) -> Result<(u64, String)> {
    let file = File::create(output_path).map_err(|e| io_err(output_path, e))?;
    let mut zip_writer = ZipWriter::new(file);

    let zip_options =
        SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .last_modified_time(DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0).map_err(
                |e| KernelError::Reconciliation(format!("Zip date creation error: {e}")),
            )?);

    for member in members {
        zip_writer
            .start_file(member.filename, zip_options)
            .map_err(|e| KernelError::Reconciliation(format!("Zip file start error: {e}")))?;
        zip_writer
            .write_all(&member.bytes)
            .map_err(|e| io_err(output_path, e))?;
    }

    let mut finished_file = zip_writer
        .finish()
        .map_err(|e| KernelError::Reconciliation(format!("Zip finish error: {e}")))?;
    finished_file.flush().map_err(|e| io_err(output_path, e))?;

    let package_size_bytes = finished_file
        .metadata()
        .map_err(|e| io_err(output_path, e))?
        .len();

    // Calculate physical archive SHA-256 digest
    let mut zip_reader = File::open(output_path).map_err(|e| io_err(output_path, e))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = zip_reader
            .read(&mut buf)
            .map_err(|e| io_err(output_path, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let physical_digest = crate::canonical::lower_hex(hasher.finalize());

    Ok((package_size_bytes, physical_digest))
}

fn format_canonical_json<T: serde::Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut vec = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"  ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut vec, formatter);
    value
        .serialize(&mut serializer)
        .map_err(KernelError::Json)?;
    vec.push(b'\n');
    Ok(vec)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    crate::canonical::lower_hex(hasher.finalize())
}

fn io_err(path: &Path, source: std::io::Error) -> KernelError {
    KernelError::Io {
        path: path.to_owned(),
        source,
    }
}

fn get_existing_idempotent_publication(
    connection: &Connection,
    job_id: &str,
    idempotency_key: &str,
    request_hash: &str,
) -> Result<Option<PublicationRecord>> {
    let attempt: Option<(String, String)> = connection
        .query_row(
            "SELECT attempt_id, request_hash
             FROM publication_attempts
             WHERE job_id = ? AND idempotency_key = ?",
            [job_id, idempotency_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    let Some((pub_id, persisted_request_hash)) = attempt else {
        return Ok(None);
    };
    if persisted_request_hash != request_hash {
        return Err(KernelError::Conflict(
            "publication idempotency key was already used for a different command".to_owned(),
        ));
    }

    crate::publication_read_model::get_publication_detail_for_job_from_conn(
        connection, job_id, &pub_id,
    )
}

fn persist_publication_record(
    connection: &Connection,
    record: &PublicationRecord,
    idempotency_key: &str,
    request_hash: &str,
    canonical_request_json: &str,
) -> Result<()> {
    connection.execute(
        "INSERT INTO publication_attempts (
            attempt_id, schema_version, job_id, candidate_id, candidate_hash,
            approval_id, approval_hash, contract_version, practitioner,
            publication_reason, idempotency_key, request_hash, canonical_request_json,
            created_at
         ) VALUES (?, 1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            record.publication_id,
            record.job_id,
            record.candidate_id,
            record.candidate_hash,
            record.approval_id,
            record.approval_hash,
            record.contract_version,
            record.practitioner,
            record.publication_reason,
            idempotency_key,
            request_hash,
            canonical_request_json,
            record.created_at,
        ],
    )?;

    let manifest_json = serde_json::to_string(&record.manifest).map_err(KernelError::Json)?;

    connection.execute(
        "INSERT INTO publication_records (
            publication_id, schema_version, attempt_id, job_id, candidate_id,
            candidate_hash, approval_id, approval_hash, contract_version,
            semantic_publication_hash, physical_archive_digest, package_size_bytes,
            member_count, included_row_count, excluded_row_count, archive_relative_path,
            practitioner, publication_reason, canonical_manifest_json, created_at
         ) VALUES (?, 1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            record.publication_id,
            record.publication_id,
            record.job_id,
            record.candidate_id,
            record.candidate_hash,
            record.approval_id,
            record.approval_hash,
            record.contract_version,
            record.semantic_publication_hash,
            record.physical_archive_digest,
            record.package_size_bytes,
            record.member_count,
            record.included_row_count,
            record.excluded_row_count,
            record.archive_relative_path,
            record.practitioner,
            record.publication_reason,
            manifest_json,
            record.created_at,
        ],
    )?;

    for member in &record.manifest.member_inventory {
        connection.execute(
            "INSERT INTO publication_manifest_members (
                publication_id, filename, media_type, row_count, byte_size, sha256_digest
             ) VALUES (?, ?, ?, ?, ?, ?)",
            params![
                record.publication_id,
                member.filename,
                member.media_type,
                member.row_count,
                member.byte_size,
                member.sha256_digest,
            ],
        )?;
    }

    Ok(())
}

trait OptionalExt<T> {
    fn optional(self) -> std::result::Result<Option<T>, rusqlite::Error>;
}

impl<T> OptionalExt<T> for std::result::Result<T, rusqlite::Error> {
    fn optional(self) -> std::result::Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(val) => Ok(Some(val)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(err),
        }
    }
}
