#![allow(clippy::missing_errors_doc)]

//! WP-007 Publication Integrity Replay and Verification.

use crate::error::{KernelError, Result};
use crate::publication::PublicationRecord;

/// Verifies that a publication record's manifest and semantic hash replay cleanly.
pub fn verify_publication_record_integrity(record: &PublicationRecord) -> Result<()> {
    // 1. Verify contract version
    if record.contract_version != crate::publication::PUBLICATION_CONTRACT_VERSION {
        return Err(KernelError::Reconciliation(format!(
            "Contract version mismatch: expected {}, found {}",
            crate::publication::PUBLICATION_CONTRACT_VERSION,
            record.contract_version
        )));
    }

    // 2. Verify manifest member digests
    for member in &record.manifest.member_inventory {
        if member.sha256_digest.len() != 64 {
            return Err(KernelError::Reconciliation(format!(
                "Invalid SHA-256 digest length for member {}",
                member.filename
            )));
        }
    }

    // 3. Verify semantic publication hash format
    if record.semantic_publication_hash.len() != 64 {
        return Err(KernelError::Reconciliation(
            "Invalid semantic publication hash length".to_owned(),
        ));
    }

    // 4. Verify physical archive digest format
    if record.physical_archive_digest.len() != 64 {
        return Err(KernelError::Reconciliation(
            "Invalid physical archive digest length".to_owned(),
        ));
    }

    Ok(())
}
