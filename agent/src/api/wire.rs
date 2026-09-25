//! Wire protocol v1: envelopes and parameter types. See `docs/protocol.md`.
//!
//! Parameter structs use `deny_unknown_fields` so a misspelt option (`"copys": 2`) is an
//! error rather than a silently ignored setting on a physical print.

use base64::Engine;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use kiln_core::error::{ErrorCode, PrintError};
use kiln_core::model::*;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;
pub const SUPPORTED_PROTOCOL_VERSIONS: &[u32] = &[1];

/// Client → agent.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestEnvelope {
    pub protocol_version: Option<u32>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub id: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// Agent → client reply to one request.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseEnvelope<'a> {
    pub protocol_version: u32,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub id: Option<&'a str>,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<PrintError>,
}

impl<'a> ResponseEnvelope<'a> {
    pub fn from_result(id: Option<&'a str>, result: Result<Value, PrintError>) -> Self {
        let (ok, result, error) = match result {
            Ok(value) => (true, Some(value), None),
            Err(err) => (false, None, Some(err)),
        };
        Self {
            protocol_version: PROTOCOL_VERSION,
            kind: "response",
            id,
            ok,
            result,
            error,
        }
    }
}

/// Agent → client notification.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventEnvelope<'a> {
    pub protocol_version: u32,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub event: &'a str,
    pub seq: u64,
    pub timestamp: DateTime<Utc>,
    pub data: Value,
}

pub fn parse_params<T: DeserializeOwned>(params: Value) -> Result<T, PrintError> {
    let params = if params.is_null() {
        Value::Object(Default::default())
    } else {
        params
    };
    serde_json::from_value(params)
        .map_err(|e| PrintError::invalid_payload(format!("invalid params: {e}")))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelloParams {
    pub protocol_versions: Vec<u32>,
    pub client: ClientInfo,
    pub auth: AuthParams,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientInfo {
    pub name: String,
    pub version: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum AuthParams {
    Token { token: String },
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrinterParams {
    pub printer_id: Option<String>,
    pub printer: Option<String>,
}

pub fn selector(
    printer_id: Option<String>,
    printer: Option<String>,
) -> Result<PrinterSelector, PrintError> {
    match (printer_id, printer) {
        (Some(id), None) => Ok(PrinterSelector::Id(PrinterId(id))),
        (None, Some(name)) => Ok(PrinterSelector::Name(name)),
        (Some(_), Some(_)) => Err(PrintError::invalid_payload(
            "give either printerId or printer, not both",
        )),
        (None, None) => Err(PrintError::invalid_payload(
            "printerId or printer is required",
        )),
    }
}

/// How RAW `data` is encoded in JSON.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DataEncoding {
    /// Standard base64 (RFC 4648 §4, padded). Byte-exact for any payload.
    #[default]
    #[serde(alias = "binary")]
    Base64,
    Hex,
    /// The JSON string's UTF-8 bytes (convenient for ZPL/EPL/TSPL text).
    #[serde(alias = "utf-8", alias = "text")]
    Utf8,
    /// Each character U+0000..=U+00FF becomes one byte.
    #[serde(alias = "iso-8859-1")]
    Latin1,
}

impl DataEncoding {
    pub fn decode(self, data: &str, max_bytes: u64) -> Result<Bytes, PrintError> {
        let estimate = match self {
            Self::Base64 => data.len() as u64 / 4 * 3,
            Self::Hex => data.len() as u64 / 2,
            Self::Utf8 | Self::Latin1 => data.len() as u64,
        };
        // Reject before allocating the decoded buffer.
        if estimate > max_bytes + 3 {
            return Err(too_large(estimate, max_bytes));
        }
        let bytes = match self {
            Self::Base64 => base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|e| {
                    PrintError::invalid_payload(format!("data is not valid base64: {e}"))
                })?,
            Self::Hex => hex::decode(data)
                .map_err(|e| PrintError::invalid_payload(format!("data is not valid hex: {e}")))?,
            Self::Utf8 => data.as_bytes().to_vec(),
            Self::Latin1 => data
                .chars()
                .map(|c| {
                    u8::try_from(u32::from(c)).map_err(|_| {
                        PrintError::invalid_payload(format!(
                            "character U+{:04X} does not fit latin1 encoding",
                            u32::from(c)
                        ))
                    })
                })
                .collect::<Result<_, _>>()?,
        };
        if bytes.len() as u64 > max_bytes {
            return Err(too_large(bytes.len() as u64, max_bytes));
        }
        Ok(Bytes::from(bytes))
    }
}

fn too_large(size: u64, limit: u64) -> PrintError {
    PrintError::new(
        ErrorCode::PayloadTooLarge,
        format!("document exceeds the {limit}-byte limit"),
    )
    .with_details(serde_json::json!({ "sizeBytes": size, "limitBytes": limit }))
}

/// `print.raw` / `POST /v1/print/raw`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RawPrintParams {
    pub printer_id: Option<String>,
    pub printer: Option<String>,
    pub data: String,
    #[serde(default)]
    pub encoding: DataEncoding,
    pub language: Option<String>,
    pub copies: Option<u32>,
    pub job_name: Option<String>,
    pub idempotency_key: Option<String>,
}

/// `print.text` / `POST /v1/print/text`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TextPrintParams {
    pub printer_id: Option<String>,
    pub printer: Option<String>,
    pub text: String,
    #[serde(default)]
    pub options: TextOptions,
    pub copies: Option<u32>,
    pub job_name: Option<String>,
    pub idempotency_key: Option<String>,
}

/// Where a binary document (PDF, image) comes from: exactly one of inline `data`, a local
/// `path` or a `url`. Paths and URLs are only honoured inside administrator allow-lists.
#[derive(Debug, Clone)]
pub enum SourceSpec {
    Inline {
        data: String,
        encoding: DataEncoding,
    },
    Path(String),
    Url(String),
}

fn source_spec(
    data: Option<String>,
    encoding: DataEncoding,
    path: Option<String>,
    url: Option<String>,
) -> Result<SourceSpec, PrintError> {
    match (data, path, url) {
        (Some(data), None, None) => Ok(SourceSpec::Inline { data, encoding }),
        (None, Some(path), None) => Ok(SourceSpec::Path(path)),
        (None, None, Some(url)) => Ok(SourceSpec::Url(url)),
        _ => Err(PrintError::invalid_payload(
            "give exactly one of data, path or url",
        )),
    }
}

/// `print.pdf` / `POST /v1/print/pdf`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PdfPrintParams {
    pub printer_id: Option<String>,
    pub printer: Option<String>,
    pub data: Option<String>,
    #[serde(default)]
    pub encoding: DataEncoding,
    pub path: Option<String>,
    pub url: Option<String>,
    #[serde(default)]
    pub options: PdfOptions,
    pub copies: Option<u32>,
    pub job_name: Option<String>,
    pub idempotency_key: Option<String>,
}

/// `print.image` / `POST /v1/print/image`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImagePrintParams {
    pub printer_id: Option<String>,
    pub printer: Option<String>,
    pub data: Option<String>,
    #[serde(default)]
    pub encoding: DataEncoding,
    pub path: Option<String>,
    pub url: Option<String>,
    #[serde(default)]
    pub options: ImageOptions,
    pub copies: Option<u32>,
    pub job_name: Option<String>,
    pub idempotency_key: Option<String>,
}

/// `print.html` / `POST /v1/print/html`. HTML is always inline: the agent never
/// navigates to remote pages.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HtmlPrintParams {
    pub printer_id: Option<String>,
    pub printer: Option<String>,
    pub html: String,
    #[serde(default)]
    pub options: HtmlOptions,
    pub copies: Option<u32>,
    pub job_name: Option<String>,
    pub idempotency_key: Option<String>,
}

/// A request whose document bytes still have to be resolved from a [`SourceSpec`].
pub struct PendingRequest {
    pub source: SourceSpec,
    build: Box<dyn FnOnce(Bytes) -> Document + Send>,
    printer: PrinterSelector,
    copies: u32,
    job_name: Option<String>,
    idempotency_key: Option<String>,
}

impl std::fmt::Debug for PendingRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingRequest")
            .field("source", &self.source)
            .field("printer", &self.printer)
            .finish_non_exhaustive()
    }
}

impl PendingRequest {
    pub fn complete(self, data: Bytes) -> PrintRequest {
        PrintRequest {
            printer: self.printer,
            document: (self.build)(data),
            copies: self.copies,
            job_name: self.job_name,
            idempotency_key: self.idempotency_key,
        }
    }
}

impl PdfPrintParams {
    pub fn into_pending(self) -> Result<PendingRequest, PrintError> {
        let options = self.options;
        Ok(PendingRequest {
            source: source_spec(self.data, self.encoding, self.path, self.url)?,
            build: Box::new(move |data| Document::Pdf(PdfDocument { data, options })),
            printer: selector(self.printer_id, self.printer)?,
            copies: self.copies.unwrap_or(1),
            job_name: self.job_name,
            idempotency_key: self.idempotency_key,
        })
    }
}

impl ImagePrintParams {
    pub fn into_pending(self) -> Result<PendingRequest, PrintError> {
        let options = self.options;
        Ok(PendingRequest {
            source: source_spec(self.data, self.encoding, self.path, self.url)?,
            build: Box::new(move |data| Document::Image(ImageDocument { data, options })),
            printer: selector(self.printer_id, self.printer)?,
            copies: self.copies.unwrap_or(1),
            job_name: self.job_name,
            idempotency_key: self.idempotency_key,
        })
    }
}

impl HtmlPrintParams {
    pub fn into_request(self, max_bytes: u64) -> Result<PrintRequest, PrintError> {
        if self.html.len() as u64 > max_bytes {
            return Err(too_large(self.html.len() as u64, max_bytes));
        }
        Ok(PrintRequest {
            printer: selector(self.printer_id, self.printer)?,
            document: Document::Html(HtmlDocument {
                html: self.html,
                options: self.options,
            }),
            copies: self.copies.unwrap_or(1),
            job_name: self.job_name,
            idempotency_key: self.idempotency_key,
        })
    }
}

/// `print.submit` / `POST /v1/print`: `{ printerId, type, ...type-specific fields }`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenericPrintParams {
    #[serde(rename = "type")]
    pub document_type: String,
    #[serde(flatten)]
    pub rest: serde_json::Map<String, Value>,
}

impl RawPrintParams {
    pub fn into_request(self, max_bytes: u64) -> Result<PrintRequest, PrintError> {
        Ok(PrintRequest {
            printer: selector(self.printer_id, self.printer)?,
            document: Document::Raw(RawDocument {
                data: self.encoding.decode(&self.data, max_bytes)?,
                language: self.language,
            }),
            copies: self.copies.unwrap_or(1),
            job_name: self.job_name,
            idempotency_key: self.idempotency_key,
        })
    }
}

impl TextPrintParams {
    pub fn into_request(self, max_bytes: u64) -> Result<PrintRequest, PrintError> {
        if self.text.len() as u64 > max_bytes {
            return Err(too_large(self.text.len() as u64, max_bytes));
        }
        Ok(PrintRequest {
            printer: selector(self.printer_id, self.printer)?,
            document: Document::Text(TextDocument {
                text: self.text,
                options: self.options,
            }),
            copies: self.copies.unwrap_or(1),
            job_name: self.job_name,
            idempotency_key: self.idempotency_key,
        })
    }
}

/// `print.label` / `POST /v1/print/label`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LabelPrintParams {
    pub printer_id: Option<String>,
    pub printer: Option<String>,
    pub label: LabelDocument,
    pub copies: Option<u32>,
    pub job_name: Option<String>,
    pub idempotency_key: Option<String>,
}

/// `print.receipt` / `POST /v1/print/receipt`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReceiptPrintParams {
    pub printer_id: Option<String>,
    pub printer: Option<String>,
    pub receipt: ReceiptDocument,
    pub copies: Option<u32>,
    pub job_name: Option<String>,
    pub idempotency_key: Option<String>,
}

/// `print.dotmatrix` / `POST /v1/print/dotmatrix`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DotMatrixPrintParams {
    pub printer_id: Option<String>,
    pub printer: Option<String>,
    pub document: DotMatrixDocument,
    pub copies: Option<u32>,
    pub job_name: Option<String>,
    pub idempotency_key: Option<String>,
}

fn structured(
    printer_id: Option<String>,
    printer: Option<String>,
    document: Document,
    copies: Option<u32>,
    job_name: Option<String>,
    idempotency_key: Option<String>,
    max_bytes: u64,
) -> Result<PrintRequest, PrintError> {
    let size = document.size_bytes();
    if size > max_bytes {
        return Err(too_large(size, max_bytes));
    }
    Ok(PrintRequest {
        printer: selector(printer_id, printer)?,
        document,
        copies: copies.unwrap_or(1),
        job_name,
        idempotency_key,
    })
}

impl LabelPrintParams {
    pub fn into_request(self, max_bytes: u64) -> Result<PrintRequest, PrintError> {
        structured(
            self.printer_id,
            self.printer,
            Document::Label(self.label),
            self.copies,
            self.job_name,
            self.idempotency_key,
            max_bytes,
        )
    }
}

impl ReceiptPrintParams {
    pub fn into_request(self, max_bytes: u64) -> Result<PrintRequest, PrintError> {
        structured(
            self.printer_id,
            self.printer,
            Document::Receipt(self.receipt),
            self.copies,
            self.job_name,
            self.idempotency_key,
            max_bytes,
        )
    }
}

impl DotMatrixPrintParams {
    pub fn into_request(self, max_bytes: u64) -> Result<PrintRequest, PrintError> {
        structured(
            self.printer_id,
            self.printer,
            Document::DotMatrix(self.document),
            self.copies,
            self.job_name,
            self.idempotency_key,
            max_bytes,
        )
    }
}

/// A `print.submit` request split by document type.
#[derive(Debug)]
pub enum TypedPrint {
    Ready(PrintRequest),
    Pending(PendingRequest),
}

impl GenericPrintParams {
    pub fn into_typed(self, max_bytes: u64) -> Result<TypedPrint, PrintError> {
        let rest = Value::Object(self.rest);
        match self.document_type.to_ascii_uppercase().as_str() {
            "RAW" => Ok(TypedPrint::Ready(
                parse_params::<RawPrintParams>(rest)?.into_request(max_bytes)?,
            )),
            "TEXT" => Ok(TypedPrint::Ready(
                parse_params::<TextPrintParams>(rest)?.into_request(max_bytes)?,
            )),
            "HTML" => Ok(TypedPrint::Ready(
                parse_params::<HtmlPrintParams>(rest)?.into_request(max_bytes)?,
            )),
            "PDF" => Ok(TypedPrint::Pending(
                parse_params::<PdfPrintParams>(rest)?.into_pending()?,
            )),
            "LABEL" => Ok(TypedPrint::Ready(
                parse_params::<LabelPrintParams>(rest)?.into_request(max_bytes)?,
            )),
            "RECEIPT" => Ok(TypedPrint::Ready(
                parse_params::<ReceiptPrintParams>(rest)?.into_request(max_bytes)?,
            )),
            "DOT_MATRIX" | "DOTMATRIX" => Ok(TypedPrint::Ready(
                parse_params::<DotMatrixPrintParams>(rest)?.into_request(max_bytes)?,
            )),
            "IMAGE" => Ok(TypedPrint::Pending(
                parse_params::<ImagePrintParams>(rest)?.into_pending()?,
            )),
            other => Err(PrintError::new(
                ErrorCode::UnsupportedDocument,
                format!("unknown document type '{other}'"),
            )),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JobsListParams {
    /// One status or several.
    #[serde(default)]
    pub status: StatusList,
    pub printer_id: Option<String>,
    pub client_id: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(untagged)]
pub enum StatusList {
    #[default]
    Any,
    One(String),
    Many(Vec<String>),
}

impl StatusList {
    pub fn parse(self) -> Result<Vec<JobStatus>, PrintError> {
        let raw = match self {
            Self::Any => vec![],
            Self::One(s) => s.split(',').map(str::to_owned).collect(),
            Self::Many(v) => v,
        };
        raw.iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| {
                JobStatus::parse(&s.to_ascii_uppercase())
                    .ok_or_else(|| PrintError::invalid_payload(format!("unknown job status '{s}'")))
            })
            .collect()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JobIdParams {
    pub job_id: String,
}

impl JobIdParams {
    pub fn job_id(&self) -> Result<JobId, PrintError> {
        self.job_id
            .parse()
            .map_err(|_| PrintError::invalid_payload("jobId must be a UUID"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_encodings_are_byte_exact() {
        let all: Vec<u8> = (0..=255).collect();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&all);
        assert_eq!(
            &DataEncoding::Base64.decode(&b64, 1024).expect("b64")[..],
            &all[..]
        );
        assert_eq!(
            &DataEncoding::Hex
                .decode(&hex::encode(&all), 1024)
                .expect("hex")[..],
            &all[..]
        );
        assert_eq!(
            &DataEncoding::Utf8.decode("^XA^XZ", 1024).expect("utf8")[..],
            b"^XA^XZ"
        );
        let latin: String = all.iter().map(|b| char::from(*b)).collect();
        assert_eq!(
            &DataEncoding::Latin1.decode(&latin, 1024).expect("latin1")[..],
            &all[..]
        );
        assert!(DataEncoding::Latin1.decode("€", 1024).is_err());
        assert!(DataEncoding::Base64.decode("not base64!", 1024).is_err());
    }

    #[test]
    fn size_limit_applies_before_and_after_decoding() {
        let err = DataEncoding::Base64
            .decode(&"A".repeat(4000), 100)
            .expect_err("too big");
        assert_eq!(err.error_code, ErrorCode::PayloadTooLarge);
        let err = DataEncoding::Utf8.decode("12345", 4).expect_err("too big");
        assert_eq!(err.error_code, ErrorCode::PayloadTooLarge);
    }

    #[test]
    fn binary_is_an_alias_for_base64() {
        let p: RawPrintParams = parse_params(
            serde_json::json!({"printerId": "x", "encoding": "binary", "data": "AAE="}),
        )
        .expect("parse");
        let req = p.into_request(10).expect("request");
        let Document::Raw(raw) = req.document else {
            panic!("raw")
        };
        assert_eq!(&raw.data[..], &[0, 1]);
    }

    #[test]
    fn typos_are_rejected() {
        let err = parse_params::<RawPrintParams>(
            serde_json::json!({"printerId": "x", "data": "", "copys": 2}),
        )
        .expect_err("unknown field");
        assert_eq!(err.error_code, ErrorCode::InvalidPayload);
    }

    #[test]
    fn generic_print_dispatches_on_type() {
        let p: GenericPrintParams = parse_params(serde_json::json!({
            "type": "text", "printer": "Laser", "text": "hi", "options": {"mode": "RAW"}, "copies": 2
        }))
        .expect("parse");
        let TypedPrint::Ready(req) = p.into_typed(100).expect("request") else {
            panic!("ready")
        };
        assert_eq!(req.copies, 2);
        assert!(matches!(req.document, Document::Text(_)));

        let p: GenericPrintParams = parse_params(serde_json::json!({
            "type": "PDF", "printer": "Laser", "data": "JVBERi0=", "options": {"pageRange": "1-2", "duplex": "LONG_EDGE"}
        }))
        .expect("parse");
        let TypedPrint::Pending(pending) = p.into_typed(100).expect("pending") else {
            panic!("pending")
        };
        assert!(matches!(pending.source, SourceSpec::Inline { .. }));
        let req = pending.complete(Bytes::from_static(b"%PDF-"));
        let Document::Pdf(pdf) = req.document else {
            panic!("pdf")
        };
        assert_eq!(pdf.options.duplex, Some(Duplex::LongEdge));

        let p: GenericPrintParams =
            parse_params(serde_json::json!({"type": "EXCEL", "printer": "Laser"})).expect("parse");
        assert_eq!(
            p.into_typed(100).expect_err("excel").error_code,
            ErrorCode::UnsupportedDocument
        );
    }

    #[test]
    fn sources_are_mutually_exclusive() {
        let both = parse_params::<PdfPrintParams>(
            serde_json::json!({"printer": "P", "data": "x", "path": "C:/a.pdf"}),
        )
        .expect("parse");
        assert!(both.into_pending().is_err());
        let none =
            parse_params::<ImagePrintParams>(serde_json::json!({"printer": "P"})).expect("parse");
        assert!(none.into_pending().is_err());
        let url = parse_params::<ImagePrintParams>(
            serde_json::json!({"printer": "P", "url": "https://x/y.png"}),
        )
        .expect("parse");
        assert!(matches!(
            url.into_pending().expect("pending").source,
            SourceSpec::Url(_)
        ));
    }

    #[test]
    fn status_lists() {
        assert_eq!(
            StatusList::One("queued,failed".into()).parse().expect("ok"),
            vec![JobStatus::Queued, JobStatus::Failed]
        );
        assert!(StatusList::Many(vec!["DONE".into()]).parse().is_err());
    }
}
