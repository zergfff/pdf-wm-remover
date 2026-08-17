//! PDF watermark remover core library.
//!
//! Content-stream level watermark removal:
//! 1. Parse each page's content stream into BT..ET blocks (via lopdf Content operations).
//! 2. Analyze: cluster repeated text blocks across pages (text + font size).
//! 3. Remove: delete only the BT..ET blocks whose text matches user-confirmed
//!    keywords — never touches body text/graphics (unlike redaction rectangles).
//! 4. Strip encryption/permissions by dropping the /Encrypt entry on save.

use std::collections::BTreeMap;
use std::path::Path;

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId};

// ---------------------------------------------------------------------------
// Text block extraction from content operations
// ---------------------------------------------------------------------------

/// A text block extracted from one BT..ET segment.
#[derive(Debug, Clone, PartialEq)]
pub struct TextBlock {
    /// Concatenated text drawn by Tj/TJ inside the block.
    pub text: String,
    /// Font size from the last Tf before the text ops (approximation).
    pub size: f32,
    /// Index range of operations belonging to this block in the parsed stream.
    pub op_start: usize,
    pub op_end: usize, // exclusive
}

/// Decode a PDF string operand (literal or hex) to bytes.
fn operand_bytes(obj: &Object) -> Option<Vec<u8>> {
    match obj {
        Object::String(bytes, _) => Some(bytes.clone()),
        _ => None,
    }
}

/// Extract block strings from a vector of operations by walking BT..ET.
/// Returns blocks with their op index ranges (for later deletion).
pub fn extract_text_blocks(ops: &[Operation]) -> Vec<TextBlock> {
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < ops.len() {
        if ops[i].operator == "BT" {
            let start = i;
            let mut j = i + 1;
            let mut texts: Vec<Vec<u8>> = Vec::new();
            let mut size: f32 = 0.0;
            while j < ops.len() && ops[j].operator != "ET" {
                // Tf sets current font size
                if ops[j].operator == "Tf" {
                    if let Some(Object::Integer(n)) = ops[j].operands.get(1) {
                        size = *n as f32;
                    } else if let Some(Object::Real(f)) = ops[j].operands.get(1) {
                        size = *f;
                    }
                }
                // Tj: single string
                if ops[j].operator == "Tj" {
                    if let Some(bytes) = ops[j].operands.first().and_then(operand_bytes) {
                        texts.push(bytes);
                    }
                }
                // TJ: array of strings and numeric kerning
                if ops[j].operator == "TJ" {
                    if let Some(Object::Array(arr)) = ops[j].operands.first() {
                        for item in arr {
                            if let Some(bytes) = operand_bytes(item) {
                                texts.push(bytes);
                            }
                        }
                    }
                }
                // ' and " show-text operators
                if ops[j].operator == "'" {
                    if let Some(bytes) = ops[j].operands.first().and_then(operand_bytes) {
                        texts.push(bytes);
                    }
                }
                if ops[j].operator == "\"" {
                    if let Some(bytes) = ops[j].operands.get(2).and_then(operand_bytes) {
                        texts.push(bytes);
                    }
                }
                j += 1;
            }
            // Block ends at ET (or EOF)
            let end = if j < ops.len() { j + 1 } else { ops.len() };
            let joined = texts.concat();
            // Skip empty blocks (no text drawn)
            if !joined.is_empty() {
                let text = String::from_utf8_lossy(&joined).to_string();
                blocks.push(TextBlock {
                    text,
                    size,
                    op_start: start,
                    op_end: end,
                });
            }
            i = end;
        } else {
            i += 1;
        }
    }
    blocks
}

// ---------------------------------------------------------------------------
// Analysis: cluster repeated text blocks across pages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Candidate {
    /// Block text (also used as the match keyword for removal).
    pub text: String,
    /// Font size.
    pub size: f32,
    /// Number of pages where this text appeared.
    pub count: usize,
    /// Sample page index (0-based) for preview.
    pub sample_page: usize,
}

/// Analyze a document and return text candidates that repeat across pages.
/// `ratio_threshold` is 0..=1.0; blocks appearing on at least
/// `max(2, total_pages * ratio)` pages are candidates.
pub fn analyze(
    doc: &Document,
    ratio_threshold: f64,
    min_pages: usize,
) -> Result<Vec<Candidate>, lopdf::Error> {
    let pages: BTreeMap<u32, ObjectId> = doc.get_pages();
    let total = pages.len();
    let threshold = ((total as f64) * ratio_threshold).max(min_pages as f64);

    // (text, size) -> (count, first_page)
    let mut clusters: BTreeMap<(String, u32), (usize, usize)> = BTreeMap::new();
    for (page_no, (_page_key, page_id)) in pages.iter().enumerate() {
        let content_bytes = page_content_bytes(doc, *page_id)?;
        let content = decode_content_or_empty(&content_bytes);
        for block in extract_text_blocks(&content.operations) {
            let text = collapse_ws(&block.text);
            if text.len() < 2 {
                continue;
            }
            let key = (text, (block.size * 10.0) as u32);
            let entry = clusters.entry(key).or_insert((0, page_no));
            entry.0 += 1;
        }
    }

    let mut candidates: Vec<Candidate> = clusters
        .into_iter()
        .filter(|(_, (count, _))| (*count as f64) >= threshold)
        .map(|((text, size_f), (count, first_page))| Candidate {
            text,
            size: size_f as f32 / 10.0,
            count,
            sample_page: first_page,
        })
        .collect();
    candidates.sort_by(|a, b| b.count.cmp(&a.count));
    Ok(candidates)
}

/// Collapse runs of whitespace to a single space and trim.
fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_ws {
                out.push(' ');
            }
            prev_ws = true;
        } else {
            out.push(c);
            prev_ws = false;
        }
    }
    out.trim().to_string()
}

/// Decode content bytes into operations; on failure return an empty operation list.
fn decode_content_or_empty(data: &[u8]) -> Content<Vec<Operation>> {
    Content::decode(data).unwrap_or(Content { operations: vec![] })
}

// ---------------------------------------------------------------------------
// Content stream helpers
// ---------------------------------------------------------------------------

/// Return the raw (decompressed) content bytes of a page, handling a single
/// stream or an array of streams.
fn page_content_bytes(doc: &Document, page_id: ObjectId) -> Result<Vec<u8>, lopdf::Error> {
    let page_obj = doc.get_object(page_id)?;
    let dict = match page_obj {
        Object::Dictionary(d) => d.clone(),
        Object::Reference(id) => doc.get_dictionary(*id)?.clone(),
        _ => return Ok(Vec::new()),
    };
    match dict.get(b"Contents") {
        Ok(Object::Reference(id)) => match doc.get_object(*id)? {
            Object::Stream(stream) => stream.decompressed_content(),
            _ => Ok(Vec::new()),
        },
        Ok(Object::Array(arr)) => {
            let mut out = Vec::new();
            for item in arr {
                let obj_ref = match item {
                    Object::Reference(id) => doc.get_object(*id)?,
                    other => other,
                };
                if let Object::Stream(stream) = obj_ref {
                    if let Ok(bytes) = stream.decompressed_content() {
                        out.extend_from_slice(&bytes);
                    }
                }
            }
            Ok(out)
        }
        _ => Ok(Vec::new()),
    }
}

// ---------------------------------------------------------------------------
// Removal
// ---------------------------------------------------------------------------

/// Remove BT..ET text blocks whose text matches any keyword (case-insensitive
/// substring) from one page. Returns number of blocks removed.
/// Handles Contents as single stream or array; empty streams are dropped.
pub fn remove_watermarks_from_page(
    doc: &mut Document,
    page_id: ObjectId,
    keywords: &[String],
) -> Result<usize, lopdf::Error> {
    if keywords.is_empty() {
        return Ok(0);
    }
    let kws: Vec<String> = keywords.iter().map(|k| k.to_lowercase()).collect();

    // Resolve Contents
    let page_obj = doc.get_object(page_id)?;
    let dict = match page_obj {
        Object::Dictionary(d) => d.clone(),
        Object::Reference(id) => doc.get_dictionary(*id)?.clone(),
        _ => return Ok(0),
    };
    let contents_refs: Vec<ObjectId> = match dict.get(b"Contents") {
        Ok(Object::Reference(id)) => vec![*id],
        Ok(Object::Array(arr)) => arr
            .iter()
            .filter_map(|item| match item {
                Object::Reference(id) => Some(*id),
                _ => None,
            })
            .collect(),
        _ => return Ok(0),
    };

    let mut removed_total = 0;
    let mut kept_ids: Vec<ObjectId> = Vec::new();
    for cid in &contents_refs {
        let obj = doc.get_object(*cid)?;
        let stream = match obj {
            Object::Stream(s) => s.clone(),
            _ => continue,
        };
        let raw = stream.decompressed_content().unwrap_or_default();
        let content = decode_content_or_empty(&raw);
        let blocks = extract_text_blocks(&content.operations);
        let any_match = blocks
            .iter()
            .any(|b| block_matches_keywords(&b.text, &kws));
        if !any_match {
            kept_ids.push(*cid);
            continue;
        }
        let mut ops: Vec<Operation> = Vec::new();
        let mut cur = 0;
        for block in &blocks {
            // copy ops before block
            while cur < block.op_start {
                ops.push(content.operations[cur].clone());
                cur += 1;
            }
            if block_matches_keywords(&block.text, &kws) {
                // skip this block entirely
                cur = block.op_end;
                removed_total += 1;
            } else {
                while cur < block.op_end {
                    ops.push(content.operations[cur].clone());
                    cur += 1;
                }
            }
        }
        while cur < content.operations.len() {
            ops.push(content.operations[cur].clone());
            cur += 1;
        }
        // encode back
        let new_content: Content<Vec<Operation>> = Content { operations: ops };
        if let Ok(encoded) = new_content.encode() {
            // If stream is now only q/Q shells or empty, drop it entirely.
            // Note: q/Q are not in the operations list (they are not parsed as
            // operations by the parser; they survive in the raw stream).
            let stripped: String = String::from_utf8_lossy(&encoded)
                .chars()
                .filter(|c| !c.is_whitespace() && *c != 'q' && *c != 'Q')
                .collect();
            if stripped.is_empty() {
                // empty shell - don't keep
                continue;
            }
            // Update the existing stream object in place
            let obj_mut = doc.get_object_mut(*cid)?;
            if let Object::Stream(s) = obj_mut {
                s.set_content(encoded);
            }
            kept_ids.push(*cid);
        } else {
            // encode failed - keep original
            kept_ids.push(*cid);
        }
    }

    // Rewrite Contents key (single ref if one stream, else array, else remove)
    let page_obj_mut = doc.get_object_mut(page_id)?;
    if let Object::Dictionary(d) = page_obj_mut {
        if kept_ids.is_empty() {
            d.remove(b"Contents");
        } else if kept_ids.len() == 1 {
            d.set("Contents", Object::Reference(kept_ids[0]));
        } else {
            let arr: Vec<Object> = kept_ids.iter().map(|id| Object::Reference(*id)).collect();
            d.set("Contents", Object::Array(arr));
        }
    }

    Ok(removed_total)
}

fn block_matches_keywords(text: &str, kws: &[String]) -> bool {
    let lower = collapse_ws(text).to_lowercase();
    kws.iter().any(|k| !k.is_empty() && lower.contains(k.as_str()))
}

/// Recursively process Form XObjects embedded in page resources.
pub fn remove_watermarks_from_resources(
    doc: &mut Document,
    resources: Option<&Dictionary>,
    keywords: &[String],
) -> Result<usize, lopdf::Error> {
    let mut total = 0;
    let Some(res) = resources else {
        return Ok(0);
    };
    let Ok(Object::Dictionary(xo_dict)) = res.get(b"XObject") else {
        return Ok(0);
    };
    let xo_ids: Vec<(Vec<u8>, ObjectId)> = xo_dict
        .iter()
        .filter_map(|(name, obj)| match obj {
            Object::Reference(id) => Some((name.clone(), *id)),
            _ => None,
        })
        .collect();
    for (_name, xo_id) in xo_ids {
        let obj = doc.get_object(xo_id)?;
        let stream = match obj {
            Object::Stream(s) => s.clone(), // clone to release the borrow
            _ => continue,
        };
        let is_form = stream
            .dict
            .get(b"Subtype")
            .map(|s| s.as_name().map(|n| n == b"Form".as_slice()).unwrap_or(false))
            .unwrap_or(false);
            if is_form {
                // clone the stream's dict so we can release the immutable borrow
                // before taking a mutable borrow later.
                let nested_resources = stream.dict.get(b"Resources").ok().map(|r| r.clone());
                let raw = stream.decompressed_content().unwrap_or_default();
                let content = decode_content_or_empty(&raw);
                let blocks = extract_text_blocks(&content.operations);
                let mut ops: Vec<Operation> = Vec::new();
                let mut cur = 0;
                let mut removed = 0;
                for block in &blocks {
                    while cur < block.op_start {
                        ops.push(content.operations[cur].clone());
                        cur += 1;
                    }
                    if block_matches_keywords(&block.text, keywords) {
                        cur = block.op_end;
                        removed += 1;
                    } else {
                        while cur < block.op_end {
                            ops.push(content.operations[cur].clone());
                            cur += 1;
                        }
                    }
                }
                while cur < content.operations.len() {
                    ops.push(content.operations[cur].clone());
                    cur += 1;
                }
                if removed > 0 {
                    let new_content: Content<Vec<Operation>> = Content { operations: ops };
                    if let Ok(encoded) = new_content.encode() {
                        let obj_mut = doc.get_object_mut(xo_id)?;
                        if let Object::Stream(s) = obj_mut {
                            s.set_content(encoded);
                        }
                    }
                    total += removed;
                }
                // recurse into nested resources of the form
                if let Some(nested_res) = nested_resources {
                    let nested_dict = match &nested_res {
                        Object::Reference(nres_id) => doc.get_dictionary(*nres_id).cloned().ok(),
                        Object::Dictionary(d2) => Some(d2.clone()),
                        _ => None,
                    };
                    total += remove_watermarks_from_resources(doc, nested_dict.as_ref(), keywords)?;
                }
            }
    }
    Ok(total)
}

// ---------------------------------------------------------------------------
// High-level entry
// ---------------------------------------------------------------------------

/// Remove watermarks from a PDF file and save the cleaned output.
/// The output is saved without encryption (permission restrictions dropped).
pub fn remove_watermarks(
    input: &Path,
    output: &Path,
    keywords: &[String],
    password: Option<&str>,
) -> Result<RemovalReport, anyhow::Error> {
    let mut doc = load_document(input, password)?;

    let page_ids: Vec<ObjectId> = doc.get_pages().values().copied().collect();
    let mut removed_total = 0usize;
    let mut pages_touched = 0usize;
    for page_id in &page_ids {
        let n = remove_watermarks_from_page(&mut doc, *page_id, keywords)?;
        if n > 0 {
            pages_touched += 1;
        }
        removed_total += n;
        // also process form xobjects in page resources
        if let Ok(dict) = doc.get_dictionary(*page_id) {
            if let Ok(res) = dict.get(b"Resources") {
                let res_dict = match res {
                    Object::Reference(id) => doc.get_dictionary(*id).cloned().ok(),
                    Object::Dictionary(d2) => Some(d2.clone()),
                    _ => None,
                };
                removed_total +=
                    remove_watermarks_from_resources(&mut doc, res_dict.as_ref(), keywords)?;
            }
        }
    }

    // Strip encryption: remove /Encrypt from trailer and delete the object.
    doc.trailer.remove(b"Encrypt");
    doc.save(output)?;

    Ok(RemovalReport {
        removed_blocks: removed_total,
        pages_touched,
        total_pages: page_ids.len(),
        was_encrypted: false,
    })
}

/// Load a document, auto-decrypting empty-password encryption.
pub fn load_document(path: &Path, password: Option<&str>) -> Result<Document, anyhow::Error> {
    if let Some(pw) = password {
        // lopdf's load already attempts empty-password decryption automatically;
        // only call decrypt explicitly when a real password was provided.
        let mut doc = Document::load(path)?;
        if doc.is_encrypted() {
            doc.decrypt(pw)?;
        }
        Ok(doc)
    } else {
        let doc = Document::load(path)?;
        Ok(doc)
    }
}

#[derive(Debug, Clone)]
pub struct RemovalReport {
    pub removed_blocks: usize,
    pub pages_touched: usize,
    pub total_pages: usize,
    pub was_encrypted: bool,
}

/// Quick check: does the content stream of a page still contain the keyword?
#[allow(dead_code)]
pub fn page_has_keyword(doc: &Document, page_id: ObjectId, keyword: &str) -> bool {
    let bytes = page_content_bytes(doc, page_id).unwrap_or_default();
    let content = decode_content_or_empty(&bytes);
    extract_text_blocks(&content.operations)
        .iter()
        .any(|b| block_matches_keywords(&b.text, &[keyword.to_lowercase()]))
}