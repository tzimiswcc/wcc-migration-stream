//! Deterministic WP-005 effective-value derivation.
//!
//! Historical source and mapped projections remain immutable. This module
//! replays one exact original field and, when present, one valid immutable
//! correction effectivation. It never updates source, mapping, or validation
//! evidence.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::{
    KernelError, Result,
    canonical::{canonical_hash, canonical_json_string, parse_unique_json},
    decisions::DecisionScope,
    mapping::{MappingDocument, project_record},
};

#[derive(Clone, Debug)]
pub(crate) struct EffectiveField {
    pub(crate) received_source_value: Option<String>,
    pub(crate) original_value: Option<String>,
    pub(crate) original_lineage: Value,
    pub(crate) original_value_hash: String,
    pub(crate) value: Option<String>,
    pub(crate) effective_value_hash: String,
    pub(crate) active_overlay_id: Option<String>,
}

type EffectivationRow = (
    String,
    String,
    i64,
    String,
    String,
    String,
    Option<String>,
    String,
    String,
    String,
    String,
    String,
    String,
);

#[allow(clippy::too_many_lines)]
pub(crate) fn load_effective_field(
    connection: &Connection,
    scope: &DecisionScope,
    mapping: &MappingDocument,
) -> Result<EffectiveField> {
    let row: Option<(i64, i64, String)> = connection
        .query_row(
            "SELECT line_start, line_end, fields_json
             FROM source_records
             WHERE job_id = ?1 AND file_path = ?2 AND source_ordinal = ?3",
            params![
                scope.job_id,
                scope.accepted_source_file,
                sqlite_i64(scope.source_ordinal, "source ordinal")?
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let (line_start, line_end, fields_json) = row.ok_or_else(|| {
        KernelError::DecisionIntegrity(
            "decision scope does not name one accepted source row".to_owned(),
        )
    })?;
    if nonnegative_u64(line_start, "line start")? != scope.line_start
        || nonnegative_u64(line_end, "line end")? != scope.line_end
    {
        return Err(KernelError::DecisionIntegrity(
            "decision scope line identity differs from accepted source evidence".to_owned(),
        ));
    }
    let source_values: Vec<String> = serde_json::from_str(&fields_json).map_err(|error| {
        KernelError::DecisionIntegrity(format!(
            "accepted source row JSON cannot be replayed: {error}"
        ))
    })?;
    let projected = project_record(
        mapping,
        &scope.accepted_source_file,
        scope.source_ordinal,
        scope.line_start,
        scope.line_end,
        source_values,
    )?;
    if projected.entity_id != scope.canonical_entity_id {
        return Err(KernelError::DecisionIntegrity(
            "decision scope entity differs from the immutable mapping projection".to_owned(),
        ));
    }
    let original_value = projected
        .canonical_values
        .get(&scope.canonical_field_id)
        .cloned()
        .ok_or_else(|| {
            KernelError::DecisionIntegrity(
                "decision scope field is absent from the immutable mapping projection".to_owned(),
            )
        })?;
    let original_lineage = projected
        .lineage
        .get(&scope.canonical_field_id)
        .cloned()
        .ok_or_else(|| {
            KernelError::DecisionIntegrity(
                "decision scope field has no immutable mapping lineage".to_owned(),
            )
        })?;
    let received_source_value = original_lineage
        .get("sourceColumnOrdinal")
        .and_then(Value::as_u64)
        .and_then(|ordinal| usize::try_from(ordinal).ok())
        .and_then(|ordinal| projected.source_values.get(ordinal))
        .cloned();
    let original_value_hash = original_value_hash(scope, &original_value, &original_lineage)?;

    let initial_effective_hash =
        effective_value_hash(scope, &original_value_hash, None, None, 0, &original_value)?;
    let mut statement = connection.prepare(
        "SELECT ce.effectivation_id, ce.correction_overlay_id,
                    ce.decision_generation, ce.original_value_hash,
                    ce.prior_effective_value_hash, ce.effective_value_kind,
                    ce.effective_value_text, ce.effective_value_hash,
                    ce.effective_lineage_json, ce.effective_lineage_hash,
                    ce.effectivation_hash, co.overlay_hash,
                    co.prior_effective_value_hash
             FROM correction_effectivations ce
             JOIN issue_decisions d ON d.decision_id = ce.decision_id
             JOIN decision_attempts a ON a.attempt_id = d.decision_attempt_id
             JOIN correction_overlays co
               ON co.overlay_id = ce.correction_overlay_id
             WHERE a.job_id = ?1 AND a.validation_run_id = ?2
               AND a.accepted_source_file = ?3 AND a.source_ordinal = ?4
               AND a.canonical_entity_id = ?5 AND a.canonical_field_id = ?6
               AND a.profile_id = ?7 AND a.profile_version = ?8
               AND a.profile_hash = ?9
               AND a.mapping_id = ?10 AND a.mapping_version = ?11
               AND a.mapping_hash = ?12
             ORDER BY ce.effectivation_id",
    )?;
    let rows = statement.query_map(
        params![
            scope.job_id,
            scope.validation_run_id,
            scope.accepted_source_file,
            sqlite_i64(scope.source_ordinal, "source ordinal")?,
            scope.canonical_entity_id,
            scope.canonical_field_id,
            scope.profile_id,
            scope.profile_version,
            scope.profile_hash,
            scope.mapping_id,
            scope.mapping_version,
            scope.mapping_hash,
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
            ))
        },
    )?;
    let mut by_prior = HashMap::<String, EffectivationRow>::new();
    for row in rows {
        let row: EffectivationRow = row?;
        if by_prior.insert(row.4.clone(), row).is_some() {
            return Err(KernelError::DecisionIntegrity(
                "effective-value history forks from one prior hash".to_owned(),
            ));
        }
    }
    if by_prior.is_empty() {
        return Ok(EffectiveField {
            received_source_value,
            original_value: original_value.clone(),
            original_lineage,
            original_value_hash,
            value: original_value,
            effective_value_hash: initial_effective_hash,
            active_overlay_id: None,
        });
    }

    let mut current_hash = initial_effective_hash;
    let mut active = None;
    let mut visited = HashSet::from([current_hash.clone()]);
    while let Some(effectivation) = by_prior.remove(&current_hash) {
        let (
            effectivation_id,
            overlay_id,
            generation,
            stored_original_hash,
            stored_prior_effective_hash,
            value_kind,
            value,
            stored_effective_hash,
            lineage_json,
            stored_lineage_hash,
            stored_effectivation_hash,
            overlay_hash,
            overlay_prior_effective_hash,
        ) = effectivation;
        if generation <= 0
            || stored_original_hash != original_value_hash
            || stored_prior_effective_hash != overlay_prior_effective_hash
            || stored_prior_effective_hash != current_hash
        {
            return Err(KernelError::DecisionIntegrity(
                "correction effectivation has an invalid generation or value binding".to_owned(),
            ));
        }
        let generation = u64::try_from(generation).map_err(|_| {
            KernelError::DecisionIntegrity("effectivation generation does not fit u64".to_owned())
        })?;
        validate_value_kind(&value_kind, value.as_deref())?;
        let lineage = parse_unique_json(lineage_json.as_bytes()).map_err(|error| {
            KernelError::DecisionIntegrity(format!(
                "effective lineage JSON cannot be replayed: {error}"
            ))
        })?;
        let expected_lineage = effective_lineage(
            &original_lineage,
            &overlay_id,
            &overlay_hash,
            &stored_prior_effective_hash,
            generation,
        );
        if canonical_json_string(&lineage)? != lineage_json
            || lineage != expected_lineage
            || canonical_hash(&lineage)? != stored_lineage_hash
        {
            return Err(KernelError::DecisionIntegrity(
                "effective lineage bytes or hash differ from canonical evidence".to_owned(),
            ));
        }
        let expected_effective_hash = effective_value_hash(
            scope,
            &original_value_hash,
            Some(&stored_prior_effective_hash),
            Some(&overlay_hash),
            generation,
            &value,
        )?;
        if expected_effective_hash != stored_effective_hash
            || !visited.insert(stored_effective_hash.clone())
        {
            return Err(KernelError::DecisionIntegrity(
                "effective value hash differs or its history cycles".to_owned(),
            ));
        }
        let material = effectivation_material(
            &effectivation_id,
            &overlay_id,
            generation,
            &original_value_hash,
            &stored_prior_effective_hash,
            &value_kind,
            &value,
            &stored_effective_hash,
            &stored_lineage_hash,
        );
        if canonical_hash(&material)? != stored_effectivation_hash {
            return Err(KernelError::DecisionIntegrity(
                "correction effectivation hash differs from canonical evidence".to_owned(),
            ));
        }
        current_hash.clone_from(&stored_effective_hash);
        active = Some((overlay_id, value, stored_effective_hash));
    }
    if !by_prior.is_empty() {
        return Err(KernelError::DecisionIntegrity(
            "effective-value history is disconnected or cyclic".to_owned(),
        ));
    }
    let (overlay_id, value, effective_value_hash) = active.ok_or_else(|| {
        KernelError::DecisionIntegrity("effective-value history has no reachable tip".to_owned())
    })?;
    Ok(EffectiveField {
        received_source_value,
        original_value,
        original_lineage,
        original_value_hash,
        value,
        effective_value_hash,
        active_overlay_id: Some(overlay_id),
    })
}

#[allow(clippy::ref_option)]
pub(crate) fn original_value_hash(
    scope: &DecisionScope,
    value: &Option<String>,
    lineage: &Value,
) -> Result<String> {
    canonical_hash(&json!({
        "contract": "migration-stream-original-canonical-value/v1",
        "jobId": scope.job_id,
        "validationRunId": scope.validation_run_id,
        "sourceFile": scope.accepted_source_file,
        "sourceOrdinal": scope.source_ordinal,
        "entityId": scope.canonical_entity_id,
        "fieldId": scope.canonical_field_id,
        "value": value,
        "lineage": lineage,
    }))
}

#[allow(clippy::ref_option)]
pub(crate) fn effective_value_hash(
    scope: &DecisionScope,
    original_value_hash: &str,
    prior_effective_value_hash: Option<&str>,
    overlay_hash: Option<&str>,
    generation: u64,
    value: &Option<String>,
) -> Result<String> {
    canonical_hash(&json!({
        "contract": "migration-stream-effective-canonical-value/v1",
        "jobId": scope.job_id,
        "validationRunId": scope.validation_run_id,
        "sourceFile": scope.accepted_source_file,
        "sourceOrdinal": scope.source_ordinal,
        "entityId": scope.canonical_entity_id,
        "fieldId": scope.canonical_field_id,
        "originalValueHash": original_value_hash,
        "priorEffectiveValueHash": prior_effective_value_hash,
        "overlayHash": overlay_hash,
        "effectivationGeneration": generation,
        "value": value,
    }))
}

pub(crate) fn effective_lineage(
    original_lineage: &Value,
    overlay_id: &str,
    overlay_hash: &str,
    prior_effective_value_hash: &str,
    generation: u64,
) -> Value {
    json!({
        "contract": "migration-stream-effective-lineage/v1",
        "originalLineage": original_lineage,
        "correctionOverlayId": overlay_id,
        "correctionOverlayHash": overlay_hash,
        "priorEffectiveValueHash": prior_effective_value_hash,
        "effectivationGeneration": generation,
    })
}

#[allow(clippy::ref_option)]
pub(crate) fn value_kind(value: &Option<String>) -> &'static str {
    match value {
        None => "NULL",
        Some(value) if value.is_empty() => "EMPTY",
        Some(_) => "TEXT",
    }
}

#[allow(clippy::too_many_arguments, clippy::ref_option)]
pub(crate) fn effectivation_material(
    effectivation_id: &str,
    overlay_id: &str,
    generation: u64,
    original_value_hash: &str,
    prior_effective_value_hash: &str,
    effective_value_kind: &str,
    effective_value: &Option<String>,
    effective_value_hash: &str,
    effective_lineage_hash: &str,
) -> Value {
    json!({
        "contract": "migration-stream-correction-effectivation/v1",
        "effectivationId": effectivation_id,
        "correctionOverlayId": overlay_id,
        "decisionGeneration": generation,
        "originalValueHash": original_value_hash,
        "priorEffectiveValueHash": prior_effective_value_hash,
        "effectiveValueKind": effective_value_kind,
        "effectiveValue": effective_value,
        "effectiveValueHash": effective_value_hash,
        "effectiveLineageHash": effective_lineage_hash,
    })
}

fn validate_value_kind(kind: &str, value: Option<&str>) -> Result<()> {
    let valid = matches!((kind, value), ("NULL", None) | ("EMPTY", Some("")))
        || (kind == "TEXT" && value.is_some_and(|value| !value.is_empty()));
    if valid {
        Ok(())
    } else {
        Err(KernelError::DecisionIntegrity(
            "effective value kind and payload are inconsistent".to_owned(),
        ))
    }
}

fn sqlite_i64(value: u64, label: &str) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| KernelError::DecisionBound(format!("{label} does not fit SQLite INTEGER")))
}

fn nonnegative_u64(value: i64, label: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| KernelError::DecisionIntegrity(format!("{label} is negative")))
}
