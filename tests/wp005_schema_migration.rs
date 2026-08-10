#![allow(clippy::too_many_lines)]

use std::path::{Path, PathBuf};

use migration_stream::state::recover_workspace;
use rusqlite::{Connection, OptionalExtension};

#[path = "support/object_workspace.rs"]
mod object_workspace_support;

const DATABASE_FILENAME: &str = "state.sqlite3";
const V6_TABLES: [&str; 8] = [
    "decision_attempts",
    "decision_attempt_events",
    "correction_overlays",
    "reevaluation_runs",
    "reevaluation_events",
    "reevaluation_issues",
    "issue_decisions",
    "correction_effectivations",
];

fn database_path(workspace: &Path) -> PathBuf {
    workspace.join(DATABASE_FILENAME)
}

fn open_database(workspace: &Path) -> Connection {
    let connection = Connection::open(database_path(workspace)).expect("open state database");
    connection
        .pragma_update(None, "foreign_keys", true)
        .expect("enable foreign keys");
    connection
}

fn initialize_current_schema(workspace: &Path) {
    if !workspace
        .join(migration_stream::object_workspace::OBJECT_WORKSPACE_DESCRIPTOR_FILENAME)
        .exists()
    {
        object_workspace_support::initialize_test_workspace(workspace);
    }
    recover_workspace(workspace).expect("migrate and recover current workspace schema");
}

fn downgrade_current_to_v5(workspace: &Path) {
    let connection = Connection::open(database_path(workspace)).expect("open current database");
    connection
        .execute_batch(
            "DROP TABLE engagement_remediation_issue_members;
             DROP TABLE engagement_remediation_pack_members;
             DROP TABLE engagement_remediation_pack_results;
             DROP TABLE engagement_remediation_packs;
             DROP TABLE engagement_remediation_pack_selections;
             DROP TABLE engagement_remediation_pack_attempts;
             DROP TABLE engagement_validation_comparison_items;
             DROP TABLE engagement_validation_comparisons;
             DROP TABLE engagement_tour_finding_members;
             DROP TABLE engagement_tour_finding_sets;
             DROP TABLE engagement_tour_heads;
             DROP TABLE engagement_tours;
             DROP TRIGGER validation_runs_source_reuse_no_update;
             DROP TRIGGER validation_runs_source_reuse_insert_guard;
             DROP INDEX validation_runs_source_reuse;
             ALTER TABLE validation_runs DROP COLUMN source_reuse_hash;
             ALTER TABLE validation_runs DROP COLUMN source_reuse_id;
             DROP TABLE engagement_mapping_reuse_authorities;
             DROP TABLE engagement_mapping_authorities;
             DROP TABLE engagement_comparison_namespaces;
             DROP TABLE engagement_tour_command_attempts;
             DROP TABLE engagement_tour_attempts;
             DROP TABLE engagement_mapping_approvals;
             DROP TABLE engagement_mapping_workspace_attempts;
             DROP TABLE engagement_mapping_draft_revisions;
             DROP TABLE engagement_mapping_draft_heads;
             DROP TABLE migration_result_actions;
             DROP TABLE migration_result_candidates;
             DROP TABLE reconciliation_row_lineage_references;
             DROP TABLE reconciliation_row_classifications;
             DROP TABLE reconciliation_category_counts;
             DROP TABLE reconciliation_snapshots;
             DROP TABLE reconciliation_attempt_events;
             DROP TABLE reconciliation_attempts;
             DROP TABLE reconciliation_policy_activations;
             DROP TABLE reconciliation_policy_documents;
             DROP TABLE correction_effectivations;
             DROP TABLE issue_decisions;
             DROP TABLE reevaluation_issues;
             DROP TABLE reevaluation_events;
             DROP TABLE reevaluation_runs;
             DROP TABLE correction_overlays;
             DROP TABLE decision_attempt_events;
             DROP TABLE decision_attempts;
             DROP INDEX validation_issues_wp005_scope;
             DROP INDEX validation_runs_wp005_scope;
             PRAGMA user_version = 5;",
        )
        .expect("remove only v6 objects");
}

fn seed_v5_authority(connection: &Connection) {
    connection
        .execute_batch(
            "INSERT INTO jobs (
                 id, state, source_name, source_path, source_hash, source_size,
                 stored_archive, policy_json, manifest_entries, csv_files,
                 expanded_bytes, source_records, stored_records, pending_records,
                 created_at, updated_at, completed_at
             ) VALUES (
                 'job-1', 'COMPLETE', 'synthetic.zip', '/synthetic.zip',
                 printf('%064d', 1), 10, 'packages/job-1.zip', '{}', 1, 1,
                 10, 1, 1, 0, '2026-07-22T00:00:00Z',
                 '2026-07-22T00:00:01Z', '2026-07-22T00:00:01Z'
             );
             INSERT INTO archive_entries (
                 job_id, path, is_directory, is_csv,
                 compressed_size, expanded_size, crc32
             ) VALUES ('job-1', 'data.csv', 0, 1, 10, 10, 1);
             INSERT INTO csv_files (
                 job_id, path, header_json, source_records, stored_records
             ) VALUES ('job-1', 'data.csv', '[\"field\"]', 1, 1);
             INSERT INTO source_records (
                 job_id, file_path, source_ordinal, line_start, line_end, fields_json
             ) VALUES ('job-1', 'data.csv', 1, 2, 2, '[\"old\"]');

             INSERT INTO profile_versions (
                 profile_id, profile_version, profile_hash, schema_hash,
                 document_json, declared_status, validation_hash, registered_at
             ) VALUES (
                 'profile-1', '1', printf('%064d', 2), printf('%064d', 3),
                 '{}', 'APPROVED', printf('%064d', 4), '2026-07-22T00:00:02Z'
             );
             INSERT INTO profile_approvals (
                 approval_id, profile_id, profile_version, profile_hash,
                 approver, reason, acknowledged, approved_at, approval_hash
             ) VALUES (
                 'approval-1', 'profile-1', '1', printf('%064d', 2),
                 'synthetic-owner', 'synthetic approval', 1,
                 '2026-07-22T00:00:03Z', printf('%064d', 5)
             );
             INSERT INTO mapping_validation_attempts (
                 id, input_hash, state, mapping_id, mapping_version,
                 mapping_hash, errors_json, created_at, attempt_hash
             ) VALUES (
                 'mapping-attempt-1', printf('%064d', 6), 'VALID',
                 'mapping-1', '1', printf('%064d', 7), '[]',
                 '2026-07-22T00:00:04Z', printf('%064d', 8)
             );
             INSERT INTO mapping_versions (
                 mapping_id, mapping_version, mapping_hash, attempt_id,
                 profile_id, profile_version, profile_hash, source_hash,
                 validated_job_id, document_json, coverage_json, coverage_hash,
                 created_at
             ) VALUES (
                 'mapping-1', '1', printf('%064d', 7), 'mapping-attempt-1',
                 'profile-1', '1', printf('%064d', 2), printf('%064d', 1),
                 'job-1', '{}', '{}', printf('%064d', 9),
                 '2026-07-22T00:00:05Z'
             );
             INSERT INTO validation_runs (
                 id, identity_hash, job_id, source_hash,
                 profile_id, profile_version, profile_hash,
                 mapping_id, mapping_version, mapping_hash,
                 rule_set_hash, engine_version, policy_json, policy_hash,
                 state, result_json, result_hash, terminal_hash,
                 issue_count, rows_processed, created_at, updated_at, completed_at
             ) VALUES (
                 'validation-1', printf('%064d', 10), 'job-1', printf('%064d', 1),
                 'profile-1', '1', printf('%064d', 2),
                 'mapping-1', '1', printf('%064d', 7),
                 printf('%064d', 11), 'test-engine', '{}', printf('%064d', 12),
                 'COMPLETE', '{}', printf('%064d', 13), printf('%064d', 14),
                 1, 1, '2026-07-22T00:00:06Z', '2026-07-22T00:00:07Z',
                 '2026-07-22T00:00:07Z'
             );
             INSERT INTO validation_issues (
                 run_id, issue_id, severity, rule_id, issue_code, display_message,
                 entity_id, field_id, source_file, source_ordinal,
                 line_start, line_end, lineage_json, issue_hash
             ) VALUES (
                 'validation-1', 'issue-1', 'ERROR', 'rule-1', 'REQUIRED',
                 'synthetic issue', 'entity-1', 'field-1', 'data.csv', 1,
                 2, 2, '{}', printf('%064d', 15)
             );",
        )
        .expect("seed exact v5 authority");
}

fn insert_corrected_attempt(connection: &Connection, suffix: &str, idempotency: &str) {
    connection
        .execute(
            "INSERT INTO decision_attempts (
                 attempt_id, schema_version, idempotency_key, request_hash,
                 canonical_request_json, job_id, validation_run_id, issue_id,
                 issue_hash, accepted_source_file, source_ordinal, line_start, line_end,
                 canonical_entity_id, canonical_field_id,
                 profile_id, profile_version, profile_hash,
                 mapping_id, mapping_version, mapping_hash,
                 disposition, practitioner_id, reason, evidence_reference,
                 authority_separation_acknowledged,
                 row_scope_exclusion_acknowledged,
                 expected_active_decision_id, expected_active_generation,
                 expected_effective_value_hash, created_at
             ) VALUES (
                 ?1, 1, ?2, printf('%064d', 16), '{}',
                 'job-1', 'validation-1', 'issue-1', printf('%064d', 15),
                 'data.csv', 1, 2, 2, 'entity-1', 'field-1',
                 'profile-1', '1', printf('%064d', 2),
                 'mapping-1', '1', printf('%064d', 7),
                 'CORRECTED', 'practitioner-1', 'bounded synthetic correction', NULL,
                 1, 0, NULL, 0, printf('%064d', 17),
                 '2026-07-22T00:00:08Z'
             )",
            [suffix, idempotency],
        )
        .expect("insert corrected attempt");
}

fn insert_overlay(connection: &Connection, overlay_id: &str, attempt_id: &str, hash: i64) {
    connection
        .execute(
            "INSERT INTO correction_overlays (
                 overlay_id, schema_version, decision_attempt_id, disposition,
                 decision_request_hash,
                 prior_effective_value_hash, operation, replacement_text,
                 practitioner_id, reason, canonical_overlay_json, overlay_hash, created_at
             ) VALUES (
                 ?1, 1, ?2, 'CORRECTED', printf('%064d', 16), printf('%064d', 17),
                 'REPLACE_EFFECTIVE_TEXT', 'new', 'practitioner-1',
                 'bounded synthetic correction', '{}', printf('%064d', ?3),
                 '2026-07-22T00:00:09Z'
             )",
            rusqlite::params![overlay_id, attempt_id, hash],
        )
        .expect("insert correction overlay");
}

#[test]
fn migrates_v5_atomically_and_preserves_existing_history() {
    let temporary = tempfile::tempdir().expect("temporary workspace");
    initialize_current_schema(temporary.path());
    downgrade_current_to_v5(temporary.path());

    {
        let connection = open_database(temporary.path());
        connection
            .execute_batch(
                "INSERT INTO jobs (
                     id, state, source_name, source_path, policy_json,
                     error_code, created_at, updated_at
                 ) VALUES (
                     'preserved-v5-job', 'FAILED', 'synthetic.zip',
                     '/synthetic.zip', '{}', 'SYNTHETIC_FAILURE',
                     '2026-07-22T00:00:00Z', '2026-07-22T00:00:00Z'
                 );",
            )
            .expect("seed v5 history");
    }

    initialize_current_schema(temporary.path());
    let connection = open_database(temporary.path());
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("schema version");
    assert_eq!(version, migration_stream::state::SCHEMA_VERSION);
    let preserved: String = connection
        .query_row(
            "SELECT id FROM jobs WHERE id = 'preserved-v5-job'",
            [],
            |row| row.get(0),
        )
        .expect("preserved v5 row");
    assert_eq!(preserved, "preserved-v5-job");
    for table in V6_TABLES {
        let found: String = connection
            .query_row(
                "SELECT name FROM sqlite_schema WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get(0),
            )
            .unwrap_or_else(|_| panic!("missing v6 table {table}"));
        assert_eq!(found, table);
    }
    let foreign_key_violation: Option<String> = connection
        .query_row("PRAGMA foreign_key_check", [], |row| row.get(0))
        .optional()
        .expect("foreign-key check");
    assert_eq!(foreign_key_violation, None);
}

#[test]
fn failed_v5_migration_rolls_back_every_prior_v6_statement_and_retries_cleanly() {
    let temporary = tempfile::tempdir().expect("temporary workspace");
    initialize_current_schema(temporary.path());
    downgrade_current_to_v5(temporary.path());
    {
        let connection = Connection::open(database_path(temporary.path())).expect("open v5 db");
        connection
            .execute_batch("CREATE TABLE reevaluation_issues (sentinel TEXT NOT NULL);")
            .expect("install deterministic migration collision");
    }

    let error = recover_workspace(temporary.path()).expect_err("migration collision must fail");
    assert!(error.to_string().contains("reevaluation_issues"));

    {
        let connection =
            Connection::open(database_path(temporary.path())).expect("inspect rollback");
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("rolled-back version");
        assert_eq!(version, 5);
        for absent in [
            "decision_attempts",
            "decision_attempt_events",
            "correction_overlays",
            "reevaluation_runs",
            "reevaluation_events",
        ] {
            let found: Option<String> = connection
                .query_row(
                    "SELECT name FROM sqlite_schema WHERE type = 'table' AND name = ?1",
                    [absent],
                    |row| row.get(0),
                )
                .optional()
                .expect("inspect rolled-back object");
            assert_eq!(found, None, "{absent} survived failed migration");
        }
        let v6_index: Option<String> = connection
            .query_row(
                "SELECT name FROM sqlite_schema
                 WHERE type = 'index' AND name = 'validation_runs_wp005_scope'",
                [],
                |row| row.get(0),
            )
            .optional()
            .expect("inspect rolled-back index");
        assert_eq!(v6_index, None);
        connection
            .execute("DROP TABLE reevaluation_issues", [])
            .expect("remove deterministic collision");
    }

    initialize_current_schema(temporary.path());
    let connection = open_database(temporary.path());
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("retried schema version");
    assert_eq!(version, migration_stream::state::SCHEMA_VERSION);
}

#[test]
fn active_workspace_authority_is_replayed_before_a_supported_schema_migration() {
    let temporary = tempfile::tempdir().expect("temporary workspace");
    initialize_current_schema(temporary.path());
    downgrade_current_to_v5(temporary.path());
    {
        let connection = Connection::open(database_path(temporary.path())).expect("open v5 db");
        connection
            .execute_batch("DROP TRIGGER object_workspace_bindings_no_update;")
            .expect("weaken exact active authority");
    }

    let error = recover_workspace(temporary.path())
        .expect_err("weakened active authority must block schema migration");
    assert!(
        error
            .to_string()
            .contains("object_workspace_bindings_no_update")
    );
    let connection =
        Connection::open(database_path(temporary.path())).expect("inspect unchanged v5 db");
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("unchanged schema version");
    assert_eq!(version, 5);
    let v6_table: Option<String> = connection
        .query_row(
            "SELECT name FROM sqlite_schema
             WHERE type = 'table' AND name = 'decision_attempts'",
            [],
            |row| row.get(0),
        )
        .optional()
        .expect("inspect blocked migration");
    assert_eq!(v6_table, None);
}

#[test]
fn active_workspace_zero_schema_tamper_is_rejected_without_bootstrap_mutation() {
    let temporary = tempfile::tempdir().expect("temporary workspace");
    initialize_current_schema(temporary.path());
    let before = {
        let connection =
            Connection::open(database_path(temporary.path())).expect("open active database");
        connection
            .pragma_update(None, "user_version", 0)
            .expect("install zero-version tamper");
        sqlite_schema(&connection)
    };

    let error =
        recover_workspace(temporary.path()).expect_err("zero schema version must fail closed");
    assert!(
        error
            .to_string()
            .contains("requires a supported schema version between 1")
    );
    let connection =
        Connection::open(database_path(temporary.path())).expect("inspect rejected database");
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("unchanged zero version");
    assert_eq!(version, 0);
    assert_eq!(sqlite_schema(&connection), before);
}

fn sqlite_schema(connection: &Connection) -> Vec<(String, String, String)> {
    let mut statement = connection
        .prepare(
            "SELECT type, name, COALESCE(sql, '')
             FROM sqlite_schema
             ORDER BY type, name",
        )
        .expect("prepare SQLite schema snapshot");
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("query SQLite schema snapshot")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect SQLite schema snapshot")
}

#[test]
fn v6_is_closed_append_only_and_enforces_exact_parent_authority() {
    let temporary = tempfile::tempdir().expect("temporary workspace");
    initialize_current_schema(temporary.path());
    let connection = open_database(temporary.path());
    seed_v5_authority(&connection);

    let invalid_disposition = connection.execute_batch(
        "INSERT INTO decision_attempts (
             attempt_id, schema_version, idempotency_key, request_hash,
             canonical_request_json, job_id, validation_run_id, issue_id,
             issue_hash, accepted_source_file, source_ordinal, line_start, line_end,
             canonical_entity_id, canonical_field_id,
             profile_id, profile_version, profile_hash,
             mapping_id, mapping_version, mapping_hash,
             disposition, practitioner_id, reason,
             authority_separation_acknowledged,
             row_scope_exclusion_acknowledged,
             expected_active_generation, created_at
         ) VALUES (
             'invalid-disposition', 1, 'invalid-disposition', printf('%064d', 20),
             '{}', 'job-1', 'validation-1', 'issue-1', printf('%064d', 15),
             'data.csv', 1, 2, 2, 'entity-1', 'field-1',
             'profile-1', '1', printf('%064d', 2),
             'mapping-1', '1', printf('%064d', 7),
             'AUTO_CORRECT', 'practitioner-1', 'must fail', 1, 0, 0,
             '2026-07-22T00:00:08Z'
         );",
    );
    assert!(invalid_disposition.is_err());

    let missing_parent = connection.execute_batch(
        "INSERT INTO decision_attempts (
             attempt_id, schema_version, idempotency_key, request_hash,
             canonical_request_json, job_id, validation_run_id, issue_id,
             issue_hash, accepted_source_file, source_ordinal, line_start, line_end,
             canonical_entity_id, canonical_field_id,
             profile_id, profile_version, profile_hash,
             mapping_id, mapping_version, mapping_hash,
             disposition, practitioner_id, reason,
             authority_separation_acknowledged,
             row_scope_exclusion_acknowledged,
             expected_active_generation, created_at
         ) VALUES (
             'missing-parent', 1, 'missing-parent', printf('%064d', 21), '{}',
             'missing-job', 'missing-run', 'missing-issue', printf('%064d', 22),
             'missing.csv', 1, 1, 1, 'entity-1', 'field-1',
             'profile-1', '1', printf('%064d', 2),
             'mapping-1', '1', printf('%064d', 7),
             'WAIVED', 'practitioner-1', 'must fail', 1, 0, 0,
             '2026-07-22T00:00:08Z'
         );",
    );
    assert!(missing_parent.is_err());

    insert_corrected_attempt(&connection, "attempt-failed", "idem-failed");
    insert_overlay(&connection, "overlay-failed", "attempt-failed", 23);
    connection
        .execute_batch(
            "INSERT INTO reevaluation_runs (
                 reevaluation_id, identity_hash, decision_attempt_id,
                 correction_overlay_id, disposition, job_id,
                 source_validation_run_id, source_issue_id,
                 rule_set_hash, engine_version, policy_json, policy_hash,
                 dependency_scope_json, dependency_scope_hash, state,
                 terminal_hash, error_code, error_message, created_at, completed_at
             ) VALUES (
                 'reevaluation-failed', printf('%064d', 24), 'attempt-failed',
                 'overlay-failed', 'CORRECTED', 'job-1', 'validation-1', 'issue-1',
                 printf('%064d', 11), 'test-engine', '{}', printf('%064d', 12),
                 '{}', printf('%064d', 25), 'FAILED', printf('%064d', 26),
                 'SYNTHETIC_FAILURE', 'bounded failure',
                 '2026-07-22T00:00:10Z', '2026-07-22T00:00:11Z'
             );",
        )
        .expect("insert nonqualifying terminal re-evaluation");

    let premature_decision = connection.execute_batch(
        "INSERT INTO issue_decisions (
             decision_id, schema_version, decision_attempt_id,
             job_id, validation_run_id, issue_id, disposition,
             predecessor_generation, generation,
             correction_overlay_id, reevaluation_id, reevaluation_state,
             practitioner_id, reason, lifecycle_outcome,
             canonical_decision_json, decision_hash, committed_at
         ) VALUES (
             'decision-failed', 1, 'attempt-failed',
             'job-1', 'validation-1', 'issue-1', 'CORRECTED',
             0, 1, 'overlay-failed', 'reevaluation-failed', 'COMPLETE',
             'practitioner-1', 'bounded synthetic correction', 'EFFECTIVE',
             '{}', printf('%064d', 27), '2026-07-22T00:00:12Z'
         );",
    );
    assert!(
        premature_decision.is_err(),
        "a failed re-evaluation granted corrected authority"
    );
    let false_effectivation = connection.execute_batch(
        "INSERT INTO correction_effectivations (
             effectivation_id, decision_id, decision_attempt_id,
             correction_overlay_id, reevaluation_id,
             decision_disposition, reevaluation_state, decision_generation,
             original_value_hash, prior_effective_value_hash,
             effective_value_kind, effective_value_text,
             effective_value_hash, effective_lineage_json, effective_lineage_hash,
             effectivation_hash, effectivated_at
         ) VALUES (
             'false-effectivation', 'decision-failed', 'attempt-failed',
             'overlay-failed', 'reevaluation-failed', 'CORRECTED', 'COMPLETE', 1,
             printf('%064d', 18), printf('%064d', 17),
             'TEXT', 'new', printf('%064d', 28),
             '{}', printf('%064d', 29), printf('%064d', 30),
             '2026-07-22T00:00:13Z'
         );",
    );
    assert!(
        false_effectivation.is_err(),
        "effectivation existed without a qualifying decision and COMPLETE run"
    );

    let update_error = connection
        .execute(
            "UPDATE correction_overlays SET reason = 'changed' WHERE overlay_id = 'overlay-failed'",
            [],
        )
        .expect_err("overlay update must fail");
    assert!(update_error.to_string().contains("immutable"));
    let delete_error = connection
        .execute(
            "DELETE FROM reevaluation_runs WHERE reevaluation_id = 'reevaluation-failed'",
            [],
        )
        .expect_err("re-evaluation delete must fail");
    assert!(delete_error.to_string().contains("immutable"));
}

#[test]
fn complete_reevaluation_evidence_and_effectivation_commit_in_one_transaction() {
    let temporary = tempfile::tempdir().expect("temporary workspace");
    initialize_current_schema(temporary.path());
    let connection = open_database(temporary.path());
    seed_v5_authority(&connection);
    insert_corrected_attempt(&connection, "attempt-complete", "idem-complete");
    insert_overlay(&connection, "overlay-complete", "attempt-complete", 31);

    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
             INSERT INTO decision_attempt_events (
                 attempt_id, state, event_hash, occurred_at
             ) VALUES (
                 'attempt-complete', 'RECEIVED', printf('%064d', 44),
                 '2026-07-22T00:00:08Z'
             );
             INSERT INTO reevaluation_runs (
                 reevaluation_id, identity_hash, decision_attempt_id,
                 correction_overlay_id, disposition, job_id,
                 source_validation_run_id, source_issue_id,
                 rule_set_hash, engine_version, policy_json, policy_hash,
                 dependency_scope_json, dependency_scope_hash, state,
                 result_json, result_hash, terminal_hash,
                 issue_count, rows_processed, created_at, completed_at
             ) VALUES (
                 'reevaluation-complete', printf('%064d', 32), 'attempt-complete',
                 'overlay-complete', 'CORRECTED', 'job-1', 'validation-1', 'issue-1',
                 printf('%064d', 11), 'test-engine', '{}', printf('%064d', 12),
                 '{}', printf('%064d', 33), 'COMPLETE', '{}', printf('%064d', 34),
                 printf('%064d', 35), 1, 1,
                 '2026-07-22T00:00:10Z', '2026-07-22T00:00:11Z'
             );
             INSERT INTO reevaluation_events (
                 reevaluation_id, state, event_hash, occurred_at
             ) VALUES
                 ('reevaluation-complete', 'PLANNED', printf('%064d', 36),
                     '2026-07-22T00:00:10Z'),
                 ('reevaluation-complete', 'RUNNING', printf('%064d', 37),
                     '2026-07-22T00:00:10.500Z'),
                 ('reevaluation-complete', 'COMPLETE', printf('%064d', 38),
                     '2026-07-22T00:00:11Z');
             INSERT INTO reevaluation_issues (
                 reevaluation_id, issue_id, outcome,
                 source_validation_run_id, source_issue_id,
                 severity, rule_id, issue_code, display_message,
                 entity_id, field_id, source_file, source_ordinal,
                 line_start, line_end, lineage_json, issue_hash
             ) VALUES (
                 'reevaluation-complete', 'reevaluated-issue-1', 'RESOLVED',
                 'validation-1', 'issue-1', 'INFORMATION', 'rule-1',
                 'RESOLVED', 'original issue resolved', 'entity-1', 'field-1',
                 'data.csv', 1, 2, 2, '{}', printf('%064d', 39)
             );
             INSERT INTO issue_decisions (
                 decision_id, schema_version, decision_attempt_id,
                 job_id, validation_run_id, issue_id, disposition,
                 predecessor_generation, generation,
                 correction_overlay_id, reevaluation_id, reevaluation_state,
                 practitioner_id, reason, lifecycle_outcome,
                 canonical_decision_json, decision_hash, committed_at
             ) VALUES (
                 'decision-complete', 1, 'attempt-complete',
                 'job-1', 'validation-1', 'issue-1', 'CORRECTED',
                 0, 1, 'overlay-complete', 'reevaluation-complete', 'COMPLETE',
                 'practitioner-1', 'bounded synthetic correction', 'EFFECTIVE',
                 '{}', printf('%064d', 40), '2026-07-22T00:00:12Z'
             );
             INSERT INTO correction_effectivations (
                 effectivation_id, decision_id, decision_attempt_id,
                 correction_overlay_id, reevaluation_id,
                 decision_disposition, reevaluation_state, decision_generation,
                 original_value_hash, prior_effective_value_hash,
                 effective_value_kind, effective_value_text,
                 effective_value_hash, effective_lineage_json, effective_lineage_hash,
                 effectivation_hash, effectivated_at
             ) VALUES (
                 'effectivation-complete', 'decision-complete', 'attempt-complete',
                 'overlay-complete', 'reevaluation-complete',
                 'CORRECTED', 'COMPLETE', 1, printf('%064d', 45),
                 printf('%064d', 17),
                 'TEXT', 'new', printf('%064d', 41), '{}', printf('%064d', 42),
                 printf('%064d', 43), '2026-07-22T00:00:13Z'
             );
             COMMIT;",
        )
        .expect("atomically commit terminal evidence and authority");

    for (table, expected) in [
        ("decision_attempts", 1_i64),
        ("decision_attempt_events", 1),
        ("correction_overlays", 1),
        ("reevaluation_runs", 1),
        ("reevaluation_events", 3),
        ("reevaluation_issues", 1),
        ("issue_decisions", 1),
        ("correction_effectivations", 1),
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("count committed evidence");
        assert_eq!(count, expected, "unexpected {table} count");
    }

    for table in V6_TABLES {
        let update_error = connection
            .execute(&format!("UPDATE {table} SET rowid = rowid"), [])
            .expect_err("append-only table update must fail");
        assert!(
            update_error.to_string().contains("immutable"),
            "{table} did not enforce append-only update: {update_error}"
        );
        let error = connection
            .execute(&format!("DELETE FROM {table}"), [])
            .expect_err("append-only table delete must fail");
        assert!(
            error.to_string().contains("immutable"),
            "{table} did not enforce append-only deletion: {error}"
        );
    }
}

#[test]
fn v6_enforces_utf8_byte_bounds_in_sqlite_before_evidence_materialization() {
    let temporary = tempfile::tempdir().expect("temporary workspace");
    initialize_current_schema(temporary.path());
    let connection = open_database(temporary.path());
    seed_v5_authority(&connection);
    insert_corrected_attempt(&connection, "attempt-byte-bound", "idem-byte-bound");

    let oversized_detail = "é".repeat(2_049);
    let event_error = connection
        .execute(
            "INSERT INTO decision_attempt_events (
                 attempt_id, state, detail, event_hash, occurred_at
             ) VALUES (?1, 'RECEIVED', ?2, printf('%064d', 61),
                       '2026-07-22T00:00:08Z')",
            rusqlite::params!["attempt-byte-bound", oversized_detail],
        )
        .expect_err("UTF-8 event detail over 4096 bytes must fail in SQLite");
    assert!(event_error.to_string().contains("CHECK constraint failed"));

    let oversized_replacement = "é".repeat(524_289);
    let overlay_error = connection
        .execute(
            "INSERT INTO correction_overlays (
                 overlay_id, schema_version, decision_attempt_id, disposition,
                 decision_request_hash, prior_effective_value_hash,
                 operation, replacement_text, practitioner_id, reason,
                 canonical_overlay_json, overlay_hash, created_at
             ) VALUES (
                 'overlay-byte-bound', 1, 'attempt-byte-bound', 'CORRECTED',
                 printf('%064d', 16), printf('%064d', 17),
                 'REPLACE_EFFECTIVE_TEXT', ?1, 'practitioner-1',
                 'bounded synthetic correction', '{}', printf('%064d', 62),
                 '2026-07-22T00:00:09Z'
             )",
            [oversized_replacement],
        )
        .expect_err("UTF-8 replacement over one MiB must fail in SQLite");
    assert!(
        overlay_error
            .to_string()
            .contains("CHECK constraint failed")
    );

    for (table, column) in [
        ("decision_attempts", "canonical_request_json"),
        ("decision_attempt_events", "detail"),
        ("correction_overlays", "replacement_text"),
        ("reevaluation_runs", "result_json"),
        ("reevaluation_events", "detail"),
        ("reevaluation_issues", "lineage_json"),
        ("issue_decisions", "canonical_decision_json"),
        ("correction_effectivations", "effective_value_text"),
    ] {
        let schema: String = connection
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get(0),
            )
            .expect("read v6 table DDL");
        assert!(
            schema.contains(&format!("CAST({column} AS BLOB)")),
            "{table}.{column} lacks a SQLite UTF-8 byte ceiling"
        );
    }
}
