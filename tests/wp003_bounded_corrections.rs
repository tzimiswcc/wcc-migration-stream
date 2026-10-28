#![allow(clippy::too_many_lines)]

use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

use migration_stream::{
    KernelError,
    canonical::{canonical_hash, canonical_json_string},
    intake::ingest_package,
    mapping::{MappingValidationReport, validate_mapping},
    policy::IntakePolicy,
    profile_registry::{
        ProfileAuthority, RegistryLimits, approve_profile, inspect_profile, register_profile,
    },
    state::integrity_check,
    validation::{ValidationPolicy, ValidationRunState, run_validation, validation_status},
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use tempfile::TempDir;
use zip::{CompressionMethod, DateTime, System, ZipWriter, write::SimpleFileOptions};

#[path = "support/object_workspace.rs"]
mod object_workspace_support;

const PROFILE_ID: &str = "synthetic.customer.readiness";
const PROFILE_VERSION: &str = "1.0.0";
const SALES_PROFILE_ID: &str = "synthetic.sales-document.readiness";
const SALES_PROFILE_FIXTURE: &str =
    "object-packs/synthetic.sales-document.readiness/1.0.0/synthetic-profile.reviewed.json";
const MAPPING_VERSION: &str = "1.0.0";
const STANDARD_MAPPING_ID: &str = "synthetic.customer.correction";
const GOLDEN_MAPPING_ID: &str = "synthetic.customer.all-rules-golden";
const PROFILE_SCHEMA: &str = "schemas/migration-object-profile.schema.json";
const MAPPING_SCHEMA: &str = "schemas/source-mapping-version.schema.json";
const PROFILE_FIXTURE: &str = "fixtures/wp003/synthetic-profile.reviewed.json";
const POLICY_FIXTURE: &str = "fixtures/wp003/validation-policy.json";
const MAPPING_ATTEMPTS_UPDATE_GUARD_SQL: &str = "CREATE TRIGGER mapping_validation_attempts_no_update BEFORE UPDATE ON mapping_validation_attempts BEGIN SELECT RAISE(ABORT, 'mapping validation attempt history is immutable'); END";
const MAPPING_VERSIONS_UPDATE_GUARD_SQL: &str = "CREATE TRIGGER mapping_versions_no_update BEFORE UPDATE ON mapping_versions BEGIN SELECT RAISE(ABORT, 'mapping version history is immutable'); END";

struct AuthoritySetup {
    workspace: PathBuf,
    job_id: String,
    source_hash: String,
    profile_hash: String,
}

struct GovernedFixture {
    temporary: TempDir,
    setup: AuthoritySetup,
    mapping_path: PathBuf,
}

fn project_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn intake_policy() -> IntakePolicy {
    IntakePolicy {
        minimum_free_space_bytes: 0,
        transaction_batch_rows: 2,
        ..IntakePolicy::default()
    }
}

fn expect_error<T>(result: migration_stream::Result<T>, code: &str) -> KernelError {
    match result {
        Ok(_) => panic!("expected {code} failure"),
        Err(error) => {
            assert_eq!(error.code(), code, "unexpected error: {error}");
            error
        }
    }
}

fn with_immutability_guard_disabled<T>(
    connection: &Connection,
    name: &str,
    exact_sql: &str,
    action: impl FnOnce(&Connection) -> T,
) -> T {
    connection
        .execute_batch(&format!("DROP TRIGGER {name};"))
        .expect("open test-only immutable-history mutation window");
    let result = action(connection);
    connection
        .execute_batch(exact_sql)
        .expect("restore exact immutable-history guard");
    result
}

fn write_zip(path: &Path, entries: &[(&str, &str)]) {
    let file = File::create(path).expect("create synthetic ZIP");
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .last_modified_time(DateTime::default())
        .unix_permissions(0o644)
        .system(System::Unix);
    for (name, contents) in entries {
        writer.start_file(*name, options).expect("start ZIP entry");
        writer
            .write_all(contents.as_bytes())
            .expect("write ZIP entry");
    }
    writer.finish().expect("finish synthetic ZIP");
}

fn write_json(path: &Path, value: &Value) {
    fs::write(
        path,
        serde_json::to_vec_pretty(value).expect("serialize JSON"),
    )
    .expect("write JSON fixture");
}

fn approve_exact_profile(workspace: &Path, profile_path: &Path) -> String {
    let registered = register_profile(
        workspace,
        &project_path(PROFILE_SCHEMA),
        profile_path,
        &RegistryLimits::default(),
    )
    .expect("register reviewed profile");
    let approved = approve_profile(
        workspace,
        PROFILE_ID,
        PROFILE_VERSION,
        &registered.profile_hash,
        "WP-003 bounded-correction regression",
        "Authorize this exact synthetic profile for local mapping and validation only",
        true,
        &RegistryLimits::default(),
    )
    .expect("approve exact profile");
    assert_eq!(approved.authority, ProfileAuthority::LocalMappingValidation);
    approved.profile_hash
}

fn ingest_and_authorize(
    temporary: &TempDir,
    archive_path: &Path,
    profile_path: &Path,
) -> AuthoritySetup {
    let workspace = temporary.path().join("workspace");
    object_workspace_support::initialize_test_workspace(&workspace);
    let intake = ingest_package(archive_path, &workspace, &intake_policy())
        .expect("ingest synthetic package");
    let profile_hash = approve_exact_profile(&workspace, profile_path);
    AuthoritySetup {
        workspace,
        job_id: intake.job_id,
        source_hash: intake.source_hash,
        profile_hash,
    }
}

fn ingest_and_authorize_sales(temporary: &TempDir, archive_path: &Path) -> AuthoritySetup {
    let workspace = temporary.path().join("workspace");
    object_workspace_support::initialize_test_workspace(&workspace);
    let intake = ingest_package(archive_path, &workspace, &intake_policy())
        .expect("ingest synthetic sales package");
    let registered = register_profile(
        &workspace,
        &project_path(PROFILE_SCHEMA),
        &project_path(SALES_PROFILE_FIXTURE),
        &RegistryLimits::default(),
    )
    .expect("register reviewed synthetic sales profile");
    let approved = approve_profile(
        &workspace,
        SALES_PROFILE_ID,
        PROFILE_VERSION,
        &registered.profile_hash,
        "WP-003 synthetic sales lookup regression",
        "Authorize this exact target-neutral profile for local mapping and validation only",
        true,
        &RegistryLimits::default(),
    )
    .expect("approve exact synthetic sales profile");
    assert_eq!(approved.authority, ProfileAuthority::LocalMappingValidation);
    AuthoritySetup {
        workspace,
        job_id: intake.job_id,
        source_hash: intake.source_hash,
        profile_hash: approved.profile_hash,
    }
}

fn mapping_document(setup: &AuthoritySetup, mapping_id: &str, source_files: &[&str]) -> Value {
    let mut bindings = vec![json!({
        "bindingId": "synthetic.customer.duplicate-group",
        "targetFieldId": "duplicateGroupKey",
        "kind": "constant",
        "constant": "SYNTHETIC-DUPLICATE"
    })];
    for (index, source_file) in source_files.iter().enumerate() {
        bindings.extend([
            json!({
                "bindingId": format!("synthetic.file-{index}.category"),
                "sourceFile": source_file,
                "sourceColumn": "category",
                "sourceColumnOrdinal": 1,
                "targetFieldId": "category",
                "kind": "lookup",
                "lookupTableId": "synthetic-category-map"
            }),
            json!({
                "bindingId": format!("synthetic.file-{index}.name"),
                "sourceFile": source_file,
                "sourceColumn": "organizationName1",
                "sourceColumnOrdinal": 2,
                "targetFieldId": "name",
                "kind": "trim"
            }),
            json!({
                "bindingId": format!("synthetic.file-{index}.notes"),
                "sourceFile": source_file,
                "sourceColumn": "notes",
                "sourceColumnOrdinal": 3,
                "targetFieldId": "notes",
                "kind": "empty_as_null"
            }),
            json!({
                "bindingId": format!("synthetic.file-{index}.source-id"),
                "sourceFile": source_file,
                "sourceColumn": "migrationPartnerId",
                "sourceColumnOrdinal": 0,
                "targetFieldId": "sourceCustomerId",
                "kind": "direct"
            }),
        ]);
    }
    json!({
        "schemaVersion": 1,
        "mappingId": mapping_id,
        "version": MAPPING_VERSION,
        "profile": {
            "profileId": PROFILE_ID,
            "version": PROFILE_VERSION,
            "sha256": setup.profile_hash
        },
        "source": {
            "packageSha256": setup.source_hash,
            "sourceFiles": source_files
        },
        "rootEntityId": "Customer",
        "bindings": bindings,
        "referenceTables": [
            {
                "tableId": "synthetic-category-domain",
                "version": "1.0.0",
                "values": {
                    "ORGANIZATION": "ORGANIZATION",
                    "PERSON": "PERSON"
                }
            },
            {
                "tableId": "synthetic-category-map",
                "version": "1.0.0",
                "values": {
                    "1": "PERSON",
                    "2": "ORGANIZATION"
                }
            }
        ],
        "author": "Migration Stream synthetic correction proof",
        "reviewer": "Synthetic bounded-correction review evidence"
    })
}

fn sales_mapping_document(setup: &AuthoritySetup, mapping_id: &str) -> Value {
    let binding = |binding_id: &str,
                   entity_id: &str,
                   source_file: &str,
                   source_column: &str,
                   source_column_ordinal: usize,
                   target_field_id: &str,
                   kind: &str,
                   lookup_table_id: Option<&str>| {
        let mut value = json!({
            "bindingId": binding_id,
            "targetEntityId": entity_id,
            "sourceFile": source_file,
            "sourceColumn": source_column,
            "sourceColumnOrdinal": source_column_ordinal,
            "targetFieldId": target_field_id,
            "kind": kind
        });
        if let Some(table_id) = lookup_table_id {
            value["lookupTableId"] = json!(table_id);
        }
        value
    };
    json!({
        "schemaVersion": 1,
        "mappingId": mapping_id,
        "version": MAPPING_VERSION,
        "profile": {
            "profileId": SALES_PROFILE_ID,
            "version": PROFILE_VERSION,
            "sha256": setup.profile_hash
        },
        "source": {
            "packageSha256": setup.source_hash,
            "sourceFiles": ["deal_book.csv", "deal_detail.csv"]
        },
        "rootEntityId": "SalesDocument",
        "sourceEntityAssignments": [
            {"sourceFile": "deal_book.csv", "entityId": "SalesDocument"},
            {"sourceFile": "deal_detail.csv", "entityId": "SalesDocumentItem"}
        ],
        "bindings": [
            binding("header.source-document-id", "SalesDocument", "deal_book.csv", "case_ref", 0, "sourceDocumentId", "direct", None),
            binding("header.customer-id", "SalesDocument", "deal_book.csv", "party_ref", 1, "customerId", "trim", None),
            binding("header.document-date", "SalesDocument", "deal_book.csv", "booked_on", 2, "documentDate", "direct", None),
            binding("header.document-type", "SalesDocument", "deal_book.csv", "route_code", 3, "documentType", "lookup", Some("sales-document-route-map")),
            binding("header.currency", "SalesDocument", "deal_book.csv", "money_iso", 4, "currency", "uppercase_ascii", None),
            binding("header.customer-reference", "SalesDocument", "deal_book.csv", "buy_ref", 5, "customerReference", "empty_as_null", None),
            binding("header.selling-unit", "SalesDocument", "deal_book.csv", "owning_desk", 6, "sellingUnit", "uppercase_ascii", None),
            binding("item.parent-document-id", "SalesDocumentItem", "deal_detail.csv", "case_link", 0, "parentDocumentId", "direct", None),
            binding("item.source-item-id", "SalesDocumentItem", "deal_detail.csv", "seq_ref", 1, "sourceItemId", "direct", None),
            binding("item.product-id", "SalesDocumentItem", "deal_detail.csv", "offer_ref", 2, "productId", "trim", None),
            binding("item.quantity", "SalesDocumentItem", "deal_detail.csv", "demand_qty", 3, "quantity", "direct", None),
            binding("item.unit-of-measure", "SalesDocumentItem", "deal_detail.csv", "measure", 4, "unitOfMeasure", "uppercase_ascii", None),
            binding("item.unit-price", "SalesDocumentItem", "deal_detail.csv", "net_rate", 5, "unitPrice", "empty_as_null", None),
            binding("item.requested-date", "SalesDocumentItem", "deal_detail.csv", "need_by", 6, "requestedDate", "empty_as_null", None)
        ],
        "relationshipBindings": [{
            "relationshipId": "sales-document.items",
            "fromEntityId": "SalesDocument",
            "toEntityId": "SalesDocumentItem",
            "cardinality": "1..*",
            "keyPairs": [{
                "fromFieldId": "sourceDocumentId",
                "toFieldId": "parentDocumentId"
            }]
        }],
        "referenceTables": [
            {
                "tableId": "sales-document-type-domain",
                "version": "1.0.0",
                "entityId": "SalesDocument",
                "values": {"RETURN": "RETURN", "STANDARD": "STANDARD"}
            },
            {
                "tableId": "sales-document-route-map",
                "version": "1.0.0",
                "entityId": "SalesDocument",
                "values": {"N": "STANDARD", "R": "RETURN"}
            }
        ],
        "author": "Migration Stream synthetic sales lookup proof",
        "reviewer": "WP-003 fail-closed lookup review evidence"
    })
}

fn register_mapping(
    setup: &AuthoritySetup,
    mapping_path: &Path,
) -> migration_stream::Result<MappingValidationReport> {
    validate_mapping(
        &setup.workspace,
        &setup.job_id,
        &project_path(MAPPING_SCHEMA),
        &project_path(PROFILE_SCHEMA),
        mapping_path,
    )
}

fn setup_standard_fixture() -> GovernedFixture {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let setup = ingest_and_authorize(
        &temporary,
        &project_path("fixtures/packages/valid.zip"),
        &project_path(PROFILE_FIXTURE),
    );
    let mapping_path = temporary.path().join("mapping.json");
    write_json(
        &mapping_path,
        &mapping_document(&setup, STANDARD_MAPPING_ID, &["customer_accounts.csv"]),
    );
    register_mapping(&setup, &mapping_path).expect("register governed mapping");
    GovernedFixture {
        temporary,
        setup,
        mapping_path,
    }
}

fn validation_policy() -> ValidationPolicy {
    ValidationPolicy::from_path(&project_path(POLICY_FIXTURE))
        .expect("load bounded validation policy")
}

fn validation_run_count(workspace: &Path) -> i64 {
    Connection::open(workspace.join("state.sqlite3"))
        .expect("open authoritative state")
        .query_row("SELECT COUNT(*) FROM validation_runs", [], |row| row.get(0))
        .expect("count validation runs")
}

fn latest_validation_run_id(workspace: &Path) -> String {
    Connection::open(workspace.join("state.sqlite3"))
        .expect("open authoritative validation state")
        .query_row(
            "SELECT id FROM validation_runs ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("load latest validation run")
}

fn validation_event_states(workspace: &Path, run_id: &str) -> Vec<String> {
    let connection =
        Connection::open(workspace.join("state.sqlite3")).expect("open validation events");
    let mut statement = connection
        .prepare("SELECT state FROM validation_events WHERE run_id = ?1 ORDER BY sequence")
        .expect("prepare validation-event query");
    statement
        .query_map([run_id], |row| row.get(0))
        .expect("query validation events")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect validation events")
}

fn validation_issue_evidence(workspace: &Path, run_id: &str) -> Vec<Value> {
    let connection =
        Connection::open(workspace.join("state.sqlite3")).expect("open authoritative state");
    let mut statement = connection
        .prepare(
            "SELECT source_ordinal, severity, rule_id, issue_code, issue_id, issue_hash
             FROM validation_issues
             WHERE run_id = ?1
             ORDER BY source_ordinal, rule_id, issue_code, issue_id",
        )
        .expect("prepare golden issue query");
    statement
        .query_map([run_id], |row| {
            Ok(json!({
                "sourceOrdinal": row.get::<_, u64>(0)?,
                "severity": row.get::<_, String>(1)?,
                "ruleId": row.get::<_, String>(2)?,
                "issueCode": row.get::<_, String>(3)?,
                "issueId": row.get::<_, String>(4)?,
                "issueSha256": row.get::<_, String>(5)?,
            }))
        })
        .expect("query golden issues")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect golden issues")
}

fn mapping_rejection(
    setup: &AuthoritySetup,
    temporary: &TempDir,
    label: &str,
    document: &Value,
    expected_code: &str,
    expected_diagnostic: &str,
) {
    let path = temporary.path().join(format!("matrix-{label}.json"));
    write_json(&path, document);
    let error = expect_error(register_mapping(setup, &path), expected_code);
    assert!(
        error
            .to_string()
            .to_ascii_lowercase()
            .contains(&expected_diagnostic.to_ascii_lowercase()),
        "{label} failed for the wrong reason: {error}"
    );
}

#[test]
fn all_required_rule_types_match_checked_in_golden_evidence() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let archive = temporary.path().join("all-rules-golden.zip");
    let long_name = "A".repeat(65);
    let csv = format!(
        "migrationPartnerId,category,organizationName1,notes\n\
         ID-1,1,Alice,pass\n\
         ,3,Bob,required allowed and reference\n\
         ID-3,1,{long_name},maximum length\n\
         ID-4,1,Αθήνα,pattern\n\
         ID-5,2,Organization One,first organization\n\
         ID-6,2,Organization Two,duplicate organization\n\
         ID-7,2,,conditional required\n"
    );
    write_zip(&archive, &[("customer_accounts.csv", &csv)]);
    let setup = ingest_and_authorize(&temporary, &archive, &project_path(PROFILE_FIXTURE));

    let mut mapping = mapping_document(&setup, GOLDEN_MAPPING_ID, &["customer_accounts.csv"]);
    mapping["referenceTables"][1]["values"]["3"] = json!("OUTSIDE");
    let mapping_path = temporary.path().join("all-rules-golden-mapping.json");
    write_json(&mapping_path, &mapping);
    let mapping_report =
        register_mapping(&setup, &mapping_path).expect("register all-rule golden mapping");

    let first = run_validation(
        &setup.workspace,
        &setup.job_id,
        GOLDEN_MAPPING_ID,
        MAPPING_VERSION,
        &project_path(PROFILE_SCHEMA),
        &validation_policy(),
    )
    .expect("run all-rule golden validation");
    let issues = validation_issue_evidence(&setup.workspace, &first.run_id);
    let actual = json!({
        "schemaVersion": 1,
        "fixtureKind": "SYNTHETIC_ONLY",
        "source": {
            "packageSha256": setup.source_hash,
            "file": "customer_accounts.csv",
            "rows": 7,
            "columns": 4
        },
        "profile": {
            "profileId": PROFILE_ID,
            "version": PROFILE_VERSION,
            "sha256": setup.profile_hash
        },
        "mapping": {
            "mappingId": GOLDEN_MAPPING_ID,
            "version": MAPPING_VERSION,
            "sha256": mapping_report.mapping_hash,
            "mappedCanonicalFields": mapping_report.coverage.mapped_canonical_fields,
            "defaultedCanonicalFields": mapping_report.coverage.defaulted_canonical_fields,
            "unmappedSourceColumns": mapping_report.coverage.unmapped_source_columns,
            "requiredCanonicalFieldsWithoutCoverage":
                mapping_report.coverage.required_canonical_fields_without_coverage,
            "invalidBindings": mapping_report.coverage.invalid_bindings
        },
        "validation": {
            "identitySha256": first.identity_hash,
            "resultSha256": first.result_hash,
            "rowsProcessed": first.result.rows_processed,
            "issueCount": first.result.issue_count,
            "severityCounts": first.result.severity_counts,
            "ruleCounts": first.result.rule_counts,
            "issueHashes": first.result.issue_hashes,
            "issues": issues
        },
        "claims": {
            "externalSystemCompatibility": false,
            "targetCompatibility": false,
            "clientData": false
        }
    });
    let expected: Value = serde_json::from_slice(
        &fs::read(project_path(
            "fixtures/wp003/expected-all-rules-golden.json",
        ))
        .expect("read checked-in all-rule golden evidence"),
    )
    .expect("parse checked-in all-rule golden evidence");
    assert_eq!(
        actual, expected,
        "all-rule evidence differs from the checked-in golden contract"
    );

    let second_intake = ingest_package(&archive, &setup.workspace, &intake_policy())
        .expect("ingest independent identical all-rule source");
    let second = run_validation(
        &setup.workspace,
        &second_intake.job_id,
        GOLDEN_MAPPING_ID,
        MAPPING_VERSION,
        &project_path(PROFILE_SCHEMA),
        &validation_policy(),
    )
    .expect("replay all-rule golden validation");
    assert_ne!(first.run_id, second.run_id);
    assert_eq!(first.identity_hash, second.identity_hash);
    assert_eq!(first.result_hash, second.result_hash);
    assert_eq!(first.result, second.result);
    assert_eq!(
        validation_issue_evidence(&setup.workspace, &first.run_id),
        validation_issue_evidence(&setup.workspace, &second.run_id)
    );
}

#[test]
fn mapping_rejection_matrix_reports_the_intended_closed_boundary() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let setup = ingest_and_authorize(
        &temporary,
        &project_path("fixtures/packages/valid.zip"),
        &project_path(PROFILE_FIXTURE),
    );
    let base = mapping_document(
        &setup,
        "synthetic.customer.matrix-base",
        &["customer_accounts.csv"],
    );

    let mut unknown_source = base.clone();
    unknown_source["mappingId"] = json!("synthetic.customer.matrix-unknown-source");
    unknown_source["source"]["sourceFiles"] = json!(["absent.csv"]);
    for binding in unknown_source["bindings"]
        .as_array_mut()
        .expect("mapping bindings")
    {
        if binding.get("sourceFile").is_some() {
            binding["sourceFile"] = json!("absent.csv");
        }
    }
    mapping_rejection(
        &setup,
        &temporary,
        "unknown-source",
        &unknown_source,
        "MAPPING_INVALID",
        "not an accepted csv",
    );

    let mut unknown_target = base.clone();
    unknown_target["mappingId"] = json!("synthetic.customer.matrix-unknown-target");
    unknown_target["bindings"][3]["targetFieldId"] = json!("inventedField");
    mapping_rejection(
        &setup,
        &temporary,
        "unknown-target",
        &unknown_target,
        "MAPPING_INVALID",
        "unknown canonical field",
    );

    let mut cardinality = base.clone();
    cardinality["mappingId"] = json!("synthetic.customer.matrix-cardinality");
    cardinality["bindings"][1]["cardinality"] = json!("many");
    mapping_rejection(
        &setup,
        &temporary,
        "cardinality",
        &cardinality,
        "MAPPING_INVALID",
        "cardinality",
    );

    let mut empty_constant = base.clone();
    empty_constant["mappingId"] = json!("synthetic.customer.matrix-empty-constant");
    empty_constant["bindings"][0]["constant"] = json!("");
    mapping_rejection(
        &setup,
        &temporary,
        "empty-constant",
        &empty_constant,
        "MAPPING_INVALID",
        "empty",
    );

    let mut empty_lookup = base.clone();
    empty_lookup["mappingId"] = json!("synthetic.customer.matrix-empty-lookup");
    empty_lookup["referenceTables"][1]["values"]["1"] = json!("");
    mapping_rejection(
        &setup,
        &temporary,
        "empty-lookup",
        &empty_lookup,
        "MAPPING_INVALID",
        "empty",
    );

    let mut version_mismatch = base.clone();
    version_mismatch["mappingId"] = json!("synthetic.customer.matrix-profile-version");
    version_mismatch["profile"]["version"] = json!("9.9.9");
    mapping_rejection(
        &setup,
        &temporary,
        "profile-version",
        &version_mismatch,
        "NOT_FOUND",
        "registered profile",
    );

    let mut collision = base;
    collision["mappingId"] = json!("synthetic.customer.matrix-collision");
    let collision_path = temporary.path().join("matrix-collision-original.json");
    write_json(&collision_path, &collision);
    register_mapping(&setup, &collision_path).expect("register collision baseline");
    collision["reviewer"] = json!("Different synthetic review evidence");
    mapping_rejection(
        &setup,
        &temporary,
        "mapping-collision",
        &collision,
        "MAPPING_CONFLICT",
        "different canonical bytes",
    );
}

#[test]
fn multi_file_required_coverage_is_per_file_and_same_targets_are_scoped() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let archive = temporary.path().join("multi.zip");
    let header = "migrationPartnerId,category,organizationName1,notes\n";
    write_zip(
        &archive,
        &[
            ("a.csv", &format!("{header}A-1,1,Alice,first\n")),
            ("b.csv", &format!("{header}B-1,2,Beta,second\n")),
        ],
    );
    let setup = ingest_and_authorize(&temporary, &archive, &project_path(PROFILE_FIXTURE));

    let valid = mapping_document(&setup, "synthetic.customer.multi", &["a.csv", "b.csv"]);
    let mut missing_file_coverage = valid.clone();
    missing_file_coverage["bindings"]
        .as_array_mut()
        .expect("mapping bindings")
        .retain(|binding| binding.get("sourceFile").and_then(Value::as_str) != Some("b.csv"));
    let invalid_path = temporary.path().join("missing-b-coverage.json");
    write_json(&invalid_path, &missing_file_coverage);
    let error = expect_error(register_mapping(&setup, &invalid_path), "MAPPING_INVALID");
    assert!(error.to_string().contains("b.csv:category"));
    assert!(error.to_string().contains("b.csv:sourceCustomerId"));

    let valid_path = temporary.path().join("valid-multi.json");
    write_json(&valid_path, &valid);
    let report = register_mapping(&setup, &valid_path)
        .expect("same canonical targets are valid in distinct source-file scopes");
    assert_eq!(report.coverage.mapped_canonical_fields.len(), 5);
    assert_eq!(
        report.coverage.required_canonical_fields_without_coverage,
        [] as [migration_stream::mapping::RequiredCanonicalFieldIdentity; 0]
    );
}

#[test]
fn mapping_lookup_miss_is_explicit_and_cannot_complete() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let archive = temporary.path().join("lookup-miss.zip");
    write_zip(
        &archive,
        &[(
            "customer_accounts.csv",
            "migrationPartnerId,category,organizationName1,notes\nMISS-1,9,Unknown,synthetic\n",
        )],
    );
    let setup = ingest_and_authorize(&temporary, &archive, &project_path(PROFILE_FIXTURE));
    let mapping_path = temporary.path().join("lookup-mapping.json");
    write_json(
        &mapping_path,
        &mapping_document(
            &setup,
            "synthetic.customer.lookup-miss",
            &["customer_accounts.csv"],
        ),
    );
    register_mapping(&setup, &mapping_path).expect("mapping shape is valid before row projection");

    let error = expect_error(
        run_validation(
            &setup.workspace,
            &setup.job_id,
            "synthetic.customer.lookup-miss",
            MAPPING_VERSION,
            &project_path(PROFILE_SCHEMA),
            &validation_policy(),
        ),
        "VALIDATION_INVALID",
    );
    assert!(error.to_string().contains("no exact entry"));
    assert!(error.to_string().contains("source value suppressed"));

    let connection =
        Connection::open(setup.workspace.join("state.sqlite3")).expect("open authoritative state");
    let (run_id, state, result_hash): (String, String, Option<String>) = connection
        .query_row(
            "SELECT id, state, result_hash FROM validation_runs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read failed validation attempt");
    assert_eq!(state, "FAILED");
    assert!(result_hash.is_none());
    assert_eq!(
        validation_status(&setup.workspace, &run_id)
            .expect("failed validation status")
            .state,
        ValidationRunState::Failed
    );
    let complete_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM validation_runs WHERE state = 'COMPLETE'",
            [],
            |row| row.get(0),
        )
        .expect("count complete validation runs");
    assert_eq!(complete_count, 0);
}

#[test]
fn sales_mapping_unconfigured_route_key_fails_closed_without_value_or_row_issue() {
    const SUPPRESSED_ROUTE_KEY: &str = "UNCONFIGURED-ROUTE-9";
    const MAPPING_ID: &str = "synthetic.sales-document.lookup-miss";

    let temporary = tempfile::tempdir().expect("temporary directory");
    let archive = temporary.path().join("sales-lookup-miss.zip");
    write_zip(
        &archive,
        &[
            (
                "deal_book.csv",
                &format!(
                    "case_ref,party_ref,booked_on,route_code,money_iso,buy_ref,owning_desk\n\
                     SD-LOOKUP-1,CUST-SYNTHETIC,2026-09-01,{SUPPRESSED_ROUTE_KEY},eur,PO-SYNTHETIC,north\n"
                ),
            ),
            (
                "deal_detail.csv",
                "case_link,seq_ref,offer_ref,demand_qty,measure,net_rate,need_by\n\
                 SD-LOOKUP-1,0010,SKU-SYNTHETIC,1,ea,1.00,2026-09-10\n",
            ),
        ],
    );
    let setup = ingest_and_authorize_sales(&temporary, &archive);
    let mapping_path = temporary.path().join("sales-lookup-mapping.json");
    write_json(&mapping_path, &sales_mapping_document(&setup, MAPPING_ID));
    register_mapping(&setup, &mapping_path)
        .expect("sales mapping shape is valid before received-value projection");

    let error = expect_error(
        run_validation(
            &setup.workspace,
            &setup.job_id,
            MAPPING_ID,
            MAPPING_VERSION,
            &project_path(PROFILE_SCHEMA),
            &validation_policy(),
        ),
        "VALIDATION_INVALID",
    );
    let diagnostic = error.to_string();
    assert!(diagnostic.contains("no exact entry"));
    assert!(diagnostic.contains("source value suppressed"));
    assert!(
        !diagnostic.contains(SUPPRESSED_ROUTE_KEY),
        "lookup diagnostic exposed the unconfigured received source key"
    );

    let connection =
        Connection::open(setup.workspace.join("state.sqlite3")).expect("open authoritative state");
    let (run_id, state, result_hash): (String, String, Option<String>) = connection
        .query_row(
            "SELECT id, state, result_hash FROM validation_runs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read failed sales lookup validation attempt");
    assert_eq!(state, "FAILED");
    assert!(result_hash.is_none());
    assert_eq!(
        validation_status(&setup.workspace, &run_id)
            .expect("failed sales lookup validation status")
            .state,
        ValidationRunState::Failed
    );
    let issue_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM validation_issues WHERE run_id = ?1",
            [&run_id],
            |row| row.get(0),
        )
        .expect("count sales lookup issues");
    assert_eq!(
        issue_count, 0,
        "WP-003 lookup misses remain terminal projection failures, not row issues"
    );
}

#[test]
fn missing_exact_reference_table_is_rejected_before_zero_row_execution() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let archive = temporary.path().join("zero-rows.zip");
    write_zip(
        &archive,
        &[(
            "customer_accounts.csv",
            "migrationPartnerId,category,organizationName1,notes\n",
        )],
    );
    let mut profile: Value = serde_json::from_slice(
        &fs::read(project_path(PROFILE_FIXTURE)).expect("read profile fixture"),
    )
    .expect("parse profile fixture");
    profile["rules"][6]["parameters"]["tableId"] = json!("absent-category-domain");
    profile["rules"][6]["parameters"]["tableVersion"] = json!("9.9.9");
    let profile_path = temporary.path().join("missing-reference-profile.json");
    write_json(&profile_path, &profile);
    let setup = ingest_and_authorize(&temporary, &archive, &profile_path);
    let connection =
        Connection::open(setup.workspace.join("state.sqlite3")).expect("open authoritative state");
    let accepted_rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM source_records WHERE job_id = ?1",
            [&setup.job_id],
            |row| row.get(0),
        )
        .expect("count accepted zero-row source");
    assert_eq!(accepted_rows, 0);
    drop(connection);

    let mapping_path = temporary.path().join("zero-row-mapping.json");
    write_json(
        &mapping_path,
        &mapping_document(
            &setup,
            "synthetic.customer.missing-reference",
            &["customer_accounts.csv"],
        ),
    );
    register_mapping(&setup, &mapping_path).expect("mapping does not invent profile rule tables");
    let error = expect_error(
        run_validation(
            &setup.workspace,
            &setup.job_id,
            "synthetic.customer.missing-reference",
            MAPPING_VERSION,
            &project_path(PROFILE_SCHEMA),
            &validation_policy(),
        ),
        "VALIDATION_INVALID",
    );
    assert!(error.to_string().contains("references absent table"));
    assert_eq!(validation_run_count(&setup.workspace), 0);
}

#[test]
fn cleanup_pending_is_inert_but_relevant_conflict_blocks_dependent_work() {
    let fixture = setup_standard_fixture();
    let connection = Connection::open(fixture.setup.workspace.join("state.sqlite3"))
        .expect("open authoritative state");
    let before_counts: (i64, i64, i64) = connection
        .query_row(
            "SELECT source_records, stored_records, pending_records FROM jobs WHERE id = ?1",
            [&fixture.setup.job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read accepted-source counts");
    connection
        .execute(
            "INSERT INTO partial_cleanups (
                 cleanup_id, namespace, attempt_id, job_id, state,
                 cleanup_attempts, created_at, updated_at, quarantined_at
             ) VALUES (
                 'synthetic-cleanup-proof', 'packages', ?1, ?1, 'CLEANUP_PENDING',
                 1, '2026-07-22T00:00:00.000Z', '2026-07-22T00:00:00.000Z',
                 '2026-07-22T00:00:00.000Z'
             )",
            [&fixture.setup.job_id],
        )
        .expect("insert inert cleanup-pending evidence");
    drop(connection);

    let profile_before = inspect_profile(&fixture.setup.workspace, PROFILE_ID, PROFILE_VERSION)
        .expect("inspect authority before inert cleanup");
    assert_eq!(
        profile_before.authority,
        ProfileAuthority::LocalMappingValidation
    );
    register_mapping(&fixture.setup, &fixture.mapping_path)
        .expect("inert cleanup pending must not become source authority or block exact mapping");
    let validation = run_validation(
        &fixture.setup.workspace,
        &fixture.setup.job_id,
        STANDARD_MAPPING_ID,
        MAPPING_VERSION,
        &project_path(PROFILE_SCHEMA),
        &validation_policy(),
    )
    .expect("inert cleanup pending must not affect accepted source validation");
    let profile_after = inspect_profile(&fixture.setup.workspace, PROFILE_ID, PROFILE_VERSION)
        .expect("inspect authority after inert cleanup");
    assert_eq!(profile_after.authority, profile_before.authority);

    let connection = Connection::open(fixture.setup.workspace.join("state.sqlite3"))
        .expect("reopen authoritative state");
    let after_counts: (i64, i64, i64) = connection
        .query_row(
            "SELECT source_records, stored_records, pending_records FROM jobs WHERE id = ?1",
            [&fixture.setup.job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read source counts after inert cleanup");
    assert_eq!(after_counts, before_counts);
    connection
        .execute(
            "UPDATE partial_cleanups
             SET state = 'CONFLICT', last_error_code = 'RECOVERY_CONFLICT',
                 last_error_message = 'synthetic simultaneous live/tombstone proof'
             WHERE cleanup_id = 'synthetic-cleanup-proof'",
            [],
        )
        .expect("promote deliberate cleanup evidence to conflict");
    drop(connection);

    expect_error(
        register_mapping(&fixture.setup, &fixture.mapping_path),
        "CONFLICT",
    );
    let run_count_before = validation_run_count(&fixture.setup.workspace);
    expect_error(
        run_validation(
            &fixture.setup.workspace,
            &fixture.setup.job_id,
            STANDARD_MAPPING_ID,
            MAPPING_VERSION,
            &project_path(PROFILE_SCHEMA),
            &validation_policy(),
        ),
        "CONFLICT",
    );
    assert_eq!(
        validation_run_count(&fixture.setup.workspace),
        run_count_before
    );
    assert_eq!(
        validation_status(&fixture.setup.workspace, &validation.run_id)
            .expect("prior complete validation remains immutable")
            .state,
        ValidationRunState::Complete
    );
}

#[test]
fn integrity_reconciles_coverage_results_rules_and_lifecycle_after_rehashed_tamper() {
    let fixture = setup_standard_fixture();
    let validation = run_validation(
        &fixture.setup.workspace,
        &fixture.setup.job_id,
        STANDARD_MAPPING_ID,
        MAPPING_VERSION,
        &project_path(PROFILE_SCHEMA),
        &validation_policy(),
    )
    .expect("complete validation before tamper proof");
    let baseline = integrity_check(&fixture.setup.workspace).expect("baseline integrity replay");
    assert!(baseline.ok, "baseline issues: {:?}", baseline.issues);

    let connection = Connection::open(fixture.setup.workspace.join("state.sqlite3"))
        .expect("open authoritative state for deliberate tamper");
    let coverage_json: String = connection
        .query_row(
            "SELECT coverage_json FROM mapping_versions
             WHERE mapping_id = ?1 AND mapping_version = ?2",
            params![STANDARD_MAPPING_ID, MAPPING_VERSION],
            |row| row.get(0),
        )
        .expect("read mapping coverage");
    let mut coverage: Value = serde_json::from_str(&coverage_json).expect("parse mapping coverage");
    coverage["mappingId"] = json!("synthetic.customer.rehashed-tamper");
    let tampered_coverage_json = canonical_json_string(&coverage).expect("canonical coverage");
    let tampered_coverage_hash = canonical_hash(&coverage).expect("hash tampered coverage");
    with_immutability_guard_disabled(
        &connection,
        "mapping_versions_no_update",
        MAPPING_VERSIONS_UPDATE_GUARD_SQL,
        |connection| {
            connection
                .execute(
                    "UPDATE mapping_versions SET coverage_json = ?1, coverage_hash = ?2
                     WHERE mapping_id = ?3 AND mapping_version = ?4",
                    params![
                        tampered_coverage_json,
                        tampered_coverage_hash,
                        STANDARD_MAPPING_ID,
                        MAPPING_VERSION
                    ],
                )
                .expect("tamper and rehash mapping coverage identity");
        },
    );

    let run_evidence: (String, String, String, String, String, String) = connection
        .query_row(
            "SELECT source_hash, profile_hash, mapping_hash, policy_hash,
                    engine_version, result_json
             FROM validation_runs WHERE id = ?1",
            [&validation.run_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .expect("read validation evidence");
    let tampered_rule_set = "0".repeat(64);
    let tampered_identity_hash = canonical_hash(&json!({
        "contract": run_evidence.4,
        "sourceHash": run_evidence.0,
        "profileHash": run_evidence.1,
        "mappingHash": run_evidence.2,
        "ruleSetHash": tampered_rule_set,
        "policyHash": run_evidence.3
    }))
    .expect("rehash tampered validation identity");
    let mut result: Value = serde_json::from_str(&run_evidence.5).expect("parse validation result");
    let original_errors = result["severityCounts"]
        .get("ERROR")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    result["severityCounts"]["ERROR"] = json!(original_errors + 1);
    result["ruleSetHash"] = json!(tampered_rule_set);
    let tampered_result_json = canonical_json_string(&result).expect("canonical tampered result");
    let tampered_result_hash = canonical_hash(&result).expect("hash tampered result");
    connection
        .execute(
            "UPDATE validation_runs
             SET rule_set_hash = ?1, identity_hash = ?2, result_json = ?3, result_hash = ?4
             WHERE id = ?5",
            params![
                "0".repeat(64),
                tampered_identity_hash,
                tampered_result_json,
                tampered_result_hash,
                validation.run_id
            ],
        )
        .expect("tamper and rehash validation result/rule identity");
    connection
        .execute(
            "UPDATE validation_events SET state = 'FAILED'
             WHERE sequence = (
                 SELECT MIN(sequence) FROM validation_events WHERE run_id = ?1
             )",
            [&validation.run_id],
        )
        .expect("tamper validation lifecycle event");
    drop(connection);

    let replay = integrity_check(&fixture.setup.workspace).expect("tampered integrity replay");
    assert!(!replay.ok);
    let diagnostics = replay.issues.join("\n").to_ascii_lowercase();
    assert!(diagnostics.contains("coverage identities differ"));
    assert!(diagnostics.contains("rule-set hash differs"));
    assert!(diagnostics.contains("result differs from deterministic"));
    assert!(diagnostics.contains("lifecycle events do not match"));
}

#[test]
fn deterministic_replay_rejects_a_deleted_issue_with_fully_rehashed_aggregates() {
    let fixture = setup_standard_fixture();
    let validation = run_validation(
        &fixture.setup.workspace,
        &fixture.setup.job_id,
        STANDARD_MAPPING_ID,
        MAPPING_VERSION,
        &project_path(PROFILE_SCHEMA),
        &validation_policy(),
    )
    .expect("complete validation before coherent issue tamper");
    let connection = Connection::open(fixture.setup.workspace.join("state.sqlite3"))
        .expect("open validation evidence for coherent tamper");
    let removed: (String, String, String, String, String, String) = connection
        .query_row(
            "SELECT issue_id, issue_hash, severity, rule_id, entity_id, field_id
             FROM validation_issues WHERE run_id = ?1 ORDER BY issue_id LIMIT 1",
            [&validation.run_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .expect("select issue to remove");
    let source_file: String = connection
        .query_row(
            "SELECT source_file FROM validation_issues
             WHERE run_id = ?1 AND issue_id = ?2",
            params![validation.run_id, removed.0],
            |row| row.get(0),
        )
        .expect("load removed issue source file");
    connection
        .execute(
            "DELETE FROM validation_issues WHERE run_id = ?1 AND issue_id = ?2",
            params![validation.run_id, removed.0],
        )
        .expect("delete one persisted issue");
    let result_json: String = connection
        .query_row(
            "SELECT result_json FROM validation_runs WHERE id = ?1",
            [&validation.run_id],
            |row| row.get(0),
        )
        .expect("load completed result");
    let mut result: Value = serde_json::from_str(&result_json).expect("parse completed result");
    result["issueCount"] = json!(4);
    for (group, key) in [
        ("severityCounts", removed.2.as_str()),
        ("ruleCounts", removed.3.as_str()),
        ("entityCounts", removed.4.as_str()),
        ("fieldCounts", removed.5.as_str()),
        ("sourceFileCounts", source_file.as_str()),
    ] {
        let counts = result[group].as_object_mut().expect("result count map");
        let remaining = counts
            .get(key)
            .and_then(Value::as_u64)
            .expect("removed issue aggregate")
            .saturating_sub(1);
        if remaining == 0 {
            counts.remove(key);
        } else {
            counts.insert(key.to_owned(), json!(remaining));
        }
    }
    result["issueHashes"]
        .as_array_mut()
        .expect("result issue hashes")
        .retain(|hash| hash.as_str() != Some(removed.1.as_str()));
    let changed_result_json = canonical_json_string(&result).expect("canonical changed result");
    let changed_result_hash = canonical_hash(&result).expect("hash changed result");
    connection
        .execute(
            "UPDATE validation_runs
             SET issue_count = 4, result_json = ?1, result_hash = ?2
             WHERE id = ?3",
            params![changed_result_json, changed_result_hash, validation.run_id],
        )
        .expect("rehash all persisted issue aggregates");
    drop(connection);

    let status_error = expect_error(
        validation_status(&fixture.setup.workspace, &validation.run_id),
        "INTEGRITY_FAILED",
    );
    assert!(
        status_error
            .to_string()
            .contains("persisted rows/issues differ from deterministic")
    );

    let replay = integrity_check(&fixture.setup.workspace)
        .expect("replay coherently rehashed issue deletion");
    assert!(!replay.ok);
    assert!(
        replay
            .issues
            .join("\n")
            .contains("persisted rows/issues differ from deterministic")
    );
}

#[test]
fn aggregate_pattern_budget_persists_terminal_bounded_attempt() {
    let fixture = setup_standard_fixture();
    let mut policy = validation_policy();
    policy.maximum_total_pattern_bytes = 1;
    let error = expect_error(
        run_validation(
            &fixture.setup.workspace,
            &fixture.setup.job_id,
            STANDARD_MAPPING_ID,
            MAPPING_VERSION,
            &project_path(PROFILE_SCHEMA),
            &policy,
        ),
        "VALIDATION_BOUND_EXCEEDED",
    );
    assert!(error.to_string().contains("aggregate bound"));
    assert_eq!(validation_run_count(&fixture.setup.workspace), 1);
    let connection = Connection::open(fixture.setup.workspace.join("state.sqlite3"))
        .expect("open bounded validation evidence");
    let (run_id, state, result_json, result_hash, error_code): (
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = connection
        .query_row(
            "SELECT id, state, result_json, result_hash, error_code FROM validation_runs",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .expect("read aggregate-bound attempt");
    assert_eq!(state, "BOUND_EXCEEDED");
    assert!(result_json.is_none());
    assert!(result_hash.is_none());
    assert_eq!(error_code.as_deref(), Some("VALIDATION_BOUND_EXCEEDED"));
    let event_states = connection
        .prepare("SELECT state FROM validation_events WHERE run_id = ?1 ORDER BY sequence")
        .expect("prepare aggregate-bound lifecycle query")
        .query_map([&run_id], |row| row.get::<_, String>(0))
        .expect("query aggregate-bound lifecycle")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect aggregate-bound lifecycle");
    assert_eq!(event_states, vec!["RUNNING", "BOUND_EXCEEDED"]);
    drop(connection);
    let integrity = integrity_check(&fixture.setup.workspace).expect("replay bounded attempt");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

#[test]
fn aggregate_pattern_program_budget_persists_terminal_bounded_attempt() {
    let fixture = setup_standard_fixture();
    let mut policy = validation_policy();
    policy.maximum_pattern_program_bytes = 1;
    let error = expect_error(
        run_validation(
            &fixture.setup.workspace,
            &fixture.setup.job_id,
            STANDARD_MAPPING_ID,
            MAPPING_VERSION,
            &project_path(PROFILE_SCHEMA),
            &policy,
        ),
        "VALIDATION_BOUND_EXCEEDED",
    );
    assert!(error.to_string().contains("aggregate program bound"));
    let connection = Connection::open(fixture.setup.workspace.join("state.sqlite3"))
        .expect("open program-bound validation evidence");
    let (run_id, state, result_hash): (String, String, Option<String>) = connection
        .query_row(
            "SELECT id, state, result_hash FROM validation_runs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read program-bound attempt");
    assert_eq!(state, "BOUND_EXCEEDED");
    assert!(result_hash.is_none());
    assert_eq!(
        validation_status(&fixture.setup.workspace, &run_id)
            .expect("read program-bound status")
            .state,
        ValidationRunState::BoundExceeded
    );
    drop(connection);
    let integrity =
        integrity_check(&fixture.setup.workspace).expect("replay program-bound attempt");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

#[test]
fn aggregate_lookup_budget_persists_terminal_bounded_attempt() {
    let fixture = setup_standard_fixture();
    let mut policy = validation_policy();
    policy.maximum_lookup_entries = 1;
    let error = expect_error(
        run_validation(
            &fixture.setup.workspace,
            &fixture.setup.job_id,
            STANDARD_MAPPING_ID,
            MAPPING_VERSION,
            &project_path(PROFILE_SCHEMA),
            &policy,
        ),
        "VALIDATION_BOUND_EXCEEDED",
    );
    assert!(error.to_string().contains("mapping lookup tables require"));
    let connection = Connection::open(fixture.setup.workspace.join("state.sqlite3"))
        .expect("open lookup-bound validation evidence");
    let (run_id, state, result_hash, error_code): (String, String, Option<String>, Option<String>) =
        connection
            .query_row(
                "SELECT id, state, result_hash, error_code FROM validation_runs",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("read lookup-bound attempt");
    assert_eq!(state, "BOUND_EXCEEDED");
    assert!(result_hash.is_none());
    assert_eq!(error_code.as_deref(), Some("VALIDATION_BOUND_EXCEEDED"));
    let event_states = connection
        .prepare("SELECT state FROM validation_events WHERE run_id = ?1 ORDER BY sequence")
        .expect("prepare lookup-bound lifecycle query")
        .query_map([&run_id], |row| row.get::<_, String>(0))
        .expect("query lookup-bound lifecycle")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect lookup-bound lifecycle");
    assert_eq!(event_states, vec!["RUNNING", "BOUND_EXCEEDED"]);
    drop(connection);
    let integrity = integrity_check(&fixture.setup.workspace).expect("replay lookup-bound attempt");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

#[test]
fn cumulative_projection_source_bytes_persist_a_terminal_bounded_attempt() {
    let fixture = setup_standard_fixture();
    Connection::open(fixture.setup.workspace.join("state.sqlite3"))
        .expect("open accepted source evidence for bounded tamper")
        .execute(
            "UPDATE source_records
             SET fields_json = CAST(zeroblob(33554433) AS TEXT)
             WHERE job_id = ?1 AND file_path = 'customer_accounts.csv'
               AND source_ordinal = 1",
            [&fixture.setup.job_id],
        )
        .expect("install source batch above the cumulative projection-byte bound");

    let error = expect_error(
        run_validation(
            &fixture.setup.workspace,
            &fixture.setup.job_id,
            STANDARD_MAPPING_ID,
            MAPPING_VERSION,
            &project_path(PROFILE_SCHEMA),
            &validation_policy(),
        ),
        "VALIDATION_BOUND_EXCEEDED",
    );
    assert!(error.to_string().contains("projection batch source bytes"));
    let run_id = latest_validation_run_id(&fixture.setup.workspace);
    let status = validation_status(&fixture.setup.workspace, &run_id)
        .expect("read terminal projection-bound attempt");
    assert_eq!(status.state, ValidationRunState::BoundExceeded);
    assert_eq!(
        status.error_code.as_deref(),
        Some("VALIDATION_BOUND_EXCEEDED")
    );
    assert_eq!(
        validation_event_states(&fixture.setup.workspace, &run_id),
        vec!["RUNNING".to_owned(), "BOUND_EXCEEDED".to_owned()]
    );
}

#[test]
fn integrity_replays_unreferenced_reused_valid_mapping_attempts() {
    let fixture = setup_standard_fixture();
    let reused = register_mapping(&fixture.setup, &fixture.mapping_path)
        .expect("record a second exact VALID mapping attempt");
    assert!(reused.reused_immutable_version);
    let baseline =
        integrity_check(&fixture.setup.workspace).expect("replay complete mapping-attempt history");
    assert!(baseline.ok, "baseline issues: {:?}", baseline.issues);

    let connection = Connection::open(fixture.setup.workspace.join("state.sqlite3"))
        .expect("open mapping attempt evidence");
    with_immutability_guard_disabled(
        &connection,
        "mapping_validation_attempts_no_update",
        MAPPING_ATTEMPTS_UPDATE_GUARD_SQL,
        |connection| {
            connection
                .execute(
                    "UPDATE mapping_validation_attempts SET input_hash = ?1 WHERE id = ?2",
                    params!["0".repeat(64), reused.attempt_id],
                )
                .expect("tamper unreferenced VALID attempt");
        },
    );
    drop(connection);
    let replay =
        integrity_check(&fixture.setup.workspace).expect("replay tampered reused VALID attempt");
    assert!(!replay.ok);
    assert!(
        replay
            .issues
            .join("\n")
            .contains("immutable evidence is invalid")
    );
}

#[test]
fn integrity_replays_retained_invalid_mapping_attempts() {
    let fixture = setup_standard_fixture();
    let mut invalid = mapping_document(
        &fixture.setup,
        "synthetic.customer.retained-invalid",
        &["customer_accounts.csv"],
    );
    invalid["bindings"]
        .as_array_mut()
        .expect("mapping bindings")
        .retain(|binding| binding["targetFieldId"] != "sourceCustomerId");
    let invalid_path = fixture.temporary.path().join("retained-invalid.json");
    write_json(&invalid_path, &invalid);
    expect_error(
        register_mapping(&fixture.setup, &invalid_path),
        "MAPPING_INVALID",
    );
    let baseline =
        integrity_check(&fixture.setup.workspace).expect("replay retained INVALID mapping attempt");
    assert!(baseline.ok, "baseline issues: {:?}", baseline.issues);

    let connection = Connection::open(fixture.setup.workspace.join("state.sqlite3"))
        .expect("open invalid mapping attempt evidence");
    let attempt_id: String = connection
        .query_row(
            "SELECT id FROM mapping_validation_attempts
             WHERE state = 'INVALID' ORDER BY created_at DESC, id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("load retained INVALID attempt");
    with_immutability_guard_disabled(
        &connection,
        "mapping_validation_attempts_no_update",
        MAPPING_ATTEMPTS_UPDATE_GUARD_SQL,
        |connection| {
            connection
                .execute(
                    "UPDATE mapping_validation_attempts
                     SET errors_json = '[{\"code\":\"MAPPING_INVALID\",\"detail\":\"tampered\"}]'
                     WHERE id = ?1",
                    [&attempt_id],
                )
                .expect("tamper retained INVALID attempt diagnostics");
        },
    );
    drop(connection);
    let replay =
        integrity_check(&fixture.setup.workspace).expect("replay tampered INVALID attempt");
    assert!(!replay.ok);
    assert!(
        replay
            .issues
            .join("\n")
            .contains("immutable evidence is invalid")
    );
}

#[test]
fn required_rule_without_parameters_runs_but_parameterized_rule_still_fails_closed() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let mut profile: Value = serde_json::from_slice(
        &fs::read(project_path(PROFILE_FIXTURE)).expect("read profile fixture"),
    )
    .expect("parse profile fixture");
    profile["rules"][0]
        .as_object_mut()
        .expect("required rule object")
        .remove("parameters");
    let required_profile_path = temporary.path().join("required-without-parameters.json");
    write_json(&required_profile_path, &profile);

    let setup = ingest_and_authorize(
        &temporary,
        &project_path("fixtures/packages/valid.zip"),
        &required_profile_path,
    );
    let mapping_path = temporary
        .path()
        .join("required-without-parameters-mapping.json");
    write_json(
        &mapping_path,
        &mapping_document(
            &setup,
            "synthetic.customer.required-default",
            &["customer_accounts.csv"],
        ),
    );
    register_mapping(&setup, &mapping_path).expect("register exact governed mapping");
    let validation = run_validation(
        &setup.workspace,
        &setup.job_id,
        "synthetic.customer.required-default",
        MAPPING_VERSION,
        &project_path(PROFILE_SCHEMA),
        &validation_policy(),
    )
    .expect("run approved required rule with omitted optional parameters");
    assert_eq!(validation.result.rows_processed, 4);
    assert_eq!(validation.result.issue_count, 5);

    profile["rules"][1]
        .as_object_mut()
        .expect("parameterized rule object")
        .remove("parameters");
    let invalid_profile_path = temporary.path().join("max-length-without-parameters.json");
    write_json(&invalid_profile_path, &profile);
    let invalid_workspace = temporary.path().join("invalid-profile-workspace");
    let error = expect_error(
        register_profile(
            &invalid_workspace,
            &project_path(PROFILE_SCHEMA),
            &invalid_profile_path,
            &RegistryLimits::default(),
        ),
        "PROFILE_SEMANTIC_INVALID",
    );
    assert!(error.to_string().contains("parameter keys"));
}

#[test]
fn integrity_binds_validation_events_to_run_errors_and_timestamps() {
    let fixture = setup_standard_fixture();
    let complete = run_validation(
        &fixture.setup.workspace,
        &fixture.setup.job_id,
        STANDARD_MAPPING_ID,
        MAPPING_VERSION,
        &project_path(PROFILE_SCHEMA),
        &validation_policy(),
    )
    .expect("complete validation before lifecycle tamper proof");

    let mut bounded_policy = validation_policy();
    bounded_policy.maximum_issues = 1;
    expect_error(
        run_validation(
            &fixture.setup.workspace,
            &fixture.setup.job_id,
            STANDARD_MAPPING_ID,
            MAPPING_VERSION,
            &project_path(PROFILE_SCHEMA),
            &bounded_policy,
        ),
        "VALIDATION_BOUND_EXCEEDED",
    );
    let connection = Connection::open(fixture.setup.workspace.join("state.sqlite3"))
        .expect("open validation lifecycle evidence");
    let bounded_run_id: String = connection
        .query_row(
            "SELECT id FROM validation_runs WHERE state = 'BOUND_EXCEEDED'",
            [],
            |row| row.get(0),
        )
        .expect("read bounded run ID");
    drop(connection);
    let baseline = integrity_check(&fixture.setup.workspace).expect("baseline integrity replay");
    assert!(baseline.ok, "baseline issues: {:?}", baseline.issues);

    let connection = Connection::open(fixture.setup.workspace.join("state.sqlite3"))
        .expect("reopen validation lifecycle evidence");
    connection
        .execute(
            "UPDATE validation_runs
             SET created_at = '2026-01-01T00:00:00Z',
                 completed_at = '2026-01-01T00:00:01Z'
             WHERE id = ?1",
            [&complete.run_id],
        )
        .expect("tamper run timestamps without changing events");
    connection
        .execute(
            "UPDATE validation_events
             SET error_code = 'OTHER_BOUND', detail = 'different but nonempty detail'
             WHERE sequence = (
                 SELECT MAX(sequence) FROM validation_events WHERE run_id = ?1
             )",
            [&bounded_run_id],
        )
        .expect("tamper terminal event error evidence");
    drop(connection);

    let replay = integrity_check(&fixture.setup.workspace).expect("tampered integrity replay");
    assert!(!replay.ok);
    let diagnostics = replay.issues.join("\n");
    assert!(diagnostics.contains("RUNNING event is not bound to its created_at"));
    assert!(diagnostics.contains("terminal event is not bound to its completed_at"));
    assert!(diagnostics.contains("terminal event error evidence differs"));
}

#[test]
fn validation_policy_preflight_rejects_oversized_and_linked_files() {
    const MAXIMUM_TEST_POLICY_BYTES: u64 = 256 * 1024;

    let temporary = tempfile::tempdir().expect("temporary directory");
    let oversized = temporary.path().join("oversized-policy.json");
    File::create(&oversized)
        .expect("create oversized policy")
        .set_len(MAXIMUM_TEST_POLICY_BYTES + 1)
        .expect("size oversized policy");
    let oversized_error = expect_error(ValidationPolicy::from_path(&oversized), "POLICY_INVALID");
    assert!(oversized_error.to_string().contains("maximum is 262144"));

    let link = temporary.path().join("linked-policy.json");
    create_policy_symlink(&project_path(POLICY_FIXTURE), &link);
    let link_error = expect_error(ValidationPolicy::from_path(&link), "POLICY_INVALID");
    assert!(link_error.to_string().contains("regular non-link file"));

    let directory_error = expect_error(
        ValidationPolicy::from_path(temporary.path()),
        "POLICY_INVALID",
    );
    assert!(
        directory_error
            .to_string()
            .contains("regular non-link file")
    );
}

#[cfg(unix)]
fn create_policy_symlink(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).expect("create policy symlink");
}

#[cfg(windows)]
fn create_policy_symlink(target: &Path, link: &Path) {
    std::os::windows::fs::symlink_file(target, link).expect("create policy symlink");
}
