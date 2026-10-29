// DuckDB loadable extension: BGP AS-path + community helpers, in SQL.
//
// Thin quack-rs binding over two verbatim modules ported from Query-farm's
// vgi-bgp bgp-core crate (MIT): `aspath` (pure over a decoded `&[u32]` AS path)
// and `community` (pure over a community string). Both are dependency-free —
// only `std` — so this extension pulls in nothing but libduckdb-sys. Only the
// Arrow IPC transport was replaced with direct C-API vector I/O.
//
// Function surface (v1 — 6 flat scalars):
//
//   bgp_path_length(path)        -> BIGINT    hops in the path ([] -> 0)
//   bgp_origin_asn(path)         -> UINTEGER  last ASN (the origin); NULL for empty
//   bgp_as_path_prepends(path)   -> BIGINT    repeated-ASN prepends
//   bgp_path_contains(path, asn) -> BOOLEAN   is `asn` anywhere in the path
//   bgp_community_parse(comm)    -> STRUCT(asn, value)  standard `a:b`; NULL for large/well-known
//   bgp_is_large_community(comm) -> BOOLEAN    RFC 8092 three-part `a:b:c`
//
// `path` is a VARCHAR of whitespace-separated ASNs in path order (the origin
// AS that announced the prefix is last), e.g. `'100 200 300'` — the textual
// form of an `as_path` column emitted by a RIB/UPDATE dump. `comm` is a
// VARCHAR community (standard `65001:100`, large `65001:1:2`, or a well-known
// mnemonic like `NO_EXPORT`). All functions are total: an empty/unparseable
// path or a non-matching community yields NULL (struct origin / path origin)
// or 0/false — never a SQL error.
//
// Not ported in v1: the full MRT/UPDATE `decode.rs` surface (MRT records,
// prefixes, communities-as-LIST, AS-set expansion) — that needs the
// `bgpkit-parser`/flate2/bzip2 stack and a BLOB MRT input; and the worker's
// table sources (`read_rib`/`read_updates`/`peers`).

use quack_rs::connection::{Connection, Registrar};
use quack_rs::error::ExtensionError;
use quack_rs::scalar::ScalarFunctionBuilder;
use quack_rs::types::{LogicalType, TypeId};
use quack_rs::vector::{VectorReader, VectorWriter};

mod aspath;
mod community;

/// Read a VARCHAR cell as a String; None when NULL/invalid.
unsafe fn cell_str(value: &VectorReader, row: usize) -> Option<String> {
    if !value.is_valid(row) {
        return None;
    }
    Some(value.read_str(row).to_string())
}

/// Parse a whitespace-separated ASN string into a `&[u32]`-shaped path.
/// Tokens that are not a non-negative integer are skipped; an empty / all-
/// garbage input yields an empty path (so `path_length` -> 0, `origin_asn` -> NULL).
fn parse_path(raw: &str) -> Vec<u32> {
    raw.split_whitespace()
        .filter_map(|tok| tok.parse::<u32>().ok())
        .collect()
}

/// Read the `path` VARCHAR column into a `Vec<u32>`; empty for NULL/blank.
unsafe fn path_of(value: &VectorReader, row: usize) -> Vec<u32> {
    cell_str(value, row).map(|s| parse_path(&s)).unwrap_or_default()
}

// ─────────────────────────────────────────────────────────────────────────────
// bgp_path_length(path) -> BIGINT
// ─────────────────────────────────────────────────────────────────────────────

quack_rs::scalar_callback!(path_length_fn, |_info, input, output| {
    unsafe {
        let path = VectorReader::new(input, 0);
        let row_count = path.row_count();
        let mut writer = VectorWriter::new(output);
        for row in 0..row_count {
            if path.is_valid(row) {
                writer.write_i64(row, aspath::path_length(&path_of(&path, row)));
            } else {
                writer.set_null(row);
            }
        }
    }
});

// ─────────────────────────────────────────────────────────────────────────────
// bgp_origin_asn(path) -> UINTEGER  (NULL for an empty path)
// ─────────────────────────────────────────────────────────────────────────────

quack_rs::scalar_callback!(origin_asn_fn, |_info, input, output| {
    unsafe {
        let path = VectorReader::new(input, 0);
        let row_count = path.row_count();
        let mut writer = VectorWriter::new(output);
        for row in 0..row_count {
            if !path.is_valid(row) {
                writer.set_null(row);
                continue;
            }
            match aspath::origin_asn(&path_of(&path, row)) {
                Some(asn) => writer.write_u32(row, asn),
                None => writer.set_null(row),
            }
        }
    }
});

// ─────────────────────────────────────────────────────────────────────────────
// bgp_as_path_prepends(path) -> BIGINT
// ─────────────────────────────────────────────────────────────────────────────

quack_rs::scalar_callback!(prepends_fn, |_info, input, output| {
    unsafe {
        let path = VectorReader::new(input, 0);
        let row_count = path.row_count();
        let mut writer = VectorWriter::new(output);
        for row in 0..row_count {
            if path.is_valid(row) {
                writer.write_i64(row, aspath::as_path_prepends(&path_of(&path, row)));
            } else {
                writer.set_null(row);
            }
        }
    }
});

// ─────────────────────────────────────────────────────────────────────────────
// bgp_path_contains(path, asn) -> BOOLEAN
// ─────────────────────────────────────────────────────────────────────────────

quack_rs::scalar_callback!(contains_fn, |_info, input, output| {
    unsafe {
        let path = VectorReader::new(input, 0);
        let asn = VectorReader::new(input, 1);
        let row_count = path.row_count();
        let mut writer = VectorWriter::new(output);
        for row in 0..row_count {
            if path.is_valid(row) && asn.is_valid(row) {
                writer.write_bool(
                    row,
                    aspath::path_contains(&path_of(&path, row), asn.read_i64(row) as u32),
                );
            } else {
                writer.set_null(row);
            }
        }
    }
});

// ─────────────────────────────────────────────────────────────────────────────
// bgp_community_parse(comm) -> STRUCT(asn, value)   (NULL struct for large/well-known)
// ─────────────────────────────────────────────────────────────────────────────

const COMMUNITY_FIELDS: &[(&str, TypeId)] =
    &[("asn", TypeId::UInteger), ("value", TypeId::UInteger)];

quack_rs::scalar_callback!(community_parse_fn, |_info, input, output| {
    unsafe {
        let comm = VectorReader::new(input, 0);
        let row_count = comm.row_count();
        let mut sw = quack_rs::vector::StructWriter::new(output, COMMUNITY_FIELDS.len());
        for row in 0..row_count {
            match cell_str(&comm, row)
                .as_deref()
                .and_then(community::community_parse)
            {
                Some((asn, value)) => {
                    sw.write_u32(row, 0, asn);
                    sw.write_u32(row, 1, value);
                }
                None => {
                    sw.set_null(row, 0);
                    sw.set_null(row, 1);
                }
            }
        }
    }
});

// ─────────────────────────────────────────────────────────────────────────────
// bgp_is_large_community(comm) -> BOOLEAN
// ─────────────────────────────────────────────────────────────────────────────

quack_rs::scalar_callback!(large_community_fn, |_info, input, output| {
    unsafe {
        let comm = VectorReader::new(input, 0);
        let row_count = comm.row_count();
        let mut writer = VectorWriter::new(output);
        for row in 0..row_count {
            match cell_str(&comm, row) {
                Some(raw) => writer.write_bool(row, community::is_large_community(&raw)),
                None => writer.set_null(row),
            }
        }
    }
});

// ─────────────────────────────────────────────────────────────────────────────
// Registration + entry point
// ─────────────────────────────────────────────────────────────────────────────

unsafe fn register_all(con: &Connection) -> Result<(), ExtensionError> {
    con.register_scalar(
        ScalarFunctionBuilder::new("bgp_path_length")
            .param(TypeId::Varchar)
            .returns(TypeId::BigInt)
            .function(path_length_fn),
    )?;

    con.register_scalar(
        ScalarFunctionBuilder::new("bgp_origin_asn")
            .param(TypeId::Varchar)
            .returns(TypeId::UInteger)
            .function(origin_asn_fn),
    )?;

    con.register_scalar(
        ScalarFunctionBuilder::new("bgp_as_path_prepends")
            .param(TypeId::Varchar)
            .returns(TypeId::BigInt)
            .function(prepends_fn),
    )?;

    con.register_scalar(
        ScalarFunctionBuilder::new("bgp_path_contains")
            .param(TypeId::Varchar)
            .param(TypeId::Integer)
            .returns(TypeId::Boolean)
            .function(contains_fn),
    )?;

    con.register_scalar(
        ScalarFunctionBuilder::new("bgp_community_parse")
            .param(TypeId::Varchar)
            .returns_logical(LogicalType::struct_type(COMMUNITY_FIELDS))
            .function(community_parse_fn),
    )?;

    con.register_scalar(
        ScalarFunctionBuilder::new("bgp_is_large_community")
            .param(TypeId::Varchar)
            .returns(TypeId::Boolean)
            .function(large_community_fn),
    )?;

    Ok(())
}

quack_rs::entry_point_v2!(bgp_init_c_api, |con| register_all(con));
