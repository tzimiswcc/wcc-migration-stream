//! One server-derived product-spine read model.
//!
//! The journey is navigation evidence, not authority. Every link is emitted
//! only after the corresponding persisted engagement or startup-fixed
//! workbench capability has been verified by its owning service.

use serde::{Deserialize, Serialize};

use crate::object_workspace::{ObjectWorkspaceState, ObjectWorkspaceStatus};

pub const JOURNEY_SCHEMA: &str = "migration-stream/journey/v2";
pub const MULTI_ENTITY_DOWNSTREAM_REASON: &str =
    "Multi-entity downstream authority is not implemented yet.";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum JourneyStageId {
    Setup,
    Destination,
    Source,
    Mapping,
    Validate,
    Compare,
    Issues,
    Decisions,
    Reconciliation,
    Package,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum JourneyStageStatus {
    Completed,
    Current,
    Available,
    Locked,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JourneyBinding {
    pub engagement_id: Option<String>,
    pub engagement_generation: Option<u64>,
    pub job_id: Option<String>,
    pub source_hash: Option<String>,
    pub manifest_hash: Option<String>,
    pub profiling_run_id: Option<String>,
    pub object_pack_id: Option<String>,
    pub destination_origin: Option<String>,
    pub destination_authority_id: Option<String>,
    pub destination_origin_hash: Option<String>,
    pub profile_id: Option<String>,
    pub profile_version: Option<String>,
    pub profile_hash: Option<String>,
    pub mapping_id: Option<String>,
    pub mapping_version: Option<String>,
    pub mapping_hash: Option<String>,
    pub validation_run_id: Option<String>,
    pub validation_result_hash: Option<String>,
    pub active_tour_id: Option<String>,
    pub active_tour_number: Option<u64>,
    pub active_tour_hash: Option<String>,
    pub comparison_id: Option<String>,
    pub comparison_hash: Option<String>,
    pub entity_count: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JourneyStage {
    pub id: JourneyStageId,
    pub label: String,
    pub status: JourneyStageStatus,
    pub available: bool,
    pub href: Option<String>,
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JourneyView {
    pub schema_version: String,
    pub current_stage_id: JourneyStageId,
    pub workspace: ObjectWorkspaceStatus,
    pub binding: JourneyBinding,
    pub stages: Vec<JourneyStage>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JourneySurface {
    Engagement,
    Workbench,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DownstreamJourneyAuthority {
    Enabled,
    MultiEntityLocked,
    Disabled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct JourneyDerivation {
    pub binding: JourneyBinding,
    pub surface: JourneySurface,
    pub engagement_exists: bool,
    pub source_complete: bool,
    pub destination_complete: bool,
    pub mapping_complete: bool,
    pub operator_governed_mapping: bool,
    pub validation_complete: bool,
    pub issues_available: bool,
    pub comparison_available: bool,
    pub comparison_pending: bool,
    pub comparison_complete: bool,
    pub downstream: DownstreamJourneyAuthority,
    pub decisions_available: bool,
    pub reconciliation_available: bool,
    pub package_available: bool,
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn derive_journey(input: JourneyDerivation, workspace: ObjectWorkspaceStatus) -> JourneyView {
    if workspace.state != ObjectWorkspaceState::Active {
        return workspace_required_journey(workspace);
    }
    let workbench_hrefs = input.surface == JourneySurface::Workbench;
    let workspace_destination =
        input.binding.destination_origin.as_deref() == Some("WORKSPACE_PROFILE");
    let engagement_mapping_href = "/mapping";
    let established = input.engagement_exists || workbench_hrefs;
    let workbench_source_current =
        workbench_hrefs && (!input.source_complete || !input.destination_complete);
    let setup = if established {
        completed(
            if workbench_hrefs { "/" } else { "/setup" },
            "Setup / Workspace",
        )
    } else {
        current("/setup", "Setup / Workspace")
    };
    let source = if workbench_source_current {
        current("/files", "Source")
    } else if input.source_complete {
        completed(
            if workbench_hrefs {
                "/files"
            } else {
                "/setup#profiled-source"
            },
            "Source",
        )
    } else if !established {
        locked(
            JourneyStageId::Source,
            "Source",
            "Start a local engagement before accepting source evidence.",
        )
    } else if workbench_hrefs {
        locked(
            JourneyStageId::Source,
            "Source",
            "This read-only workbench has no completed startup-selected source.",
        )
    } else if input.destination_complete {
        current("/setup", "Source")
    } else {
        available("/setup", "Source")
    };
    let destination = if input.destination_complete {
        completed(
            if workbench_hrefs && !workspace_destination {
                "/profile"
            } else {
                "/destination"
            },
            "Destination",
        )
    } else if !established {
        locked(
            JourneyStageId::Destination,
            "Destination",
            "Start a local engagement before defining or choosing a destination.",
        )
    } else if workbench_hrefs {
        locked(
            JourneyStageId::Destination,
            "Destination",
            "This read-only workbench has no selected destination authority.",
        )
    } else {
        current("/destination", "Destination")
    };
    let mapping = if input.mapping_complete {
        completed(
            if workbench_hrefs {
                "/mapping"
            } else {
                engagement_mapping_href
            },
            "Mapping",
        )
    } else if input.destination_complete && input.source_complete {
        current(engagement_mapping_href, "Mapping")
    } else {
        locked(
            JourneyStageId::Mapping,
            "Mapping",
            if !input.destination_complete && !input.source_complete {
                "Complete an approved destination and an accepted, sample-free source profile before mapping."
            } else if !input.destination_complete {
                "Approve the exact destination profile before preparing a mapping."
            } else {
                "Accept and sample-free profile the source before preparing a mapping."
            },
        )
    };
    let zero_issue_downstream = input.validation_complete
        && !input.issues_available
        && input.downstream == DownstreamJourneyAuthority::Enabled;
    let validate = if input.validation_complete && (input.issues_available || zero_issue_downstream)
    {
        completed("/validation", "Validate")
    } else if input.validation_complete || input.mapping_complete {
        current("/validation", "Validate")
    } else {
        locked(
            JourneyStageId::Validate,
            "Validate",
            "Approve a complete mapping before launching validation.",
        )
    };
    let compare = if input.comparison_pending {
        current("/comparison", "Compare")
    } else if input.comparison_complete {
        completed("/comparison", "Compare")
    } else if input.comparison_available {
        available("/comparison", "Compare")
    } else {
        locked(
            JourneyStageId::Compare,
            "Compare",
            if input.validation_complete {
                "Compared validation is not enabled for this profile and mapping authority."
            } else {
                "Complete validation before comparing a revised source."
            },
        )
    };
    let issues = if input.issues_available {
        match input.downstream {
            DownstreamJourneyAuthority::Enabled => completed("/issues", "Issues"),
            DownstreamJourneyAuthority::MultiEntityLocked
            | DownstreamJourneyAuthority::Disabled
                if input.comparison_pending =>
            {
                available("/issues", "Issues")
            }
            DownstreamJourneyAuthority::MultiEntityLocked
            | DownstreamJourneyAuthority::Disabled => current("/issues", "Issues"),
        }
    } else {
        locked(
            JourneyStageId::Issues,
            "Issues",
            if input.validation_complete {
                "The active validation completed without First Issues evidence."
            } else {
                "Complete validation before reviewing First Issues."
            },
        )
    };
    let multi_reason = (input.downstream == DownstreamJourneyAuthority::MultiEntityLocked)
        .then_some(MULTI_ENTITY_DOWNSTREAM_REASON);
    let decisions = downstream_stage(
        JourneyStageId::Decisions,
        "Decisions",
        "/decisions",
        input.decisions_available,
        multi_reason.unwrap_or(if zero_issue_downstream {
            "No First Issues are available for governed decisions."
        } else {
            "Governed issue-decision authority is not enabled."
        }),
        !input.comparison_pending,
    );
    let reconciliation = downstream_stage(
        JourneyStageId::Reconciliation,
        "Reconciliation",
        "/reconciliation",
        input.reconciliation_available,
        multi_reason.unwrap_or("Governed reconciliation and result authority is not enabled."),
        zero_issue_downstream && !input.comparison_pending,
    );
    let package = downstream_stage(
        JourneyStageId::Package,
        "Package",
        "/publication",
        input.package_available,
        multi_reason.unwrap_or("Readiness-package authority is not enabled."),
        false,
    );

    let current_stage_id = if workbench_source_current {
        JourneyStageId::Source
    } else if input.comparison_pending {
        JourneyStageId::Compare
    } else if input.decisions_available {
        JourneyStageId::Decisions
    } else if zero_issue_downstream && input.reconciliation_available {
        JourneyStageId::Reconciliation
    } else if input.issues_available {
        if input.comparison_pending {
            JourneyStageId::Compare
        } else {
            JourneyStageId::Issues
        }
    } else if input.validation_complete || input.mapping_complete {
        JourneyStageId::Validate
    } else if input.destination_complete && input.source_complete {
        JourneyStageId::Mapping
    } else if input.destination_complete {
        JourneyStageId::Source
    } else if input.source_complete || established {
        JourneyStageId::Destination
    } else {
        JourneyStageId::Setup
    };

    JourneyView {
        schema_version: JOURNEY_SCHEMA.to_owned(),
        current_stage_id,
        workspace,
        binding: input.binding,
        stages: vec![
            setup,
            destination,
            source,
            mapping,
            validate,
            compare,
            issues,
            decisions,
            reconciliation,
            package,
        ],
    }
}

fn workspace_required_journey(workspace: ObjectWorkspaceStatus) -> JourneyView {
    let reason = workspace
        .unavailable_reason
        .as_deref()
        .unwrap_or("Verify this object workspace before continuing.");
    let mut stages = vec![current("/setup", "Setup / Workspace")];
    for (id, label) in [
        (JourneyStageId::Destination, "Destination"),
        (JourneyStageId::Source, "Source"),
        (JourneyStageId::Mapping, "Mapping"),
        (JourneyStageId::Validate, "Validate"),
        (JourneyStageId::Compare, "Compare"),
        (JourneyStageId::Issues, "Issues"),
        (JourneyStageId::Decisions, "Decisions"),
        (JourneyStageId::Reconciliation, "Reconciliation"),
        (JourneyStageId::Package, "Package"),
    ] {
        stages.push(locked(id, label, reason));
    }
    JourneyView {
        schema_version: JOURNEY_SCHEMA.to_owned(),
        current_stage_id: JourneyStageId::Setup,
        workspace,
        binding: JourneyBinding::default(),
        stages,
    }
}

fn completed(href: &str, label: &str) -> JourneyStage {
    stage(
        stage_id(label),
        label,
        JourneyStageStatus::Completed,
        Some(href),
        None,
    )
}

fn current(href: &str, label: &str) -> JourneyStage {
    stage(
        stage_id(label),
        label,
        JourneyStageStatus::Current,
        Some(href),
        None,
    )
}

fn available(href: &str, label: &str) -> JourneyStage {
    stage(
        stage_id(label),
        label,
        JourneyStageStatus::Available,
        Some(href),
        None,
    )
}

fn locked(id: JourneyStageId, label: &str, reason: &str) -> JourneyStage {
    stage(id, label, JourneyStageStatus::Locked, None, Some(reason))
}

fn downstream_stage(
    id: JourneyStageId,
    label: &str,
    href: &str,
    enabled: bool,
    unavailable_reason: &str,
    current_when_enabled: bool,
) -> JourneyStage {
    match (enabled, current_when_enabled) {
        (false, _) => locked(id, label, unavailable_reason),
        (true, true) => current(href, label),
        (true, false) => available(href, label),
    }
}

fn stage(
    id: JourneyStageId,
    label: &str,
    status: JourneyStageStatus,
    href: Option<&str>,
    unavailable_reason: Option<&str>,
) -> JourneyStage {
    JourneyStage {
        id,
        label: label.to_owned(),
        status,
        available: href.is_some(),
        href: href.map(str::to_owned),
        unavailable_reason: unavailable_reason.map(str::to_owned),
    }
}

fn stage_id(label: &str) -> JourneyStageId {
    match label {
        "Setup / Workspace" => JourneyStageId::Setup,
        "Destination" => JourneyStageId::Destination,
        "Source" => JourneyStageId::Source,
        "Mapping" => JourneyStageId::Mapping,
        "Validate" => JourneyStageId::Validate,
        "Compare" => JourneyStageId::Compare,
        "Issues" => JourneyStageId::Issues,
        "Decisions" => JourneyStageId::Decisions,
        "Reconciliation" => JourneyStageId::Reconciliation,
        "Package" => JourneyStageId::Package,
        _ => unreachable!("journey labels are fixed at compile time"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object_workspace::{
        OBJECT_WORKSPACE_DESCRIPTOR_SCHEMA, OBJECT_WORKSPACE_STATUS_SCHEMA, ObjectWorkspaceRecord,
    };

    fn active_workspace() -> ObjectWorkspaceStatus {
        ObjectWorkspaceStatus {
            schema_version: OBJECT_WORKSPACE_STATUS_SCHEMA.to_owned(),
            state: ObjectWorkspaceState::Active,
            state_hash: "d".repeat(64),
            required_action: None,
            unavailable_reason: None,
            pending_command: None,
            workspace: Some(ObjectWorkspaceRecord {
                schema_version: OBJECT_WORKSPACE_DESCRIPTOR_SCHEMA.to_owned(),
                workspace_id: "00000000-0000-4000-8000-000000000001".to_owned(),
                generation: 1,
                display_name: "Test workspace".to_owned(),
                migration_object_name_or_scope: "Test object".to_owned(),
                creator_attribution: "Test operator".to_owned(),
                established_by: "INITIALIZED".to_owned(),
                created_at: "2026-07-29T00:00:00.000Z".to_owned(),
                descriptor_hash: "e".repeat(64),
            }),
        }
    }

    fn active_journey(input: JourneyDerivation) -> JourneyView {
        derive_journey(input, active_workspace())
    }

    fn binding() -> JourneyBinding {
        JourneyBinding {
            engagement_id: Some("engagement-1".to_owned()),
            engagement_generation: Some(7),
            job_id: Some("job-1".to_owned()),
            profile_id: Some("profile-1".to_owned()),
            profile_version: Some("1.0.0".to_owned()),
            profile_hash: Some("a".repeat(64)),
            mapping_id: Some("mapping-1".to_owned()),
            mapping_version: Some("1.0.0".to_owned()),
            mapping_hash: Some("b".repeat(64)),
            validation_run_id: Some("validation-1".to_owned()),
            validation_result_hash: Some("c".repeat(64)),
            entity_count: Some(2),
            ..JourneyBinding::default()
        }
    }

    fn assert_contract(view: &JourneyView) {
        assert_eq!(
            view.stages
                .iter()
                .filter(|stage| stage.status == JourneyStageStatus::Current)
                .count(),
            1
        );
        for stage in &view.stages {
            assert_eq!(stage.available, stage.href.is_some());
            if stage.status == JourneyStageStatus::Locked {
                assert!(stage.unavailable_reason.is_some());
            } else {
                assert!(stage.unavailable_reason.is_none());
            }
        }
    }

    #[test]
    fn empty_engagement_fails_closed_after_setup() {
        let view = active_journey(JourneyDerivation {
            binding: JourneyBinding::default(),
            surface: JourneySurface::Engagement,
            engagement_exists: false,
            source_complete: false,
            destination_complete: false,
            mapping_complete: false,
            operator_governed_mapping: false,
            validation_complete: false,
            issues_available: false,
            comparison_available: false,
            comparison_pending: false,
            comparison_complete: false,
            downstream: DownstreamJourneyAuthority::Disabled,
            decisions_available: false,
            reconciliation_available: false,
            package_available: false,
        });
        assert_contract(&view);
        assert_eq!(view.current_stage_id, JourneyStageId::Setup);
        assert_eq!(view.stages.len(), 10);
        assert_eq!(view.stages[0].status, JourneyStageStatus::Current);
        assert!(view.stages[1..].iter().all(|stage| {
            stage.status == JourneyStageStatus::Locked
                && !stage.available
                && stage.href.is_none()
                && stage.unavailable_reason.is_some()
        }));
    }

    #[test]
    fn profiled_source_opens_destination_without_inventing_mapping_authority() {
        let view = active_journey(JourneyDerivation {
            binding: binding(),
            surface: JourneySurface::Engagement,
            engagement_exists: true,
            source_complete: true,
            destination_complete: false,
            mapping_complete: false,
            operator_governed_mapping: false,
            validation_complete: false,
            issues_available: false,
            comparison_available: false,
            comparison_pending: false,
            comparison_complete: false,
            downstream: DownstreamJourneyAuthority::Disabled,
            decisions_available: false,
            reconciliation_available: false,
            package_available: false,
        });
        assert_contract(&view);
        assert_eq!(view.current_stage_id, JourneyStageId::Destination);
        assert_eq!(view.stages[1].status, JourneyStageStatus::Current);
        assert_eq!(view.stages[2].status, JourneyStageStatus::Completed);
        assert_eq!(view.stages[3].status, JourneyStageStatus::Locked);
    }

    #[test]
    fn engagement_mapping_links_use_the_dedicated_workshop() {
        let controlled = active_journey(JourneyDerivation {
            binding: binding(),
            surface: JourneySurface::Engagement,
            engagement_exists: true,
            source_complete: true,
            destination_complete: true,
            mapping_complete: false,
            operator_governed_mapping: false,
            validation_complete: false,
            issues_available: false,
            comparison_available: false,
            comparison_pending: false,
            comparison_complete: false,
            downstream: DownstreamJourneyAuthority::Disabled,
            decisions_available: false,
            reconciliation_available: false,
            package_available: false,
        });
        assert_eq!(controlled.current_stage_id, JourneyStageId::Mapping);
        assert_eq!(controlled.stages[3].href.as_deref(), Some("/mapping"));

        let operator = active_journey(JourneyDerivation {
            binding: binding(),
            surface: JourneySurface::Engagement,
            engagement_exists: true,
            source_complete: true,
            destination_complete: true,
            mapping_complete: false,
            operator_governed_mapping: true,
            validation_complete: false,
            issues_available: false,
            comparison_available: false,
            comparison_pending: false,
            comparison_complete: false,
            downstream: DownstreamJourneyAuthority::Disabled,
            decisions_available: false,
            reconciliation_available: false,
            package_available: false,
        });
        assert_eq!(operator.stages[3].href.as_deref(), Some("/mapping"));
    }

    #[test]
    fn direct_workbench_never_links_to_engagement_only_controls() {
        let interrupted_source = active_journey(JourneyDerivation {
            binding: JourneyBinding {
                job_id: Some("interrupted-job".to_owned()),
                ..JourneyBinding::default()
            },
            surface: JourneySurface::Workbench,
            engagement_exists: false,
            source_complete: false,
            destination_complete: false,
            mapping_complete: false,
            operator_governed_mapping: false,
            validation_complete: false,
            issues_available: false,
            comparison_available: false,
            comparison_pending: false,
            comparison_complete: false,
            downstream: DownstreamJourneyAuthority::Disabled,
            decisions_available: false,
            reconciliation_available: true,
            package_available: false,
        });
        assert_contract(&interrupted_source);
        assert_eq!(interrupted_source.current_stage_id, JourneyStageId::Source);
        assert_eq!(
            interrupted_source.stages[2].status,
            JourneyStageStatus::Current
        );
        assert_eq!(interrupted_source.stages[2].href.as_deref(), Some("/files"));
        assert_eq!(
            interrupted_source.stages[1].status,
            JourneyStageStatus::Locked
        );

        let without_governance = active_journey(JourneyDerivation {
            binding: binding(),
            surface: JourneySurface::Workbench,
            engagement_exists: false,
            source_complete: true,
            destination_complete: false,
            mapping_complete: false,
            operator_governed_mapping: false,
            validation_complete: false,
            issues_available: false,
            comparison_available: false,
            comparison_pending: false,
            comparison_complete: false,
            downstream: DownstreamJourneyAuthority::Disabled,
            decisions_available: false,
            reconciliation_available: false,
            package_available: false,
        });
        assert_contract(&without_governance);
        assert_eq!(without_governance.current_stage_id, JourneyStageId::Source);
        assert_eq!(without_governance.stages[2].href.as_deref(), Some("/files"));
        assert_eq!(
            without_governance.stages[1].status,
            JourneyStageStatus::Locked
        );
        assert!(without_governance.stages[1].href.is_none());

        let pending_validation = active_journey(JourneyDerivation {
            binding: binding(),
            surface: JourneySurface::Workbench,
            engagement_exists: false,
            source_complete: true,
            destination_complete: true,
            mapping_complete: true,
            operator_governed_mapping: false,
            validation_complete: false,
            issues_available: false,
            comparison_available: false,
            comparison_pending: false,
            comparison_complete: false,
            downstream: DownstreamJourneyAuthority::Disabled,
            decisions_available: false,
            reconciliation_available: false,
            package_available: false,
        });
        assert_eq!(
            pending_validation.current_stage_id,
            JourneyStageId::Validate
        );
        assert_eq!(
            pending_validation.stages[4].href.as_deref(),
            Some("/validation")
        );
    }

    #[test]
    fn single_entity_handoff_emits_only_workbench_safe_downstream_links() {
        let view = active_journey(JourneyDerivation {
            binding: binding(),
            surface: JourneySurface::Workbench,
            engagement_exists: true,
            source_complete: true,
            destination_complete: true,
            mapping_complete: true,
            operator_governed_mapping: false,
            validation_complete: true,
            issues_available: true,
            comparison_available: true,
            comparison_pending: false,
            comparison_complete: false,
            downstream: DownstreamJourneyAuthority::Enabled,
            decisions_available: true,
            reconciliation_available: true,
            package_available: true,
        });
        assert_contract(&view);
        assert_eq!(view.current_stage_id, JourneyStageId::Decisions);
        assert_eq!(view.stages[0].href.as_deref(), Some("/"));
        assert_eq!(view.stages[1].href.as_deref(), Some("/profile"));
        assert_eq!(view.stages[2].href.as_deref(), Some("/files"));
        assert_eq!(view.stages[5].status, JourneyStageStatus::Available);
        assert_eq!(view.stages[5].href.as_deref(), Some("/comparison"));
        assert_eq!(view.stages[6].status, JourneyStageStatus::Completed);
        assert_eq!(view.stages[7].href.as_deref(), Some("/decisions"));
        assert_eq!(view.stages[8].href.as_deref(), Some("/reconciliation"));
        assert_eq!(view.stages[9].href.as_deref(), Some("/publication"));
    }

    #[test]
    fn pending_single_entity_revision_makes_compare_the_only_current_stage() {
        let view = active_journey(JourneyDerivation {
            binding: binding(),
            surface: JourneySurface::Workbench,
            engagement_exists: true,
            source_complete: true,
            destination_complete: true,
            mapping_complete: true,
            operator_governed_mapping: false,
            validation_complete: true,
            issues_available: true,
            comparison_available: true,
            comparison_pending: true,
            comparison_complete: true,
            downstream: DownstreamJourneyAuthority::Enabled,
            decisions_available: true,
            reconciliation_available: true,
            package_available: true,
        });
        assert_contract(&view);
        assert_eq!(view.current_stage_id, JourneyStageId::Compare);
        assert_eq!(view.stages[5].status, JourneyStageStatus::Current);
        assert_eq!(view.stages[7].status, JourneyStageStatus::Available);
        assert_eq!(view.stages[8].status, JourneyStageStatus::Available);
        assert_eq!(view.stages[9].status, JourneyStageStatus::Available);
    }

    #[test]
    fn workspace_profile_workbench_keeps_destination_workshop_link() {
        let mut workspace_binding = binding();
        workspace_binding.object_pack_id = Some("workspace.profile".to_owned());
        workspace_binding.destination_origin = Some("WORKSPACE_PROFILE".to_owned());
        workspace_binding.destination_authority_id = Some("origin-1".to_owned());
        workspace_binding.destination_origin_hash = Some("d".repeat(64));
        let view = active_journey(JourneyDerivation {
            binding: workspace_binding,
            surface: JourneySurface::Workbench,
            engagement_exists: true,
            source_complete: true,
            destination_complete: true,
            mapping_complete: true,
            operator_governed_mapping: false,
            validation_complete: true,
            issues_available: true,
            comparison_available: false,
            comparison_pending: false,
            comparison_complete: false,
            downstream: DownstreamJourneyAuthority::Enabled,
            decisions_available: true,
            reconciliation_available: true,
            package_available: true,
        });
        assert_contract(&view);
        assert_eq!(view.stages[1].href.as_deref(), Some("/destination"));
    }

    #[test]
    fn multi_entity_tour_one_stops_at_issues_with_exact_reason() {
        let view = active_journey(JourneyDerivation {
            binding: binding(),
            surface: JourneySurface::Engagement,
            engagement_exists: true,
            source_complete: true,
            destination_complete: true,
            mapping_complete: true,
            operator_governed_mapping: true,
            validation_complete: true,
            issues_available: true,
            comparison_available: true,
            comparison_pending: false,
            comparison_complete: false,
            downstream: DownstreamJourneyAuthority::MultiEntityLocked,
            decisions_available: false,
            reconciliation_available: false,
            package_available: false,
        });
        assert_contract(&view);
        assert_eq!(view.current_stage_id, JourneyStageId::Issues);
        assert_eq!(view.stages[5].status, JourneyStageStatus::Available);
        for stage in &view.stages[7..] {
            assert_eq!(stage.status, JourneyStageStatus::Locked);
            assert_eq!(
                stage.unavailable_reason.as_deref(),
                Some(MULTI_ENTITY_DOWNSTREAM_REASON)
            );
            assert!(stage.href.is_none());
        }
    }

    #[test]
    fn completed_tour_two_preserves_comparison_and_multi_entity_boundary() {
        let mut exact_binding = binding();
        exact_binding.active_tour_id = Some("tour-2".to_owned());
        exact_binding.active_tour_number = Some(2);
        exact_binding.active_tour_hash = Some("d".repeat(64));
        exact_binding.comparison_id = Some("comparison-2".to_owned());
        exact_binding.comparison_hash = Some("e".repeat(64));
        let view = active_journey(JourneyDerivation {
            binding: exact_binding.clone(),
            surface: JourneySurface::Engagement,
            engagement_exists: true,
            source_complete: true,
            destination_complete: true,
            mapping_complete: true,
            operator_governed_mapping: true,
            validation_complete: true,
            issues_available: true,
            comparison_available: true,
            comparison_pending: false,
            comparison_complete: true,
            downstream: DownstreamJourneyAuthority::MultiEntityLocked,
            decisions_available: false,
            reconciliation_available: false,
            package_available: false,
        });
        assert_contract(&view);
        assert_eq!(view.binding, exact_binding);
        assert_eq!(view.stages[5].status, JourneyStageStatus::Completed);
        assert_eq!(view.stages[6].status, JourneyStageStatus::Current);
        assert!(view.stages[7..].iter().all(|stage| !stage.available));
    }

    #[test]
    fn zero_issue_multi_entity_validation_stays_on_validation() {
        let view = active_journey(JourneyDerivation {
            binding: binding(),
            surface: JourneySurface::Engagement,
            engagement_exists: true,
            source_complete: true,
            destination_complete: true,
            mapping_complete: true,
            operator_governed_mapping: true,
            validation_complete: true,
            issues_available: false,
            comparison_available: false,
            comparison_pending: false,
            comparison_complete: false,
            downstream: DownstreamJourneyAuthority::MultiEntityLocked,
            decisions_available: false,
            reconciliation_available: false,
            package_available: false,
        });
        assert_contract(&view);
        assert_eq!(view.current_stage_id, JourneyStageId::Validate);
        assert_eq!(view.stages[4].status, JourneyStageStatus::Current);
        assert_eq!(view.stages[4].href.as_deref(), Some("/validation"));
        assert_eq!(view.stages[6].status, JourneyStageStatus::Locked);
    }

    #[test]
    fn zero_issue_single_entity_continues_to_readiness_without_decisions() {
        let view = active_journey(JourneyDerivation {
            binding: binding(),
            surface: JourneySurface::Workbench,
            engagement_exists: true,
            source_complete: true,
            destination_complete: true,
            mapping_complete: true,
            operator_governed_mapping: false,
            validation_complete: true,
            issues_available: false,
            comparison_available: false,
            comparison_pending: false,
            comparison_complete: false,
            downstream: DownstreamJourneyAuthority::Enabled,
            decisions_available: false,
            reconciliation_available: true,
            package_available: true,
        });
        assert_contract(&view);
        assert_eq!(view.current_stage_id, JourneyStageId::Reconciliation);
        assert_eq!(view.stages[4].status, JourneyStageStatus::Completed);
        assert_eq!(view.stages[6].status, JourneyStageStatus::Locked);
        assert_eq!(view.stages[7].status, JourneyStageStatus::Locked);
        assert_eq!(
            view.stages[7].unavailable_reason.as_deref(),
            Some("No First Issues are available for governed decisions.")
        );
        assert_eq!(view.stages[8].status, JourneyStageStatus::Current);
        assert_eq!(view.stages[9].status, JourneyStageStatus::Available);
    }
}
