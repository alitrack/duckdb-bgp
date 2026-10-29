//! SQL-level integration tests: LOAD the built `.duckdb_extension` into a real
//! in-memory DuckDB and exercise the full function surface (6 scalars).
//!
//! Run: `cargo test --release` (after the cdylib is built and stamped to
//! `target/release/bgp.duckdb_extension`).
//!
//! No BLOB inputs here — all six functions take VARCHAR, so no `from_hex` /
//! `x'HEX'` harness pitfall. `path` is a whitespace-separated ASN string in
//! path order (origin AS last).

use quack_rs::testing::InMemoryDb;

fn db() -> InMemoryDb {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/release/bgp.duckdb_extension");
    assert!(path.exists(), "build first: make release -> {path:?}");
    let d = InMemoryDb::open_unsigned().expect("open in-memory db");
    d.execute_batch(&format!("LOAD '{}'", path.display()))
        .expect("load extension");
    d
}

#[test]
fn path_length_and_origin() {
    let d = db();
    // '100 200 300' -> 3 hops, origin (last) = 300.
    let len: i64 = d.query_one("SELECT bgp_path_length('100 200 300')").unwrap();
    assert_eq!(len, 3);
    let origin: u32 = d.query_one("SELECT bgp_origin_asn('100 200 300')").unwrap();
    assert_eq!(origin, 300);
    // Empty / blank path -> 0 hops, NULL origin (total).
    let len_empty: i64 = d.query_one("SELECT bgp_path_length('')").unwrap();
    assert_eq!(len_empty, 0);
    let origin_empty: Option<u32> = d.query_one("SELECT bgp_origin_asn('')").unwrap();
    assert!(origin_empty.is_none());
    // NULL path -> NULL (total). Feed NULL through a VALUES column so DuckDB
    // cannot constant-fold the literal to a `Null`-typed result.
    let len_null: Option<i64> = d
        .query_one("SELECT bgp_path_length(p) FROM (SELECT NULL::VARCHAR AS p)")
        .unwrap();
    assert!(len_null.is_none());
}

#[test]
fn prepends() {
    let d = db();
    // [1,1,1,2] -> two prepends (two of the three 1s pad the first 1).
    let p: i64 = d.query_one("SELECT bgp_as_path_prepends('1 1 1 2')").unwrap();
    assert_eq!(p, 2);
    // [1,2,3] -> no consecutive repeats -> 0.
    let p2: i64 = d.query_one("SELECT bgp_as_path_prepends('1 2 3')").unwrap();
    assert_eq!(p2, 0);
}

#[test]
fn path_contains() {
    let d = db();
    let hit: bool = d
        .query_one("SELECT bgp_path_contains('100 200 300', 200)")
        .unwrap();
    assert!(hit);
    let miss: bool = d
        .query_one("SELECT bgp_path_contains('100 200 300', 999)")
        .unwrap();
    assert!(!miss);
    // NULL asn -> NULL (total). Route the NULL through a column so DuckDB does
    // not constant-fold the literal to an untyped `Null` result.
    let nul: Option<bool> = d
        .query_one("SELECT bgp_path_contains(p, a) FROM (SELECT '100 200 300' AS p, NULL::INTEGER AS a)")
        .unwrap();
    assert!(nul.is_none());
}

#[test]
fn community_parse_struct() {
    let d = db();
    // Standard '65001:100' -> STRUCT(asn=65001, value=100). v1.1.0 struct
    // `->` returns a JSON rendering for numeric fields, so CAST the children.
    let asn: i64 = d
        .query_one("SELECT CAST(bgp_community_parse('65001:100') -> 'asn' AS BIGINT)")
        .unwrap();
    assert_eq!(asn, 65001);
    let value: i64 = d
        .query_one("SELECT CAST(bgp_community_parse('65001:100') -> 'value' AS BIGINT)")
        .unwrap();
    assert_eq!(value, 100);
    // Large community '65001:1:2' -> NULL struct (third part disqualifies it).
    // A NULL child renders as the string "null" in v1.1.0, not SQL NULL.
    let large_asn: String = d
        .query_one("SELECT CAST(bgp_community_parse('65001:1:2') -> 'asn' AS VARCHAR)")
        .unwrap();
    assert_eq!(large_asn, "null");
    // Well-known mnemonic -> NULL struct too.
    let wk: String = d
        .query_one("SELECT CAST(bgp_community_parse('NO_EXPORT') -> 'asn' AS VARCHAR)")
        .unwrap();
    assert_eq!(wk, "null");
}

#[test]
fn is_large_community() {
    let d = db();
    let large: bool = d
        .query_one("SELECT bgp_is_large_community('65001:1:2')")
        .unwrap();
    assert!(large);
    let standard: bool = d
        .query_one("SELECT bgp_is_large_community('65001:100')")
        .unwrap();
    assert!(!standard);
    let mnemonic: bool = d
        .query_one("SELECT bgp_is_large_community('NO_EXPORT')")
        .unwrap();
    assert!(!mnemonic);
    // NULL -> NULL (total).
    let nul: Option<bool> = d
        .query_one("SELECT bgp_is_large_community(NULL)")
        .unwrap();
    assert!(nul.is_none());
}
