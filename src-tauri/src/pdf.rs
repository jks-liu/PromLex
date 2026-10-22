//! Edit the original object graph; never round-trip artwork through a PDF generator.
use std::collections::{BTreeSet, HashMap};

use lopdf::{
    content::{Content, Operation},
    Dictionary, Document, Object, ObjectId, Stream,
};

use crate::files::FileError;

/// PDF FontFile streams must embed one sfnt face, not the whole TTC container.
/// The lightweight printpdf serializer otherwise embeds a collection verbatim,
/// and readers select face zero even when fontdb selected a bold/italic face.
pub(crate) fn standalone_font_face(bytes: &[u8], index: u32) -> Option<std::borrow::Cow<'_, [u8]>> {
    if !bytes.starts_with(b"ttcf") {
        return Some(std::borrow::Cow::Borrowed(bytes));
    }
    let read_u32 = |offset: usize| -> Option<u32> {
        Some(u32::from_be_bytes(
            bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
        ))
    };
    if index >= read_u32(8)? {
        return None;
    }
    let directory = read_u32(12usize.checked_add((index as usize).checked_mul(4)?)?)? as usize;
    let header = bytes.get(directory..directory.checked_add(12)?)?;
    let source_count = u16::from_be_bytes(header[4..6].try_into().ok()?) as usize;
    if source_count == 0 || source_count > 4095 {
        return None;
    }
    let mut entries = Vec::new();
    for table in 0..source_count {
        let offset = directory.checked_add(12 + table * 16)?;
        let entry = bytes.get(offset..offset.checked_add(16)?)?;
        // A digital signature is invalid after rebuilding the sfnt container.
        if &entry[..4] != b"DSIG" {
            entries.push((offset, entry));
        }
    }
    let count = entries.len();
    if count == 0 {
        return None;
    }
    let mut result = vec![0; 12 + count * 16];
    result[..12].copy_from_slice(header);
    let selector = (usize::BITS - 1 - count.leading_zeros()) as u16;
    let search_range = (1_u16 << selector) * 16;
    result[4..6].copy_from_slice(&(count as u16).to_be_bytes());
    result[6..8].copy_from_slice(&search_range.to_be_bytes());
    result[8..10].copy_from_slice(&selector.to_be_bytes());
    result[10..12].copy_from_slice(&((count as u16) * 16 - search_range).to_be_bytes());
    let mut head = None;
    for (table, (source_entry, entry)) in entries.into_iter().enumerate() {
        let offset = read_u32(source_entry + 8)? as usize;
        let length = read_u32(source_entry + 12)? as usize;
        let data = bytes.get(offset..offset.checked_add(length)?)?;
        let new_offset = result.len();
        let target_entry = 12 + table * 16;
        result[target_entry..target_entry + 16].copy_from_slice(entry);
        result[target_entry + 8..target_entry + 12]
            .copy_from_slice(&u32::try_from(new_offset).ok()?.to_be_bytes());
        result[target_entry + 12..target_entry + 16]
            .copy_from_slice(&u32::try_from(length).ok()?.to_be_bytes());
        result.extend_from_slice(data);
        result.resize((result.len() + 3) & !3, 0);
        if &entry[..4] == b"head" {
            if length < 12 {
                return None;
            }
            head = Some(new_offset + 8);
            result[new_offset + 8..new_offset + 12].fill(0);
            let checksum = sfnt_checksum(&result[new_offset..]);
            result[target_entry + 4..target_entry + 8].copy_from_slice(&checksum.to_be_bytes());
        }
    }
    let head = head?;
    let adjustment = 0xB1B0AFBA_u32.wrapping_sub(sfnt_checksum(&result));
    result[head..head + 4].copy_from_slice(&adjustment.to_be_bytes());
    Some(std::borrow::Cow::Owned(result))
}

fn sfnt_checksum(bytes: &[u8]) -> u32 {
    bytes.chunks(4).fold(0_u32, |sum, chunk| {
        let mut word = [0; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum.wrapping_add(u32::from_be_bytes(word))
    })
}

fn error(error: impl std::fmt::Display) -> FileError {
    FileError::Message(format!("Unable to process PDF layout: {error}"))
}

fn resolve_dictionary(document: &Document, value: &Object) -> Result<Dictionary, FileError> {
    document
        .dereference(value)
        .map_err(error)?
        .1
        .as_dict()
        .cloned()
        .map_err(error)
}

fn inherited_optional(
    document: &Document,
    page: ObjectId,
    key: &[u8],
) -> Result<Option<Object>, FileError> {
    let mut current = page;
    let mut visited = BTreeSet::new();
    while visited.insert(current) {
        let dictionary = document.get_dictionary(current).map_err(error)?;
        if let Ok(value) = dictionary.get(key) {
            return Ok(Some(value.clone()));
        }
        let Ok(parent) = dictionary.get(b"Parent") else {
            return Ok(None);
        };
        current = parent.as_reference().map_err(error)?;
    }
    Err(error("cyclic page tree"))
}

fn inherited(document: &Document, page: ObjectId, key: &[u8]) -> Result<Object, FileError> {
    inherited_optional(document, page, key)?
        .ok_or_else(|| error(format!("missing {}", String::from_utf8_lossy(key))))
}

fn resources(document: &Document, page: ObjectId) -> Result<Dictionary, FileError> {
    match inherited_optional(document, page, b"Resources")? {
        Some(value) => resolve_dictionary(document, &value),
        None => Ok(Dictionary::new()),
    }
}

/// Inline Form XObjects only in the disposable extraction copy. Resource names
/// are scoped per invocation, including repeated forms with different transforms.
pub(crate) fn extraction_copy(bytes: &[u8]) -> Result<Vec<u8>, FileError> {
    let mut document = Document::load_mem(bytes).map_err(error)?;
    let mut sequence = 0;
    for page in document.get_pages().into_values() {
        let source_resources = resources(&document, page)?;
        let content = Content::decode(&document.get_page_content(page)).map_err(error)?;
        let mut merged = Dictionary::new();
        let operations = expand_forms(
            &document,
            &content.operations,
            &source_resources,
            &mut merged,
            &mut sequence,
            &mut BTreeSet::new(),
        )?;
        let operations = filter_text(restore_text_state(operations), true);
        document
            .change_page_content(page, Content { operations }.encode().map_err(error)?)
            .map_err(error)?;
        document
            .get_dictionary_mut(page)
            .map_err(error)?
            .set("Resources", merged);
    }
    let mut output = Vec::new();
    document.save_to(&mut output).map_err(error)?;
    Ok(output)
}

fn expand_forms(
    document: &Document,
    operations: &[Operation],
    scope: &Dictionary,
    merged: &mut Dictionary,
    sequence: &mut usize,
    active: &mut BTreeSet<ObjectId>,
) -> Result<Vec<Operation>, FileError> {
    *sequence += 1;
    let prefix = format!("PL{}", sequence);
    let mut names = HashMap::new();
    for category in [
        "Font",
        "XObject",
        "ExtGState",
        "ColorSpace",
        "Pattern",
        "Shading",
        "Properties",
    ] {
        if let Ok(value) = scope.get(category.as_bytes()) {
            let entries = resolve_dictionary(document, value)?;
            let mut target = merged
                .get(category.as_bytes())
                .and_then(Object::as_dict)
                .cloned()
                .unwrap_or_default();
            for (name, value) in entries.iter() {
                let renamed = [prefix.as_bytes(), b"_", name].concat();
                names.insert((category.to_string(), name.clone()), renamed.clone());
                target.set(renamed, value.clone());
            }
            merged.set(category, target);
        }
    }
    let mut output = Vec::new();
    for operation in operations {
        // printpdf's parser only flushes its rectangle builder for some paint
        // operators. Normalize re to a closed path in the extraction copy so
        // stroked table borders and filled backgrounds remain layout obstacles.
        if operation.operator == "re" && operation.operands.len() == 4 {
            let values = operation
                .operands
                .iter()
                .map(Object::as_float)
                .collect::<Result<Vec<_>, _>>()
                .map_err(error)?;
            let (x, y, w, h) = (values[0], values[1], values[2], values[3]);
            output.extend([
                Operation::new("m", vec![x.into(), y.into()]),
                Operation::new("l", vec![(x + w).into(), y.into()]),
                Operation::new("l", vec![(x + w).into(), (y + h).into()]),
                Operation::new("l", vec![x.into(), (y + h).into()]),
                Operation::new("h", vec![]),
            ]);
            continue;
        }
        if operation.operator == "Do" {
            if let Some(name) = operation.operands.first().and_then(|v| v.as_name().ok()) {
                if let Ok(value) = scope.get(b"XObject") {
                    let objects = resolve_dictionary(document, value)?;
                    if let Ok(Object::Reference(id)) = objects.get(name) {
                        let stream = document
                            .get_object(*id)
                            .and_then(Object::as_stream)
                            .map_err(error)?;
                        if stream.dict.get(b"Subtype").and_then(Object::as_name).ok()
                            == Some(b"Form")
                        {
                            if active.len() >= 64 || !active.insert(*id) {
                                return Err(error("recursive Form XObject"));
                            }
                            let child_scope = match stream.dict.get(b"Resources") {
                                Ok(value) => resolve_dictionary(document, value)?,
                                Err(_) => scope.clone(),
                            };
                            let data = stream.get_plain_content().map_err(error)?;
                            let child = Content::decode(&data).map_err(error)?;
                            output.push(Operation::new("q", vec![]));
                            if let Ok(matrix) =
                                stream.dict.get(b"Matrix").and_then(Object::as_array)
                            {
                                output.push(Operation::new("cm", matrix.clone()));
                            }
                            output.extend(expand_forms(
                                document,
                                &child.operations,
                                &child_scope,
                                merged,
                                sequence,
                                active,
                            )?);
                            output.push(Operation::new("Q", vec![]));
                            active.remove(id);
                            continue;
                        }
                    }
                }
            }
        }
        let mut operation = operation.clone();
        let category = match operation.operator.as_str() {
            "Tf" => Some("Font"),
            "Do" => Some("XObject"),
            "gs" => Some("ExtGState"),
            "cs" | "CS" => Some("ColorSpace"),
            "scn" | "SCN" => Some("Pattern"),
            "sh" => Some("Shading"),
            "BDC" | "DP" => Some("Properties"),
            _ => None,
        };
        if let Some(category) = category {
            for operand in &mut operation.operands {
                if let Object::Name(name) = operand {
                    if let Some(renamed) = names.get(&(category.to_string(), name.clone())) {
                        *name = renamed.clone();
                    }
                }
            }
        }
        output.push(operation);
    }
    Ok(output)
}

/// The geometry extractor restores CTM at Q but not font/spacing state. Emit
/// explicit restores in the disposable copy, including at the end of forms.
fn restore_text_state(operations: Vec<Operation>) -> Vec<Operation> {
    let mut state = HashMap::from([
        ("Tc", vec![0.into()]),
        ("Tw", vec![0.into()]),
        ("Tz", vec![100.into()]),
        ("Ts", vec![0.into()]),
        ("TL", vec![0.into()]),
        ("Tr", vec![0.into()]),
    ]);
    let mut stack = Vec::new();
    let mut output = Vec::new();
    for operation in operations {
        match operation.operator.as_str() {
            "q" => stack.push(state.clone()),
            "Q" => {
                output.push(operation);
                if let Some(saved) = stack.pop() {
                    state = saved;
                    for key in ["Tf", "Tc", "Tw", "Tz", "Ts", "TL", "Tr"] {
                        if let Some(value) = state.get(key) {
                            output.push(Operation::new(key, value.clone()));
                        }
                    }
                }
                continue;
            }
            "Tf" => {
                state.insert("Tf", operation.operands.clone());
            }
            "Tc" => {
                state.insert("Tc", operation.operands.clone());
            }
            "Tw" => {
                state.insert("Tw", operation.operands.clone());
            }
            "Tz" => {
                state.insert("Tz", operation.operands.clone());
            }
            "Ts" => {
                state.insert("Ts", operation.operands.clone());
            }
            "TL" => {
                state.insert("TL", operation.operands.clone());
            }
            "Tr" => {
                state.insert("Tr", operation.operands.clone());
            }
            "TD" if operation.operands.len() == 2 => {
                if let Ok(ty) = operation.operands[1].as_float() {
                    state.insert("TL", vec![(-ty).into()]);
                }
            }
            "\"" if operation.operands.len() == 3 => {
                state.insert("Tw", vec![operation.operands[0].clone()]);
                state.insert("Tc", vec![operation.operands[1].clone()]);
            }
            _ => {}
        }
        output.push(operation);
    }
    output
}

fn filter_text(operations: Vec<Operation>, extraction: bool) -> Vec<Operation> {
    let mut result = Vec::new();
    let mut mode = 0;
    let mut stack = Vec::new();
    for operation in operations {
        match operation.operator.as_str() {
            "q" => stack.push(mode),
            "Q" => {
                if let Some(saved) = stack.pop() {
                    mode = saved;
                }
            }
            "Tr" => {
                mode = operation
                    .operands
                    .first()
                    .and_then(|v| v.as_i64().ok())
                    .unwrap_or(0)
            }
            _ => {}
        }
        let shows_text = matches!(operation.operator.as_str(), "Tj" | "TJ" | "'" | "\"");
        if shows_text {
            if extraction && !matches!(mode, 3 | 7) || !extraction && mode == 3 {
                result.push(operation);
                continue;
            }
            if !extraction && mode >= 4 {
                // Preserve text-shaped clipping of subsequent artwork without
                // painting the original characters over the translation.
                result.push(Operation::new("Tr", vec![7.into()]));
                result.push(operation);
                result.push(Operation::new("Tr", vec![mode.into()]));
                continue;
            }
        }
        match operation.operator.as_str() {
            "Tj" | "TJ" => {}
            "'" => result.push(Operation::new("T*", vec![])),
            "\"" => {
                if operation.operands.len() == 3 {
                    result.push(Operation::new("Tw", vec![operation.operands[0].clone()]));
                    result.push(Operation::new("Tc", vec![operation.operands[1].clone()]));
                }
                result.push(Operation::new("T*", vec![]));
            }
            _ => result.push(operation),
        }
    }
    result
}

fn strip_forms(
    document: &mut Document,
    scope: &Dictionary,
    visited: &mut BTreeSet<ObjectId>,
) -> Result<(), FileError> {
    let Ok(value) = scope.get(b"XObject") else {
        return Ok(());
    };
    let objects = resolve_dictionary(document, value)?;
    for (_, value) in objects.iter() {
        let Ok(id) = value.as_reference() else {
            continue;
        };
        if !visited.insert(id) {
            continue;
        }
        let stream = document
            .get_object(id)
            .and_then(Object::as_stream)
            .map_err(error)?
            .clone();
        if stream.dict.get(b"Subtype").and_then(Object::as_name).ok() != Some(b"Form") {
            continue;
        }
        let child_scope = match stream.dict.get(b"Resources") {
            Ok(value) => resolve_dictionary(document, value)?,
            Err(_) => scope.clone(),
        };
        let content =
            Content::decode(&stream.get_plain_content().map_err(error)?).map_err(error)?;
        let mut replacement = stream;
        replacement.set_plain_content(
            Content {
                operations: filter_text(content.operations, false),
            }
            .encode()
            .map_err(error)?,
        );
        document.objects.insert(id, Object::Stream(replacement));
        strip_forms(document, &child_scope, visited)?;
    }
    Ok(())
}

/// Graft generated translation forms onto the source pages. All original page
/// dictionaries, annotations, outlines, metadata, images and artwork survive.
pub(crate) fn apply_translation(source: &[u8], overlay: &[u8]) -> Result<Vec<u8>, FileError> {
    let mut document = Document::load_mem(source).map_err(error)?;
    let mut overlay = Document::load_mem(overlay).map_err(error)?;
    overlay.renumber_objects_with(document.max_id + 1);
    let source_pages = document.get_pages().into_values().collect::<Vec<_>>();
    let overlay_pages = overlay.get_pages().into_values().collect::<Vec<_>>();
    if source_pages.len() != overlay_pages.len() {
        return Err(error("translation page count changed"));
    }
    let mut forms = Vec::new();
    for page in &overlay_pages {
        let mut dictionary = Dictionary::new();
        dictionary.set("Type", Object::Name(b"XObject".to_vec()));
        dictionary.set("Subtype", Object::Name(b"Form".to_vec()));
        dictionary.set("Resources", resources(&overlay, *page)?);
        forms.push((dictionary, overlay.get_page_content(*page)));
    }
    document.max_id = document.max_id.max(overlay.max_id);
    document.objects.extend(overlay.objects);
    let mut visited = BTreeSet::new();
    for (page, (mut dictionary, data)) in source_pages.into_iter().zip(forms) {
        let mut scope = resources(&document, page)?;
        strip_forms(&mut document, &scope, &mut visited)?;
        dictionary.set("BBox", inherited(&document, page, b"MediaBox")?);
        let form = document.add_object(Stream::new(dictionary, data));
        let mut objects = match scope.get(b"XObject") {
            Ok(value) => resolve_dictionary(&document, value)?,
            Err(_) => Dictionary::new(),
        };
        let mut name = b"PromLexTranslation".to_vec();
        while objects.has(&name) {
            name.push(b'_');
        }
        objects.set(name.clone(), Object::Reference(form));
        scope.set("XObject", objects);
        let content = Content::decode(&document.get_page_content(page)).map_err(error)?;
        let mut operations = vec![Operation::new("q", vec![])];
        operations.extend(filter_text(content.operations, false));
        operations.extend([
            Operation::new("Q", vec![]),
            Operation::new("q", vec![]),
            Operation::new("Do", vec![Object::Name(name)]),
            Operation::new("Q", vec![]),
        ]);
        document
            .change_page_content(page, Content { operations }.encode().map_err(error)?)
            .map_err(error)?;
        document
            .get_dictionary_mut(page)
            .map_err(error)?
            .set("Resources", scope);
    }
    // Only objects reachable from the source catalog are needed (the imported
    // overlay catalog/pages themselves are deliberately unused).
    document.prune_objects();
    document.compress();
    let mut output = Vec::new();
    document.save_to(&mut output).map_err(error)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::{extraction_copy, filter_text};
    use lopdf::{
        content::{Content, Operation},
        dictionary, Document, Object, Stream,
    };

    #[test]
    fn collection_fonts_embed_the_selected_face_with_valid_sfnt_checksums() {
        let faces = [
            printpdf::BuiltinFont::Helvetica.get_subset_font().bytes,
            printpdf::BuiltinFont::HelveticaBold.get_subset_font().bytes,
        ];
        let mut collection = b"ttcf\0\x01\0\0\0\0\0\x02\0\0\0\0\0\0\0\0".to_vec();
        for (index, face) in faces.iter().enumerate() {
            let base = collection.len();
            collection[12 + index * 4..16 + index * 4]
                .copy_from_slice(&(base as u32).to_be_bytes());
            collection.extend_from_slice(face);
            let count = u16::from_be_bytes(face[4..6].try_into().unwrap()) as usize;
            for table in 0..count {
                let entry = 12 + table * 16;
                let offset = u32::from_be_bytes(face[entry + 8..entry + 12].try_into().unwrap());
                collection[base + entry + 8..base + entry + 12]
                    .copy_from_slice(&(offset + base as u32).to_be_bytes());
            }
            collection.resize((collection.len() + 3) & !3, 0);
        }
        for (index, face) in faces.iter().enumerate() {
            let extracted = super::standalone_font_face(&collection, index as u32).unwrap();
            assert_eq!(super::sfnt_checksum(&extracted), 0xB1B0AFBA);
            let expected = printpdf::ParsedFont::from_bytes(face, 0, &mut vec![]).unwrap();
            let parsed = printpdf::ParsedFont::from_bytes(&extracted, 0, &mut vec![]).unwrap();
            assert_eq!(parsed.glyph_widths, expected.glyph_widths);
            assert_eq!(parsed.codepoint_to_glyph, expected.codepoint_to_glyph);
        }
        assert!(super::standalone_font_face(&collection, 2).is_none());
        assert!(super::standalone_font_face(&collection[..15], 0).is_none());
    }

    #[test]
    fn clipping_text_keeps_its_mask_and_invisible_text_is_excluded_from_extraction() {
        let show = Operation::new("Tj", vec![Object::string_literal("mask")]);
        let operations = vec![
            Operation::new("q", vec![]),
            Operation::new("Tr", vec![4.into()]),
            show.clone(),
            Operation::new("Q", vec![]),
            show.clone(),
            Operation::new("Tr", vec![3.into()]),
            show.clone(),
        ];
        let edited = filter_text(operations, false);
        assert_eq!(
            edited
                .iter()
                .filter(|operation| operation.operator == "Tj")
                .count(),
            2
        );
        assert!(edited.windows(3).any(|run| run[0].operator == "Tr"
            && run[0].operands == vec![7.into()]
            && run[1].operator == show.operator
            && run[1].operands == show.operands));
        let extracted = filter_text(edited, true);
        assert!(!extracted.iter().any(|operation| operation.operator == "Tj"));
    }

    #[test]
    fn extraction_keeps_blank_pages_without_resources() {
        let mut document = Document::with_version("1.7");
        let pages = document.new_object_id();
        let stream = document.add_object(Stream::new(lopdf::Dictionary::new(), vec![]));
        let page = document
            .add_object(dictionary! { "Type" => "Page", "Parent" => pages, "Contents" => stream });
        document.objects.insert(
            pages,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => vec![Object::Reference(page)], "Count" => 1,
                "MediaBox" => vec![0.into(),0.into(),600.into(),800.into()],
            }),
        );
        let root = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
        document.trailer.set("Root", root);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        let copy = Document::load_mem(&extraction_copy(&bytes).unwrap()).unwrap();
        assert_eq!(copy.get_pages().len(), 1);
        assert!(Content::decode(&copy.get_page_content(page))
            .unwrap()
            .operations
            .is_empty());
    }
}
