//! Pure Rust asset compression shared by native and WASM packaging hosts.
//! No filesystem, environment, clock, thread, network or JavaScript operations.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};

pub const MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_BATCH_INPUT_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_BATCH_WORK_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 96 * 1024 * 1024;
pub const MAX_JSON_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_JOBS: usize = 1024;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
}
impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
fn error(code: &'static str, message: impl Into<String>) -> Error {
    Error {
        code,
        message: message.into(),
    }
}

fn default_level() -> u8 {
    9
}
fn default_quality() -> u8 {
    11
}
fn default_window() -> u8 {
    22
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BrotliMode {
    Generic,
    #[default]
    Text,
    Font,
}

/// Adapter-node's defaults are Gzip level 9, or Brotli Text/11/window22 with
/// actual input size. Explicit generic/font modes and lower qualities are also
/// supported. No large-window extension or native platform encoder is enabled.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "format", rename_all = "camelCase", deny_unknown_fields)]
pub enum Compression {
    Gzip {
        #[serde(default = "default_level")]
        level: u8,
    },
    Brotli {
        #[serde(default = "default_quality")]
        quality: u8,
        #[serde(default)]
        mode: BrotliMode,
        /// 10..24, default upstream Brotli window 22.
        #[serde(default = "default_window")]
        lgwin: u8,
        /// When supplied, must match the actual source byte count. Accepts the
        /// Kit plan's legacy field spelling as well as camelCase JSON.
        #[serde(default, rename = "sizeHint", alias = "size_hint")]
        size_hint: Option<usize>,
    },
}
impl Compression {
    #[must_use]
    pub fn adapter_gzip() -> Self {
        Self::Gzip { level: 9 }
    }
    #[must_use]
    pub fn adapter_brotli() -> Self {
        Self::Brotli {
            quality: 11,
            mode: BrotliMode::Text,
            lgwin: 22,
            size_hint: None,
        }
    }
    fn resolved(&self, input_len: usize) -> Result<Self, Error> {
        match self {
            Self::Gzip { level } if *level <= 9 => Ok(self.clone()),
            Self::Gzip { .. } => Err(error(
                "invalid_options",
                "Gzip level must be between 0 and 9",
            )),
            Self::Brotli {
                quality,
                lgwin,
                size_hint,
                mode,
            } => {
                if *quality > 11 || !(10..=24).contains(lgwin) {
                    return Err(error(
                        "invalid_options",
                        "Brotli quality must be 0..11 and window 10..24",
                    ));
                }
                if size_hint.is_some_and(|hint| hint != input_len) {
                    return Err(error(
                        "stale_source",
                        "Brotli size hint differs from actual source bytes",
                    ));
                }
                Ok(Self::Brotli {
                    quality: *quality,
                    lgwin: *lgwin,
                    mode: *mode,
                    size_hint: Some(input_len),
                })
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Job {
    /// Opaque source/output identities: the codec never interprets filesystem paths.
    pub source: String,
    pub destination: String,
    pub source_sha256: String,
    pub compression: Compression,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Batch {
    pub sources: BTreeMap<String, Vec<u8>>,
    pub jobs: Vec<Job>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Output {
    pub source_sha256: String,
    pub output_sha256: String,
    pub source_bytes: usize,
    /// The fully resolved codec parameters, including actual Brotli size hint.
    pub compression: Compression,
    pub bytes: Vec<u8>,
}

#[must_use]
pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

struct BoundedWriter {
    bytes: Vec<u8>,
    limit: usize,
    exhausted: bool,
}
impl BoundedWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            exhausted: false,
        }
    }
}
impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.exhausted = true;
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "Compressed output limit exceeded",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Compress one source. Encoding is deterministic for the pinned Rust codecs;
/// DEFLATE/Brotli bitstreams need not equal another encoder's output byte-for-byte.
pub fn compress(input: &[u8], compression: &Compression) -> Result<Output, Error> {
    compress_with_limit(input, compression, MAX_OUTPUT_BYTES)
}

/// Smaller budgets may be supplied by batch/worker hosts. Input is fully bounded
/// before invoking a codec, and a bounded writer enforces the output cap while
/// encoding, rather than after allocating a potentially unbounded Vec.
pub fn compress_with_limit(
    input: &[u8],
    compression: &Compression,
    max_output_bytes: usize,
) -> Result<Output, Error> {
    if input.len() > MAX_SOURCE_BYTES || max_output_bytes > MAX_OUTPUT_BYTES {
        return Err(error(
            "size_limit",
            "Compression request exceeds native limits",
        ));
    }
    let compression = compression.resolved(input.len())?;
    let mut output = BoundedWriter::new(max_output_bytes);
    let result = match &compression {
        Compression::Gzip { level } => {
            // Node/zlib on Unix emits MTIME=0, OS=3, no optional fields. Selecting
            // these explicitly keeps output headers identical across native/WASM.
            let mut encoder = flate2::GzBuilder::new()
                .mtime(0)
                .operating_system(3)
                .write(&mut output, flate2::Compression::new(u32::from(*level)));
            encoder
                .write_all(input)
                .and_then(|()| encoder.finish().map(|_| ()))
        }
        Compression::Brotli {
            quality,
            mode,
            lgwin,
            size_hint,
        } => {
            let params = brotli::enc::BrotliEncoderParams {
                quality: i32::from(*quality),
                lgwin: i32::from(*lgwin),
                mode: match mode {
                    BrotliMode::Generic => {
                        brotli::enc::backward_references::BrotliEncoderMode::BROTLI_MODE_GENERIC
                    }
                    BrotliMode::Text => {
                        brotli::enc::backward_references::BrotliEncoderMode::BROTLI_MODE_TEXT
                    }
                    BrotliMode::Font => {
                        brotli::enc::backward_references::BrotliEncoderMode::BROTLI_MODE_FONT
                    }
                },
                size_hint: size_hint.unwrap_or(input.len()),
                // Preserve reference defaults: automatic lgblock=0, normal
                // window, static dictionary and literal context modelling.
                ..Default::default()
            };
            brotli::BrotliCompress(&mut &input[..], &mut output, &params).map(|_| ())
        }
    };
    if let Err(failure) = result {
        return Err(error(
            if output.exhausted {
                "size_limit"
            } else {
                "codec_error"
            },
            failure.to_string(),
        ));
    }
    Ok(Output {
        source_sha256: sha256(input),
        output_sha256: sha256(&output.bytes),
        source_bytes: input.len(),
        compression,
        bytes: output.bytes,
    })
}

fn identity(value: &str) -> Result<(), Error> {
    if value.is_empty() || value.len() > 4096 || value.contains('\0') {
        Err(error(
            "invalid_request",
            "Source and destination identities must contain 1..4096 bytes without NUL",
        ))
    } else {
        Ok(())
    }
}

/// Preflight every job before starting work. No partial result is exposed if any
/// input, identity, codec option or aggregate output fails. Jobs run sequentially
/// and share immutable source bytes, avoiding a codec allocation per batch item.
pub fn compress_batch(batch: &Batch) -> Result<BTreeMap<String, Output>, Error> {
    if batch.jobs.len() > MAX_JOBS || batch.sources.len() > MAX_JOBS {
        return Err(error("size_limit", "Too many compression jobs or sources"));
    }
    let total_input = batch
        .sources
        .values()
        .try_fold(0usize, |total, bytes| total.checked_add(bytes.len()))
        .ok_or_else(|| error("size_limit", "Source size overflow"))?;
    if total_input > MAX_BATCH_INPUT_BYTES
        || batch
            .sources
            .values()
            .any(|bytes| bytes.len() > MAX_SOURCE_BYTES)
    {
        return Err(error("size_limit", "Batch source limit exceeded"));
    }
    let mut hashes = BTreeMap::new();
    for (name, bytes) in &batch.sources {
        identity(name)?;
        hashes.insert(name.as_str(), sha256(bytes));
    }
    let mut destinations = BTreeSet::new();
    let mut work = 0usize;
    for job in &batch.jobs {
        identity(&job.source)?;
        identity(&job.destination)?;
        if !destinations.insert(&job.destination) {
            return Err(error(
                "duplicate_output",
                "Compression destinations must be unique",
            ));
        }
        let bytes = batch
            .sources
            .get(&job.source)
            .ok_or_else(|| error("missing_source", format!("Missing source {}", job.source)))?;
        if hashes.get(job.source.as_str()) != Some(&job.source_sha256) {
            return Err(error(
                "stale_source",
                format!("Source digest mismatch for {}", job.source),
            ));
        }
        job.compression.resolved(bytes.len())?;
        work = work
            .checked_add(bytes.len())
            .ok_or_else(|| error("size_limit", "Work size overflow"))?;
    }
    if work > MAX_BATCH_WORK_BYTES {
        return Err(error(
            "size_limit",
            "Repeated batch input exceeds work limit",
        ));
    }
    let mut remaining = MAX_OUTPUT_BYTES;
    let mut result = BTreeMap::new();
    for job in &batch.jobs {
        let output = compress_with_limit(&batch.sources[&job.source], &job.compression, remaining)?;
        remaining -= output.bytes.len();
        result.insert(job.destination.clone(), output);
    }
    Ok(result)
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", deny_unknown_fields)]
enum Request {
    Compress {
        bytes: Vec<u8>,
        compression: Compression,
    },
    CompressBatch {
        batch: Batch,
    },
}

fn bounded_json_value(value: impl Serialize) -> Result<Value, Error> {
    // Numeric byte arrays expand when converted to serde_json::Value. Bound the
    // encoded representation before that conversion, including envelope room.
    let mut output = BoundedWriter::new(MAX_JSON_BYTES - 128);
    serde_json::to_writer(&mut output, &value).map_err(|failure| {
        error(
            if output.exhausted {
                "size_limit"
            } else {
                "serialization"
            },
            failure.to_string(),
        )
    })?;
    serde_json::from_slice(&output.bytes)
        .map_err(|failure| error("serialization", failure.to_string()))
}

/// JSON transport uses numeric byte arrays and is capped at 16 MiB in both
/// directions. Native hosts should use the typed API for larger asset batches.
pub fn dispatch(input: &[u8]) -> Value {
    let result = (|| -> Result<Value, Error> {
        if input.len() > MAX_JSON_BYTES {
            return Err(error("size_limit", "JSON request exceeds 16 MiB"));
        }
        let request: Request = serde_json::from_slice(input)
            .map_err(|failure| error("invalid_request", failure.to_string()))?;
        match request {
            Request::Compress { bytes, compression } => {
                bounded_json_value(compress(&bytes, &compression)?)
            }
            Request::CompressBatch { batch } => bounded_json_value(compress_batch(&batch)?),
        }
    })();
    match result {
        Ok(result) => json!({"ok":true,"result":result}),
        Err(error) => json!({"ok":false,"error":error}),
    }
}

pub fn request_json(input: &str) -> String {
    let mut output = BoundedWriter::new(MAX_JSON_BYTES);
    match serde_json::to_writer(&mut output, &dispatch(input.as_bytes())) {
        Ok(()) => String::from_utf8(output.bytes).expect("JSON serialization emits UTF-8"),
        Err(failure) => json!({"ok":false,"error":{"code":if output.exhausted {"size_limit"} else {"serialization"},"message":failure.to_string()}}).to_string(),
    }
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn compress_request_json(input: &str) -> String {
    request_json(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn independent_node_vectors_decode_and_match_gzip_header_and_trailer() {
        // Generated by Node v24.21.0: gzipSync(level:9), and
        // brotliCompressSync(MODE_TEXT, QUALITY:11, SIZE_HINT:12).
        // Node's lib/zlib.js passes the supplied params through without changing
        // unspecified defaults; encode.h specifies default LGWIN=22.
        let hex = |text: &str| {
            (0..text.len())
                .step_by(2)
                .map(|offset| u8::from_str_radix(&text[offset..offset + 2], 16).unwrap())
                .collect::<Vec<_>>()
        };
        let gzip = hex("1f8b0800000000000203cb48cdc9c95728cf2fca49e102002d3b08af0c000000");
        let brotli = hex("8b058068656c6c6f20776f726c640a03");
        let source = b"hello world\n";
        let mut decoded = Vec::new();
        flate2::read::GzDecoder::new(&gzip[..])
            .read_to_end(&mut decoded)
            .unwrap();
        assert_eq!(decoded, source);
        decoded.clear();
        brotli::BrotliDecompress(&mut &brotli[..], &mut decoded).unwrap();
        assert_eq!(decoded, source);
        let native = compress(source, &Compression::adapter_gzip()).unwrap();
        assert_eq!(&native.bytes[..10], &gzip[..10]);
        assert_eq!(
            &native.bytes[native.bytes.len() - 8..],
            &gzip[gzip.len() - 8..]
        );
        decoded.clear();
        let native = compress(source, &Compression::adapter_brotli()).unwrap();
        brotli::BrotliDecompress(&mut &native.bytes[..], &mut decoded).unwrap();
        assert_eq!(decoded, source);
    }

    #[test]
    fn gzip_header_roundtrip_and_identity_are_deterministic() {
        let source = "const name = '😀';\r\n".repeat(100);
        let one = compress(source.as_bytes(), &Compression::adapter_gzip()).unwrap();
        let two = compress(source.as_bytes(), &Compression::adapter_gzip()).unwrap();
        assert_eq!(one.bytes, two.bytes);
        assert_eq!(&one.bytes[..10], &[31, 139, 8, 0, 0, 0, 0, 0, 2, 3]);
        assert_eq!(one.output_sha256, sha256(&one.bytes));
        assert_eq!(one.source_sha256, sha256(source.as_bytes()));
        let mut decoded = Vec::new();
        flate2::read::GzDecoder::new(&one.bytes[..])
            .read_to_end(&mut decoded)
            .unwrap();
        assert_eq!(decoded, source.as_bytes());
        let trailer_size = u32::from_le_bytes(one.bytes[one.bytes.len() - 4..].try_into().unwrap());
        assert_eq!(trailer_size as usize, source.len());
    }

    #[test]
    fn brotli_text_defaults_roundtrip_unicode_and_empty_sources() {
        for source in [b"".as_slice(), "<p>😀 Unicode text</p>\n".as_bytes()] {
            let output = compress(source, &Compression::adapter_brotli()).unwrap();
            let again = compress(source, &Compression::adapter_brotli()).unwrap();
            assert_eq!(output.bytes, again.bytes);
            assert_eq!(
                output.compression,
                Compression::Brotli {
                    quality: 11,
                    mode: BrotliMode::Text,
                    lgwin: 22,
                    size_hint: Some(source.len())
                }
            );
            let mut decoded = Vec::new();
            brotli::BrotliDecompress(&mut &output.bytes[..], &mut decoded).unwrap();
            assert_eq!(decoded, source);
        }
    }

    #[test]
    fn batch_binds_outputs_to_exact_source_and_rejects_duplicate_destinations() {
        let source = b"export const answer = 42;".to_vec();
        let job = Job {
            source: "client/app.js".into(),
            destination: "client/app.js.gz".into(),
            source_sha256: sha256(&source),
            compression: Compression::adapter_gzip(),
        };
        let mut batch = Batch {
            sources: BTreeMap::from([(job.source.clone(), source)]),
            jobs: vec![job.clone()],
        };
        let output = compress_batch(&batch).unwrap();
        assert_eq!(output[&job.destination].source_sha256, job.source_sha256);
        batch.jobs.push(job.clone());
        assert_eq!(compress_batch(&batch).unwrap_err().code, "duplicate_output");
        batch.jobs.pop();
        batch.sources.get_mut(&job.source).unwrap().push(0);
        assert_eq!(compress_batch(&batch).unwrap_err().code, "stale_source");
    }

    #[test]
    fn options_and_output_budgets_fail_explicitly() {
        assert_eq!(
            compress(b"x", &Compression::Gzip { level: 10 })
                .unwrap_err()
                .code,
            "invalid_options"
        );
        assert_eq!(
            compress(
                b"x",
                &Compression::Brotli {
                    quality: 11,
                    mode: BrotliMode::Text,
                    lgwin: 22,
                    size_hint: Some(2)
                }
            )
            .unwrap_err()
            .code,
            "stale_source"
        );
        for encoding in [Compression::adapter_gzip(), Compression::adapter_brotli()] {
            assert_eq!(
                compress_with_limit(b"hello world", &encoding, 1)
                    .unwrap_err()
                    .code,
                "size_limit"
            );
        }
        assert_eq!(
            dispatch(br#"{"op":"compress","bytes":[256],"compression":{"format":"gzip"}}"#)["ok"],
            false
        );
    }

    #[test]
    fn adapter_job_json_and_worker_protocol_preserve_identities() {
        let digest = sha256(b"x");
        let input = json!({"op":"compressBatch","batch":{"sources":{"asset":[120]},"jobs":[
            {"source":"asset","destination":"asset.br","sourceSha256":digest,
             "compression":{"format":"brotli","quality":11,"mode":"text","size_hint":1}}]}});
        let response: Value = serde_json::from_str(&request_json(&input.to_string())).unwrap();
        assert_eq!(response["ok"], true);
        assert_eq!(response["result"]["asset.br"]["sourceSha256"], digest);
        assert_eq!(response["result"]["asset.br"]["compression"]["sizeHint"], 1);
        assert_eq!(response["result"]["asset.br"]["compression"]["lgwin"], 22);
    }
}
