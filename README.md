# duckdb-bgp

**BGP AS-path + community helpers, in SQL.** Feed a whitespace-separated AS path
(`'100 200 300'`) or a community string (`'65001:100'`) and get back the path
length, the origin AS, the number of prepends, a membership test, and a parsed
or classified community — right in DuckDB.

```sql
LOAD 'bgp.duckdb_extension';

SELECT as_path,
       bgp_path_length  (as_path) AS hops,
       bgp_origin_asn   (as_path) AS origin,
       bgp_as_path_prepends(as_path) AS prepends,
       bgp_path_contains(as_path, 100) AS has_transit
FROM rib;

-- Classify communities
SELECT community,
       bgp_community_parse(community)       AS parsed,   -- STRUCT(asn, value) or NULL
       bgp_is_large_community(community)    AS is_large  -- RFC 8092 a:b:c
FROM rib;
```

## Why

AS-path and community attributes are the workhorse of route-policy analysis —
"which route did AS 100 prepend on?", "how many prepends does this route carry?",
"does this path transit AS 200?", "is this community a large (RFC 8092) one?".
Today those are usually bolted on with a Python/`bgpkit-parser` script or a
hand-rolled `split` in SQL. This extension makes the primitives SQL functions:
the path is a `VARCHAR` column, the answers are scalar values you can `GROUP BY`,
`JOIN` against a transit table, or push straight into `MATCH_RECOGNIZE`.

## Functions

| Function | Returns | Notes |
|---|---|---|
| `bgp_path_length(path)` | `BIGINT` | Hops in the path (its length). `''` / `NULL` → `0`. |
| `bgp_origin_asn(path)` | `UINTEGER` | The last (right-most) ASN — the origin that announced the prefix. `NULL` for an empty/blank path. |
| `bgp_as_path_prepends(path)` | `BIGINT` | Number of AS-path prepends: a consecutive repeat of the preceding ASN. `'1 1 1 2'` → `2`, `'1 2 3'` → `0`. |
| `bgp_path_contains(path, asn)` | `BOOLEAN` | Whether `asn` appears anywhere in the path. `NULL` `asn` → `false`. |
| `bgp_community_parse(community)` | `STRUCT(asn, value)` | Split a **standard** community `"<asn>:<value>"` into its two parts. A large community (`a:b:c`), a well-known mnemonic (`NO_EXPORT`), or a non-numeric part → `NULL` struct. |
| `bgp_is_large_community(community)` | `BOOLEAN` | RFC 8092: exactly three colon-separated unsigned-integer parts, `"<global>:<data1>:<data2>"`. `NULL` input → `NULL`. |

`path` is a `VARCHAR` of whitespace-separated ASNs in **path order** (the origin
AS is last): `'100 200 300'` means the route was announced by AS 300, relayed
through 200, and received from neighbor 100. Tokens that are not a non-negative
integer are skipped. All functions are **total**: an empty/unparseable path or
mismatched community yields `0` / `NULL` / `false` — never a SQL error.

## Scope (v1)

- **The six flat AS-path + community scalars** (verbatim port of the pure
  `aspath` and `community` modules from Query-farm's `vgi-bgp` `bgp-core` crate,
  MIT — both are dependency-free, so this extension pulls in **nothing** but
  `libduckdb-sys`).
- **Not ported:** the full MRT / UPDATE `decode.rs` surface (MRT record parsing,
  prefixes, communities-as-`LIST`, AS-set expansion) — that needs the
  `bgpkit-parser`/`flate2`/`bzip2` stack and a `BLOB` MRT input; and the worker's
  table sources (`read_rib` / `read_updates` / `peers`).

## Build

```sh
make release        # -> target/release/bgp.duckdb_extension
make test           # bundled DuckDB runs tests/integration.rs (SQL-level)
```

The cdylib links only the DuckDB C extension ABI (resolved at `LOAD` time) and
the two in-crate logic modules (pure `std`, no native, no I/O). `panic =
"unwind"` keeps a bug in the extension from aborting the host process.
