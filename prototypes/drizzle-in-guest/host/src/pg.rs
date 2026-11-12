//! The bridge between JSON (what an invoke call carries) and Postgres.
//!
//! Parameters: every parameter is sent in Postgres *text* format and the
//! server infers its type from the statement, so the host needs no per-type
//! encoder. JSON `null` is SQL NULL; arrays bound to an array parameter
//! become an array literal; other objects and arrays become JSON text.
//!
//! Rows: tokio-postgres reads results in binary format; each column is
//! turned into the JSON value node-postgres would give Drizzle (with the type
//! parsers Drizzle's node-postgres driver installs): int2/int4/float as
//! numbers, int8 and numeric as strings, date/time/timestamp as Postgres text,
//! json/jsonb parsed, arrays as JSON arrays, bytea as `\x...` hex text (the
//! one difference: node-postgres gives a Buffer, which JSON cannot carry).

use std::error::Error;
use std::fmt::Write as _;

use bytes::BytesMut;
use serde_json::{Map, Value, json};
use tokio_postgres::Row;
use tokio_postgres::types::{FromSql, IsNull, Kind, ToSql, Type, to_sql_checked};

type BoxError = Box<dyn Error + Sync + Send>;

/// A parameter in text format, of whatever type the server inferred.
#[derive(Debug)]
pub struct TextParam(pub Option<String>);

impl ToSql for TextParam {
    fn to_sql(&self, _ty: &Type, out: &mut BytesMut) -> Result<IsNull, BoxError> {
        match &self.0 {
            None => Ok(IsNull::Yes),
            Some(s) => {
                out.extend_from_slice(s.as_bytes());
                Ok(IsNull::No)
            }
        }
    }

    fn accepts(_ty: &Type) -> bool {
        true
    }

    fn encode_format(&self, _ty: &Type) -> tokio_postgres::types::Format {
        tokio_postgres::types::Format::Text
    }

    to_sql_checked!();
}

fn array_literal(values: &[Value]) -> String {
    let mut out = String::from("{");
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        match v {
            Value::Null => out.push_str("NULL"),
            Value::Array(inner) => out.push_str(&array_literal(inner)),
            other => {
                let text = match other {
                    Value::String(s) => s.clone(),
                    Value::Bool(b) => b.to_string(),
                    _ => other.to_string(),
                };
                out.push('"');
                out.push_str(&text.replace('\\', "\\\\").replace('"', "\\\""));
                out.push('"');
            }
        }
    }
    out.push('}');
    out
}

/// One JSON parameter as text for a parameter of type `ty`.
pub fn param(value: &Value, ty: &Type) -> TextParam {
    TextParam(match value {
        Value::Null => None,
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) => Some(s.clone()),
        Value::Array(items) if matches!(ty.kind(), Kind::Array(_)) => Some(array_literal(items)),
        other => Some(other.to_string()),
    })
}

/// Any column, raw bytes (binary format).
struct Raw(Vec<u8>);

impl<'a> FromSql<'a> for Raw {
    fn from_sql(_ty: &Type, raw: &'a [u8]) -> Result<Self, BoxError> {
        Ok(Raw(raw.to_vec()))
    }
    fn accepts(_ty: &Type) -> bool {
        true
    }
}

/// Binary `numeric` to its exact decimal text.
fn numeric_text(raw: &[u8]) -> Result<String, BoxError> {
    let be16 = |i: usize| -> Result<i16, BoxError> {
        raw.get(i..i + 2)
            .map(|b| i16::from_be_bytes([b[0], b[1]]))
            .ok_or_else(|| "short numeric".into())
    };
    let ndigits = be16(0)? as usize;
    let weight = be16(2)? as i32;
    let sign = be16(4)? as u16;
    let dscale = be16(6)? as usize;
    match sign {
        0xC000 => return Ok("NaN".into()),
        0xD000 => return Ok("Infinity".into()),
        0xF000 => return Ok("-Infinity".into()),
        _ => {}
    }
    let digits: Vec<i16> = (0..ndigits).map(|i| be16(8 + 2 * i)).collect::<Result<_, _>>()?;
    let digit = |k: i32| -> i16 {
        if k >= 0 && (k as usize) < digits.len() { digits[k as usize] } else { 0 }
    };
    let mut s = String::new();
    if sign == 0x4000 {
        s.push('-');
    }
    if weight < 0 {
        s.push('0');
    } else {
        for k in 0..=weight {
            if k == 0 {
                write!(s, "{}", digit(k))?;
            } else {
                write!(s, "{:04}", digit(k))?;
            }
        }
    }
    if dscale > 0 {
        let mut frac = String::new();
        let mut k = weight + 1;
        while frac.len() < dscale {
            write!(frac, "{:04}", digit(k))?;
            k += 1;
        }
        frac.truncate(dscale);
        s.push('.');
        s.push_str(&frac);
    }
    Ok(s)
}

fn ts(v: chrono::NaiveDateTime) -> String {
    v.format("%Y-%m-%d %H:%M:%S%.f").to_string()
}

macro_rules! get {
    ($row:expr, $i:expr, $t:ty, $f:expr) => {{
        let v: Option<$t> = $row.try_get($i)?;
        match v {
            None => Value::Null,
            Some(v) => $f(v),
        }
    }};
}

macro_rules! get_array {
    ($row:expr, $i:expr, $t:ty, $f:expr) => {{
        let v: Option<Vec<Option<$t>>> = $row.try_get($i)?;
        match v {
            None => Value::Null,
            Some(items) => Value::Array(
                items
                    .into_iter()
                    .map(|x| match x {
                        None => Value::Null,
                        Some(x) => $f(x),
                    })
                    .collect(),
            ),
        }
    }};
}

fn scalar(row: &Row, i: usize, ty: &Type) -> Result<Value, BoxError> {
    Ok(match *ty {
        Type::BOOL => get!(row, i, bool, Value::Bool),
        Type::INT2 => get!(row, i, i16, |v| json!(v)),
        Type::INT4 => get!(row, i, i32, |v| json!(v)),
        Type::OID => get!(row, i, u32, |v| json!(v)),
        Type::INT8 => get!(row, i, i64, |v: i64| Value::String(v.to_string())),
        Type::FLOAT4 => get!(row, i, f32, |v| json!(v)),
        Type::FLOAT8 => get!(row, i, f64, |v| json!(v)),
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::UNKNOWN => {
            get!(row, i, String, Value::String)
        }
        Type::UUID => get!(row, i, uuid::Uuid, |v: uuid::Uuid| Value::String(v.to_string())),
        Type::JSON | Type::JSONB => get!(row, i, Value, |v| v),
        Type::TIMESTAMP => get!(row, i, chrono::NaiveDateTime, |v| Value::String(ts(v))),
        Type::TIMESTAMPTZ => get!(row, i, chrono::DateTime<chrono::Utc>, |v: chrono::DateTime<chrono::Utc>| {
            Value::String(format!("{}+00", ts(v.naive_utc())))
        }),
        Type::DATE => get!(row, i, chrono::NaiveDate, |v: chrono::NaiveDate| Value::String(v.to_string())),
        Type::TIME => get!(row, i, chrono::NaiveTime, |v: chrono::NaiveTime| Value::String(v.format("%H:%M:%S%.f").to_string())),
        Type::BYTEA => get!(row, i, Vec<u8>, |v: Vec<u8>| Value::String(format!("\\x{}", hex::encode(v)))),
        Type::NUMERIC => {
            let v: Option<Raw> = row.try_get(i)?;
            match v {
                None => Value::Null,
                Some(Raw(b)) => Value::String(numeric_text(&b)?),
            }
        }
        _ => match ty.kind() {
            // An enum's binary form is its label.
            Kind::Enum(_) => {
                let v: Option<Raw> = row.try_get(i)?;
                match v {
                    None => Value::Null,
                    Some(Raw(b)) => Value::String(String::from_utf8(b)?),
                }
            }
            _ => return Err(format!("column type {} is not supported by this spike", ty.name()).into()),
        },
    })
}

fn column(row: &Row, i: usize) -> Result<Value, BoxError> {
    let ty = row.columns()[i].type_();
    if let Kind::Array(elem) = ty.kind() {
        return Ok(match *elem {
            Type::BOOL => get_array!(row, i, bool, Value::Bool),
            Type::INT2 => get_array!(row, i, i16, |v| json!(v)),
            Type::INT4 => get_array!(row, i, i32, |v| json!(v)),
            Type::INT8 => get_array!(row, i, i64, |v: i64| Value::String(v.to_string())),
            Type::FLOAT4 => get_array!(row, i, f32, |v| json!(v)),
            Type::FLOAT8 => get_array!(row, i, f64, |v| json!(v)),
            Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME => get_array!(row, i, String, Value::String),
            Type::UUID => get_array!(row, i, uuid::Uuid, |v: uuid::Uuid| Value::String(v.to_string())),
            Type::JSON | Type::JSONB => get_array!(row, i, Value, |v| v),
            _ => return Err(format!("array type {} is not supported by this spike", ty.name()).into()),
        });
    }
    scalar(row, i, ty)
}

/// Rows as Drizzle's proxy driver wants them: arrays for `all`, objects for
/// `execute`.
pub fn rows(rows: &[Row], method: &str) -> Result<Vec<Value>, BoxError> {
    rows.iter()
        .map(|row| {
            if method == "all" {
                (0..row.len()).map(|i| column(row, i)).collect::<Result<Vec<_>, _>>().map(Value::Array)
            } else {
                let mut obj = Map::new();
                for (i, c) in row.columns().iter().enumerate() {
                    obj.insert(c.name().to_string(), column(row, i)?);
                }
                Ok(Value::Object(obj))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(ndigits: i16, weight: i16, sign: u16, dscale: u16, digits: &[i16]) -> Vec<u8> {
        let mut v = Vec::new();
        for x in [ndigits, weight, sign as i16, dscale as i16] {
            v.extend_from_slice(&x.to_be_bytes());
        }
        for d in digits {
            v.extend_from_slice(&d.to_be_bytes());
        }
        v
    }

    #[test]
    fn numeric() {
        // 12345.6789 = 1|2345|6789, weight 1
        assert_eq!(numeric_text(&enc(3, 1, 0, 4, &[1, 2345, 6789])).unwrap(), "12345.6789");
        // 0.0001 = weight -1, digit 1
        assert_eq!(numeric_text(&enc(1, -1, 0, 4, &[1])).unwrap(), "0.0001");
        // -0.00000123 weight -2: 0.0000|0123 -> digits [123] at weight -2
        assert_eq!(numeric_text(&enc(1, -2, 0x4000, 8, &[123])).unwrap(), "-0.00000123");
        // 10000 = 1|0000, ndigits 1 weight 1
        assert_eq!(numeric_text(&enc(1, 1, 0, 0, &[1])).unwrap(), "10000");
        assert_eq!(numeric_text(&enc(0, 0, 0, 2, &[])).unwrap(), "0.00");
    }
}
