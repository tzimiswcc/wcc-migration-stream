use std::{fs, path::Path};

use serde::{Deserialize, Serialize};

use crate::{KernelError, Result};

/// Persisted, conservative limits for WP-001 synthetic intake.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IntakePolicy {
    pub spool_buffer_bytes: usize,
    pub minimum_free_space_bytes: u64,
    #[serde(default = "default_maximum_source_bytes")]
    pub maximum_source_bytes: u64,
    pub maximum_entries: usize,
    pub maximum_entry_path_bytes: usize,
    pub maximum_expanded_bytes: u64,
    pub maximum_compression_ratio: f64,
    pub maximum_csv_field_bytes: usize,
    pub maximum_csv_record_bytes: usize,
    #[serde(default = "default_maximum_csv_columns")]
    pub maximum_csv_columns: usize,
    #[serde(default = "default_maximum_csv_records")]
    pub maximum_csv_records: u64,
    pub transaction_batch_rows: usize,
}

const fn default_maximum_source_bytes() -> u64 {
    512 * 1024 * 1024
}

const fn default_maximum_csv_columns() -> usize {
    1_024
}

const fn default_maximum_csv_records() -> u64 {
    1_000_000
}

impl Default for IntakePolicy {
    fn default() -> Self {
        Self {
            spool_buffer_bytes: 64 * 1024,
            minimum_free_space_bytes: 64 * 1024 * 1024,
            maximum_source_bytes: default_maximum_source_bytes(),
            maximum_entries: 1_000,
            maximum_entry_path_bytes: 1_024,
            maximum_expanded_bytes: 256 * 1024 * 1024,
            maximum_compression_ratio: 100.0,
            maximum_csv_field_bytes: 1024 * 1024,
            maximum_csv_record_bytes: 4 * 1024 * 1024,
            maximum_csv_columns: default_maximum_csv_columns(),
            maximum_csv_records: default_maximum_csv_records(),
            transaction_batch_rows: 500,
        }
    }
}

impl IntakePolicy {
    /// Load a policy from JSON or return the conservative WP-001 defaults.
    ///
    /// # Errors
    ///
    /// Returns an I/O, JSON, or policy-consistency error when a supplied file
    /// cannot be loaded as a valid policy.
    pub fn from_path(path: Option<&Path>) -> Result<Self> {
        let Some(path) = path else {
            return Ok(Self::default());
        };
        let bytes = fs::read(path).map_err(|error| KernelError::io(path, error))?;
        let policy = serde_json::from_slice(&bytes)?;
        Self::validate(&policy)?;
        Ok(policy)
    }

    /// Check that all resource limits are positive and internally consistent.
    ///
    /// # Errors
    ///
    /// Returns a policy error when a limit is zero, non-finite, or conflicts
    /// with another configured limit.
    pub fn validate(policy: &Self) -> Result<()> {
        if policy.spool_buffer_bytes == 0
            || policy.maximum_entries == 0
            || policy.maximum_source_bytes == 0
            || policy.maximum_entry_path_bytes == 0
            || policy.maximum_expanded_bytes == 0
            || !policy.maximum_compression_ratio.is_finite()
            || policy.maximum_compression_ratio < 1.0
            || policy.maximum_csv_field_bytes == 0
            || policy.maximum_csv_record_bytes < policy.maximum_csv_field_bytes
            || policy.maximum_csv_columns == 0
            || policy.maximum_csv_records == 0
            || policy.transaction_batch_rows == 0
        {
            return Err(KernelError::PolicyInvalid(
                "resource limits must be positive and internally consistent".to_owned(),
            ));
        }
        Ok(())
    }
}
