# WP-003 synthetic authority, mapping, and validation proof pack

Everything in this directory is synthetic test evidence. It does not describe
client data, an external source or target, or target compatibility.

The pack reuses `fixtures/packages/valid.zip`, whose accepted package SHA-256 is
`656ed753f1dd7973906f428f526d9069c00c60c84ddc490599328d2217746ca3`.
Its only CSV is `customer_accounts.csv`, with the exact headers
`migrationPartnerId`, `category`, `organizationName1`, and `notes`.

`synthetic-profile.reviewed.json` is deliberately `REVIEWED_DRAFT` and has no
runtime authority merely because it is checked in or schema-valid. Register it
and perform the explicit local human-approval action before mapping or
validation.

The invalid templates separately exercise an unknown source column, conflicting
target bindings, missing required coverage, a tombstone source reference, an
unsupported operation, a missing lookup table, and an exact profile-hash
mismatch. Schema-invalid and schema-valid-but-semantically-invalid cases remain
distinct on purpose.

`valid-mapping.template.json` and each invalid file ending in `.template.json`
contain the JSON string `__PROFILE_SHA256__`. They are syntactically valid JSON
templates, not schema-valid finalized mapping documents. Replace that token with
the exact 64-character lowercase SHA-256 returned by profile registration. Do
not replace it with a guessed, raw-file, generated-catalogue, or draft hash.
`invalid-profile-hash.json` intentionally keeps a schema-valid all-zero hash so
the authority mismatch is the rejection under test.

The valid template intentionally:

- copies the received migration ID directly;
- maps a constant duplicate-group key used by the scoped uniqueness proof;
- maps category codes `1` and `2` through an exact bounded lookup;
- trims the received organization name;
- maps an empty notes value to canonical null.

The profile exercises required, maximum length, allowed values, safe pattern,
scoped uniqueness, conditional requiredness, and versioned reference lookup.
The expected summary records semantic row/issue expectations. Canonical hashes
and deterministic issue IDs are recorded only by execution against the exact
approved profile and finalized mapping.

`expected-all-rules-golden.json` is the literal seven-rule failure oracle used
with a deterministic in-test synthetic archive. It fixes the accepted source,
profile, mapping, validation identity/result hashes, and all ten issue
identities without changing the smaller five-issue smoke fixture.
