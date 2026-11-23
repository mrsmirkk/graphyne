//! PostgreSQL over the network (rust-postgres; TLS through the system's SChannel).
//!
//! Results come in PostgreSQL's binary format; [`decode`] turns every column type into a
//! [`Value`] — exact `numeric`s, dates and times, UUIDs, JSON, network addresses, arrays —
//! without extra dependencies.

use crate::config::{ServerConfig, SslMode};
use crate::{Column, Driver, EXACT_COUNT_BELOW, Interrupter, ResultSet, RowCount, Schema, StatementResult, Table, Value};
use postgres::fallible_iterator::FallibleIterator;
use postgres::types::{FromSql, Kind, ToSql, Type};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

type BoxError = Box<dyn std::error::Error + Sync + Send>;

pub(crate) struct Pg {
    client: postgres::Client,
}

/// A readable message: the server's error (with DETAIL / HINT) or the connection problem.
fn pg_err(e: postgres::Error) -> String {
    match e.as_db_error() {
        Some(db) => {
            let mut s = format!("{}: {}", db.severity(), db.message());
            if let Some(d) = db.detail() {
                s.push_str(&format!("\nDETAIL: {d}"));
            }
            if let Some(h) = db.hint() {
                s.push_str(&format!("\nHINT: {h}"));
            }
            s
        }
        None => {
            // "error connecting to server: <why>" — keep the why.
            let mut s = e.to_string();
            let mut source = std::error::Error::source(&e);
            while let Some(cause) = source {
                let c = cause.to_string();
                if !s.contains(&c) {
                    s.push_str(&format!(": {c}"));
                }
                source = cause.source();
            }
            s
        }
    }
}

pub(crate) fn open(cfg: &ServerConfig, password: Option<String>) -> Result<(Pg, Interrupter), String> {
    let mut config = postgres::Config::new();
    // Through an SSH tunnel: connect to its local end; the certificate is still checked
    // against the server's own name. (`port` adds a port per call: set it once.)
    let port = cfg.via.map_or(cfg.port, |v| v.port());
    config.host(if cfg.host.is_empty() { "localhost" } else { cfg.host.as_str() }).port(port).application_name("Graphyne").connect_timeout(Duration::from_secs(10));
    if let Some(via) = cfg.via {
        config.hostaddr(via.ip());
    }
    if !cfg.user.is_empty() {
        config.user(cfg.user.as_str());
    }
    if !cfg.database.is_empty() {
        config.dbname(cfg.database.as_str());
    }
    if let Some(p) = &password {
        config.password(p.as_str());
    }
    config.ssl_mode(match cfg.ssl {
        SslMode::Disable => postgres::config::SslMode::Disable,
        SslMode::Prefer => postgres::config::SslMode::Prefer,
        SslMode::Require | SslMode::VerifyFull => postgres::config::SslMode::Require,
    });
    // Like libpq: only "verify" checks the certificate.
    let verify = cfg.ssl == SslMode::VerifyFull;
    let tls = native_tls::TlsConnector::builder().danger_accept_invalid_certs(!verify).danger_accept_invalid_hostnames(!verify).build().map_err(|e| e.to_string())?;
    let tls = postgres_native_tls::MakeTlsConnector::new(tls);
    let client = config.connect(tls.clone()).map_err(pg_err)?;
    let token = client.cancel_token();
    Ok((Pg { client }, Arc::new(move || drop(token.cancel_query(tls.clone())))))
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// A cell of any type, decoded from the binary format.
struct Cell(Value);

impl<'a> FromSql<'a> for Cell {
    fn from_sql(ty: &Type, raw: &'a [u8]) -> Result<Self, BoxError> {
        decode(ty, raw).map(Cell)
    }

    fn from_sql_null(_: &Type) -> Result<Self, BoxError> {
        Ok(Cell(Value::Null))
    }

    fn accepts(_: &Type) -> bool {
        true
    }
}

fn be<const N: usize>(raw: &[u8], at: usize) -> Result<[u8; N], BoxError> {
    raw.get(at..at + N).and_then(|s| s.try_into().ok()).ok_or_else(|| "truncated value".into())
}

fn utf8(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw).into_owned()
}

pub(crate) fn decode(ty: &Type, raw: &[u8]) -> Result<Value, BoxError> {
    Ok(match ty.oid() {
        16 => Value::Text(if raw.first() == Some(&1) { "true" } else { "false" }.into()),
        21 => Value::Int(i16::from_be_bytes(be(raw, 0)?) as i64),
        23 => Value::Int(i32::from_be_bytes(be(raw, 0)?) as i64),
        20 => Value::Int(i64::from_be_bytes(be(raw, 0)?)),
        // oid, xid, cid, regclass… are unsigned 32-bit.
        26 | 28 | 29 | 2205 | 24 | 2206 => Value::Int(u32::from_be_bytes(be(raw, 0)?) as i64),
        700 => Value::Real(f32::from_be_bytes(be(raw, 0)?) as f64),
        701 => Value::Real(f64::from_be_bytes(be(raw, 0)?)),
        1700 => Value::Num(numeric(raw)?),
        790 => {
            let cents = i64::from_be_bytes(be(raw, 0)?);
            Value::Num(format!("{}{}.{:02}", if cents < 0 { "-" } else { "" }, cents.unsigned_abs() / 100, cents.unsigned_abs() % 100))
        }
        17 => Value::blob(raw),
        2950 => Value::Text(uuid(raw)?),
        1082 => Value::Text(date(i32::from_be_bytes(be(raw, 0)?))),
        1114 => Value::Text(timestamp(i64::from_be_bytes(be(raw, 0)?))),
        1184 => Value::Text(format!("{}+00", timestamp(i64::from_be_bytes(be(raw, 0)?)))),
        1083 => Value::Text(time(i64::from_be_bytes(be(raw, 0)?))),
        1266 => {
            let us = i64::from_be_bytes(be(raw, 0)?);
            // Seconds west of UTC.
            let west = i32::from_be_bytes(be(raw, 8)?);
            Value::Text(format!("{}{}", time(us), offset(-west)))
        }
        1186 => Value::Text(interval(i64::from_be_bytes(be(raw, 0)?), i32::from_be_bytes(be(raw, 8)?), i32::from_be_bytes(be(raw, 12)?))),
        3802 => Value::Text(utf8(raw.get(1..).unwrap_or_default())),
        18 => Value::Text((raw.first().copied().unwrap_or(b' ') as char).to_string()),
        869 | 650 => Value::Text(inet(raw)?),
        _ => match ty.kind() {
            Kind::Array(member) => Value::Text(array(member, raw)?),
            Kind::Domain(inner) => return decode(inner, raw),
            // text, varchar, name, json, xml, citext, enums… travel as text.
            _ => match std::str::from_utf8(raw) {
                Ok(s) if !s.contains('\0') => Value::Text(s.to_string()),
                _ => Value::blob(raw),
            },
        },
    })
}

/// `numeric`: digits in base 10 000 with a weight, a sign and a display scale.
fn numeric(raw: &[u8]) -> Result<String, BoxError> {
    let word = |at: usize| be::<2>(raw, at).map(u16::from_be_bytes);
    let ndigits = word(0)? as usize;
    let weight = word(2)? as i16 as i32;
    let sign = word(4)?;
    let dscale = word(6)? as usize;
    match sign {
        0xC000 => return Ok("NaN".into()),
        0xD000 => return Ok("Infinity".into()),
        0xF000 => return Ok("-Infinity".into()),
        _ => {}
    }
    let digits: Vec<u16> = (0..ndigits).map(|i| word(8 + 2 * i)).collect::<Result<_, _>>()?;
    let digit = |i: i32| if i < 0 { 0 } else { digits.get(i as usize).copied().unwrap_or(0) };
    let mut out = String::new();
    if sign == 0x4000 {
        out.push('-');
    }
    if weight < 0 {
        out.push('0');
    } else {
        for i in 0..=weight {
            if i == 0 { out.push_str(&digit(i).to_string()) } else { out.push_str(&format!("{:04}", digit(i))) }
        }
    }
    if dscale > 0 {
        let mut frac = String::new();
        let mut i = weight + 1;
        while frac.len() < dscale {
            frac.push_str(&format!("{:04}", digit(i)));
            i += 1;
        }
        frac.truncate(dscale);
        out.push('.');
        out.push_str(&frac);
    }
    Ok(out)
}

fn uuid(raw: &[u8]) -> Result<String, BoxError> {
    let b: [u8; 16] = be(raw, 0)?;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    Ok(format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32]))
}

/// Days since 1970-01-01 → (year, month, day) (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe as i64 + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// PostgreSQL counts dates from 2000-01-01.
const PG_EPOCH_DAYS: i64 = 10_957;

fn date(days: i32) -> String {
    match days {
        i32::MAX => "infinity".into(),
        i32::MIN => "-infinity".into(),
        _ => {
            let (y, m, d) = civil_from_days(days as i64 + PG_EPOCH_DAYS);
            if y > 0 { format!("{y:04}-{m:02}-{d:02}") } else { format!("{:04}-{m:02}-{d:02} BC", 1 - y) }
        }
    }
}

/// Microseconds since midnight → `HH:MM:SS[.ffffff]`.
fn time(us: i64) -> String {
    let secs = us / 1_000_000;
    let frac = us % 1_000_000;
    let base = format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60);
    if frac == 0 { base } else { format!("{base}.{}", format!("{frac:06}").trim_end_matches('0')) }
}

fn timestamp(us: i64) -> String {
    match us {
        i64::MAX => "infinity".into(),
        i64::MIN => "-infinity".into(),
        _ => {
            let day = 86_400_000_000;
            format!("{} {}", date(us.div_euclid(day) as i32), time(us.rem_euclid(day)))
        }
    }
}

fn offset(east_secs: i32) -> String {
    let sign = if east_secs < 0 { '-' } else { '+' };
    let s = east_secs.unsigned_abs();
    if s % 3600 == 0 { format!("{sign}{:02}", s / 3600) } else { format!("{sign}{:02}:{:02}", s / 3600, s / 60 % 60) }
}

/// Like PostgreSQL's own output: `1 year 2 mons 3 days 04:05:06`.
fn interval(us: i64, days: i32, months: i32) -> String {
    let mut parts = Vec::new();
    let (years, mons) = (months / 12, months % 12);
    let plural = |n: i32, one: &str, many: &str| format!("{n} {}", if n.abs() == 1 { one } else { many });
    if years != 0 {
        parts.push(plural(years, "year", "years"));
    }
    if mons != 0 {
        parts.push(plural(mons, "mon", "mons"));
    }
    if days != 0 {
        parts.push(plural(days, "day", "days"));
    }
    if us != 0 || parts.is_empty() {
        parts.push(format!("{}{}", if us < 0 { "-" } else { "" }, time(us.abs())));
    }
    parts.join(" ")
}

fn inet(raw: &[u8]) -> Result<String, BoxError> {
    let (family, bits, is_cidr, len) = (raw.first().copied(), raw.get(1).copied().unwrap_or(0), raw.get(2) == Some(&1), raw.get(3).copied().unwrap_or(0) as usize);
    let addr = raw.get(4..4 + len).ok_or("truncated address")?;
    let text = if family == Some(2) {
        addr.iter().map(|b| b.to_string()).collect::<Vec<_>>().join(".")
    } else {
        let words: Vec<u16> = addr.chunks(2).map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)])).collect();
        std::net::Ipv6Addr::new(words[0], words[1], words[2], words[3], words[4], words[5], words[6], words[7]).to_string()
    };
    let full = if family == Some(2) { 32 } else { 128 };
    Ok(if is_cidr || bits != full { format!("{text}/{bits}") } else { text })
}

/// Arrays in PostgreSQL's text form: `{1,2,3}`, `{{a,b},{c,d}}`, `{"x y",NULL}`.
fn array(member: &Type, raw: &[u8]) -> Result<String, BoxError> {
    let ndim = i32::from_be_bytes(be(raw, 0)?) as usize;
    if ndim == 0 {
        return Ok("{}".into());
    }
    let dims: Vec<usize> = (0..ndim).map(|d| be::<4>(raw, 12 + 8 * d).map(|b| i32::from_be_bytes(b).max(0) as usize)).collect::<Result<_, _>>()?;
    let mut at = 12 + 8 * ndim;
    let total: usize = dims.iter().product();
    let mut items = Vec::with_capacity(total);
    for _ in 0..total {
        let len = i32::from_be_bytes(be(raw, at)?);
        at += 4;
        if len < 0 {
            items.push("NULL".to_string());
            continue;
        }
        let bytes = raw.get(at..at + len as usize).ok_or("truncated array")?;
        at += len as usize;
        let text = decode(member, bytes)?.to_text();
        let needs_quotes = text.is_empty() || text.eq_ignore_ascii_case("null") || text.chars().any(|c| matches!(c, ',' | '{' | '}' | '"' | '\\') || c.is_whitespace());
        items.push(if needs_quotes { format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\"")) } else { text });
    }
    fn nest(items: &[String], dims: &[usize]) -> String {
        if dims.len() == 1 {
            return format!("{{{}}}", items.join(","));
        }
        let size: usize = dims[1..].iter().product();
        let parts: Vec<String> = items.chunks(size.max(1)).map(|c| nest(c, &dims[1..])).collect();
        format!("{{{}}}", parts.join(","))
    }
    Ok(nest(&items, &dims))
}

impl Driver for Pg {
    fn schema(&mut self) -> Result<Schema, String> {
        const USER_SCHEMAS: &str = "table_schema NOT IN ('pg_catalog', 'information_schema') AND table_schema NOT LIKE 'pg\\_toast%' AND table_schema NOT LIKE 'pg\\_temp%'";
        let tables = self
            .client
            .query(&format!("SELECT table_schema::text, table_name::text, table_type::text FROM information_schema.tables WHERE {USER_SCHEMAS} ORDER BY table_schema <> 'public', table_schema, lower(table_name)"), &[])
            .map_err(pg_err)?;
        let columns = self
            .client
            .query(
                &format!(
                    "SELECT table_schema::text, table_name::text, column_name::text, \
                     CASE WHEN data_type IN ('USER-DEFINED', 'ARRAY') THEN udt_name::text ELSE data_type::text END, is_nullable::text \
                     FROM information_schema.columns WHERE {USER_SCHEMAS} ORDER BY table_schema, table_name, ordinal_position"
                ),
                &[],
            )
            .map_err(pg_err)?;
        let keys = self
            .client
            .query(
                "SELECT tc.table_schema::text, tc.table_name::text, kcu.column_name::text FROM information_schema.table_constraints tc \
                 JOIN information_schema.key_column_usage kcu ON kcu.constraint_schema = tc.constraint_schema AND kcu.constraint_name = tc.constraint_name \
                 WHERE tc.constraint_type = 'PRIMARY KEY'",
                &[],
            )
            .map_err(pg_err)?;
        let primary: HashSet<(String, String, String)> = keys.iter().map(|r| (r.get(0), r.get(1), r.get(2))).collect();
        let mut by_table: HashMap<(String, String), Vec<Column>> = HashMap::new();
        for r in &columns {
            let (schema, table, name, type_name, nullable): (String, String, String, String, String) = (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4));
            // `_int4` is how arrays are named: show `int4[]`.
            let type_name = type_name.strip_prefix('_').map(|t| format!("{t}[]")).unwrap_or(type_name);
            let primary_key = primary.contains(&(schema.clone(), table.clone(), name.clone()));
            by_table.entry((schema, table)).or_default().push(Column { name, type_name, not_null: nullable == "NO", primary_key });
        }
        let mut schema = Schema::default();
        for r in &tables {
            let (s, name, kind): (String, String, String) = (r.get(0), r.get(1), r.get(2));
            let columns = by_table.remove(&(s.clone(), name.clone())).unwrap_or_default();
            let table = Table { schema: (s != "public").then_some(s), name, columns };
            if kind == "VIEW" { schema.views.push(table) } else { schema.tables.push(table) }
        }
        Ok(schema)
    }

    fn run(&mut self, sql: &str, limit: usize) -> Result<StatementResult, String> {
        let statement = self.client.prepare(sql).map_err(pg_err)?;
        if statement.columns().is_empty() {
            let affected = self.client.execute(&statement, &[]).map_err(pg_err)?;
            return Ok(StatementResult::Done { affected });
        }
        let columns: Vec<String> = statement.columns().iter().map(|c| c.name().to_string()).collect();
        let count = columns.len();
        let mut rows_iter = self.client.query_raw(&statement, std::iter::empty::<&dyn ToSql>()).map_err(pg_err)?;
        let mut rows = Vec::new();
        let mut truncated = false;
        while let Some(row) = rows_iter.next().map_err(pg_err)? {
            if rows.len() == limit {
                truncated = true;
                break;
            }
            let mut cells = Vec::with_capacity(count);
            for i in 0..count {
                cells.push(row.try_get::<_, Cell>(i).map_err(pg_err)?.0);
            }
            rows.push(cells);
        }
        Ok(StatementResult::Rows(ResultSet { columns, rows, truncated }))
    }

    fn count_rows(&mut self, schema: Option<&str>, table: &str) -> Result<RowCount, String> {
        let name = self.quote_table(schema, table);
        // The planner's estimate first: counting a huge table takes long.
        let estimate: i64 = self.client.query_one("SELECT COALESCE((SELECT reltuples::bigint FROM pg_class WHERE oid = to_regclass($1)), -1)", &[&name]).map_err(pg_err)?.get(0);
        if estimate >= EXACT_COUNT_BELOW as i64 {
            return Ok(RowCount { rows: estimate as u64, exact: false });
        }
        let rows: i64 = self.client.query_one(&format!("SELECT count(*) FROM {name}"), &[]).map_err(pg_err)?.get(0);
        Ok(RowCount { rows: rows as u64, exact: true })
    }

    fn quote_ident(&self, name: &str) -> String {
        quote(name)
    }

    fn version(&mut self) -> Result<String, String> {
        let v: String = self.client.query_one("SHOW server_version", &[]).map_err(pg_err)?.get(0);
        Ok(format!("PostgreSQL {}", v.split_whitespace().next().unwrap_or(&v)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numeric_bytes(weight: i16, sign: u16, dscale: u16, digits: &[u16]) -> Vec<u8> {
        let mut v = Vec::new();
        for w in [digits.len() as u16, weight as u16, sign, dscale] {
            v.extend(w.to_be_bytes());
        }
        for d in digits {
            v.extend(d.to_be_bytes());
        }
        v
    }

    #[test]
    fn binary_values_read_like_postgres_prints_them() {
        assert_eq!(numeric(&numeric_bytes(0, 0, 2, &[12, 5000])).unwrap(), "12.50");
        assert_eq!(numeric(&numeric_bytes(-1, 0, 3, &[10])).unwrap(), "0.001");
        assert_eq!(numeric(&numeric_bytes(1, 0x4000, 1, &[1234, 5678, 9000])).unwrap(), "-12345678.9");
        assert_eq!(numeric(&numeric_bytes(1, 0, 0, &[100])).unwrap(), "1000000");
        assert_eq!(numeric(&numeric_bytes(-2, 0, 5, &[1000])).unwrap(), "0.00001");
        assert_eq!(numeric(&numeric_bytes(0, 0xC000, 0, &[])).unwrap(), "NaN");
        assert_eq!(date(0), "2000-01-01");
        assert_eq!(date(9_452), "2025-11-17");
        assert_eq!(date(-730_119), "0001-01-01");
        assert_eq!(date(-730_120), "0001-12-31 BC");
        assert_eq!(timestamp(86_400_000_000 + 3_723_500_000), "2000-01-02 01:02:03.5");
        assert_eq!(timestamp(-1), "1999-12-31 23:59:59.999999");
        assert_eq!(interval(3_723_000_000, 3, 14), "1 year 2 mons 3 days 01:02:03");
        assert_eq!(interval(0, 0, 0), "00:00:00");
        assert_eq!(offset(-5 * 3600), "-05");
        assert_eq!(offset(5 * 3600 + 1800), "+05:30");
        assert_eq!(uuid(&[0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 1, 2, 3, 4, 5, 6, 7, 8]).unwrap(), "12345678-9abc-def0-0102-030405060708");
        assert_eq!(inet(&[2, 32, 0, 4, 192, 168, 0, 1]).unwrap(), "192.168.0.1");
        assert_eq!(inet(&[2, 24, 1, 4, 10, 0, 0, 0]).unwrap(), "10.0.0.0/24");

        // int4[] {1,NULL,3} and text[] {"a b",c}.
        let mut ints = Vec::new();
        for w in [1i32, 1, 23, 3, 1] {
            ints.extend(w.to_be_bytes());
        }
        for item in [Some(1i32), None, Some(3)] {
            match item {
                Some(n) => {
                    ints.extend(4i32.to_be_bytes());
                    ints.extend(n.to_be_bytes());
                }
                None => ints.extend((-1i32).to_be_bytes()),
            }
        }
        assert_eq!(array(&Type::INT4, &ints).unwrap(), "{1,NULL,3}");
        let mut texts = Vec::new();
        for w in [1i32, 0, 25, 2, 1] {
            texts.extend(w.to_be_bytes());
        }
        for s in ["a b", "c"] {
            texts.extend((s.len() as i32).to_be_bytes());
            texts.extend(s.as_bytes());
        }
        assert_eq!(array(&Type::TEXT, &texts).unwrap(), "{\"a b\",c}");
        assert_eq!(decode(&Type::NUMERIC, &numeric_bytes(0, 0, 2, &[12, 5000])).unwrap(), Value::Num("12.50".into()));
        assert_eq!(decode(&Type::JSONB, b"\x01{\"a\":1}").unwrap(), Value::Text("{\"a\":1}".into()));
    }
}
