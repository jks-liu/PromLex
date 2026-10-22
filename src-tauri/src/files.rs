use std::{
    collections::{BTreeSet, HashMap},
    io::{Cursor, Read, Write},
    sync::Arc,
};

use fontdb::{Database as FontDatabase, Family as FontFamily, Query as FontQuery, Style};
use printpdf::{
    Color, FontId, Op, ParsedFont, PdfDocument, PdfFontHandle, PdfParseOptions, PdfSaveOptions, Pt,
    Rgb, TextItem, TextMatrix,
};
use quick_xml::{
    escape::{escape, unescape},
    events::Event,
    Reader,
};
use serde_json::Value;
use thiserror::Error;
use tokio::{
    sync::mpsc,
    task::JoinSet,
    time::{sleep, Duration},
};
use tokio_util::sync::CancellationToken;
use zip::{write::FileOptions, CompressionMethod, ZipArchive, ZipWriter};

use crate::{
    ai::{contains_segment_separator, AiError, StreamCallback, SEGMENT_SEPARATOR},
    models::{FileBatchFailure, FileOutputMode, FileProgress, TranslateRequest},
    scheduler::{AiScheduler, Priority},
    store::AppStore,
};

#[derive(Debug, Error)]
pub enum FileError {
    #[error("{0}")]
    Message(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Invalid ZIP/Office document: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("XML error: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error("CSV error: {0}")]
    Csv(#[from] csv::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("File translation was cancelled")]
    Cancelled,
}

pub type ProgressCallback = Arc<dyn Fn(FileProgress) + Send + Sync>;

#[derive(Debug)]
pub struct FileTranslationResult {
    pub filename: String,
    pub media_type: String,
    pub content: Vec<u8>,
    pub translated_segments: usize,
    pub skipped_segments: usize,
    pub total_segments: usize,
    pub total_batches: usize,
    pub failed_segments: usize,
    pub failed_batches: Vec<FileBatchFailure>,
    pub retry_context: Option<FileRetryContext>,
}

#[derive(Debug, Clone)]
pub struct FileRetryContext {
    filename: String,
    bytes: Vec<u8>,
    options: TranslateRequest,
    output_mode: FileOutputMode,
    cached_batches: HashMap<usize, CachedBatch>,
}

impl FileRetryContext {
    pub(crate) fn new(
        filename: &str,
        bytes: Vec<u8>,
        options: TranslateRequest,
        output_mode: FileOutputMode,
    ) -> Self {
        Self {
            filename: filename.to_string(),
            bytes,
            options,
            output_mode,
            cached_batches: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
enum CachedBatch {
    Text(Vec<String>),
    Image(Vec<u8>),
}

struct BatchState {
    cached_batches: HashMap<usize, CachedBatch>,
    failed_batches: Vec<FileBatchFailure>,
    cancel: CancellationToken,
}

impl Default for BatchState {
    fn default() -> Self {
        Self {
            cached_batches: HashMap::new(),
            failed_batches: Vec::new(),
            cancel: CancellationToken::new(),
        }
    }
}

#[derive(Default)]
struct Counts {
    translated: usize,
    failed: usize,
    skipped: usize,
    total_segments: usize,
    total_batches: usize,
    completed_batches: usize,
    stage: String,
    streaming_batch: Option<usize>,
    streaming_segments: usize,
    streaming_batch_segments: usize,
    streaming_text: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub async fn translate_file_with_cancel(
    store: &AppStore,
    scheduler: &AiScheduler,
    filename: &str,
    bytes: Vec<u8>,
    options: TranslateRequest,
    output_mode: FileOutputMode,
    progress: ProgressCallback,
    cancel: CancellationToken,
) -> Result<FileTranslationResult, FileError> {
    translate_file_with_cache(
        store,
        scheduler,
        filename,
        bytes,
        options,
        output_mode,
        progress,
        HashMap::new(),
        cancel,
    )
    .await
}

pub async fn retry_file_with_cancel(
    store: &AppStore,
    scheduler: &AiScheduler,
    context: FileRetryContext,
    progress: ProgressCallback,
    cancel: CancellationToken,
) -> Result<FileTranslationResult, FileError> {
    let FileRetryContext {
        filename,
        bytes,
        options,
        output_mode,
        cached_batches,
    } = context;
    translate_file_with_cache(
        store,
        scheduler,
        &filename,
        bytes,
        options,
        output_mode,
        progress,
        cached_batches,
        cancel,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn translate_file_with_cache(
    store: &AppStore,
    scheduler: &AiScheduler,
    filename: &str,
    bytes: Vec<u8>,
    mut options: TranslateRequest,
    output_mode: FileOutputMode,
    progress: ProgressCallback,
    cached_batches: HashMap<usize, CachedBatch>,
    cancel: CancellationToken,
) -> Result<FileTranslationResult, FileError> {
    let source_bytes = bytes.clone();
    let mut batch_state = BatchState {
        cached_batches,
        cancel,
        ..BatchState::default()
    };
    let extension = filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut counts = Counts {
        stage: "preparing".to_string(),
        ..Counts::default()
    };
    report(&counts, &progress);
    ensure_not_cancelled(&batch_state.cancel)?;
    if options.summarize && options.context_summary.is_none() {
        counts.stage = "summarizing".to_string();
        report(&counts, &progress);
        let context_limit = store
            .provider(&options.provider_id)
            .map(|provider| (provider.context_size / 2).clamp(1, 4_096))
            .unwrap_or(4_096);
        let sample = extract_summary_text(filename, &bytes, context_limit)?;
        if !sample.trim().is_empty() {
            let summary = tokio::select! {
                result = scheduler.summarize(options.clone(), sample, Priority::Low) => result,
                _ = batch_state.cancel.cancelled() => return Err(FileError::Cancelled),
            };
            options.context_summary = Some(summary.map_err(|error| {
                FileError::Message(format!("Unable to summarize document: {error}"))
            })?);
        }
    }
    let translated = match extension.as_str() {
        "docx" => {
            translate_office(
                store,
                scheduler,
                &bytes,
                &options,
                output_mode,
                OfficeKind::Word,
                &mut counts,
                &progress,
                &mut batch_state,
            )
            .await?
        }
        "pptx" => {
            translate_office(
                store,
                scheduler,
                &bytes,
                &options,
                output_mode,
                OfficeKind::PowerPoint,
                &mut counts,
                &progress,
                &mut batch_state,
            )
            .await?
        }
        "xlsx" => {
            translate_office(
                store,
                scheduler,
                &bytes,
                &options,
                output_mode,
                OfficeKind::Excel,
                &mut counts,
                &progress,
                &mut batch_state,
            )
            .await?
        }
        "json" => {
            translate_json(
                store,
                scheduler,
                &bytes,
                &options,
                output_mode,
                &mut counts,
                &progress,
                &mut batch_state,
            )
            .await?
        }
        "csv" => {
            translate_csv(
                store,
                scheduler,
                &bytes,
                &options,
                output_mode,
                &mut counts,
                &progress,
                &mut batch_state,
            )
            .await?
        }
        "srt" | "vtt" => {
            translate_caption(
                store,
                scheduler,
                &bytes,
                &options,
                output_mode,
                &mut counts,
                &progress,
                &mut batch_state,
            )
            .await?
        }
        "txt" | "md" | "markdown" | "html" | "htm" => {
            translate_plain_file(
                store,
                scheduler,
                &bytes,
                &options,
                output_mode,
                &mut counts,
                &progress,
                &mut batch_state,
            )
            .await?
        }
        "pdf" => translate_pdf(
            store,
            scheduler,
            &bytes,
            &options,
            output_mode,
            &mut counts,
            &progress,
            &mut batch_state,
        )
        .await?,
        "png" | "jpg" | "jpeg" | "webp" => {
            translate_image_file(
                store,
                scheduler,
                filename,
                bytes,
                &options,
                &mut counts,
                &progress,
                &mut batch_state,
            )
            .await?
        }
        _ => {
            return Err(FileError::Message(
                "Unsupported file type. Use PDF, DOCX, PPTX, XLSX, TXT, Markdown, HTML, CSV, JSON, SRT, VTT, PNG, JPG or WebP."
                    .to_string(),
            ))
        }
    };
    counts.stage = "completed".to_string();
    counts.streaming_batch = None;
    counts.streaming_segments = 0;
    counts.streaming_batch_segments = 0;
    counts.streaming_text = None;
    report(&counts, &progress);
    let retry_context = if batch_state.failed_batches.is_empty() {
        None
    } else {
        Some(FileRetryContext {
            filename: filename.to_string(),
            bytes: source_bytes,
            options: options.clone(),
            output_mode,
            cached_batches: batch_state.cached_batches,
        })
    };
    Ok(FileTranslationResult {
        filename: output_filename(filename),
        media_type: output_media_type(filename),
        content: translated,
        translated_segments: counts.translated,
        skipped_segments: counts.skipped,
        total_segments: counts.total_segments,
        total_batches: counts.total_batches,
        failed_segments: counts.failed,
        failed_batches: batch_state.failed_batches,
        retry_context,
    })
}

fn report(counts: &Counts, progress: &ProgressCallback) {
    progress(FileProgress {
        stage: counts.stage.clone(),
        total_segments: counts.total_segments,
        translated_segments: counts.translated,
        failed_segments: counts.failed,
        skipped_segments: counts.skipped,
        total_batches: counts.total_batches,
        completed_batches: counts.completed_batches,
        streaming_batch: counts.streaming_batch,
        streaming_segments: counts.streaming_segments,
        streaming_batch_segments: counts.streaming_batch_segments,
        streaming_text: counts.streaming_text.clone(),
    });
}

fn ensure_not_cancelled(cancel: &CancellationToken) -> Result<(), FileError> {
    if cancel.is_cancelled() {
        Err(FileError::Cancelled)
    } else {
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
async fn translate_image_file(
    _store: &AppStore,
    scheduler: &AiScheduler,
    filename: &str,
    bytes: Vec<u8>,
    options: &TranslateRequest,
    counts: &mut Counts,
    progress: &ProgressCallback,
    state: &mut BatchState,
) -> Result<Vec<u8>, FileError> {
    ensure_not_cancelled(&state.cancel)?;
    counts.stage = "translating".to_string();
    let batch_id = counts.total_batches;
    counts.total_segments += 1;
    counts.total_batches += 1;
    report(counts, progress);
    if let Some(CachedBatch::Image(translated)) = state.cached_batches.get(&batch_id) {
        counts.translated += 1;
        counts.completed_batches += 1;
        report(counts, progress);
        return Ok(translated.clone());
    }
    let max_retries = 2;
    let mut attempts = 0;
    let translated = loop {
        ensure_not_cancelled(&state.cancel)?;
        attempts += 1;
        let result = tokio::select! {
            result = scheduler.translate_image(options.clone(), filename.to_string(), bytes.clone(), Priority::Low) => result,
            _ = state.cancel.cancelled() => return Err(FileError::Cancelled),
        };
        match result {
            Ok(translated) => break Ok(translated),
            Err(_error) if attempts <= max_retries => {
                sleep(Duration::from_millis((250 * attempts as u64).min(2_000))).await;
            }
            Err(error) => break Err(error.to_string()),
        }
    };
    let translated = match translated {
        Ok(translated) => {
            state
                .cached_batches
                .insert(batch_id, CachedBatch::Image(translated.clone()));
            counts.translated += 1;
            translated
        }
        Err(error) => {
            state.failed_batches.push(FileBatchFailure {
                id: batch_id,
                segment_count: 1,
                attempts,
                error,
            });
            counts.failed += 1;
            bytes
        }
    };
    counts.completed_batches += 1;
    report(counts, progress);
    Ok(translated)
}

#[allow(clippy::too_many_arguments)]
async fn translate_plain_file(
    store: &AppStore,
    scheduler: &AiScheduler,
    bytes: &[u8],
    options: &TranslateRequest,
    output_mode: FileOutputMode,
    counts: &mut Counts,
    progress: &ProgressCallback,
    state: &mut BatchState,
) -> Result<Vec<u8>, FileError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| FileError::Message("Only UTF-8 text files are supported".to_string()))?;
    Ok(translate_lines(
        store,
        scheduler,
        text,
        options,
        output_mode,
        counts,
        progress,
        state,
        |line| !line.trim().is_empty(),
    )
    .await?
    .into_bytes())
}

#[allow(clippy::too_many_arguments)]
async fn translate_pdf(
    store: &AppStore,
    scheduler: &AiScheduler,
    bytes: &[u8],
    options: &TranslateRequest,
    output_mode: FileOutputMode,
    counts: &mut Counts,
    progress: &ProgressCallback,
    state: &mut BatchState,
) -> Result<Vec<u8>, FileError> {
    let mut document = parse_pdf(&crate::pdf::extraction_copy(bytes)?)?;
    let blocks = pdf_text_blocks(&document);
    if blocks.is_empty() {
        return Err(FileError::Message(
            "Unable to translate PDF because it contains no selectable text".to_string(),
        ));
    }
    let translated = translate_fragments(
        store,
        scheduler,
        blocks.iter().map(|block| block.text.clone()).collect(),
        options,
        output_mode,
        counts,
        progress,
        state,
    )
    .await?;
    render_translated_pdf(&mut document, &blocks, &translated, bytes)
}

const PDF_MIN_FONT_SIZE: f32 = 6.0;
const PDF_LINE_HEIGHT_FACTOR: f32 = 1.15;
const PDF_COLUMN_GAP_FACTOR: f32 = 2.5;

struct PdfFont {
    parsed: ParsedFont,
    id: FontId,
}

#[derive(Debug, Clone, PartialEq)]
struct PdfTextBlock {
    page_index: usize,
    bbox: [f32; 4],
    font_size: f32,
    text: String,
    style: PdfTextStyle,
    line_height: f32,
    alignment: PdfTextAlignment,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PdfTextAlignment {
    Left,
    Center,
    Right,
}

#[derive(Debug)]
struct PdfLineFragment {
    bbox: [f32; 4],
    font_size: f32,
    text: String,
    style: PdfTextStyle,
}

#[derive(Debug, Clone, PartialEq)]
struct PdfTextStyle {
    color: Color,
    bold: bool,
    italic: bool,
    // Rotation in PDF user space; geometry is projected into this local frame.
    angle: f32,
    scale: f32,
    source_font: Option<FontId>,
    size: f32,
}

impl Default for PdfTextStyle {
    fn default() -> Self {
        Self {
            color: Color::Rgb(Rgb::new(0.0, 0.0, 0.0, None)),
            bold: false,
            italic: false,
            angle: 0.0,
            scale: 1.0,
            source_font: None,
            size: 12.0,
        }
    }
}

fn pdf_matrix_product(a: [f32; 6], b: [f32; 6]) -> [f32; 6] {
    [
        a[0] * b[0] + a[2] * b[1],
        a[1] * b[0] + a[3] * b[1],
        a[0] * b[2] + a[2] * b[3],
        a[1] * b[2] + a[3] * b[3],
        a[0] * b[4] + a[2] * b[5] + a[4],
        a[1] * b[4] + a[3] * b[5] + a[5],
    ]
}

fn pdf_local_bbox(bbox: [f32; 4], angle: f32) -> [f32; 4] {
    let (s, c) = angle.sin_cos();
    let mut result = [
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    ];
    for (x, y) in [
        (bbox[0], bbox[1]),
        (bbox[2], bbox[1]),
        (bbox[0], bbox[3]),
        (bbox[2], bbox[3]),
    ] {
        let (x, y) = (c * x - s * y, s * x + c * y);
        result[0] = result[0].min(x);
        result[1] = result[1].min(y);
        result[2] = result[2].max(x);
        result[3] = result[3].max(y);
    }
    result
}

/// One style per emitted glyph, in content-stream order, matching the geometry
/// extractor. Spaces and TJ offsets do not emit glyph boxes.
fn pdf_glyph_styles(page: &printpdf::PdfPage, document: &PdfDocument) -> Vec<PdfTextStyle> {
    let identity = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mut ctm = identity;
    let mut tm = identity;
    let mut style = PdfTextStyle::default();
    let mut stack = Vec::new();
    let mut result = Vec::new();
    let mut in_text = false;
    for operation in &page.ops {
        match operation {
            Op::SaveGraphicsState => stack.push((ctm, style.clone())),
            Op::RestoreGraphicsState => {
                if let Some((matrix, saved)) = stack.pop() {
                    ctm = matrix;
                    style = saved;
                }
            }
            Op::SetTransformationMatrix { matrix } => {
                ctm = pdf_matrix_product(ctm, matrix.as_array())
            }
            Op::SetTextMatrix { matrix } => tm = matrix.as_array(),
            Op::StartTextSection => {
                in_text = true;
                tm = identity;
            }
            Op::EndTextSection => in_text = false,
            Op::SetFillColor { col } => style.color = col.clone(),
            Op::SetFont { font, size } => {
                style.size = size.0;
                style.source_font = match font {
                    PdfFontHandle::External(id) => Some(id.clone()),
                    _ => None,
                };
                let (bold, italic) = match font {
                    PdfFontHandle::Builtin(font) => (
                        font.get_font_weight() == "bold",
                        font.get_font_style() == "italic",
                    ),
                    PdfFontHandle::External(id) => {
                        let name = document
                            .resources
                            .fonts
                            .map
                            .get(id)
                            .and_then(|font| font.parsed_font.font_name.as_ref())
                            .map(|name| name.to_ascii_lowercase())
                            .unwrap_or_default();
                        (
                            name.contains("bold") || name.contains("black"),
                            name.contains("italic") || name.contains("oblique"),
                        )
                    }
                };
                style.bold = bold;
                style.italic = italic;
            }
            _ => {}
        }
        let matrix = pdf_matrix_product(ctm, tm);
        style.angle = matrix[1].atan2(matrix[0]);
        style.scale = matrix[0].hypot(matrix[1]).max(0.01);
        if !in_text {
            continue;
        }
        match operation {
            Op::ShowText { items } => {
                for item in items {
                    let count = match item {
                        TextItem::Text(text) => text.chars().filter(|c| *c != ' ').count(),
                        TextItem::GlyphIds(glyphs) => glyphs
                            .iter()
                            .filter(|g| g.cid.as_deref() != Some(" "))
                            .count(),
                        TextItem::Offset(_) => 0,
                    };
                    result.extend(std::iter::repeat_n(style.clone(), count));
                }
            }
            Op::MoveToNextLineShowText { text } | Op::SetSpacingMoveAndShowText { text, .. } => {
                result.extend(std::iter::repeat_n(
                    style.clone(),
                    text.chars().filter(|c| *c != ' ').count(),
                ));
            }
            _ => {}
        }
    }
    result
}

fn parse_pdf(bytes: &[u8]) -> Result<PdfDocument, FileError> {
    let mut warnings = Vec::new();
    let mut document = PdfDocument::parse(bytes, &PdfParseOptions::default(), &mut warnings)
        .map_err(|error| FileError::Message(format!("Unable to parse PDF: {error}")))?;
    if document.pages.is_empty() {
        return Err(FileError::Message(
            "Unable to translate PDF because it contains no pages".to_string(),
        ));
    }
    // The lightweight font parser does not read font names from font programs.
    // Recover BaseFont from the source dictionaries to retain weight/style.
    let source = lopdf::Document::load_mem(bytes)
        .map_err(|error| FileError::Message(format!("Unable to parse PDF fonts: {error}")))?;
    for page in source.get_pages().into_values() {
        for (name, dictionary) in source.get_page_fonts(page).unwrap_or_default() {
            if let Ok(base) = dictionary.get(b"BaseFont").and_then(lopdf::Object::as_name) {
                let id = FontId(String::from_utf8_lossy(&name).into_owned());
                if let Some(font) = document.resources.fonts.map.get_mut(&id) {
                    font.parsed_font.font_name = Some(String::from_utf8_lossy(base).into_owned());
                }
            }
        }
    }
    Ok(document)
}

#[cfg(test)]
fn pdf_page_rotations(bytes: &[u8]) -> Result<Vec<i64>, FileError> {
    let document = lopdf::Document::load_mem(bytes)
        .map_err(|error| FileError::Message(format!("Unable to parse PDF: {error}")))?;
    Ok(document
        .get_pages()
        .into_values()
        .map(|page_id| inherited_pdf_page_number(&document, page_id, b"Rotate").unwrap_or(0))
        .collect())
}

#[cfg(test)]
fn inherited_pdf_page_number(
    document: &lopdf::Document,
    page_id: lopdf::ObjectId,
    key: &[u8],
) -> Option<i64> {
    let mut current_id = page_id;
    for _ in 0..64 {
        let dictionary = document.get_dictionary(current_id).ok()?;
        if let Ok(value) = dictionary.get(key) {
            return value.as_i64().ok();
        }
        current_id = dictionary.get(b"Parent").ok()?.as_reference().ok()?;
    }
    None
}

fn pdf_page_barriers(page: &printpdf::PdfPage) -> Vec<[f32; 4]> {
    let mut ctm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mut stack = Vec::new();
    let mut barriers = Vec::new();
    for operation in &page.ops {
        match operation {
            Op::SaveGraphicsState => stack.push(ctm),
            Op::RestoreGraphicsState => {
                if let Some(saved) = stack.pop() {
                    ctm = saved;
                }
            }
            Op::SetTransformationMatrix { matrix } => {
                ctm = pdf_matrix_product(ctm, matrix.as_array())
            }
            _ => {}
        }
        let mut paths = Vec::new();
        match operation {
            Op::DrawLine { line } => paths.push((line.points.clone(), line.is_closed)),
            Op::DrawPolygon { polygon } => {
                for ring in &polygon.rings {
                    paths.push((ring.points.clone(), true));
                }
            }
            Op::DrawRectangle { rectangle } => {
                let polygon = rectangle.to_polygon();
                for ring in polygon.rings {
                    paths.push((ring.points, true));
                }
            }
            // In the extraction copy all Form XObjects are expanded. Remaining
            // Do operators are images, painted in the PDF unit square by CTM.
            Op::UseXobject { transform, .. } if transform.no_auto_scale => {
                let mut bbox = [
                    f32::INFINITY,
                    f32::INFINITY,
                    f32::NEG_INFINITY,
                    f32::NEG_INFINITY,
                ];
                for (x, y) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
                    let (x, y) = (
                        ctm[0] * x + ctm[2] * y + ctm[4],
                        page.media_box.height.0 - (ctm[1] * x + ctm[3] * y + ctm[5]),
                    );
                    bbox[0] = bbox[0].min(x);
                    bbox[1] = bbox[1].min(y);
                    bbox[2] = bbox[2].max(x);
                    bbox[3] = bbox[3].max(y);
                }
                barriers.extend([
                    [bbox[0], bbox[1], bbox[2], bbox[1]],
                    [bbox[0], bbox[3], bbox[2], bbox[3]],
                    [bbox[0], bbox[1], bbox[0], bbox[3]],
                    [bbox[2], bbox[1], bbox[2], bbox[3]],
                ]);
            }
            _ => {}
        }
        for (mut points, closed) in paths {
            if closed {
                if let Some(first) = points.first().cloned() {
                    points.push(first);
                }
            }
            for pair in points.windows(2) {
                if pair[0].bezier || pair[1].bezier {
                    continue;
                }
                let transform = |point: &printpdf::LinePoint| {
                    let (x, y) = (point.p.x.0, point.p.y.0);
                    (
                        ctm[0] * x + ctm[2] * y + ctm[4],
                        page.media_box.height.0 - (ctm[1] * x + ctm[3] * y + ctm[5]),
                    )
                };
                let (a, b) = (transform(&pair[0]), transform(&pair[1]));
                if (a.0 - b.0).abs() < 0.5 || (a.1 - b.1).abs() < 0.5 {
                    barriers.push([a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)]);
                }
            }
        }
    }
    barriers
}

fn pdf_text_blocks(document: &PdfDocument) -> Vec<PdfTextBlock> {
    let mut blocks = Vec::new();
    for (page_index, page) in document.extract_text_boxes().into_iter().enumerate() {
        let mut styles = pdf_glyph_styles(&document.pages[page_index], document).into_iter();
        let barriers = pdf_page_barriers(&document.pages[page_index]);
        let mut fragments = Vec::new();
        for line in page.lines {
            fragments.extend(split_pdf_line_fragments(line, &mut styles, &barriers));
        }
        fragments.sort_by(|left, right| {
            left.bbox[1]
                .total_cmp(&right.bbox[1])
                .then_with(|| left.bbox[0].total_cmp(&right.bbox[0]))
        });

        let mut page_blocks: Vec<PdfTextBlock> = Vec::new();
        for fragment in fragments {
            let destination = page_blocks
                .iter()
                .enumerate()
                .filter(|(_, block)| pdf_lines_share_paragraph(block, &fragment, &barriers))
                .min_by(|(_, left), (_, right)| {
                    let left_gap = fragment.bbox[1] - left.bbox[3];
                    let right_gap = fragment.bbox[1] - right.bbox[3];
                    left_gap.total_cmp(&right_gap).then_with(|| {
                        (fragment.bbox[0] - left.bbox[0])
                            .abs()
                            .total_cmp(&(fragment.bbox[0] - right.bbox[0]).abs())
                    })
                })
                .map(|(index, _)| index);

            if let Some(index) = destination {
                let block = &mut page_blocks[index];
                let center_delta =
                    ((block.bbox[0] + block.bbox[2]) - (fragment.bbox[0] + fragment.bbox[2])).abs();
                if (block.bbox[0] - fragment.bbox[0]).abs() > block.font_size * 0.3 {
                    if center_delta < block.font_size * 0.6 {
                        block.alignment = PdfTextAlignment::Center;
                    } else if (block.bbox[2] - fragment.bbox[2]).abs() < block.font_size * 0.3 {
                        block.alignment = PdfTextAlignment::Right;
                    }
                }
                block.line_height = (fragment.bbox[3] - block.bbox[3]).max(block.line_height);
                block.bbox = pdf_bbox_union(block.bbox, fragment.bbox);
                block.font_size = block.font_size.max(fragment.font_size);
                block.text.push(' ');
                block.text.push_str(&fragment.text);
            } else {
                page_blocks.push(PdfTextBlock {
                    page_index,
                    bbox: fragment.bbox,
                    font_size: fragment.font_size,
                    text: fragment.text,
                    style: fragment.style.clone(),
                    line_height: fragment.font_size * PDF_LINE_HEIGHT_FACTOR,
                    alignment: if fragment.style.angle.abs() < 0.01
                        && ((fragment.bbox[0] + fragment.bbox[2]) - page.width).abs()
                            < fragment.font_size
                    {
                        PdfTextAlignment::Center
                    } else {
                        PdfTextAlignment::Left
                    },
                });
            }
        }
        blocks.extend(page_blocks);
    }
    blocks
}

fn split_pdf_line_fragments(
    line: printpdf::text_boxes::TextLine,
    styles: &mut impl Iterator<Item = PdfTextStyle>,
    barriers: &[[f32; 4]],
) -> Vec<PdfLineFragment> {
    let mut fragments: Vec<PdfLineFragment> = Vec::new();
    for (word_index, word) in line.words.into_iter().enumerate() {
        for (glyph_index, glyph) in word.glyphs.into_iter().enumerate() {
            let style = styles.next().unwrap_or_default();
            if glyph.text.trim().is_empty() {
                continue;
            }
            let bbox = pdf_local_bbox(glyph.bbox, style.angle);
            let font_size = style.size * style.scale;
            let starts_new_fragment = fragments.last().is_some_and(|fragment| {
                style != fragment.style
                    || bbox[0] - fragment.bbox[2]
                        > font_size.max(fragment.font_size) * PDF_COLUMN_GAP_FACTOR
                    || barriers.iter().any(|barrier| {
                        let barrier = pdf_local_bbox(*barrier, style.angle);
                        barrier[2] - barrier[0] < 0.5
                            && barrier[0] > fragment.bbox[2]
                            && barrier[0] < bbox[0]
                            && barrier[1] < bbox[3]
                            && barrier[3] > bbox[1]
                    })
            });
            if starts_new_fragment || fragments.is_empty() {
                fragments.push(PdfLineFragment {
                    bbox,
                    font_size,
                    text: glyph.text,
                    style,
                });
            } else if let Some(fragment) = fragments.last_mut() {
                let gap = bbox[0] - fragment.bbox[2];
                fragment.bbox = pdf_bbox_union(fragment.bbox, bbox);
                fragment.font_size = fragment.font_size.max(font_size);
                // Adjacent text runs (including CJK glyphs) need no invented spaces.
                if gap > font_size * 0.35
                    || (word_index > 0 && glyph_index == 0 && gap > font_size * 0.12)
                {
                    fragment.text.push(' ');
                }
                fragment.text.push_str(&glyph.text);
            }
        }
    }
    fragments
}

fn pdf_lines_share_paragraph(
    block: &PdfTextBlock,
    line: &PdfLineFragment,
    barriers: &[[f32; 4]],
) -> bool {
    if block.style != line.style {
        return false;
    }
    if pdf_starts_list_item(&line.text) {
        return false;
    }
    if barriers.iter().any(|barrier| {
        let barrier = pdf_local_bbox(*barrier, block.style.angle);
        barrier[3] - barrier[1] < 0.5
            && barrier[1] >= block.bbox[3]
            && barrier[1] <= line.bbox[1]
            && barrier[0] < block.bbox[2].min(line.bbox[2])
            && barrier[2] > block.bbox[0].max(line.bbox[0])
    }) {
        return false;
    }
    let font_size = block.font_size.max(line.font_size).max(1.0);
    let vertical_gap = line.bbox[1] - block.bbox[3];
    if !(-font_size * 0.2..=font_size * 0.9).contains(&vertical_gap) {
        return false;
    }
    let font_ratio = block.font_size / line.font_size.max(0.1);
    if !(0.75..=1.34).contains(&font_ratio) {
        return false;
    }
    let overlap = (block.bbox[2].min(line.bbox[2]) - block.bbox[0].max(line.bbox[0])).max(0.0);
    let narrow_width = (block.bbox[2] - block.bbox[0])
        .min(line.bbox[2] - line.bbox[0])
        .max(1.0);
    let left_aligned = (block.bbox[0] - line.bbox[0]).abs() <= font_size * 1.5;
    let previous_line_is_full =
        (block.bbox[2] - block.bbox[0]) >= (line.bbox[2] - line.bbox[0]) * 0.7;
    let centered =
        ((block.bbox[0] + block.bbox[2]) - (line.bbox[0] + line.bbox[2])).abs() <= font_size * 0.6;
    let right_aligned = (block.bbox[2] - line.bbox[2]).abs() <= font_size * 0.3;
    overlap / narrow_width >= 0.6
        && (left_aligned || centered || right_aligned)
        && previous_line_is_full
}

fn pdf_starts_list_item(text: &str) -> bool {
    let text = text.trim_start();
    if text.starts_with(['•', '●', '▪', '◦', '‣'])
        || text.starts_with("- ")
        || text.starts_with("* ")
    {
        return true;
    }
    let rest = text.trim_start_matches(|character: char| character.is_ascii_digit());
    rest.len() < text.len() && (rest.starts_with(". ") || rest.starts_with(") "))
}

fn pdf_bbox_union(left: [f32; 4], right: [f32; 4]) -> [f32; 4] {
    [
        left[0].min(right[0]),
        left[1].min(right[1]),
        left[2].max(right[2]),
        left[3].max(right[3]),
    ]
}

fn render_translated_pdf(
    document: &mut PdfDocument,
    blocks: &[PdfTextBlock],
    translated: &[String],
    source: &[u8],
) -> Result<Vec<u8>, FileError> {
    if blocks.len() != translated.len() {
        return Err(FileError::Message(
            "Unable to build translated PDF because translated text does not match its layout"
                .to_string(),
        ));
    }
    if blocks
        .iter()
        .zip(translated)
        .all(|(block, text)| block.text == *text)
    {
        return Ok(source.to_vec());
    }
    let mut embedded = HashMap::new();
    let mut source_fonts = Vec::new();
    for (block, text) in blocks.iter().zip(translated) {
        let original = block
            .style
            .source_font
            .as_ref()
            .and_then(|id| document.resources.fonts.map.get(id))
            .map(|font| &font.parsed_font)
            .filter(|font| {
                !font.original_bytes.is_empty()
                    && text
                        .chars()
                        .filter(|c| !c.is_control())
                        .all(|c| font.lookup_glyph_index(c as u32).is_some())
            })
            .cloned();
        if let Some(parsed) = original {
            let key = block.style.source_font.clone().unwrap();
            if !embedded.contains_key(&key) {
                let id = document.add_font(&parsed);
                embedded.insert(key.clone(), vec![PdfFont { parsed, id }]);
            }
            source_fonts.push(Some(key));
        } else {
            source_fonts.push(None);
        }
    }
    let mut font_sets = HashMap::new();
    for bold in [false, true] {
        for italic in [false, true] {
            let text = blocks
                .iter()
                .zip(translated)
                .zip(&source_fonts)
                .filter(|((block, _), source)| {
                    source.is_none() && block.style.bold == bold && block.style.italic == italic
                })
                .map(|((_, text), _)| text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if text.is_empty() {
                continue;
            }
            let fonts = load_pdf_fonts_with_style(&text, bold, italic)?
                .into_iter()
                .map(|parsed| {
                    let id = document.add_font(&parsed);
                    PdfFont { parsed, id }
                })
                .collect::<Vec<_>>();
            font_sets.insert((bold, italic), fonts);
        }
    }
    let barriers = document
        .pages
        .iter()
        .map(pdf_page_barriers)
        .collect::<Vec<_>>();
    let layout = blocks
        .iter()
        .map(|block| {
            pdf_available_box(
                block,
                blocks,
                &barriers[block.page_index],
                &document.pages[block.page_index],
            )
        })
        .collect::<Vec<_>>();
    // Generate translation only. The original PDF is edited separately with lopdf.
    for page in &mut document.pages {
        page.ops.clear();
    }
    for (((block, text), bbox), source_font) in
        blocks.iter().zip(translated).zip(layout).zip(source_fonts)
    {
        if text.trim().is_empty() {
            continue;
        }
        let Some(page) = document.pages.get_mut(block.page_index) else {
            return Err(FileError::Message(
                "Unable to build translated PDF because a page is missing".to_string(),
            ));
        };
        let mut fitted = block.clone();
        fitted.bbox = bbox;
        page.ops.extend(render_pdf_block(
            &fitted,
            text,
            page.media_box.height.0,
            match source_font {
                Some(id) => &embedded[&id],
                None => &font_sets[&(block.style.bold, block.style.italic)],
            },
        )?);
    }

    let mut warnings = Vec::new();
    let bytes = document.save(
        &PdfSaveOptions {
            subset_fonts: true,
            ..PdfSaveOptions::default()
        },
        &mut warnings,
    );
    let bytes = crate::pdf::apply_translation(source, &bytes)?;
    if bytes.starts_with(b"%PDF-") {
        Ok(bytes)
    } else {
        Err(FileError::Message(
            "Unable to build translated PDF".to_string(),
        ))
    }
}

fn pdf_available_box(
    block: &PdfTextBlock,
    blocks: &[PdfTextBlock],
    barriers: &[[f32; 4]],
    page: &printpdf::PdfPage,
) -> [f32; 4] {
    // Rotated labels keep their original local bounds; page margins and column
    // neighbors are meaningful in page coordinates only for horizontal text.
    if block.style.angle.abs() > 0.01 {
        return block.bbox;
    }
    let mut bbox = block.bbox;
    let margin = block.bbox[0].clamp(0.0, 24.0);
    let mut right = (page.media_box.x.0 + page.media_box.width.0 - margin).max(bbox[2]);
    let mut bottom = (page.media_box.height.0 - margin).max(bbox[3]);
    for neighbor in blocks
        .iter()
        .filter(|other| other.page_index == block.page_index)
    {
        if std::ptr::eq(block, neighbor) {
            continue;
        }
        if neighbor.bbox[0] >= bbox[2] && neighbor.bbox[1] < bbox[3] && neighbor.bbox[3] > bbox[1] {
            right = right.min((bbox[2] + neighbor.bbox[0]) / 2.0);
        }
    }
    for barrier in barriers {
        if barrier[2] - barrier[0] < 0.5
            && barrier[0] >= bbox[2]
            && barrier[1] < bbox[3]
            && barrier[3] > bbox[1]
        {
            right = right.min(barrier[0] - 2.0);
        }
    }
    if block.alignment != PdfTextAlignment::Left {
        right = bbox[2];
    }
    // Reserve existing text below across the expanded width, including headings,
    // columns and table cells. Expansions never claim another block's space.
    for neighbor in blocks
        .iter()
        .filter(|other| other.page_index == block.page_index)
    {
        if std::ptr::eq(block, neighbor) {
            continue;
        }
        if neighbor.bbox[1] >= bbox[3] && neighbor.bbox[0] < right && neighbor.bbox[2] > bbox[0] {
            bottom = bottom.min(neighbor.bbox[1] - 2.0);
        }
    }
    for barrier in barriers {
        if barrier[3] - barrier[1] < 0.5
            && barrier[1] >= bbox[3]
            && barrier[0] < right
            && barrier[2] > bbox[0]
        {
            bottom = bottom.min(barrier[1] - 2.0);
        }
    }
    bbox[2] = right.max(bbox[2]);
    bbox[3] = bottom.max(bbox[3]);
    bbox
}

fn render_pdf_block(
    block: &PdfTextBlock,
    text: &str,
    page_height: f32,
    fonts: &[PdfFont],
) -> Result<Vec<Op>, FileError> {
    let width = (block.bbox[2] - block.bbox[0]).max(1.0);
    let height = (block.bbox[3] - block.bbox[1]).max(1.0);
    let (font_size, lines) = fit_pdf_text(
        text,
        block.font_size,
        width,
        height,
        block.line_height / block.font_size.max(0.1),
        fonts,
    )?;
    let line_height = font_size * (block.line_height / block.font_size.max(0.1)).max(1.0);
    let mut operations = vec![
        Op::SaveGraphicsState,
        Op::SetFillColor {
            col: block.style.color.clone(),
        },
        Op::StartTextSection,
        Op::SetCharacterSpacing { multiplier: 0.0 },
        Op::SetWordSpacing { pt: Pt(0.0) },
        Op::SetHorizontalScaling { percent: 100.0 },
        Op::SetLineOffset { multiplier: 0.0 },
        Op::SetTextRenderingMode {
            mode: printpdf::TextRenderingMode::Fill,
        },
    ];
    for (line_index, line) in lines.iter().enumerate() {
        let ascent = fonts
            .iter()
            .map(|font| {
                font.parsed.font_metrics.ascent as f32 / font.parsed.units_per_em.max(1) as f32
            })
            .fold(0.0_f32, f32::max);
        let baseline_from_top =
            block.bbox[1] + font_size * ascent + line_index as f32 * line_height;
        let (s, c) = block.style.angle.sin_cos();
        let line_width = line
            .chars()
            .map(|character| pdf_character_width(character, font_size, fonts))
            .sum::<f32>();
        let x = block.bbox[0]
            + match block.alignment {
                PdfTextAlignment::Left => 0.0,
                PdfTextAlignment::Center => (width - line_width) / 2.0,
                PdfTextAlignment::Right => width - line_width,
            };
        operations.push(Op::SetTextMatrix {
            matrix: TextMatrix::Raw([
                c,
                s,
                -s,
                c,
                c * x + s * baseline_from_top,
                page_height - (-s * x + c * baseline_from_top),
            ]),
        });
        for (font_index, run) in pdf_font_runs(line, fonts) {
            operations.push(Op::SetFont {
                font: PdfFontHandle::External(fonts[font_index].id.clone()),
                size: Pt(font_size),
            });
            operations.push(Op::ShowText {
                items: vec![TextItem::Text(run)],
            });
        }
    }
    operations.extend([Op::EndTextSection, Op::RestoreGraphicsState]);
    Ok(operations)
}

fn fit_pdf_text(
    text: &str,
    preferred_font_size: f32,
    width: f32,
    height: f32,
    line_height_factor: f32,
    fonts: &[PdfFont],
) -> Result<(f32, Vec<String>), FileError> {
    let cleaned = clean_pdf_text(text);
    let minimum = (preferred_font_size * 0.55)
        .max(PDF_MIN_FONT_SIZE)
        .min(preferred_font_size);
    let mut font_size = preferred_font_size;
    loop {
        let lines = wrap_pdf_text(&cleaned, font_size, width, fonts);
        let required_height = if lines.is_empty() {
            0.0
        } else {
            let extent = fonts
                .iter()
                .map(|font| {
                    (font.parsed.font_metrics.ascent as f32
                        - font.parsed.font_metrics.descent as f32)
                        / font.parsed.units_per_em.max(1) as f32
                })
                .fold(1.0_f32, f32::max);
            font_size * extent + (lines.len() - 1) as f32 * font_size * line_height_factor.max(1.0)
        };
        let fits_width = lines.iter().all(|line| {
            line.chars()
                .map(|character| pdf_character_width(character, font_size, fonts))
                .sum::<f32>()
                <= width + 0.1
        });
        if required_height <= height + 0.1 && fits_width {
            return Ok((font_size, lines));
        }
        if font_size <= minimum {
            return Err(FileError::Message(format!(
                "Translated PDF text cannot fit its original layout at a readable font size ({minimum:.1}pt). Try translation-only output or a shorter translation."
            )));
        }
        font_size = (font_size - 0.25).max(minimum);
    }
}

fn clean_pdf_text(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\r' | '\t'))
        .map(|character| if character == '\t' { ' ' } else { character })
        .collect()
}

#[cfg(test)]
fn load_pdf_fonts(text: &str) -> Result<Vec<ParsedFont>, FileError> {
    load_pdf_fonts_with_style(text, false, false)
}

fn load_pdf_fonts_with_style(
    text: &str,
    bold: bool,
    italic: bool,
) -> Result<Vec<ParsedFont>, FileError> {
    let required = text
        .chars()
        .filter(|character| !character.is_control())
        .collect::<BTreeSet<_>>();
    if required.is_empty() {
        return Ok(Vec::new());
    }
    let mut database = FontDatabase::new();
    database.load_system_fonts();

    let preferred_families = [
        "Noto Sans CJK SC",
        "Noto Sans SC",
        "Microsoft YaHei",
        "DengXian",
        "SimHei",
        "PingFang SC",
        "Noto Sans CJK JP",
        "Noto Sans JP",
        "Yu Gothic",
        "Meiryo",
        "Hiragino Sans",
        "Noto Sans CJK KR",
        "Noto Sans KR",
        "Malgun Gothic",
        "Apple SD Gothic Neo",
        "Arial Unicode MS",
        "Noto Sans",
        "DejaVu Sans",
        "Liberation Sans",
        "Arial",
    ];
    let mut preferred_ids = Vec::new();
    // A variable font may expose only its default (e.g. Thin) face to fontdb.
    // A family-name match alone must not override the requested text weight.
    let matches_weight = |id| {
        database.face(id).is_some_and(|face| {
            if bold {
                face.weight.0 >= 600
            } else {
                (300..=500).contains(&face.weight.0)
            }
        })
    };
    for family in preferred_families {
        if let Some(id) = database.query(&FontQuery {
            families: &[FontFamily::Name(family)],
            weight: if bold {
                fontdb::Weight::BOLD
            } else {
                fontdb::Weight::NORMAL
            },
            style: if italic { Style::Italic } else { Style::Normal },
            ..FontQuery::default()
        }) {
            if matches_weight(id) && !preferred_ids.contains(&id) {
                preferred_ids.push(id);
            }
        }
    }
    if let Some(id) = database.query(&FontQuery {
        families: &[FontFamily::SansSerif],
        weight: if bold {
            fontdb::Weight::BOLD
        } else {
            fontdb::Weight::NORMAL
        },
        style: if italic { Style::Italic } else { Style::Normal },
        ..FontQuery::default()
    }) {
        if matches_weight(id) && !preferred_ids.contains(&id) {
            preferred_ids.push(id);
        }
    }

    let requested_style = if italic { Style::Italic } else { Style::Normal };
    let requested_weight = if bold {
        fontdb::Weight::BOLD
    } else {
        fontdb::Weight::NORMAL
    };
    let mut fallback_faces = database.faces().collect::<Vec<_>>();
    fallback_faces.sort_by_key(|face| {
        (
            if face.style == requested_style {
                0
            } else if face.style == Style::Normal {
                1
            } else {
                2
            },
            face.weight.0.abs_diff(requested_weight.0),
            face.monospaced,
        )
    });
    let all_ids = fallback_faces
        .into_iter()
        .map(|face| face.id)
        .collect::<Vec<_>>();
    let mut remaining = required;
    let mut used_ids = Vec::new();
    let mut fonts = Vec::new();
    while !remaining.is_empty() {
        let selected = if fonts.is_empty() {
            best_pdf_font(&database, &preferred_ids, &used_ids, &remaining)
                .or_else(|| best_pdf_font(&database, &all_ids, &used_ids, &remaining))
        } else {
            best_pdf_font(&database, &all_ids, &used_ids, &remaining)
        };
        let Some((id, font, covered)) = selected else {
            break;
        };
        used_ids.push(id);
        for character in covered {
            remaining.remove(&character);
        }
        fonts.push(font);
    }

    if !remaining.is_empty() {
        let missing = remaining.iter().take(8).collect::<String>();
        return Err(FileError::Message(format!(
            "Unable to build translated PDF because no installed font supports: {missing}"
        )));
    }
    Ok(fonts)
}

fn best_pdf_font(
    database: &FontDatabase,
    candidates: &[fontdb::ID],
    used_ids: &[fontdb::ID],
    required: &BTreeSet<char>,
) -> Option<(fontdb::ID, ParsedFont, BTreeSet<char>)> {
    let mut best = None;
    for id in candidates
        .iter()
        .copied()
        .filter(|id| !used_ids.contains(id))
    {
        let Some(Some(mut parsed)) = database.with_face_data(id, |bytes, index| {
            let face = crate::pdf::standalone_font_face(bytes, index)?;
            ParsedFont::from_bytes(&face, 0, &mut Vec::new())
        }) else {
            continue;
        };
        parsed.font_name = database.face(id).map(|face| face.post_script_name.clone());
        let covered = required
            .iter()
            .copied()
            .filter(|character| parsed.lookup_glyph_index(*character as u32).is_some())
            .collect::<BTreeSet<_>>();
        if covered.is_empty() {
            continue;
        }
        let is_better = best
            .as_ref()
            .map(
                |(_, _, best_covered): &(fontdb::ID, ParsedFont, BTreeSet<char>)| {
                    covered.len() > best_covered.len()
                },
            )
            .unwrap_or(true);
        if is_better {
            let covers_everything = covered.len() == required.len();
            best = Some((id, parsed, covered));
            if covers_everything {
                break;
            }
        }
    }
    best
}

fn pdf_font_runs(line: &str, fonts: &[PdfFont]) -> Vec<(usize, String)> {
    let mut runs: Vec<(usize, String)> = Vec::new();
    for character in line.chars() {
        let font_index = fonts
            .iter()
            .position(|font| font.parsed.lookup_glyph_index(character as u32).is_some())
            .unwrap_or(0);
        if let Some((last_font, text)) = runs.last_mut() {
            if *last_font == font_index {
                text.push(character);
                continue;
            }
        }
        runs.push((font_index, character.to_string()));
    }
    runs
}

fn wrap_pdf_text(text: &str, font_size: f32, maximum_width: f32, fonts: &[PdfFont]) -> Vec<String> {
    let mut lines = Vec::new();
    for source_line in text.split('\n') {
        let characters = source_line
            .trim_end_matches('\r')
            .chars()
            .filter(|character| !character.is_control())
            .collect::<Vec<_>>();
        if characters.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut start = 0;
        while start < characters.len() {
            let mut width = 0.0;
            let mut end = start;
            let mut whitespace = None;
            while end < characters.len() {
                let next_width = pdf_character_width(characters[end], font_size, fonts);
                if end > start && width + next_width > maximum_width {
                    break;
                }
                width += next_width;
                if characters[end].is_whitespace() {
                    whitespace = Some(end);
                }
                end += 1;
            }
            if end == start {
                end += 1;
            }
            if end < characters.len() {
                if let Some(index) = whitespace.filter(|index| *index > start) {
                    end = index;
                } else {
                    // Keep CJK closing punctuation off the next line and opening
                    // punctuation off the previous line, without exceeding width.
                    let mut boundary = end;
                    while boundary > start + 1
                        && ("，。！？；：、）》】」』〕〉…,.!?;:%".contains(characters[boundary])
                            || "（《【「『〔〈(".contains(characters[boundary - 1]))
                    {
                        boundary -= 1;
                    }
                    end = boundary;
                }
            }
            lines.push(characters[start..end].iter().collect());
            start = end;
            while start < characters.len() && characters[start].is_whitespace() {
                start += 1;
            }
        }
    }
    lines
}

fn pdf_character_width(character: char, font_size: f32, fonts: &[PdfFont]) -> f32 {
    fonts
        .iter()
        .find_map(|font| {
            let glyph = font.parsed.lookup_glyph_index(character as u32)?;
            let width = font.parsed.get_glyph_width(glyph)? as f32;
            let units_per_em = font.parsed.units_per_em.max(1) as f32;
            Some(width / units_per_em * font_size)
        })
        .unwrap_or(font_size * 0.5)
}

fn extract_summary_text(filename: &str, bytes: &[u8], maximum: usize) -> Result<String, FileError> {
    let extension = filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut text = match extension.as_str() {
        "pdf" => parse_pdf(&crate::pdf::extraction_copy(bytes)?)?
            .extract_text()
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n"),
        "docx" | "pptx" | "xlsx" => {
            let mut output = String::new();
            for (name, is_directory, contents) in unpack_office(bytes)? {
                if is_directory || !name.ends_with(".xml") {
                    continue;
                }
                let (block, text_tag) = if extension == "docx" && name.starts_with("word/") {
                    (b"w:p".as_slice(), b"w:t".as_slice())
                } else if (extension == "pptx" && name.starts_with("ppt/"))
                    || (extension == "xlsx" && name.starts_with("xl/drawings/"))
                {
                    (b"a:p".as_slice(), b"a:t".as_slice())
                } else if extension == "xlsx" && name == "xl/sharedStrings.xml" {
                    (b"si".as_slice(), b"t".as_slice())
                } else if extension == "xlsx" && name.starts_with("xl/worksheets/") {
                    (b"is".as_slice(), b"t".as_slice())
                } else {
                    continue;
                };
                for block in extract_xml_blocks(&contents, block, text_tag)? {
                    if !block.text.trim().is_empty() {
                        output.push_str(&block.text);
                        output.push('\n');
                    }
                }
            }
            output
        }
        "json" => {
            let value: Value = serde_json::from_slice(bytes)?;
            let mut values = Vec::new();
            let mut skipped = 0;
            collect_json_texts(&value, &mut values, &mut skipped);
            values.join("\n")
        }
        _ => std::str::from_utf8(bytes)
            .map_err(|_| FileError::Message("Only UTF-8 text files can be summarized".to_string()))?
            .to_string(),
    };
    text = text.chars().take(maximum.max(1)).collect();
    Ok(text)
}

#[allow(clippy::too_many_arguments)]
async fn translate_caption(
    store: &AppStore,
    scheduler: &AiScheduler,
    bytes: &[u8],
    options: &TranslateRequest,
    output_mode: FileOutputMode,
    counts: &mut Counts,
    progress: &ProgressCallback,
    state: &mut BatchState,
) -> Result<Vec<u8>, FileError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| FileError::Message("Only UTF-8 caption files are supported".to_string()))?;
    let should_translate = |line: &str| {
        let trimmed = line.trim();
        !trimmed.is_empty()
            && !trimmed.starts_with("WEBVTT")
            && !trimmed.contains("-->")
            && trimmed.parse::<usize>().is_err()
    };
    Ok(translate_lines(
        store,
        scheduler,
        text,
        options,
        output_mode,
        counts,
        progress,
        state,
        should_translate,
    )
    .await?
    .into_bytes())
}

#[allow(clippy::too_many_arguments)]
async fn translate_lines<F>(
    store: &AppStore,
    scheduler: &AiScheduler,
    text: &str,
    options: &TranslateRequest,
    output_mode: FileOutputMode,
    counts: &mut Counts,
    progress: &ProgressCallback,
    state: &mut BatchState,
    should_translate: F,
) -> Result<String, FileError>
where
    F: Fn(&str) -> bool,
{
    let mut lines = Vec::new();
    let mut candidates = Vec::new();
    for chunk in text.split_inclusive('\n') {
        let (line, suffix) = match chunk.strip_suffix('\n') {
            Some(line) => (line, "\n"),
            None => (chunk, ""),
        };
        let translate = should_translate(line);
        if translate {
            candidates.push(line.to_string());
        } else {
            counts.skipped += 1;
        }
        lines.push((line.to_string(), suffix.to_string(), translate));
    }
    let translations = translate_fragments(
        store,
        scheduler,
        candidates,
        options,
        output_mode,
        counts,
        progress,
        state,
    )
    .await?;
    let mut translated = String::with_capacity(text.len());
    let mut candidate_index = 0;
    for (line, suffix, should_translate) in lines {
        if should_translate {
            translated.push_str(&translations[candidate_index]);
            candidate_index += 1;
        } else {
            translated.push_str(&line);
        }
        translated.push_str(&suffix);
    }
    Ok(translated)
}

#[allow(clippy::too_many_arguments)]
async fn translate_csv(
    store: &AppStore,
    scheduler: &AiScheduler,
    bytes: &[u8],
    options: &TranslateRequest,
    output_mode: FileOutputMode,
    counts: &mut Counts,
    progress: &ProgressCallback,
    state: &mut BatchState,
) -> Result<Vec<u8>, FileError> {
    let delimiter = if bytes.contains(&b'\t') && !bytes.contains(&b',') {
        b'\t'
    } else {
        b','
    };
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(delimiter)
        .from_reader(bytes);
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut candidates = Vec::new();
    for row in reader.records() {
        let row = row?;
        let values = row.iter().map(str::to_string).collect::<Vec<_>>();
        for value in &values {
            if value.trim().is_empty() {
                counts.skipped += 1;
            } else {
                candidates.push(value.clone());
            }
        }
        rows.push(values);
    }
    let translations = translate_fragments(
        store,
        scheduler,
        candidates,
        options,
        output_mode,
        counts,
        progress,
        state,
    )
    .await?;
    let mut translation_index = 0;
    let mut writer = csv::WriterBuilder::new()
        .delimiter(delimiter)
        .from_writer(Vec::new());
    for row in &mut rows {
        for value in &mut *row {
            if !value.trim().is_empty() {
                *value = translations[translation_index].clone();
                translation_index += 1;
            }
        }
        writer.write_record(row)?;
    }
    writer.flush()?;
    writer
        .into_inner()
        .map_err(|error| FileError::Io(error.into_error()))
}

#[allow(clippy::too_many_arguments)]
async fn translate_json(
    store: &AppStore,
    scheduler: &AiScheduler,
    bytes: &[u8],
    options: &TranslateRequest,
    output_mode: FileOutputMode,
    counts: &mut Counts,
    progress: &ProgressCallback,
    state: &mut BatchState,
) -> Result<Vec<u8>, FileError> {
    let mut json: Value = serde_json::from_slice(bytes)?;
    let mut candidates = Vec::new();
    collect_json_texts(&json, &mut candidates, &mut counts.skipped);
    let translations = translate_fragments(
        store,
        scheduler,
        candidates,
        options,
        output_mode,
        counts,
        progress,
        state,
    )
    .await?;
    let mut translation_index = 0;
    apply_json_texts(&mut json, &translations, &mut translation_index);
    Ok(serde_json::to_vec_pretty(&json)?)
}

fn collect_json_texts(value: &Value, texts: &mut Vec<String>, skipped: &mut usize) {
    match value {
        Value::String(text) if !text.trim().is_empty() => texts.push(text.clone()),
        Value::String(_) | Value::Null | Value::Bool(_) | Value::Number(_) => *skipped += 1,
        Value::Array(items) => {
            for item in items {
                collect_json_texts(item, texts, skipped);
            }
        }
        Value::Object(entries) => {
            for item in entries.values() {
                collect_json_texts(item, texts, skipped);
            }
        }
    }
}

fn apply_json_texts(value: &mut Value, translations: &[String], translation_index: &mut usize) {
    match value {
        Value::String(text) if !text.trim().is_empty() => {
            *text = translations[*translation_index].clone();
            *translation_index += 1;
        }
        Value::Array(items) => {
            for item in items {
                apply_json_texts(item, translations, translation_index);
            }
        }
        Value::Object(entries) => {
            for item in entries.values_mut() {
                apply_json_texts(item, translations, translation_index);
            }
        }
        _ => {}
    }
}

struct PlannedFragment {
    source: String,
    parts: Vec<FragmentPart>,
}

struct FragmentPart {
    result_index: usize,
    leading: String,
    trailing: String,
}

#[derive(Clone)]
struct PendingPart {
    result_index: usize,
    text: String,
    force_single: bool,
}

#[allow(clippy::too_many_arguments)]
async fn translate_fragments(
    store: &AppStore,
    scheduler: &AiScheduler,
    fragments: Vec<String>,
    options: &TranslateRequest,
    output_mode: FileOutputMode,
    counts: &mut Counts,
    progress: &ProgressCallback,
    state: &mut BatchState,
) -> Result<Vec<String>, FileError> {
    ensure_not_cancelled(&state.cancel)?;
    let provider = store
        .provider(&options.provider_id)
        .filter(|provider| provider.enabled)
        .ok_or_else(|| {
            FileError::Message("Selected AI provider does not exist or is disabled".to_string())
        })?;
    let maximum = (provider.context_size / 4).max(256);
    // Without a template, send raw segments individually so batch formatting
    // instructions cannot silently add a translation prompt of their own.
    let maximum_segments = if options.prompt_id.as_deref().is_none_or(str::is_empty) {
        1
    } else {
        provider.max_segments.max(1)
    };
    let max_concurrent = provider.max_concurrent.max(1);
    let file_concurrent = if provider.text_translation_model && max_concurrent > 1 {
        max_concurrent - 1
    } else {
        max_concurrent
    };
    let mut planned = Vec::with_capacity(fragments.len());
    let mut pending = Vec::new();
    for source in fragments {
        let (_, content, _) = surrounding_whitespace(&source);
        if !needs_translation(content) {
            counts.skipped += 1;
            planned.push(PlannedFragment {
                source,
                parts: Vec::new(),
            });
            continue;
        }
        let mut parts = Vec::new();
        for chunk in split_chunks(content, maximum) {
            let (leading, text, trailing) = surrounding_whitespace(&chunk);
            if text.is_empty() {
                continue;
            }
            let result_index = pending.len();
            pending.push(PendingPart {
                result_index,
                text: text.to_string(),
                force_single: contains_segment_separator(text),
            });
            parts.push(FragmentPart {
                result_index,
                leading: leading.to_string(),
                trailing: trailing.to_string(),
            });
        }
        planned.push(PlannedFragment { source, parts });
    }

    if pending.is_empty() {
        report(counts, progress);
        return Ok(planned
            .into_iter()
            .map(|fragment| fragment.source)
            .collect());
    }

    let pending_count = pending.len();
    let batches = make_batches(pending, maximum, maximum_segments, file_concurrent);
    counts.stage = "translating".to_string();
    counts.total_segments += planned
        .iter()
        .map(|fragment| fragment.parts.len())
        .sum::<usize>();
    counts.total_batches += batches.len();
    report(counts, progress);

    let mut results = vec![String::new(); pending_count];
    let (event_sender, mut event_receiver) = mpsc::unbounded_channel();
    let mut jobs = JoinSet::new();
    let mut pending_jobs = 0usize;
    let allocator = Arc::new(std::sync::atomic::AtomicUsize::new(batches.len()));
    for (batch_id, batch) in batches.into_iter().enumerate() {
        let indexes = batch
            .iter()
            .map(|part| part.result_index)
            .collect::<Vec<_>>();
        let force_single = batch.iter().any(|part| part.force_single);
        let cached = state.cached_batches.get(&batch_id).cloned();
        if !force_single {
            if let Some(CachedBatch::Text(translations)) = cached {
                if translations.len() == indexes.len() {
                    for (index, translation) in indexes.into_iter().zip(translations) {
                        results[index] = translation;
                        counts.translated += 1;
                    }
                    counts.completed_batches += 1;
                    report(counts, progress);
                    continue;
                }
                state.cached_batches.remove(&batch_id);
            } else if cached.is_some() {
                state.cached_batches.remove(&batch_id);
            }
        }

        pending_jobs += 1;
        let event_sender_for_job = event_sender.clone();
        let allocator_for_job = allocator.clone();
        let cancel = state.cancel.clone();
        jobs.spawn(process_batch_tree(
            scheduler.clone(),
            options.clone(),
            batch_id,
            batch,
            force_single,
            event_sender_for_job,
            allocator_for_job,
            cancel,
        ));
    }
    drop(event_sender);
    while pending_jobs > 0 {
        ensure_not_cancelled(&state.cancel)?;
        tokio::select! {
            event = event_receiver.recv() => {
                if let Some(event) = event {
                    match event {
                        BatchEvent::Created => {
                            counts.total_batches += 1;
                            report(counts, progress);
                        }
                        BatchEvent::Stream { batch_id, batch_segments, text } => {
                            counts.streaming_batch = Some(batch_id);
                            counts.streaming_segments = partial_batch_item_count(&text);
                            counts.streaming_batch_segments = batch_segments;
                            counts.streaming_text = Some(stream_preview(&text));
                            report(counts, progress);
                        }
                    }
                }
            }
            result = jobs.join_next() => {
                let result = result.ok_or_else(|| FileError::Message("Translation batch task ended unexpectedly".to_string()))?;
                let outcomes = result.map_err(|error| FileError::Message(format!("Translation batch task failed: {error}")))??;
                for outcome in outcomes {
                    apply_batch_result(state, counts, progress, outcome, &mut results);
                }
                pending_jobs -= 1;
            }
        }
    }
    counts.streaming_batch = None;
    counts.streaming_segments = 0;
    counts.streaming_batch_segments = 0;
    counts.streaming_text = None;
    report(counts, progress);

    let mut output = Vec::with_capacity(planned.len());
    for fragment in planned {
        if fragment.parts.is_empty() {
            output.push(fragment.source);
            continue;
        }
        let translated = fragment
            .parts
            .iter()
            .map(|part| {
                format!(
                    "{}{}{}",
                    part.leading, results[part.result_index], part.trailing
                )
            })
            .collect::<String>();
        output.push(render_fragment(&fragment.source, &translated, output_mode));
    }
    Ok(output)
}

enum BatchEvent {
    Created,
    Stream {
        batch_id: usize,
        batch_segments: usize,
        text: String,
    },
}

struct BatchOutcome {
    batch_id: usize,
    indexes: Vec<usize>,
    texts: Vec<String>,
    attempts: usize,
    result: Result<Vec<String>, String>,
}

#[allow(clippy::too_many_arguments)]
async fn process_batch_tree(
    scheduler: AiScheduler,
    options: TranslateRequest,
    batch_id: usize,
    batch: Vec<PendingPart>,
    force_single: bool,
    event_sender: mpsc::UnboundedSender<BatchEvent>,
    allocator: Arc<std::sync::atomic::AtomicUsize>,
    cancel: CancellationToken,
) -> Result<Vec<BatchOutcome>, FileError> {
    let mut queue = vec![(batch_id, batch, force_single)];
    let mut outcomes = Vec::new();
    while let Some((current_id, current, force_single)) = queue.pop() {
        ensure_not_cancelled(&cancel)?;
        let indexes = current
            .iter()
            .map(|part| part.result_index)
            .collect::<Vec<_>>();
        let texts = current
            .iter()
            .map(|part| part.text.clone())
            .collect::<Vec<_>>();
        if force_single || texts.len() == 1 {
            let mut request = options.clone();
            request.text = texts[0].clone();
            let mut attempts = 0;
            let result = loop {
                ensure_not_cancelled(&cancel)?;
                attempts += 1;
                let result = tokio::select! {
                    result = scheduler.translate(request.clone(), Priority::Low) => result.map(|value| vec![value.translated_text]),
                    _ = cancel.cancelled() => return Err(FileError::Cancelled),
                };
                match result {
                    Ok(result) => break Ok(result),
                    Err(_error) if attempts < 3 => {
                        sleep(Duration::from_millis((250 * attempts as u64).min(2_000))).await
                    }
                    Err(error) => break Err(error.to_string()),
                }
            };
            outcomes.push(BatchOutcome {
                batch_id: current_id,
                indexes,
                texts,
                attempts,
                result,
            });
            continue;
        }

        let mut attempts = 0;
        let result = loop {
            ensure_not_cancelled(&cancel)?;
            attempts += 1;
            let stream_sender = event_sender.clone();
            let stream_id = current_id;
            let stream_segments = texts.len();
            let callback: StreamCallback = Arc::new(move |text| {
                let _ = stream_sender.send(BatchEvent::Stream {
                    batch_id: stream_id,
                    batch_segments: stream_segments,
                    text,
                });
            });
            let result = tokio::select! {
                result = scheduler.translate_batch(options.clone(), texts.clone(), Priority::Low, Some(callback)) => result,
                _ = cancel.cancelled() => return Err(FileError::Cancelled),
            };
            match result {
                Ok(result) if result.len() == texts.len() => break Ok(result),
                Err(AiError::BatchFormat(error)) if texts.len() > 1 => {
                    let middle = current.len() / 2;
                    let left = current[..middle].to_vec();
                    let right = current[middle..].to_vec();
                    let left_id = allocator.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let right_id = allocator.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let _ = event_sender.send(BatchEvent::Created);
                    let _ = event_sender.send(BatchEvent::Created);
                    queue.push((right_id, right, false));
                    queue.push((left_id, left, false));
                    break Err(format!("split:{error}"));
                }
                Ok(result) => {
                    break Err(format!(
                        "AI returned {} translations for {} input segments",
                        result.len(),
                        texts.len()
                    ))
                }
                Err(_error) if attempts < 3 => {
                    sleep(Duration::from_millis((250 * attempts as u64).min(2_000))).await
                }
                Err(error) => break Err(error.to_string()),
            }
        };
        if result
            .as_ref()
            .err()
            .is_some_and(|error| error.starts_with("split:"))
        {
            continue;
        }
        outcomes.push(BatchOutcome {
            batch_id: current_id,
            indexes,
            texts,
            attempts,
            result,
        });
    }
    Ok(outcomes)
}

fn make_batches(
    pending: Vec<PendingPart>,
    maximum: usize,
    maximum_segments: usize,
    max_concurrent: usize,
) -> Vec<Vec<PendingPart>> {
    let mut forced = Vec::new();
    let mut regular = Vec::new();
    for part in pending {
        if part.force_single {
            forced.push(vec![part]);
        } else {
            regular.push(part);
        }
    }
    let desired = if !regular.is_empty()
        && regular.len() <= maximum_segments.saturating_mul(max_concurrent)
    {
        max_concurrent.min(regular.len()).max(1)
    } else {
        0
    };
    let mut groups = Vec::new();
    if desired > 0 {
        let mut start = 0;
        for group_index in 0..desired {
            let remaining = regular.len().saturating_sub(start);
            let groups_left = desired - group_index;
            let take = remaining.div_ceil(groups_left);
            groups.push(regular[start..start + take].to_vec());
            start += take;
        }
    } else if !regular.is_empty() {
        groups.push(regular);
    }
    let mut packed = Vec::new();
    for group in groups.into_iter().chain(forced) {
        let mut current = Vec::new();
        let mut chars = 0;
        for part in group {
            let part_chars = part.text.chars().count();
            if !current.is_empty()
                && (chars + part_chars > maximum || current.len() >= maximum_segments)
            {
                packed.push(current);
                current = Vec::new();
                chars = 0;
            }
            chars += part_chars;
            current.push(part);
        }
        if !current.is_empty() {
            packed.push(current);
        }
    }
    packed
}

fn apply_batch_result(
    state: &mut BatchState,
    counts: &mut Counts,
    progress: &ProgressCallback,
    outcome: BatchOutcome,
    results: &mut [String],
) {
    let BatchOutcome {
        batch_id,
        indexes,
        texts,
        attempts,
        result,
    } = outcome;
    counts.streaming_batch = None;
    counts.streaming_segments = 0;
    counts.streaming_batch_segments = 0;
    counts.streaming_text = None;
    match result {
        Ok(translations) => {
            for (index, translation) in indexes.iter().copied().zip(&translations) {
                results[index] = translation.clone();
                counts.translated += 1;
            }
            state
                .cached_batches
                .insert(batch_id, CachedBatch::Text(translations));
        }
        Err(error) => {
            for (index, text) in indexes.into_iter().zip(&texts) {
                results[index] = text.clone();
            }
            counts.failed += texts.len();
            state.failed_batches.push(FileBatchFailure {
                id: batch_id,
                segment_count: texts.len(),
                attempts,
                error,
            });
        }
    }
    counts.completed_batches += 1;
    report(counts, progress);
}

fn stream_preview(text: &str) -> String {
    const MAX_STREAM_PREVIEW_CHARS: usize = 4_000;
    let total = text.chars().count();
    if total <= MAX_STREAM_PREVIEW_CHARS {
        text.to_string()
    } else {
        text.chars()
            .skip(total - MAX_STREAM_PREVIEW_CHARS)
            .collect()
    }
}

fn partial_batch_item_count(text: &str) -> usize {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let content = normalized.trim();
    if content.is_empty() {
        return 0;
    }
    let separator = SEGMENT_SEPARATOR.trim();
    let completed = content.match_indices(separator).count();
    if content.ends_with(separator) {
        completed
    } else {
        completed + 1
    }
}

fn render_fragment(source: &str, translated: &str, output_mode: FileOutputMode) -> String {
    if output_mode == FileOutputMode::Translated {
        return restore_surrounding_whitespace(source, translated);
    }
    let (leading, content, trailing) = surrounding_whitespace(source);
    if content.is_empty() {
        source.to_string()
    } else {
        format!(
            "{}{}\n{}{}{}",
            leading,
            content,
            leading,
            translated.trim(),
            trailing
        )
    }
}

fn restore_surrounding_whitespace(source: &str, translated: &str) -> String {
    let (leading, _, trailing) = surrounding_whitespace(source);
    format!("{}{}{}", leading, translated, trailing)
}

fn surrounding_whitespace(value: &str) -> (&str, &str, &str) {
    if value.trim().is_empty() {
        return (value, "", "");
    }
    let content_start = value.len() - value.trim_start().len();
    let content_end = value.trim_end().len();
    (
        &value[..content_start],
        &value[content_start..content_end],
        &value[content_end..],
    )
}

fn split_chunks(text: &str, maximum: usize) -> Vec<String> {
    if text.chars().count() <= maximum {
        return vec![text.to_string()];
    }
    let mut output = Vec::new();
    let mut remaining = text;
    while remaining.chars().count() > maximum {
        let boundary = remaining
            .char_indices()
            .nth(maximum)
            .map(|(index, _)| index)
            .unwrap_or(remaining.len());
        let candidate = &remaining[..boundary];
        let split = candidate
            .rfind(|character: char| {
                character == '\n'
                    || character == ' '
                    || character == '。'
                    || character == '.'
                    || character == '!'
                    || character == '?'
            })
            .filter(|index| *index > boundary / 3)
            .map(|index| index + remaining[index..].chars().next().unwrap().len_utf8())
            .unwrap_or(boundary);
        output.push(remaining[..split].to_string());
        remaining = &remaining[split..];
    }
    if !remaining.is_empty() {
        output.push(remaining.to_string());
    }
    output
}

#[derive(Clone, Copy)]
enum OfficeKind {
    Word,
    PowerPoint,
    Excel,
}

#[allow(clippy::too_many_arguments)]
async fn translate_office(
    store: &AppStore,
    scheduler: &AiScheduler,
    bytes: &[u8],
    options: &TranslateRequest,
    output_mode: FileOutputMode,
    kind: OfficeKind,
    counts: &mut Counts,
    progress: &ProgressCallback,
    state: &mut BatchState,
) -> Result<Vec<u8>, FileError> {
    let entries = unpack_office(bytes)?;
    let mut translated_entries = Vec::with_capacity(entries.len());
    for (name, is_directory, contents) in entries {
        if is_directory {
            translated_entries.push((name, true, Vec::new()));
            continue;
        }
        let translated = match kind {
            OfficeKind::Word if name.starts_with("word/") && name.ends_with(".xml") => {
                translate_xml_document(
                    store,
                    scheduler,
                    &contents,
                    options,
                    output_mode,
                    b"w:p",
                    b"w:t",
                    counts,
                    progress,
                    state,
                )
                .await?
            }
            OfficeKind::PowerPoint if name.starts_with("ppt/") && name.ends_with(".xml") => {
                translate_xml_document(
                    store,
                    scheduler,
                    &contents,
                    options,
                    output_mode,
                    b"a:p",
                    b"a:t",
                    counts,
                    progress,
                    state,
                )
                .await?
            }
            OfficeKind::Excel if name.starts_with("xl/drawings/") && name.ends_with(".xml") => {
                translate_xml_document(
                    store,
                    scheduler,
                    &contents,
                    options,
                    output_mode,
                    b"a:p",
                    b"a:t",
                    counts,
                    progress,
                    state,
                )
                .await?
            }
            OfficeKind::Excel if name == "xl/sharedStrings.xml" => {
                translate_xml_document(
                    store,
                    scheduler,
                    &contents,
                    options,
                    output_mode,
                    b"si",
                    b"t",
                    counts,
                    progress,
                    state,
                )
                .await?
            }
            OfficeKind::Excel if name.starts_with("xl/worksheets/") && name.ends_with(".xml") => {
                translate_xml_document(
                    store,
                    scheduler,
                    &contents,
                    options,
                    output_mode,
                    b"is",
                    b"t",
                    counts,
                    progress,
                    state,
                )
                .await?
            }
            _ if is_office_image(kind, &name) && provider_supports_images(store, options) => {
                translate_image_file(
                    store, scheduler, &name, contents, options, counts, progress, state,
                )
                .await?
            }
            _ => {
                if is_office_image(kind, &name) {
                    counts.skipped += 1;
                }
                contents
            }
        };
        translated_entries.push((name, false, translated));
    }

    counts.stage = "packing".to_string();
    report(counts, progress);
    let mut output = ZipWriter::new(Cursor::new(Vec::new()));
    let file_options = FileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, is_directory, contents) in translated_entries {
        if is_directory {
            output.add_directory(name, file_options)?;
            continue;
        }
        output.start_file(name, file_options)?;
        output.write_all(&contents)?;
    }
    Ok(output.finish()?.into_inner())
}

fn needs_translation(text: &str) -> bool {
    let text = text.trim();
    if !text.chars().any(char::is_alphabetic) {
        return false;
    }
    // Single Latin letters are typically labels, variables or column names.
    // Keep single ideographs and words in other scripts eligible for translation.
    let mut letters = text.chars().filter(|c| c.is_alphabetic());
    if letters.next().is_some_and(|c| c.is_ascii_alphabetic()) && letters.next().is_none() {
        return false;
    }
    !(text.split_whitespace().count() == 1
        && (text.starts_with("https://") || text.starts_with("http://")))
}

fn is_office_image(kind: OfficeKind, name: &str) -> bool {
    let media_prefix = match kind {
        OfficeKind::Word => "word/media/",
        OfficeKind::PowerPoint => "ppt/media/",
        OfficeKind::Excel => "xl/media/",
    };
    let extension = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    name.starts_with(media_prefix) && matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "webp")
}

fn provider_supports_images(store: &AppStore, options: &TranslateRequest) -> bool {
    store.snapshot().providers.iter().any(|provider| {
        provider.id == options.provider_id && provider.enabled && provider.supports_images
    })
}

fn unpack_office(bytes: &[u8]) -> Result<Vec<(String, bool, Vec<u8>)>, FileError> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    let mut entries = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let mut source = archive.by_index(index)?;
        let name = source.name().to_string();
        let is_directory = source.is_dir();
        let mut contents = Vec::new();
        if !is_directory {
            source.read_to_end(&mut contents)?;
        }
        entries.push((name, is_directory, contents));
    }
    Ok(entries)
}

struct XmlBlock {
    text: String,
    ranges: Vec<std::ops::Range<usize>>,
}

fn extract_xml_blocks(
    bytes: &[u8],
    block_tag: &[u8],
    text_tag: &[u8],
) -> Result<Vec<XmlBlock>, FileError> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut blocks: Vec<XmlBlock> = Vec::new();
    let mut stack = Vec::new();
    let mut inside_text = false;
    // Word can contain both WordprocessingML and DrawingML text boxes.
    let is_block = |name: &[u8]| name == block_tag || (block_tag == b"w:p" && name == b"a:p");
    let is_text = |name: &[u8]| name == text_tag || (text_tag == b"w:t" && name == b"a:t");
    loop {
        let before = reader.buffer_position() as usize;
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) if is_block(start.name().as_ref()) => {
                stack.push(blocks.len());
                blocks.push(XmlBlock {
                    text: String::new(),
                    ranges: Vec::new(),
                });
            }
            Event::End(end) if is_block(end.name().as_ref()) => {
                stack.pop();
            }
            Event::Start(start) if is_text(start.name().as_ref()) => inside_text = true,
            Event::End(end) if is_text(end.name().as_ref()) => inside_text = false,
            Event::Text(text) if inside_text && !stack.is_empty() => {
                let raw = std::str::from_utf8(text.as_ref())
                    .map_err(|error| FileError::Message(error.to_string()))?;
                let block = &mut blocks[*stack.last().unwrap()];
                block.text.push_str(
                    &unescape(raw).map_err(|error| FileError::Message(error.to_string()))?,
                );
                block.ranges.push(before..reader.buffer_position() as usize);
            }
            Event::CData(text) if inside_text && !stack.is_empty() => {
                let block = &mut blocks[*stack.last().unwrap()];
                block.text.push_str(
                    std::str::from_utf8(text.as_ref())
                        .map_err(|error| FileError::Message(error.to_string()))?,
                );
                block.ranges.push(before..reader.buffer_position() as usize);
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok(blocks)
}

fn replace_xml_blocks(bytes: &[u8], blocks: &[XmlBlock], translations: &[String]) -> Vec<u8> {
    let mut edits = Vec::new();
    for (block, translated) in blocks.iter().zip(translations) {
        if block.text == *translated {
            continue;
        }
        for (index, range) in block.ranges.iter().enumerate() {
            edits.push((
                range.clone(),
                if index == 0 {
                    escape(translated).into_owned()
                } else {
                    String::new()
                },
            ));
        }
    }
    edits.sort_by_key(|(range, _)| range.start);
    let mut output = Vec::with_capacity(bytes.len());
    let mut cursor = 0;
    for (range, text) in edits {
        output.extend_from_slice(&bytes[cursor..range.start]);
        output.extend_from_slice(text.as_bytes());
        cursor = range.end;
    }
    output.extend_from_slice(&bytes[cursor..]);
    output
}

#[allow(clippy::too_many_arguments)]
async fn translate_xml_document(
    store: &AppStore,
    scheduler: &AiScheduler,
    bytes: &[u8],
    options: &TranslateRequest,
    output_mode: FileOutputMode,
    block_tag: &[u8],
    text_tag: &[u8],
    counts: &mut Counts,
    progress: &ProgressCallback,
    state: &mut BatchState,
) -> Result<Vec<u8>, FileError> {
    let blocks = extract_xml_blocks(bytes, block_tag, text_tag)?;
    let sources = blocks
        .iter()
        .map(|block| block.text.clone())
        .collect::<Vec<_>>();
    let translations = translate_fragments(
        store,
        scheduler,
        sources,
        options,
        output_mode,
        counts,
        progress,
        state,
    )
    .await?;
    Ok(replace_xml_blocks(bytes, &blocks, &translations))
}

fn translated_filename(filename: &str) -> String {
    match filename.rfind('.') {
        Some(index) if index > 0 => {
            format!("{}-translated{}", &filename[..index], &filename[index..])
        }
        _ => format!("{filename}-translated"),
    }
}

fn output_filename(filename: &str) -> String {
    translated_filename(filename)
}

fn output_media_type(filename: &str) -> String {
    mime_guess::from_path(filename)
        .first_or_octet_stream()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        load_pdf_fonts, make_batches, output_filename, output_media_type, parse_pdf,
        partial_batch_item_count, pdf_page_rotations, pdf_text_blocks, render_fragment,
        render_translated_pdf, split_chunks, surrounding_whitespace, wrap_pdf_text, PdfFont,
        PendingPart,
    };
    use crate::{ai::SEGMENT_SEPARATOR, models::FileOutputMode};
    use printpdf::{
        Color, Mm, Op, PdfDocument, PdfFontHandle, PdfPage, PdfSaveOptions, Pt, Rect, Rgb,
        TextItem, TextMatrix,
    };

    fn source_pdf() -> Vec<u8> {
        let mut document = PdfDocument::new("Source layout");
        let parsed = load_pdf_fonts("Original first line Original second line Page two")
            .unwrap()
            .remove(0);
        let font = document.add_font(&parsed);
        let text_ops = |lines: &[(&str, f32, f32)]| {
            let mut operations = vec![Op::StartTextSection];
            for (text, x, y) in lines {
                operations.extend([
                    Op::SetTextMatrix {
                        matrix: TextMatrix::Raw([1.0, 0.0, 0.0, 1.0, *x, *y]),
                    },
                    Op::SetFont {
                        font: PdfFontHandle::External(font.clone()),
                        size: Pt(12.0),
                    },
                    Op::ShowText {
                        items: vec![TextItem::Text((*text).to_string())],
                    },
                ]);
            }
            operations.push(Op::EndTextSection);
            operations
        };
        let mut first_page_ops = vec![
            Op::SetFillColor {
                col: Color::Rgb(Rgb::new(0.2, 0.4, 0.8, None)),
            },
            Op::DrawRectangle {
                rectangle: Rect::from_xywh(Pt(8.0), Pt(8.0), Pt(40.0), Pt(20.0)),
            },
        ];
        first_page_ops.extend(text_ops(&[
            ("Original first line", 35.0, 180.0),
            ("Original second line", 35.0, 164.0),
        ]));
        let pages = vec![
            PdfPage::new(Mm(160.0), Mm(90.0), first_page_ops),
            PdfPage::new(Mm(100.0), Mm(150.0), text_ops(&[("Page two", 24.0, 380.0)])),
        ];
        let mut warnings = Vec::new();
        let bytes = document
            .with_pages(pages)
            .save(&PdfSaveOptions::default(), &mut warnings);
        let mut source = lopdf::Document::load_mem(&bytes).unwrap();
        let second_page = source.get_pages()[&2];
        source
            .get_dictionary_mut(second_page)
            .unwrap()
            .set("Rotate", 90);
        let mut rotated = Vec::new();
        source.save_to(&mut rotated).unwrap();
        rotated
    }

    #[test]
    fn splitting_preserves_all_content() {
        let source = "word ".repeat(100);
        let chunks = split_chunks(&source, 25);
        assert_eq!(chunks.concat(), source);
        assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 25));
    }

    #[test]
    fn office_text_is_extracted_and_replaced_without_losing_runs() {
        let paragraph = br#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Hello </w:t></w:r><w:r><w:t>world &amp; team</w:t></w:r></w:p>"#;
        assert_eq!(
            super::extract_xml_blocks(paragraph, b"w:p", b"w:t").unwrap()[0].text,
            "Hello world & team"
        );

        let blocks = super::extract_xml_blocks(paragraph, b"w:p", b"w:t").unwrap();
        let translated = super::replace_xml_blocks(paragraph, &blocks, &["你好，团队".to_string()]);
        let translated_text = super::extract_xml_blocks(&translated, b"w:p", b"w:t")
            .unwrap()
            .remove(0)
            .text;
        assert_eq!(translated_text, "你好，团队");
        assert!(String::from_utf8(translated).unwrap().contains("<w:b"));
    }

    #[test]
    fn obvious_non_language_fragments_are_skipped() {
        for text in [
            "",
            "  ",
            "a",
            "Z.",
            "(B)",
            "123.45%",
            "—…！？",
            "⚠ ★ →",
            "https://example.com",
        ] {
            assert!(!super::needs_translation(text), "{text}");
        }
        for text in [
            "中",
            "是",
            "あ",
            "Yes",
            "NASA",
            "第1章",
            "https://example.com is a website",
        ] {
            assert!(super::needs_translation(text), "{text}");
        }
    }

    #[test]
    fn nested_office_textboxes_keep_their_text_separate() {
        let xml = br#"<w:p><w:r><w:t>Outer</w:t><w:drawing><w:txbxContent><w:p><w:r><w:t>Box &amp; text</w:t></w:r></w:p></w:txbxContent><a:p><a:r><a:t>Drawing</a:t></a:r></a:p></w:drawing></w:r><w:r><w:t> tail</w:t></w:r></w:p>"#;
        let blocks = super::extract_xml_blocks(xml, b"w:p", b"w:t").unwrap();
        assert_eq!(
            blocks.iter().map(|b| b.text.as_str()).collect::<Vec<_>>(),
            ["Outer tail", "Box & text", "Drawing"]
        );
        let translations = vec!["外层".into(), "文本框".into(), "绘图".into()];
        let output = super::replace_xml_blocks(xml, &blocks, &translations);
        let result = super::extract_xml_blocks(&output, b"w:p", b"w:t").unwrap();
        assert_eq!(
            result.iter().map(|b| b.text.clone()).collect::<Vec<_>>(),
            translations
        );
        assert_eq!(
            super::replace_xml_blocks(
                xml,
                &blocks,
                &blocks.iter().map(|b| b.text.clone()).collect::<Vec<_>>()
            ),
            xml
        );
        let drawing = br#"<xdr:wsDr><a:p><a:r><a:t>Excel box</a:t></a:r></a:p></xdr:wsDr>"#;
        assert_eq!(
            super::extract_xml_blocks(drawing, b"a:p", b"a:t").unwrap()[0].text,
            "Excel box"
        );
    }

    #[test]
    fn chunk_whitespace_can_be_restored_after_ai_trimming() {
        assert_eq!(
            surrounding_whitespace("  translate me \n"),
            ("  ", "translate me", " \n")
        );
        assert_eq!(surrounding_whitespace("   "), ("   ", "", ""));
    }

    #[test]
    fn bilingual_output_keeps_source_and_translation_whitespace() {
        assert_eq!(
            render_fragment("  hello ", "你好", FileOutputMode::Bilingual),
            "  hello\n  你好 "
        );
        assert_eq!(
            render_fragment("  hello ", "你好", FileOutputMode::Translated),
            "  你好 "
        );
    }

    #[test]
    fn small_batch_work_is_balanced_across_file_slots() {
        let pending = (0..4)
            .map(|index| PendingPart {
                result_index: index,
                text: format!("segment-{index}"),
                force_single: false,
            })
            .collect();
        let batches = make_batches(pending, 1_000, 16, 2);
        assert_eq!(batches.len(), 2);
        assert_eq!(batches.iter().map(Vec::len).collect::<Vec<_>>(), [2, 2]);
    }

    #[test]
    fn streaming_batch_count_tracks_the_current_segment() {
        assert_eq!(partial_batch_item_count(""), 0);
        assert_eq!(partial_batch_item_count("first"), 1);
        assert_eq!(
            partial_batch_item_count(&format!("first{SEGMENT_SEPARATOR}")),
            1
        );
        assert_eq!(
            partial_batch_item_count(&format!("first{SEGMENT_SEPARATOR}second")),
            2
        );
    }

    #[test]
    fn pdf_output_keeps_its_file_type() {
        assert_eq!(output_filename("guide.PDF"), "guide-translated.PDF");
        assert_eq!(output_media_type("guide.PDF"), "application/pdf");
    }

    #[test]
    fn unchanged_pdf_is_returned_byte_for_byte() {
        let source = source_pdf();
        let mut document = parse_pdf(&crate::pdf::extraction_copy(&source).unwrap()).unwrap();
        let blocks = pdf_text_blocks(&document);
        let translations = blocks
            .iter()
            .map(|block| block.text.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            render_translated_pdf(&mut document, &blocks, &translations, &source).unwrap(),
            source
        );
    }

    #[test]
    fn translated_pdf_preserves_pages_and_non_text_content() {
        let source = source_pdf();
        let mut document = parse_pdf(&source).unwrap();
        let rotations = pdf_page_rotations(&source).unwrap();
        assert_eq!(rotations, [0, 90]);
        let source_widths = document
            .pages
            .iter()
            .map(|page| (page.media_box.width.0, page.media_box.height.0))
            .collect::<Vec<_>>();
        let blocks = pdf_text_blocks(&document);
        assert_eq!(
            blocks.iter().filter(|block| block.page_index == 0).count(),
            1
        );
        assert_eq!(
            blocks.iter().filter(|block| block.page_index == 1).count(),
            1
        );
        let translations = blocks
            .iter()
            .map(|block| format!("Translated page {}", block.page_index + 1))
            .collect::<Vec<_>>();
        let pdf = render_translated_pdf(&mut document, &blocks, &translations, &source).unwrap();
        assert!(pdf.starts_with(b"%PDF-"));
        assert_eq!(pdf_page_rotations(&pdf).unwrap(), rotations);

        let translated = parse_pdf(&crate::pdf::extraction_copy(&pdf).unwrap()).unwrap();
        assert_eq!(translated.pages.len(), 2);
        assert_eq!(
            translated
                .pages
                .iter()
                .map(|page| (page.media_box.width.0, page.media_box.height.0))
                .collect::<Vec<_>>(),
            source_widths
        );
        let source_raw = lopdf::Document::load_mem(&source).unwrap();
        let result_raw = lopdf::Document::load_mem(&pdf).unwrap();
        let source_ops = lopdf::content::Content::decode(
            &source_raw.get_page_content(source_raw.get_pages()[&1]),
        )
        .unwrap();
        let result_ops = lopdf::content::Content::decode(
            &result_raw.get_page_content(result_raw.get_pages()[&1]),
        )
        .unwrap();
        let rectangle = source_ops
            .operations
            .iter()
            .find(|operation| operation.operator == "re")
            .unwrap();
        assert!(result_ops
            .operations
            .iter()
            .any(|operation| operation.operator == rectangle.operator
                && operation.operands == rectangle.operands));
        let extracted = translated
            .extract_text()
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n");
        let normalized = extracted.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(normalized.contains("Translated page 1"));
        assert!(normalized.contains("Translated page 2"));
        assert!(!extracted.contains("Original"));
    }

    #[test]
    fn pdf_text_wraps_long_lines_without_losing_characters() {
        let text = "a".repeat(100);
        let fonts = load_pdf_fonts(&text)
            .unwrap()
            .into_iter()
            .map(|parsed| PdfFont {
                parsed,
                id: printpdf::FontId::new(),
            })
            .collect::<Vec<_>>();
        let lines = wrap_pdf_text(&text, 12.0, 80.0, &fonts);
        assert!(lines.len() > 1);
        assert_eq!(lines.concat(), text);
    }

    fn complex_layout_pdf() -> Vec<u8> {
        use lopdf::{dictionary, Document, Object, Stream};
        let mut document = Document::with_version("1.7");
        let regular = document.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
        });
        let bold = document.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica-BoldOblique",
        });
        let inner = document.add_object(Stream::new(dictionary! {
            "Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(),0.into(),180.into(),45.into()],
            "Resources" => dictionary! { "Font" => dictionary! { "F1" => bold } },
        }, b"0.15 0.35 0.65 rg BT /F1 12 Tf 1 0 0 1 8 18 Tm (Nested label) Tj ET".to_vec()));
        let outer = document.add_object(Stream::new(dictionary! {
            "Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(),0.into(),180.into(),55.into()],
            "Matrix" => vec![1.into(),0.into(),0.into(),1.into(),0.into(),5.into()],
            "Resources" => dictionary! { "XObject" => dictionary! { "Inner" => inner } },
        }, b"0.85 0.93 1 rg 0 0 180 45 re f /Inner Do".to_vec()));
        let image = document.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Image", "Width" => 2, "Height" => 2,
                "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8,
            },
            vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0],
        ));
        let resources = document.add_object(dictionary! {
            "Font" => dictionary! { "F1" => regular, "F2" => bold },
            "XObject" => dictionary! { "Card" => outer, "Picture" => image },
        });
        let content = document.add_object(Stream::new(
            lopdf::Dictionary::new(),
            br#"
            0.15 0.35 0.65 rg BT /F2 24 Tf 1 0 0 1 60 745 Tm (Layout report) Tj ET
            0 g BT /F1 12 Tf 1 0 0 1 60 685 Tm (Left column first line.) Tj
            1 0 0 1 60 669 Tm (Left column second line.) Tj ET
            BT /F1 12 Tf 1 0 0 1 330 685 Tm (Right column first line.) Tj
            1 0 0 1 330 669 Tm (Right column second line.) Tj ET
            0.4 w 0 G 60 550 480 60 re S 60 580 m 540 580 l S 300 550 m 300 610 l S
            BT /F1 11 Tf 1 0 0 1 245 591 Tm (Quantity) Tj 1 0 0 1 307 591 Tm (Price) Tj ET
            BT /F1 11 Tf 1 0 0 1 245 569 Tm (Widgets) Tj 1 0 0 1 307 569 Tm (Total) Tj ET
            q 1 0 0 1 60 430 cm /Card Do Q q 1 0 0 1 330 430 cm /Card Do Q
            BT 1 0 0 1 60 380 Tm (Inherited face) Tj ET
            q 150 0 0 90 60 250 cm /Picture Do Q
            BT /F1 12 Tf 0 1 -1 0 575 300 Tm (Side label) Tj ET
            BT /F1 6 Tf 1 0 0 1 60 80 Tm (Small footnote.) Tj ET
        "#
            .to_vec(),
        ));
        let pages = document.new_object_id();
        let page = document.new_object_id();
        let link = document.add_object(dictionary! {
            "Type" => "Annot", "Subtype" => "Link", "Rect" => vec![60.into(),730.into(),250.into(),775.into()],
            "A" => dictionary! { "S" => "URI", "URI" => Object::string_literal("https://example.com") },
        });
        document.objects.insert(
            page,
            Object::Dictionary(dictionary! {
                "Type" => "Page", "Parent" => pages, "Contents" => content,
                "Annots" => vec![Object::Reference(link)], "Rotate" => 0,
                "TrimBox" => vec![20.into(),20.into(),580.into(),780.into()],
            }),
        );
        document.objects.insert(pages, Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => vec![Object::Reference(page)], "Count" => 1,
            "MediaBox" => vec![0.into(),0.into(),600.into(),800.into()],
            "CropBox" => vec![10.into(),10.into(),590.into(),790.into()], "Resources" => resources,
        }));
        let outline = document.new_object_id();
        let entry = document.add_object(dictionary! {
            "Title" => Object::string_literal("Layout report"), "Parent" => outline,
            "Dest" => vec![Object::Reference(page), Object::Name(b"Fit".to_vec())],
        });
        document.objects.insert(
            outline,
            Object::Dictionary(dictionary! {
                "Type" => "Outlines", "First" => entry, "Last" => entry, "Count" => 1,
            }),
        );
        let catalog = document.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => pages, "Outlines" => outline },
        );
        let info = document
            .add_object(dictionary! { "Title" => Object::string_literal("Preserved metadata") });
        document.trailer.set("Root", catalog);
        document.trailer.set("Info", info);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn pdf_layout_handles_columns_table_cells_styles_and_nested_forms() {
        let bytes = complex_layout_pdf();
        let document = parse_pdf(&crate::pdf::extraction_copy(&bytes).unwrap()).unwrap();
        let blocks = pdf_text_blocks(&document);
        assert_eq!(blocks.len(), 12, "{blocks:#?}");
        let inherited = blocks
            .iter()
            .find(|block| block.text == "Inherited face")
            .unwrap();
        assert!(!inherited.style.bold && !inherited.style.italic);
        assert_eq!(inherited.font_size, 11.0);
        let left = blocks
            .iter()
            .find(|block| block.text.starts_with("Left"))
            .unwrap();
        assert_eq!(
            left.text,
            "Left column first line. Left column second line."
        );
        let right = blocks
            .iter()
            .find(|block| block.text.starts_with("Right"))
            .unwrap();
        assert_eq!(
            right.text,
            "Right column first line. Right column second line."
        );
        for text in ["Quantity", "Price", "Widgets", "Total"] {
            assert!(blocks.iter().any(|block| block.text == text), "{blocks:#?}");
        }
        let cards = blocks
            .iter()
            .filter(|block| block.text == "Nested label")
            .collect::<Vec<_>>();
        assert_eq!(cards.len(), 2);
        assert!((cards[1].bbox[0] - cards[0].bbox[0] - 270.0).abs() < 0.1);
        let title = blocks
            .iter()
            .find(|block| block.text == "Layout report")
            .unwrap();
        assert!(title.style.bold && title.style.italic);
        assert_eq!(
            title.style.color,
            Color::Rgb(Rgb::new(0.15, 0.35, 0.65, None))
        );
        let side = blocks
            .iter()
            .find(|block| block.text == "Side label")
            .unwrap();
        assert!((side.style.angle - std::f32::consts::FRAC_PI_2).abs() < 0.01);
        let barriers = super::pdf_page_barriers(&document.pages[0]);
        let available = super::pdf_available_box(left, &blocks, &barriers, &document.pages[0]);
        assert!(available[2] < right.bbox[0]);
        assert!(
            available[3] < 190.0,
            "available={available:?}, barriers={barriers:?}"
        ); // table top is y=190 in top-left coordinates
        let quantity = blocks
            .iter()
            .find(|block| block.text == "Quantity")
            .unwrap();
        let cell = super::pdf_available_box(quantity, &blocks, &barriers, &document.pages[0]);
        assert!(cell[2] <= 298.0 && cell[3] <= 218.0);
    }

    #[test]
    fn pdf_translation_preserves_original_objects_and_readable_multilingual_layout() {
        let source = complex_layout_pdf();
        let original = lopdf::Document::load_mem(&source).unwrap();
        let mut parsed = parse_pdf(&crate::pdf::extraction_copy(&source).unwrap()).unwrap();
        let blocks = pdf_text_blocks(&parsed);
        let mut translated = blocks
            .iter()
            .map(|block| {
                match block.text.as_str() {
                    "Layout report" => "排版报告",
                    "Quantity" => "数量",
                    "Price" => "价格",
                    "Widgets" => "产品",
                    "Total" => "合计",
                    "Nested label" => "嵌套标签",
                    "Inherited face" => "继承字体",
                    "Side label" => "侧边标签",
                    "Small footnote." => "小字号脚注。",
                    text if text.starts_with("Left") => {
                        "左栏的第一句话。翻译长度增加时，应保留分栏及正文的字号和位置。"
                    }
                    _ => "右栏的第一句话。译文应在本栏内换行，避免跨栏覆盖其他文字。",
                }
                .to_string()
            })
            .collect::<Vec<_>>();
        // Linux installations often have Latin fonts but no CJK font package.
        // Still exercise Unicode embedding and the full layout/object checks.
        if load_pdf_fonts(&translated.join("\n")).is_err() {
            translated = blocks.iter().map(|block| match block.text.as_str() {
                "Layout report" => "Rapport détaillé", "Quantity" => "Quantité",
                "Price" => "Prix", "Widgets" => "Articles", "Total" => "Total",
                "Nested label" => "Étiquette", "Inherited face" => "Police héritée",
                "Side label" => "Libellé", "Small footnote." => "Petite note.",
                text if text.starts_with("Left") => "Première phrase à gauche. La traduction garde les colonnes et la taille du texte.",
                _ => "Première phrase à droite. Le texte doit rester dans sa colonne sans recouvrir les autres éléments.",
            }.to_string()).collect();
        }
        let output = render_translated_pdf(&mut parsed, &blocks, &translated, &source).unwrap();
        let result = lopdf::Document::load_mem(&output).unwrap();
        let page = original.get_pages()[&1];
        for key in [b"Parent".as_slice(), b"Annots", b"TrimBox", b"Rotate"] {
            assert_eq!(
                original.get_dictionary(page).unwrap().get(key).unwrap(),
                result.get_dictionary(page).unwrap().get(key).unwrap()
            );
        }
        let parent = original
            .get_dictionary(page)
            .unwrap()
            .get(b"Parent")
            .unwrap()
            .as_reference()
            .unwrap();
        for key in [b"MediaBox".as_slice(), b"CropBox"] {
            assert_eq!(
                original.get_dictionary(parent).unwrap().get(key).unwrap(),
                result.get_dictionary(parent).unwrap().get(key).unwrap()
            );
        }
        for key in [b"Root".as_slice(), b"Info"] {
            assert_eq!(
                original.trailer.get(key).unwrap(),
                result.trailer.get(key).unwrap()
            );
        }
        for (id, object) in &original.objects {
            if let Ok(stream) = object.as_stream() {
                if stream
                    .dict
                    .get(b"Subtype")
                    .and_then(lopdf::Object::as_name)
                    .ok()
                    == Some(b"Image")
                {
                    let preserved = result.get_object(*id).unwrap().as_stream().unwrap();
                    assert_eq!(
                        stream.get_plain_content().unwrap(),
                        preserved.get_plain_content().unwrap()
                    );
                }
            }
            if object
                .as_dict()
                .is_ok_and(|dictionary| dictionary.has(b"Dest") || dictionary.has(b"A"))
            {
                assert_eq!(object, result.get_object(*id).unwrap());
            }
        }
        let extracted = parse_pdf(&crate::pdf::extraction_copy(&output).unwrap()).unwrap();
        let text = extracted
            .extract_text()
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("");
        for translation in &translated {
            let normalized = text
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>();
            assert!(
                normalized.contains(
                    &translation
                        .chars()
                        .filter(|c| !c.is_whitespace())
                        .collect::<String>()
                ),
                "{text}"
            );
        }
        assert!(!text.contains("Nested label") && !text.contains("Layout report"));
        if let Some(directory) = std::env::var_os("PROMLEX_PDF_FIXTURE_DIR") {
            let directory = std::path::PathBuf::from(directory);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("source.pdf"), source).unwrap();
            std::fs::write(directory.join("translated.pdf"), output).unwrap();
        }
    }

    #[test]
    fn pdf_fitting_rejects_overflow_instead_of_printing_over_neighbors() {
        let fonts = load_pdf_fonts("long translation")
            .unwrap()
            .into_iter()
            .map(|parsed| PdfFont {
                parsed,
                id: printpdf::FontId::new(),
            })
            .collect::<Vec<_>>();
        assert!(super::fit_pdf_text(
            &"long translation ".repeat(100),
            12.0,
            40.0,
            12.0,
            1.15,
            &fonts
        )
        .is_err());
        let (size, lines) =
            super::fit_pdf_text("footnote", 5.0, 100.0, 10.0, 1.15, &fonts).unwrap();
        assert_eq!(size, 5.0);
        assert_eq!(lines, ["footnote"]);
    }
}
