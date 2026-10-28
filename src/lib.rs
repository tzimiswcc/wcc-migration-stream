//! Migration Stream WP-001 kernel.
//!
//! The ingestion path is object-neutral. Technical metadata import is deliberately
//! separated from canonical profile validation so parser output can never become
//! runtime authority by accident.

pub mod benchmark;
pub mod canonical;
pub mod compared_validation;
mod decision_integrity;
pub mod decision_read_model;
pub mod decision_service;
pub mod decisions;
pub mod destination;
mod durability;
pub mod edmx;
mod effective_values;
pub mod engagement;
pub mod engagement_workbench;
pub mod error;
pub mod fixture;
pub mod intake;
pub mod journey;
pub mod mapping;
pub mod metadata;
pub mod object_workspace;
pub mod policy;
pub mod profile;
pub mod profile_registry;
pub mod profiling;
pub mod project_object;
pub mod project_runtime;
pub mod publication;
pub mod publication_integrity;
pub mod publication_read_model;
pub mod publication_service;
pub mod read_model;
pub mod reconciliation_approval;
pub(crate) mod reconciliation_integrity;
pub mod reconciliation_read_model;
pub mod reconciliation_service;
pub mod runtime_bundle;
pub mod runtime_lifecycle;
pub mod state;
pub mod validation;
pub mod workbench;

pub use error::{KernelError, Result};
