# SMA tag translations

This directory's `tags-<locale>.json` files contain the embedded translations used for SMA
status, event and device tags. They are structured UTF-8 JSON documents and
can be edited with any text editor or JSON-aware IDE.

Each document contains its `locale`, source metadata, and a `tags` array.
Every tag object has these fields:

| Field | Meaning |
|---|---|
| `id` | Numeric SMA tag identifier. Must be unique within the file. |
| `short` | Compact SMA label used by protocol tools and diagnostics. |
| `unit_id` | Referenced unit tag identifier, or `0` when there is no unit reference. |
| `long` | User-facing translated description. |

Keep every locale structurally complete, preserve tag IDs when updating
translations, and validate edited files with `jq empty tags-<locale>.json`.

The source metadata records the corresponding SBFspot
`TagList<LOCALE>.txt` import name. The repository is licensed under
EUPL-1.2; see `LICENSE.md`.
