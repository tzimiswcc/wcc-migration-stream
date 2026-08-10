# Synthetic package fixtures

All values in this directory are synthetic. `customer_accounts.csv` exercises
leading zeroes, an explicit empty string, a quoted delimiter, a multiline value,
Unicode, and an empty field. The checked-in ZIP is generated from this file; tests
also construct unsafe archives in temporary directories so no unsafe payload is
ever extracted.

These records are ingestion-fidelity fixtures only. Their headers do not assert a
source mapping and do not make the reviewed-draft customer-account profile authoritative.
