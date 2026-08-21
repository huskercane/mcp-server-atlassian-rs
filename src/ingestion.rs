//! Canonical NDJSON partitioning, bounded merge, and artifact manifests.

use std::cmp::Ordering as CmpOrdering;
use std::collections::BinaryHeap;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use serde::de::{
    DeserializeOwned, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::fs;
use tokio::io::{AsyncBufReadExt, BufReader as AsyncBufReader};

use crate::constants::data_limits::MAX_STREAM_RECORD_SIZE;
use crate::transport::raw_response::{self, StreamedArtifact};

pub const ARTIFACT_VERSION: u32 = 1;

#[derive(Debug)]
pub enum SplunkJsonRowsItem {
    Fields(Vec<String>),
    Row(Vec<Value>),
}

#[derive(Debug)]
pub struct LokiStreamValue {
    pub labels: serde_json::Map<String, Value>,
    pub timestamp: String,
    pub payload: String,
}

/// Incrementally validate and parse a successful Loki log-query response.
/// Only one stream's bounded label map and one value tuple are resident in the
/// parser; completed tuples cross a bounded channel to the async normalizer.
#[allow(clippy::too_many_lines)]
pub fn stream_loki_response(
    path: PathBuf,
    capacity: usize,
) -> tokio::sync::mpsc::Receiver<Result<LokiStreamValue, String>> {
    let (sender, receiver) = tokio::sync::mpsc::channel(capacity.max(1));
    tokio::task::spawn_blocking(move || {
        struct Root<'a>(&'a tokio::sync::mpsc::Sender<Result<LokiStreamValue, String>>);
        struct Data<'a>(&'a tokio::sync::mpsc::Sender<Result<LokiStreamValue, String>>);
        struct Results<'a>(&'a tokio::sync::mpsc::Sender<Result<LokiStreamValue, String>>);
        struct StreamItem<'a>(&'a tokio::sync::mpsc::Sender<Result<LokiStreamValue, String>>);
        struct Values<'a> {
            sender: &'a tokio::sync::mpsc::Sender<Result<LokiStreamValue, String>>,
            labels: serde_json::Map<String, Value>,
        }

        fn checked_labels<E: serde::de::Error>(
            pairs: Vec<(String, String)>,
        ) -> Result<serde_json::Map<String, Value>, E> {
            let mut labels = serde_json::Map::with_capacity(pairs.len());
            for (name, value) in pairs {
                let valid = name.bytes().enumerate().all(|(index, byte)| {
                    byte == b'_'
                        || byte.is_ascii_alphanumeric() && (index > 0 || !byte.is_ascii_digit())
                });
                if !valid || name.is_empty() || value.contains('\0') {
                    return Err(E::custom("invalid Loki label name or value"));
                }
                if labels.insert(name, Value::String(value)).is_some() {
                    return Err(E::custom("duplicate Loki label"));
                }
            }
            let size = serde_json::to_vec(&labels).map_err(E::custom)?.len();
            if size > MAX_STREAM_RECORD_SIZE {
                return Err(E::custom(
                    "Loki stream labels exceed maximum decoded record size",
                ));
            }
            Ok(labels)
        }

        struct LabelPairs;
        impl<'de> Visitor<'de> for LabelPairs {
            type Value = Vec<(String, String)>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a Loki stream label object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut pairs = Vec::new();
                while let Some(name) = map.next_key::<String>()? {
                    let value = map.next_value::<String>()?;
                    pairs.push((name, value));
                    if serde_json::to_vec(&pairs)
                        .map_err(serde::de::Error::custom)?
                        .len()
                        > MAX_STREAM_RECORD_SIZE
                    {
                        return Err(serde::de::Error::custom(
                            "Loki stream labels exceed maximum decoded record size",
                        ));
                    }
                }
                Ok(pairs)
            }
        }
        struct LabelSeed;
        impl<'de> DeserializeSeed<'de> for LabelSeed {
            type Value = serde_json::Map<String, Value>;
            fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
                checked_labels(d.deserialize_map(LabelPairs)?)
            }
        }

        impl<'de> DeserializeSeed<'de> for Values<'_> {
            type Value = ();
            fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
                struct V<'a> {
                    sender: &'a tokio::sync::mpsc::Sender<Result<LokiStreamValue, String>>,
                    labels: serde_json::Map<String, Value>,
                }
                impl<'de> Visitor<'de> for V<'_> {
                    type Value = ();
                    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        f.write_str("an array of two-string Loki value tuples")
                    }
                    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
                        while let Some(tuple) = seq.next_element::<Vec<Value>>()? {
                            if tuple.len() != 2 {
                                return Err(serde::de::Error::custom(
                                    "Loki value tuple must contain exactly two elements",
                                ));
                            }
                            let timestamp = tuple[0]
                                .as_str()
                                .ok_or_else(|| {
                                    serde::de::Error::custom(
                                        "Loki value timestamp must be a string",
                                    )
                                })?
                                .to_owned();
                            let payload = tuple[1]
                                .as_str()
                                .ok_or_else(|| {
                                    serde::de::Error::custom("Loki value payload must be a string")
                                })?
                                .to_owned();
                            if serde_json::to_vec(&tuple)
                                .map_err(serde::de::Error::custom)?
                                .len()
                                > MAX_STREAM_RECORD_SIZE
                            {
                                return Err(serde::de::Error::custom(
                                    "Loki value tuple exceeds maximum decoded record size",
                                ));
                            }
                            if self
                                .sender
                                .blocking_send(Ok(LokiStreamValue {
                                    labels: self.labels.clone(),
                                    timestamp,
                                    payload,
                                }))
                                .is_err()
                            {
                                return Ok(());
                            }
                        }
                        Ok(())
                    }
                }
                d.deserialize_seq(V {
                    sender: self.sender,
                    labels: self.labels,
                })
            }
        }

        impl<'de> DeserializeSeed<'de> for StreamItem<'_> {
            type Value = ();
            fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
                struct V<'a>(&'a tokio::sync::mpsc::Sender<Result<LokiStreamValue, String>>);
                impl<'de> Visitor<'de> for V<'_> {
                    type Value = ();
                    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        f.write_str("a Loki stream result")
                    }
                    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
                        let mut labels = None;
                        let mut values_seen = false;
                        while let Some(key) = map.next_key::<String>()? {
                            match key.as_str() {
                                "stream" if labels.is_none() && !values_seen => {
                                    labels = Some(map.next_value_seed(LabelSeed)?);
                                }
                                "stream" => {
                                    return Err(serde::de::Error::custom(
                                        "duplicate or out-of-order Loki stream labels",
                                    ));
                                }
                                "values" if !values_seen => {
                                    let current = labels.clone().ok_or_else(|| {
                                        serde::de::Error::custom(
                                            "Loki stream labels must precede values",
                                        )
                                    })?;
                                    values_seen = true;
                                    map.next_value_seed(Values {
                                        sender: self.0,
                                        labels: current,
                                    })?;
                                }
                                "values" => {
                                    return Err(serde::de::Error::custom("duplicate Loki values"));
                                }
                                _ => {
                                    map.next_value::<IgnoredAny>()?;
                                }
                            }
                        }
                        if labels.is_none() {
                            return Err(serde::de::Error::custom("missing Loki stream labels"));
                        }
                        if !values_seen {
                            return Err(serde::de::Error::custom("missing Loki values"));
                        }
                        Ok(())
                    }
                }
                d.deserialize_map(V(self.0))
            }
        }

        impl<'de> DeserializeSeed<'de> for Results<'_> {
            type Value = ();
            fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
                struct V<'a>(&'a tokio::sync::mpsc::Sender<Result<LokiStreamValue, String>>);
                impl<'de> Visitor<'de> for V<'_> {
                    type Value = ();
                    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        f.write_str("a Loki streams result array")
                    }
                    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
                        while seq.next_element_seed(StreamItem(self.0))?.is_some() {}
                        Ok(())
                    }
                }
                d.deserialize_seq(V(self.0))
            }
        }

        impl<'de> DeserializeSeed<'de> for Data<'_> {
            type Value = ();
            fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
                struct V<'a>(&'a tokio::sync::mpsc::Sender<Result<LokiStreamValue, String>>);
                impl<'de> Visitor<'de> for V<'_> {
                    type Value = ();
                    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        f.write_str("Loki streams data")
                    }
                    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
                        let mut result_type = false;
                        let mut result = false;
                        while let Some(key) = map.next_key::<String>()? {
                            match key.as_str() {
                                "resultType" if !result_type && !result => {
                                    let kind = map.next_value::<String>()?;
                                    if kind != "streams" {
                                        return Err(serde::de::Error::custom(format!(
                                            "unsupported Loki resultType `{kind}`; expected `streams`"
                                        )));
                                    }
                                    result_type = true;
                                }
                                "resultType" => {
                                    return Err(serde::de::Error::custom(
                                        "duplicate or out-of-order Loki resultType",
                                    ));
                                }
                                "result" if !result => {
                                    if !result_type {
                                        return Err(serde::de::Error::custom(
                                            "Loki resultType must precede result",
                                        ));
                                    }
                                    result = true;
                                    map.next_value_seed(Results(self.0))?;
                                }
                                "result" => {
                                    return Err(serde::de::Error::custom("duplicate Loki result"));
                                }
                                _ => {
                                    map.next_value::<IgnoredAny>()?;
                                }
                            }
                        }
                        if !result_type {
                            return Err(serde::de::Error::custom("missing Loki resultType"));
                        }
                        if !result {
                            return Err(serde::de::Error::custom("missing Loki result"));
                        }
                        Ok(())
                    }
                }
                d.deserialize_map(V(self.0))
            }
        }

        impl<'de> DeserializeSeed<'de> for Root<'_> {
            type Value = ();
            fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
                struct V<'a>(&'a tokio::sync::mpsc::Sender<Result<LokiStreamValue, String>>);
                impl<'de> Visitor<'de> for V<'_> {
                    type Value = ();
                    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        f.write_str("a successful Loki response")
                    }
                    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
                        let mut success = false;
                        let mut data = false;
                        while let Some(key) = map.next_key::<String>()? {
                            match key.as_str() {
                                "status" if !success && !data => {
                                    if map.next_value::<String>()? != "success" {
                                        return Err(serde::de::Error::custom(
                                            "Loki response status is not success",
                                        ));
                                    }
                                    success = true;
                                }
                                "status" => {
                                    return Err(serde::de::Error::custom(
                                        "duplicate or out-of-order Loki status",
                                    ));
                                }
                                "data" if !data => {
                                    if !success {
                                        return Err(serde::de::Error::custom(
                                            "Loki status must precede data",
                                        ));
                                    }
                                    data = true;
                                    map.next_value_seed(Data(self.0))?;
                                }
                                "data" => {
                                    return Err(serde::de::Error::custom("duplicate Loki data"));
                                }
                                _ => {
                                    map.next_value::<IgnoredAny>()?;
                                }
                            }
                        }
                        if !success {
                            return Err(serde::de::Error::custom("missing successful Loki status"));
                        }
                        if !data {
                            return Err(serde::de::Error::custom("missing Loki data"));
                        }
                        Ok(())
                    }
                }
                d.deserialize_map(V(self.0))
            }
        }

        let result = std::fs::File::open(&path)
            .map(BufReader::new)
            .map_err(|e| e.to_string())
            .and_then(|reader| {
                let mut d = serde_json::Deserializer::from_reader(reader);
                Root(&sender)
                    .deserialize(&mut d)
                    .and_then(|()| d.end())
                    .map_err(|e| e.to_string())
            });
        if let Err(error) = result {
            let _ = sender.blocking_send(Err(error));
        }
    });
    receiver
}

/// Incrementally parse Splunk's `json_rows` object. Only the field declaration
/// is retained; each row crosses a bounded channel independently. Serde's
/// reader parser carries partial JSON tokens and UTF-8 code points across its
/// internal input-buffer boundaries.
#[allow(clippy::too_many_lines)] // The nested serde visitors keep rows streaming instead of deriving an allocating envelope.
pub fn stream_splunk_json_rows(
    path: PathBuf,
    capacity: usize,
) -> tokio::sync::mpsc::Receiver<Result<SplunkJsonRowsItem, String>> {
    let (sender, receiver) = tokio::sync::mpsc::channel(capacity.max(1));
    tokio::task::spawn_blocking(move || {
        struct RowsVisitor {
            sender: tokio::sync::mpsc::Sender<Result<SplunkJsonRowsItem, String>>,
        }
        struct RowsSeed<'a> {
            sender: &'a tokio::sync::mpsc::Sender<Result<SplunkJsonRowsItem, String>>,
        }
        impl<'de> DeserializeSeed<'de> for RowsSeed<'_> {
            type Value = ();
            fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
                struct Sequence<'a> {
                    sender: &'a tokio::sync::mpsc::Sender<Result<SplunkJsonRowsItem, String>>,
                }
                impl<'de> Visitor<'de> for Sequence<'_> {
                    type Value = ();
                    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        f.write_str("a rows array")
                    }
                    fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<(), S::Error> {
                        while let Some(row) = seq.next_element::<Vec<Value>>()? {
                            let size = serde_json::to_vec(&row)
                                .map_err(serde::de::Error::custom)?
                                .len();
                            if size > MAX_STREAM_RECORD_SIZE {
                                return Err(serde::de::Error::custom(
                                    "Splunk row exceeds maximum decoded record size",
                                ));
                            }
                            if self
                                .sender
                                .blocking_send(Ok(SplunkJsonRowsItem::Row(row)))
                                .is_err()
                            {
                                break;
                            }
                        }
                        Ok(())
                    }
                }
                deserializer.deserialize_seq(Sequence {
                    sender: self.sender,
                })
            }
        }
        impl<'de> Visitor<'de> for RowsVisitor {
            type Value = ();
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a Splunk json_rows object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
                let mut fields_seen = false;
                let mut rows_seen = false;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "fields" => {
                            if fields_seen {
                                return Err(serde::de::Error::custom(
                                    "duplicate fields declaration",
                                ));
                            }
                            if rows_seen {
                                return Err(serde::de::Error::custom(
                                    "fields declaration must precede rows",
                                ));
                            }
                            let fields = map.next_value::<Vec<String>>()?;
                            if fields.is_empty() {
                                return Err(serde::de::Error::custom(
                                    "fields declaration is empty",
                                ));
                            }
                            let mut unique = std::collections::HashSet::with_capacity(fields.len());
                            if fields
                                .iter()
                                .any(|field| field.is_empty() || !unique.insert(field.clone()))
                            {
                                return Err(serde::de::Error::custom(
                                    "field names must be non-empty and unique",
                                ));
                            }
                            fields_seen = true;
                            if self
                                .sender
                                .blocking_send(Ok(SplunkJsonRowsItem::Fields(fields)))
                                .is_err()
                            {
                                return Ok(());
                            }
                        }
                        "rows" => {
                            if rows_seen {
                                return Err(serde::de::Error::custom("duplicate rows declaration"));
                            }
                            if !fields_seen {
                                return Err(serde::de::Error::custom(
                                    "missing fields declaration before rows",
                                ));
                            }
                            rows_seen = true;
                            map.next_value_seed(RowsSeed {
                                sender: &self.sender,
                            })?;
                        }
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                if !fields_seen {
                    return Err(serde::de::Error::custom("missing fields declaration"));
                }
                if !rows_seen {
                    return Err(serde::de::Error::custom("missing rows declaration"));
                }
                Ok(())
            }
        }
        let result = std::fs::File::open(&path)
            .map(BufReader::new)
            .map_err(|e| e.to_string())
            .and_then(|reader| {
                let mut deserializer = serde_json::Deserializer::from_reader(reader);
                serde::de::Deserializer::deserialize_map(
                    &mut deserializer,
                    RowsVisitor {
                        sender: sender.clone(),
                    },
                )
                .and_then(|()| deserializer.end())
                .map_err(|e| e.to_string())
            });
        if let Err(error) = result {
            let _ = sender.blocking_send(Err(error));
        }
    });
    receiver
}

/// Parse a top-level JSON array incrementally on a blocking file thread. The
/// bounded channel propagates backpressure, so neither HTTP chunking nor a
/// large array can cause unbounded item retention.
pub fn stream_json_array<T: DeserializeOwned + Send + 'static>(
    path: PathBuf,
    capacity: usize,
) -> tokio::sync::mpsc::Receiver<Result<T, String>> {
    let (sender, receiver) = tokio::sync::mpsc::channel(capacity.max(1));
    tokio::task::spawn_blocking(move || {
        struct ArrayVisitor<T> {
            sender: tokio::sync::mpsc::Sender<Result<T, String>>,
        }
        impl<'de, T: Deserialize<'de> + Send> Visitor<'de> for ArrayVisitor<T> {
            type Value = ();
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON array")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<(), A::Error> {
                while let Some(item) = sequence.next_element::<T>()? {
                    if self.sender.blocking_send(Ok(item)).is_err() {
                        break;
                    }
                }
                Ok(())
            }
        }
        let result = std::fs::File::open(&path)
            .map(BufReader::new)
            .map_err(|error| error.to_string())
            .and_then(|reader| {
                let mut deserializer = serde_json::Deserializer::from_reader(reader);
                serde::de::Deserializer::deserialize_seq(
                    &mut deserializer,
                    ArrayVisitor {
                        sender: sender.clone(),
                    },
                )
                .map_err(|error| error.to_string())
            });
        if let Err(error) = result {
            let _ = sender.blocking_send(Err(error));
        }
    });
    receiver
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecordOrdering {
    Chronological,
    ReverseChronological,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalRecord {
    pub timestamp_ns: Option<i128>,
    pub source: String,
    pub payload: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

impl CanonicalRecord {
    fn stable_key(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(self.payload.as_bytes());
        if let Some(labels) = &self.labels {
            hash.update(labels.to_string().as_bytes());
        }
        format!(
            "{:?}\0{}\0{:x}",
            self.timestamp_ns,
            self.source,
            hash.finalize()
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QueryInterval {
    pub start_ns: i128,
    pub end_ns: i128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartitionChecksum {
    pub index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_path: Option<PathBuf>,
    pub sha256: String,
    pub records: u64,
    pub decoded_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Completeness {
    Complete,
    Partial,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactManifest {
    pub artifact_version: u32,
    pub format: String,
    pub vendor: String,
    pub query_interval: Option<QueryInterval>,
    pub ordering: RecordOrdering,
    pub total_records: u64,
    pub encoded_bytes: u64,
    pub decoded_bytes: u64,
    #[serde(default)]
    pub final_bytes: u64,
    pub final_sha256: String,
    pub partitions: Vec<PartitionChecksum>,
    pub partitions_requested: usize,
    pub partitions_succeeded: usize,
    pub partitions_failed: usize,
    pub deduplication_policy: String,
    pub duplicate_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_limit: Option<u64>,
    #[serde(default)]
    pub limit_reached: bool,
    #[serde(default)]
    pub truncated_records: u64,
    pub skipped_records: u64,
    pub diagnostics: Vec<String>,
    pub completeness: Completeness,
    pub completeness_reason: Option<String>,
}

pub fn half_open_partitions(start_ns: i128, end_ns: i128, requested: usize) -> Vec<QueryInterval> {
    try_half_open_partitions(start_ns, end_ns, requested).unwrap_or_default()
}

pub fn try_half_open_partitions(
    start_ns: i128,
    end_ns: i128,
    requested: usize,
) -> Result<Vec<QueryInterval>, &'static str> {
    if start_ns >= end_ns || requested == 0 {
        return Ok(Vec::new());
    }
    let width = end_ns
        .checked_sub(start_ns)
        .ok_or("time interval width overflow")?;
    let count = i128::try_from(requested).unwrap_or(i128::MAX).min(width);
    let base = width / count;
    let remainder = width % count;
    let mut cursor = start_ns;
    let partitions = (0..count)
        .map(|i| {
            let next = cursor
                .checked_add(base)
                .and_then(|value| value.checked_add(i128::from(i < remainder)))
                .expect("partition cursor is bounded by validated end");
            let interval = QueryInterval {
                start_ns: cursor,
                end_ns: next,
            };
            cursor = next;
            interval
        })
        .collect::<Vec<_>>();
    Ok(partitions)
}

/// Parse an RFC3339 instant without rounding. Chrono rejects fractional
/// precision beyond nanoseconds and dates outside its nanosecond timestamp
/// range.
pub fn parse_rfc3339_ns(value: &str) -> Option<i128> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()?
        .timestamp_nanos_opt()
        .map(i128::from)
}

/// Parse Loki's unambiguous absolute forms: integer epoch nanoseconds or
/// RFC3339. Float epoch seconds are deliberately left on the single path.
pub fn parse_loki_bound_ns(value: &str) -> Option<i128> {
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return value.parse::<u64>().ok().map(i128::from);
    }
    parse_rfc3339_ns(value)
}

/// Parse Splunk absolute epoch seconds or RFC3339 with no precision loss.
pub fn parse_splunk_bound_ns(value: &str) -> Option<i128> {
    if let Some(value) = parse_rfc3339_ns(value) {
        return Some(value);
    }
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || fraction.len() > 9
        || !whole
            .trim_start_matches('-')
            .bytes()
            .all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let seconds = whole.parse::<i128>().ok()?;
    let nanos = if fraction.is_empty() {
        0
    } else {
        fraction
            .parse::<i128>()
            .ok()?
            .checked_mul(10_i128.pow(u32::try_from(9 - fraction.len()).ok()?))?
    };
    seconds
        .checked_mul(1_000_000_000)?
        .checked_add(if value.starts_with('-') {
            -nanos
        } else {
            nanos
        })
}

pub fn splunk_epoch_seconds(ns: i128) -> String {
    let magnitude = ns.unsigned_abs();
    let seconds = magnitude / 1_000_000_000;
    let nanos = magnitude % 1_000_000_000;
    if ns < 0 {
        format!("-{seconds}.{nanos:09}")
    } else {
        format!("{seconds}.{nanos:09}")
    }
}

pub async fn write_partition(
    prefix: &str,
    records: impl IntoIterator<Item = CanonicalRecord>,
    max_bytes: u64,
) -> std::io::Result<StreamedArtifact> {
    let mut writer =
        raw_response::begin_artifact(prefix, "ndjson", "application/x-ndjson", max_bytes).await?;
    for record in records {
        let mut encoded = serde_json::to_vec(&record).map_err(std::io::Error::other)?;
        if encoded.len() > MAX_STREAM_RECORD_SIZE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                "canonical record exceeds maximum decoded record size",
            ));
        }
        encoded.push(b'\n');
        writer.write_chunk(&encoded).await?;
    }
    writer.commit().await
}

#[derive(Debug)]
struct HeapRecord {
    record: CanonicalRecord,
    partition: usize,
    sequence: u64,
    reverse: bool,
}

impl PartialEq for HeapRecord {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == CmpOrdering::Equal
    }
}
impl Eq for HeapRecord {}
impl PartialOrd for HeapRecord {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}
impl Ord for HeapRecord {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        let natural = self
            .record
            .timestamp_ns
            .cmp(&other.record.timestamp_ns)
            .then_with(|| self.partition.cmp(&other.partition))
            .then_with(|| self.sequence.cmp(&other.sequence));
        if self.reverse {
            natural
        } else {
            natural.reverse()
        }
    }
}

async fn read_next(
    reader: &mut AsyncBufReader<fs::File>,
    partition: usize,
    sequence: u64,
    reverse: bool,
) -> std::io::Result<Option<HeapRecord>> {
    let mut line = Vec::new();
    let read = reader.read_until(b'\n', &mut line).await?;
    if read == 0 {
        return Ok(None);
    }
    if line.len() > MAX_STREAM_RECORD_SIZE + 1 || !line.ends_with(b"\n") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::FileTooLarge,
            "partition contains oversized or unterminated NDJSON record",
        ));
    }
    let record = serde_json::from_slice(line.strip_suffix(b"\n").unwrap_or(&line))
        .map_err(std::io::Error::other)?;
    Ok(Some(HeapRecord {
        record,
        partition,
        sequence,
        reverse,
    }))
}

#[derive(Debug)]
pub struct MergeResult {
    pub artifact: StreamedArtifact,
    pub records: u64,
    pub duplicates: u64,
    pub limited: bool,
    pub max_heap_records: usize,
    pub max_dedup_records: usize,
}

#[derive(Debug, Clone)]
pub struct ValidatedPartition {
    pub path: PathBuf,
    pub checksum: PartitionChecksum,
}

/// Validate a committed canonical partition and its sidecar before it can
/// participate in a merge. Validation is streaming and retains no records.
pub async fn validate_partition(
    path: &Path,
    expected_index: usize,
    expected_vendor: &str,
) -> std::io::Result<ValidatedPartition> {
    let manifest_path = path.with_extension("manifest.json");
    let manifest: ArtifactManifest =
        serde_json::from_slice(&fs::read(&manifest_path).await?).map_err(std::io::Error::other)?;
    if manifest.artifact_version != ARTIFACT_VERSION
        || manifest.format != "canonical_ndjson"
        || manifest.vendor != expected_vendor
        || manifest.partitions_requested != 1
        || manifest.partitions_succeeded != 1
        || manifest.partitions_failed != 0
        || manifest.partitions.len() != 1
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "partition manifest does not describe one committed canonical partition",
        ));
    }
    let declared = &manifest.partitions[0];
    if declared.sha256 != manifest.final_sha256
        || declared.records != manifest.total_records
        || manifest.final_bytes != 0 && declared.decoded_bytes != manifest.final_bytes
        || declared.index != 0
        || declared
            .artifact_path
            .as_ref()
            .is_some_and(|declared_path| declared_path != path)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "partition manifest checksum or accounting relationship is invalid",
        ));
    }
    let mut reader = AsyncBufReader::new(fs::File::open(path).await?);
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut records = 0_u64;
    let mut previous_timestamp = None;
    loop {
        let mut line = Vec::new();
        let read = reader.read_until(b'\n', &mut line).await?;
        if read == 0 {
            break;
        }
        if line.len() > MAX_STREAM_RECORD_SIZE + 1 || !line.ends_with(b"\n") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "partition contains oversized or unterminated canonical NDJSON",
            ));
        }
        let record = serde_json::from_slice::<CanonicalRecord>(&line[..line.len() - 1])
            .map_err(std::io::Error::other)?;
        if let Some(previous) = previous_timestamp {
            let ordered = match manifest.ordering {
                RecordOrdering::Chronological => previous <= record.timestamp_ns,
                RecordOrdering::ReverseChronological => previous >= record.timestamp_ns,
            };
            if !ordered {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "partition canonical records violate declared ordering",
                ));
            }
        }
        previous_timestamp = Some(record.timestamp_ns);
        hasher.update(&line);
        bytes = bytes
            .checked_add(u64::try_from(line.len()).map_err(std::io::Error::other)?)
            .ok_or_else(|| std::io::Error::other("partition byte counter overflow"))?;
        records = records
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("partition record counter overflow"))?;
    }
    let sha256 = format!("{:x}", hasher.finalize());
    if bytes != declared.decoded_bytes || records != declared.records || sha256 != declared.sha256 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "partition changed after its manifest was committed",
        ));
    }
    Ok(ValidatedPartition {
        path: path.to_path_buf(),
        checksum: PartitionChecksum {
            index: expected_index,
            artifact_path: None,
            sha256,
            records,
            decoded_bytes: bytes,
        },
    })
}

pub async fn merge_partitions(
    paths: &[PathBuf],
    ordering: RecordOrdering,
    result_limit: Option<u64>,
    max_bytes: u64,
) -> std::io::Result<MergeResult> {
    merge_partitions_cancellable(
        paths,
        ordering,
        result_limit,
        max_bytes,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
}

pub async fn merge_partitions_cancellable(
    paths: &[PathBuf],
    ordering: RecordOrdering,
    result_limit: Option<u64>,
    max_bytes: u64,
    cancellation: &tokio_util::sync::CancellationToken,
) -> std::io::Result<MergeResult> {
    merge_partitions_cancellable_reserved(
        paths,
        ordering,
        result_limit,
        max_bytes,
        cancellation,
        None,
    )
    .await
}

pub async fn merge_partitions_cancellable_reserved(
    paths: &[PathBuf],
    ordering: RecordOrdering,
    result_limit: Option<u64>,
    max_bytes: u64,
    cancellation: &tokio_util::sync::CancellationToken,
    disk: Option<std::sync::Arc<crate::transport::StreamingDiskQuota>>,
) -> std::io::Result<MergeResult> {
    let reverse = ordering == RecordOrdering::ReverseChronological;
    let mut readers = Vec::with_capacity(paths.len());
    for path in paths {
        if cancellation.is_cancelled() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "canonical partition merge cancelled",
            ));
        }
        readers.push(AsyncBufReader::new(fs::File::open(path).await?));
    }
    let mut heap = BinaryHeap::with_capacity(readers.len());
    for (index, reader) in readers.iter_mut().enumerate() {
        if let Some(item) = read_next(reader, index, 0, reverse).await? {
            heap.push(item);
        }
    }
    let mut writer = raw_response::begin_artifact(
        "canonical-logs",
        "ndjson",
        "application/x-ndjson",
        max_bytes,
    )
    .await?;
    if let Some(disk) = disk {
        writer.set_disk_quota(disk);
    }
    let mut records = 0_u64;
    let mut duplicates = 0_u64;
    let mut last_key = None;
    let mut last_partition = None;
    let mut limited = false;
    let mut max_heap_records = heap.len();
    while let Some(item) = heap.pop() {
        if cancellation.is_cancelled() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "canonical partition merge cancelled",
            ));
        }
        if result_limit.is_some_and(|limit| records >= limit) {
            limited = true;
            break;
        }
        let next_sequence = item.sequence + 1;
        let key = item.record.stable_key();
        if last_key.as_deref() == Some(key.as_str()) && last_partition != Some(item.partition) {
            duplicates += 1;
            // Move the bounded boundary marker to this partition so repeated
            // records inside it remain distinct.
            last_partition = Some(item.partition);
        } else {
            let mut line = serde_json::to_vec(&item.record).map_err(std::io::Error::other)?;
            if line.len() > MAX_STREAM_RECORD_SIZE {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::FileTooLarge,
                    "canonical record exceeds maximum decoded record size",
                ));
            }
            line.push(b'\n');
            writer.write_chunk(&line).await?;
            records += 1;
            last_key = Some(key);
            last_partition = Some(item.partition);
        }
        if let Some(next) = read_next(
            &mut readers[item.partition],
            item.partition,
            next_sequence,
            reverse,
        )
        .await?
        {
            heap.push(next);
            max_heap_records = max_heap_records.max(heap.len());
        }
    }
    if cancellation.is_cancelled() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "canonical partition merge cancelled",
        ));
    }
    let artifact = writer.commit().await?;
    Ok(MergeResult {
        artifact,
        records,
        duplicates,
        limited,
        max_heap_records,
        max_dedup_records: usize::from(last_key.is_some()),
    })
}

pub async fn persist_manifest(
    artifact_path: &Path,
    manifest: &ArtifactManifest,
) -> std::io::Result<PathBuf> {
    persist_manifest_impl(artifact_path, manifest, None).await
}

pub async fn persist_manifest_reserved(
    artifact_path: &Path,
    manifest: &ArtifactManifest,
    disk: std::sync::Arc<crate::transport::StreamingDiskQuota>,
) -> std::io::Result<PathBuf> {
    persist_manifest_reserved_impl(artifact_path, manifest, disk, None).await
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ManifestFault {
    Write,
    Sync,
    Rename,
}

async fn persist_manifest_impl(
    artifact_path: &Path,
    manifest: &ArtifactManifest,
    #[cfg_attr(not(test), allow(unused_variables))] fault: Option<ManifestFault>,
) -> std::io::Result<PathBuf> {
    let path = artifact_path.with_extension("manifest.json");
    let part = artifact_path.with_extension("manifest.json.part");
    let result = async {
        let bytes = serde_json::to_vec_pretty(manifest).map_err(std::io::Error::other)?;
        #[cfg(test)]
        if fault == Some(ManifestFault::Write) {
            return Err(std::io::Error::other("injected manifest write failure"));
        }
        fs::write(&part, bytes).await?;
        let file = fs::OpenOptions::new().write(true).open(&part).await?;
        #[cfg(test)]
        if fault == Some(ManifestFault::Sync) {
            return Err(std::io::Error::other("injected manifest sync failure"));
        }
        file.sync_all().await?;
        drop(file);
        #[cfg(test)]
        if fault == Some(ManifestFault::Rename) {
            return Err(std::io::Error::other("injected manifest rename failure"));
        }
        fs::rename(&part, &path).await?;
        Ok(path)
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(&part).await;
    }
    result
}

async fn persist_manifest_reserved_impl(
    artifact_path: &Path,
    manifest: &ArtifactManifest,
    disk: std::sync::Arc<crate::transport::StreamingDiskQuota>,
    #[cfg_attr(not(test), allow(unused_variables))] fault: Option<ManifestFault>,
) -> std::io::Result<PathBuf> {
    let path = artifact_path.with_extension("manifest.json");
    let part = artifact_path.with_extension("manifest.json.part");
    let bytes = serde_json::to_vec_pretty(manifest).map_err(std::io::Error::other)?;
    let byte_count = u64::try_from(bytes.len()).map_err(std::io::Error::other)?;
    disk.reserve(byte_count)?;
    let result = async {
        #[cfg(test)]
        if fault == Some(ManifestFault::Write) {
            return Err(std::io::Error::other("injected manifest write failure"));
        }
        fs::write(&part, bytes).await?;
        let file = fs::OpenOptions::new().write(true).open(&part).await?;
        #[cfg(test)]
        if fault == Some(ManifestFault::Sync) {
            return Err(std::io::Error::other("injected manifest sync failure"));
        }
        file.sync_all().await?;
        drop(file);
        #[cfg(test)]
        if fault == Some(ManifestFault::Rename) {
            return Err(std::io::Error::other("injected manifest rename failure"));
        }
        fs::rename(&part, &path).await?;
        raw_response::attach_sidecar_reservation(artifact_path, &disk, byte_count)?;
        Ok(path)
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(&part).await;
        let _ = disk.release(byte_count);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn parse_loki(json: &str) -> Vec<Result<LokiStreamValue, String>> {
        let path = std::env::temp_dir().join(format!("loki-parser-{}.json", uuid::Uuid::new_v4()));
        tokio::fs::write(&path, json).await.unwrap();
        let mut receiver = stream_loki_response(path.clone(), 1);
        let mut items = Vec::new();
        while let Some(item) = receiver.recv().await {
            items.push(item);
        }
        let _ = tokio::fs::remove_file(path).await;
        items
    }

    fn record(timestamp_ns: i128, payload: &str) -> CanonicalRecord {
        CanonicalRecord {
            timestamp_ns: Some(timestamp_ns),
            source: "test".into(),
            payload: payload.into(),
            labels: None,
            metadata: None,
        }
    }

    fn manifest_for(artifact: &StreamedArtifact, vendor: &str, records: u64) -> ArtifactManifest {
        ArtifactManifest {
            artifact_version: ARTIFACT_VERSION,
            format: "canonical_ndjson".into(),
            vendor: vendor.into(),
            query_interval: None,
            ordering: RecordOrdering::Chronological,
            total_records: records,
            encoded_bytes: artifact.artifact.size,
            decoded_bytes: artifact.artifact.size,
            final_bytes: artifact.artifact.size,
            final_sha256: artifact.sha256.clone(),
            partitions: vec![PartitionChecksum {
                index: 0,
                artifact_path: None,
                sha256: artifact.sha256.clone(),
                records,
                decoded_bytes: artifact.artifact.size,
            }],
            partitions_requested: 1,
            partitions_succeeded: 1,
            partitions_failed: 0,
            deduplication_policy: "none_test_partition".into(),
            duplicate_count: 0,
            global_limit: None,
            limit_reached: false,
            truncated_records: 0,
            skipped_records: 0,
            diagnostics: Vec::new(),
            completeness: Completeness::Complete,
            completeness_reason: None,
        }
    }

    #[test]
    fn intervals_are_contiguous_half_open() {
        assert_eq!(
            half_open_partitions(10, 20, 3)
                .iter()
                .map(|p| (p.start_ns, p.end_ns))
                .collect::<Vec<_>>(),
            vec![(10, 14), (14, 17), (17, 20)]
        );
    }

    #[test]
    fn interval_overflow_and_nanosecond_bounds_are_explicit() {
        assert_eq!(
            try_half_open_partitions(i128::MIN, i128::MAX, 2),
            Err("time interval width overflow")
        );
        assert_eq!(
            parse_loki_bound_ns("1712345678123456789"),
            Some(1_712_345_678_123_456_789)
        );
        assert_eq!(parse_loki_bound_ns("1712345678.1"), None);
        assert_eq!(
            parse_splunk_bound_ns("1712345678.123456789"),
            Some(1_712_345_678_123_456_789)
        );
        assert_eq!(parse_splunk_bound_ns("1712345678.1234567890"), None);
        assert_eq!(splunk_epoch_seconds(-1), "-0.000000001");
    }

    #[tokio::test]
    async fn loki_parser_streams_values_and_split_utf8_safely() {
        let padding = "x".repeat(8_190);
        let json = format!(
            "{{\"status\":\"success\",\"ignored\":\"{padding}\",\"data\":{{\"resultType\":\"streams\",\"result\":[{{\"stream\":{{\"app\":\"café\"}},\"values\":[[\"1\",\"💥 first\"],[\"2\",\"second\"]]}}]}}}}"
        );
        let items = parse_loki(&json).await;
        assert_eq!(items.len(), 2);
        let first = items[0].as_ref().unwrap();
        assert_eq!(first.labels["app"], "café");
        assert_eq!(first.payload, "💥 first");
        assert_eq!(items[1].as_ref().unwrap().timestamp, "2");
    }

    #[tokio::test]
    async fn loki_parser_rejects_ambiguous_shapes_labels_and_tuples() {
        let invalid = [
            r#"{"status":"success","data":{"result":[]}}"#,
            r#"{"status":"success","data":{"resultType":"matrix","result":[]}}"#,
            r#"{"status":"success","data":{"resultType":"streams","result":[{"stream":{"bad-name":"x"},"values":[]}]}}"#,
            r#"{"status":"success","data":{"resultType":"streams","result":[{"stream":{"app":"x","app":"y"},"values":[]}]}}"#,
            r#"{"status":"success","data":{"resultType":"streams","result":[{"values":[],"stream":{}}]}}"#,
            r#"{"status":"success","data":{"resultType":"streams","result":[{"stream":{},"values":[["1"],["1","x","extra"]]}]}}"#,
            r#"{"status":"success","data":{"resultType":"streams","result":[]}} trailing"#,
        ];
        for json in invalid {
            let items = parse_loki(json).await;
            assert!(items.iter().any(Result::is_err), "accepted {json}");
        }
    }

    #[tokio::test]
    async fn merge_orders_and_deduplicates_boundary_records() {
        let first = write_partition("merge-test-a", [record(1, "a"), record(3, "same")], 4096)
            .await
            .unwrap();
        let second = write_partition("merge-test-b", [record(3, "same"), record(4, "d")], 4096)
            .await
            .unwrap();
        let result = merge_partitions(
            &[first.artifact.path, second.artifact.path],
            RecordOrdering::Chronological,
            None,
            4096,
        )
        .await
        .unwrap();
        assert_eq!(result.records, 3);
        assert_eq!(result.duplicates, 1);
        let text = fs::read_to_string(result.artifact.artifact.path)
            .await
            .unwrap();
        let timestamps = text
            .lines()
            .map(|line| {
                serde_json::from_str::<CanonicalRecord>(line)
                    .unwrap()
                    .timestamp_ns
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(timestamps, vec![1, 3, 4]);
    }

    #[tokio::test]
    async fn reverse_merge_and_global_limit_are_bounded() {
        let first = write_partition("reverse-test-a", [record(5, "e"), record(2, "b")], 4096)
            .await
            .unwrap();
        let second = write_partition("reverse-test-b", [record(4, "d"), record(1, "a")], 4096)
            .await
            .unwrap();
        let result = merge_partitions(
            &[first.artifact.path, second.artifact.path],
            RecordOrdering::ReverseChronological,
            Some(2),
            4096,
        )
        .await
        .unwrap();
        assert_eq!(result.records, 2);
        assert!(result.limited);
        assert!(result.max_heap_records <= 2);
        assert_eq!(result.max_dedup_records, 1);
        let text = fs::read_to_string(result.artifact.artifact.path)
            .await
            .unwrap();
        let timestamps = text
            .lines()
            .map(|line| {
                serde_json::from_str::<CanonicalRecord>(line)
                    .unwrap()
                    .timestamp_ns
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(timestamps, vec![5, 4]);
    }

    #[tokio::test]
    async fn preserves_intra_partition_duplicates_and_same_timestamp_distinct_records() {
        let first = write_partition(
            "merge-distinct-a",
            [record(1, "same"), record(1, "same"), record(2, "left")],
            4096,
        )
        .await
        .unwrap();
        let second = write_partition(
            "merge-distinct-b",
            [record(2, "right"), record(3, "end")],
            4096,
        )
        .await
        .unwrap();
        let result = merge_partitions(
            &[first.artifact.path, second.artifact.path],
            RecordOrdering::Chronological,
            None,
            4096,
        )
        .await
        .unwrap();
        assert_eq!(result.records, 5);
        assert_eq!(result.duplicates, 0);
    }

    #[tokio::test]
    async fn cancelled_merge_commits_no_final_artifact() {
        let part = write_partition("merge-cancel", [record(1, "a")], 4096)
            .await
            .unwrap();
        let cancellation = tokio_util::sync::CancellationToken::new();
        cancellation.cancel();
        let error = merge_partitions_cancellable(
            &[part.artifact.path],
            RecordOrdering::Chronological,
            None,
            4096,
            &cancellation,
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    }

    #[tokio::test]
    async fn validates_empty_parts_and_rejects_missing_changed_or_malformed_parts() {
        let empty = write_partition("validate-empty", [], 4096).await.unwrap();
        persist_manifest(&empty.artifact.path, &manifest_for(&empty, "splunk", 0))
            .await
            .unwrap();
        assert_eq!(
            validate_partition(&empty.artifact.path, 0, "splunk")
                .await
                .unwrap()
                .checksum
                .records,
            0
        );

        let changed = write_partition("validate-changed", [record(1, "a")], 4096)
            .await
            .unwrap();
        persist_manifest(&changed.artifact.path, &manifest_for(&changed, "splunk", 1))
            .await
            .unwrap();
        fs::write(&changed.artifact.path, b"{}\n").await.unwrap();
        assert!(
            validate_partition(&changed.artifact.path, 0, "splunk")
                .await
                .is_err()
        );

        let missing = changed
            .artifact
            .path
            .with_file_name("missing-canonical-part.ndjson");
        assert!(validate_partition(&missing, 0, "splunk").await.is_err());

        let malformed = write_partition("validate-malformed", [record(1, "a")], 4096)
            .await
            .unwrap();
        let malformed_manifest = manifest_for(&malformed, "splunk", 1);
        persist_manifest(&malformed.artifact.path, &malformed_manifest)
            .await
            .unwrap();
        fs::write(&malformed.artifact.path, b"not-json\n")
            .await
            .unwrap();
        assert!(
            validate_partition(&malformed.artifact.path, 0, "splunk")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn manifest_commit_failure_removes_partial_sidecar() {
        let artifact = write_partition("manifest-failure", [record(1, "a")], 4096)
            .await
            .unwrap();
        let final_path = artifact.artifact.path.with_extension("manifest.json");
        fs::create_dir(&final_path).await.unwrap();
        assert!(
            persist_manifest(
                &artifact.artifact.path,
                &manifest_for(&artifact, "splunk", 1)
            )
            .await
            .is_err()
        );
        assert!(
            !artifact
                .artifact
                .path
                .with_extension("manifest.json.part")
                .exists()
        );
        fs::remove_dir(final_path).await.unwrap();
    }

    #[tokio::test]
    async fn injected_manifest_failures_leave_no_sidecar_or_partial() {
        let artifact = write_partition("manifest-injected", [record(1, "a")], 4096)
            .await
            .unwrap();
        let manifest = manifest_for(&artifact, "splunk", 1);
        for fault in [
            ManifestFault::Write,
            ManifestFault::Sync,
            ManifestFault::Rename,
        ] {
            assert!(
                persist_manifest_impl(&artifact.artifact.path, &manifest, Some(fault))
                    .await
                    .is_err()
            );
            assert!(
                !artifact
                    .artifact
                    .path
                    .with_extension("manifest.json")
                    .exists()
            );
            assert!(
                !artifact
                    .artifact
                    .path
                    .with_extension("manifest.json.part")
                    .exists()
            );
        }
        let _ = raw_response::remove_artifact(&artifact.artifact.path).await;
    }

    #[tokio::test]
    async fn reserved_manifest_transitions_roll_back_or_retain_only_live_files() {
        for fault in [
            ManifestFault::Write,
            ManifestFault::Sync,
            ManifestFault::Rename,
        ] {
            let quota = std::sync::Arc::new(crate::transport::StreamingDiskQuota::new(4096));
            let mut writer = raw_response::begin_artifact(
                "reserved-manifest-fault",
                "ndjson",
                "application/x-ndjson",
                4096,
            )
            .await
            .unwrap();
            writer.set_disk_quota(quota.clone());
            writer.write_chunk(b"\n").await.unwrap();
            let artifact = writer.commit().await.unwrap();
            let manifest = manifest_for(&artifact, "splunk", 0);
            assert!(
                persist_manifest_reserved_impl(
                    &artifact.artifact.path,
                    &manifest,
                    quota.clone(),
                    Some(fault),
                )
                .await
                .is_err()
            );
            assert_eq!(quota.reserved_bytes(), artifact.artifact.size);
            assert!(
                !artifact
                    .artifact
                    .path
                    .with_extension("manifest.json.part")
                    .exists()
            );
            raw_response::remove_artifact(&artifact.artifact.path)
                .await
                .unwrap();
            assert_eq!(quota.reserved_bytes(), 0);
        }

        let quota = std::sync::Arc::new(crate::transport::StreamingDiskQuota::new(4096));
        let mut writer = raw_response::begin_artifact(
            "reserved-manifest-success",
            "ndjson",
            "application/x-ndjson",
            4096,
        )
        .await
        .unwrap();
        writer.set_disk_quota(quota.clone());
        writer.write_chunk(b"\n").await.unwrap();
        let artifact = writer.commit().await.unwrap();
        let manifest = manifest_for(&artifact, "splunk", 0);
        let manifest_path =
            persist_manifest_reserved(&artifact.artifact.path, &manifest, quota.clone())
                .await
                .unwrap();
        let intended = artifact.artifact.size + fs::metadata(&manifest_path).await.unwrap().len();
        assert_eq!(quota.reserved_bytes(), intended);
        raw_response::remove_artifact(&artifact.artifact.path)
            .await
            .unwrap();
        assert_eq!(quota.reserved_bytes(), 0);
        assert!(!manifest_path.exists());
    }

    #[tokio::test]
    async fn oversized_record_fails_without_committing() {
        let huge = record(1, &"x".repeat(MAX_STREAM_RECORD_SIZE));
        let error = write_partition("oversized-test", [huge], u64::MAX)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::FileTooLarge);
    }

    #[tokio::test]
    async fn splunk_parser_handles_tokens_and_multibyte_utf8_across_reader_buffers() {
        let padding = "x".repeat(8_190);
        let body = format!(
            r#"{{"fields":["_raw","host"],"rows":[["{padding}é-tail","api-1"],["second","api-2"]]}}"#
        );
        let mut input = raw_response::begin_artifact(
            "splunk-boundary-input",
            "json",
            "application/json",
            u64::MAX,
        )
        .await
        .unwrap();
        for chunk in body.as_bytes().chunks(3) {
            input.write_chunk(chunk).await.unwrap();
        }
        let input = input.commit().await.unwrap();
        let mut items = stream_splunk_json_rows(input.artifact.path.clone(), 1);
        match items.recv().await.unwrap().unwrap() {
            SplunkJsonRowsItem::Fields(fields) => assert_eq!(fields, ["_raw", "host"]),
            SplunkJsonRowsItem::Row(_) => panic!("fields must be emitted first"),
        }
        match items.recv().await.unwrap().unwrap() {
            SplunkJsonRowsItem::Row(row) => {
                assert_eq!(row[0].as_str().unwrap(), format!("{padding}é-tail"));
            }
            SplunkJsonRowsItem::Fields(_) => panic!("duplicate fields item"),
        }
        assert!(matches!(
            items.recv().await.unwrap().unwrap(),
            SplunkJsonRowsItem::Row(_)
        ));
        assert!(items.recv().await.is_none());
        let _ = fs::remove_file(input.artifact.path).await;
    }
}
