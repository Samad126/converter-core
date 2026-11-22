//! Apache Avro container files → JSON values (an array of the records).
use super::bad;
use crate::Result;

pub fn read(bytes: &[u8]) -> Result<serde_json::Value> {
    let reader = apache_avro::Reader::new(bytes).map_err(|e| bad(format!("not a readable Avro file: {e}")))?;
    let mut out = vec![];
    for rec in reader {
        let v = rec.map_err(|e| bad(format!("damaged Avro record: {e}")))?;
        out.push(serde_json::Value::try_from(v).map_err(|e| bad(format!("cannot represent an Avro value as JSON: {e}")))?);
        if out.len() > 5_000_000 { return Err(crate::Error::new(crate::ErrorCode::ETooLarge, "The Avro file has too many records.")); }
    }
    Ok(serde_json::Value::Array(out))
}
