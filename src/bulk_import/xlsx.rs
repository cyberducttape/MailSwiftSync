//! Bounded reader for the small OOXML subset used by mailbox-list imports.
//!
//! This intentionally does not implement Excel's formula, pivot, chart, macro,
//! or legacy binary formats. It reads worksheet names and cached cell values
//! only, after the parent module has validated archive, XML, reference, and
//! resource bounds.

use quick_xml::{Reader, events::Event};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{BufReader, Read, Seek},
};
use zip::ZipArchive;

#[derive(Debug, Clone)]
struct Sheet {
    name: String,
    path: String,
}

pub(super) struct Workbook<R = File> {
    archive: ZipArchive<R>,
    sheets: Vec<Sheet>,
    shared_strings_path: Option<String>,
}

impl<R: Read + Seek> Workbook<R> {
    pub(super) fn new(mut archive: ZipArchive<R>) -> Result<Self, String> {
        super::validate_xlsx_archive(&mut archive)?;
        let root_relationships = read_relationships(&mut archive, "_rels/.rels", "")?;
        let mut workbook_paths = root_relationships
            .values()
            .filter(|relationship| relationship.kind.ends_with("/officeDocument"))
            .map(|relationship| relationship.target.clone());
        let workbook_path = workbook_paths
            .next()
            .ok_or_else(|| "The XLSX package has no workbook relationship.".to_owned())?;
        if workbook_paths.next().is_some() {
            return Err("The XLSX package has ambiguous workbook relationships.".into());
        }
        let workbook_directory = part_directory(&workbook_path);
        let workbook_relationships_path = relationship_part_path(&workbook_path);
        let relationships = read_relationships(
            &mut archive,
            &workbook_relationships_path,
            workbook_directory,
        )?;
        let sheets = read_sheets(&mut archive, &workbook_path, &relationships)?;
        let mut shared_strings_paths = relationships
            .values()
            .filter(|relationship| relationship.kind.ends_with("/sharedStrings"))
            .map(|relationship| relationship.target.clone());
        let shared_strings_path = shared_strings_paths.next();
        if shared_strings_paths.next().is_some() {
            return Err("The XLSX workbook has ambiguous shared-string relationships.".into());
        }
        if sheets.is_empty() {
            return Err("The workbook has no worksheets.".into());
        }
        let mut sheet_parts = HashSet::new();
        for sheet in &sheets {
            if !sheet_parts.insert(sheet.path.clone()) {
                return Err("The XLSX workbook reuses a worksheet relationship target.".into());
            }
            let mut entry = archive.by_name(&sheet.path).map_err(|_| {
                "The XLSX worksheet part referenced by the workbook is missing.".to_owned()
            })?;
            if !super::validate_xlsx_sheet_entry_dimensions(&mut entry)? {
                return Err("The workbook relationship does not target a worksheet part.".into());
            }
        }
        Ok(Self {
            archive,
            sheets,
            shared_strings_path,
        })
    }

    pub(super) fn sheet_names(&self) -> Vec<String> {
        self.sheets.iter().map(|sheet| sheet.name.clone()).collect()
    }

    pub(super) fn read_sheet(&mut self, index: usize) -> Result<(usize, Vec<Vec<String>>), String> {
        let sheet = self
            .sheets
            .get(index)
            .ok_or_else(|| format!("The workbook has no worksheet at index {index}."))?
            .clone();
        let shared_strings = match self.shared_strings_path.clone() {
            Some(path) => read_shared_strings(&mut self.archive, &path)?,
            None => Vec::new(),
        };
        read_worksheet(&mut self.archive, &sheet.path, &shared_strings)
    }
}

struct Relationship {
    kind: String,
    target: String,
}

fn read_relationships<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    path: &str,
    base_directory: &str,
) -> Result<HashMap<String, Relationship>, String> {
    let entry = archive
        .by_name(path)
        .map_err(|_| format!("The XLSX package is missing relationship part {path:?}."))?;
    let mut xml = Reader::from_reader(quick_xml::encoding::DecodingReader::new(BufReader::new(
        entry,
    )));
    xml.config_mut().trim_text(true);
    let mut relationships = HashMap::new();
    let mut root_seen = false;
    let mut buffer = Vec::with_capacity(1024);
    loop {
        buffer.clear();
        match xml
            .read_event_into(&mut buffer)
            .map_err(|error| format!("XLSX relationships are malformed: {error}"))?
        {
            Event::Start(element) | Event::Empty(element) if !root_seen => {
                if element.local_name().as_ref() != "Relationships" {
                    return Err("An XLSX relationship part has an invalid root element.".into());
                }
                root_seen = true;
            }
            Event::Start(element) | Event::Empty(element)
                if element.local_name().as_ref() == "Relationship" =>
            {
                if !root_seen {
                    return Err("An XLSX relationship precedes its root element.".into());
                }
                let id = attribute(&element, "Id")?
                    .ok_or_else(|| "An XLSX relationship has no Id.".to_owned())?;
                let kind = attribute(&element, "Type")?
                    .ok_or_else(|| "An XLSX relationship has no Type.".to_owned())?;
                let target = attribute(&element, "Target")?
                    .ok_or_else(|| "An XLSX relationship has no Target.".to_owned())?;
                if attribute(&element, "TargetMode")?
                    .is_some_and(|mode| mode.eq_ignore_ascii_case("external"))
                {
                    return Err("External XLSX relationships are not supported.".into());
                }
                let target = resolve_part_path(base_directory, &target)?;
                if relationships
                    .insert(id, Relationship { kind, target })
                    .is_some()
                {
                    return Err("The XLSX package has a duplicate relationship Id.".into());
                }
            }
            Event::DocType(_) => return Err("XLSX XML document types are not supported.".into()),
            Event::Eof => break,
            _ => {}
        }
    }
    if !root_seen {
        return Err("An XLSX relationship part has no root element.".into());
    }
    Ok(relationships)
}

fn read_sheets<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    workbook_path: &str,
    relationships: &HashMap<String, Relationship>,
) -> Result<Vec<Sheet>, String> {
    let entry = archive
        .by_name(workbook_path)
        .map_err(|_| "The XLSX workbook part referenced by the package is missing.".to_owned())?;
    let mut xml = Reader::from_reader(quick_xml::encoding::DecodingReader::new(BufReader::new(
        entry,
    )));
    xml.config_mut().trim_text(true);
    let mut sheets = Vec::new();
    let mut names = HashSet::new();
    let mut root_seen = false;
    let mut in_sheets = false;
    let mut sheets_seen = false;
    let mut buffer = Vec::with_capacity(1024);
    loop {
        buffer.clear();
        match xml
            .read_event_into(&mut buffer)
            .map_err(|error| format!("The XLSX workbook is malformed: {error}"))?
        {
            Event::Start(element) if !root_seen => {
                if element.local_name().as_ref() != "workbook" {
                    return Err("The XLSX workbook part has an invalid root element.".into());
                }
                root_seen = true;
            }
            Event::Empty(element) if !root_seen => {
                if element.local_name().as_ref() != "workbook" {
                    return Err("The XLSX workbook part has an invalid root element.".into());
                }
                root_seen = true;
            }
            Event::Start(element) if element.local_name().as_ref() == "sheets" => {
                if sheets_seen {
                    return Err("The XLSX workbook has duplicate sheets elements.".into());
                }
                sheets_seen = true;
                in_sheets = true;
            }
            Event::End(element) if element.local_name().as_ref() == "sheets" => {
                in_sheets = false;
            }
            Event::Start(element) | Event::Empty(element)
                if element.local_name().as_ref() == "sheet" =>
            {
                if !root_seen || !in_sheets {
                    return Err("An XLSX sheet element is outside the sheets list.".into());
                }
                let name = attribute(&element, "name")?
                    .ok_or_else(|| "An XLSX worksheet has no name.".to_owned())?;
                let relation_id = attribute(&element, "id")?
                    .ok_or_else(|| "An XLSX worksheet has no relationship Id.".to_owned())?;
                if name.trim().is_empty() || name.len() > 31 || !names.insert(name.clone()) {
                    return Err(
                        "The XLSX workbook has an empty, overlong, or duplicate sheet name.".into(),
                    );
                }
                let relationship = relationships.get(&relation_id).ok_or_else(|| {
                    "An XLSX worksheet references a missing relationship.".to_owned()
                })?;
                if !relationship.kind.ends_with("/worksheet") {
                    return Err("An XLSX sheet relationship is not a worksheet.".into());
                }
                sheets.push(Sheet {
                    name,
                    path: relationship.target.clone(),
                });
            }
            Event::DocType(_) => return Err("XLSX XML document types are not supported.".into()),
            Event::Eof => break,
            _ => {}
        }
    }
    if !root_seen || !sheets_seen || in_sheets {
        return Err("The XLSX workbook has incomplete workbook or sheets elements.".into());
    }
    Ok(sheets)
}

fn read_shared_strings<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    path: &str,
) -> Result<Vec<String>, String> {
    let entry = archive.by_name(path).map_err(|_| {
        "The XLSX shared-string part referenced by the workbook is missing.".to_owned()
    })?;
    let mut xml = Reader::from_reader(quick_xml::encoding::DecodingReader::new(BufReader::new(
        entry,
    )));
    xml.config_mut().trim_text(false);
    let mut values = Vec::new();
    let mut current: Option<String> = None;
    let mut in_text = false;
    let mut total_bytes = 0_usize;
    let mut buffer = Vec::with_capacity(1024);
    loop {
        buffer.clear();
        match xml
            .read_event_into(&mut buffer)
            .map_err(|error| format!("The XLSX shared-string table is malformed: {error}"))?
        {
            Event::Start(element) if element.local_name().as_ref() == "si" => {
                if current.is_some() || values.len() >= crate::MAX_BULK_IMPORT_SHARED_STRINGS {
                    return Err(
                        "The XLSX shared-string table exceeds its entry limit or is nested.".into(),
                    );
                }
                current = Some(String::new());
            }
            Event::Empty(element) if element.local_name().as_ref() == "si" => {
                if current.is_some() || values.len() >= crate::MAX_BULK_IMPORT_SHARED_STRINGS {
                    return Err(
                        "The XLSX shared-string table exceeds its entry limit or is nested.".into(),
                    );
                }
                values.push(String::new());
            }
            Event::Start(element) if current.is_some() && element.local_name().as_ref() == "t" => {
                in_text = true;
            }
            Event::End(element) if element.local_name().as_ref() == "t" => in_text = false,
            Event::Text(text) if in_text => {
                let decoded = text.xml10_content();
                let value = current
                    .as_mut()
                    .ok_or_else(|| "XLSX shared-string text is outside an entry.".to_owned())?;
                append_bounded(value, &decoded, &mut total_bytes)?;
            }
            Event::CData(text) if in_text => {
                let decoded = text.xml10_content();
                let value = current
                    .as_mut()
                    .ok_or_else(|| "XLSX shared-string text is outside an entry.".to_owned())?;
                append_bounded(value, &decoded, &mut total_bytes)?;
            }
            Event::GeneralRef(reference) if in_text => {
                let decoded = resolve_reference(&reference)?;
                let value = current
                    .as_mut()
                    .ok_or_else(|| "XLSX shared-string text is outside an entry.".to_owned())?;
                append_bounded(value, &decoded, &mut total_bytes)?;
            }
            Event::End(element) if element.local_name().as_ref() == "si" => {
                values.push(current.take().ok_or_else(|| {
                    "The XLSX shared-string table has an unmatched entry end.".to_owned()
                })?);
            }
            Event::DocType(_) => return Err("XLSX XML document types are not supported.".into()),
            Event::Eof => break,
            _ => {}
        }
    }
    if current.is_some() || in_text {
        return Err("The XLSX shared-string table is incomplete.".into());
    }
    Ok(values)
}

fn append_bounded(target: &mut String, value: &str, total: &mut usize) -> Result<(), String> {
    let new_len = target
        .len()
        .checked_add(value.len())
        .ok_or_else(|| "An XLSX text value exceeds the supported size.".to_owned())?;
    let new_total = total
        .checked_add(value.len())
        .ok_or_else(|| "The XLSX text data exceeds the supported size.".to_owned())?;
    if new_len > crate::MAX_BULK_IMPORT_CELL_BYTES {
        return Err(format!(
            "An XLSX cell exceeds the {}-byte limit.",
            crate::MAX_BULK_IMPORT_CELL_BYTES
        ));
    }
    if new_total as u64 > crate::MAX_BULK_IMPORT_SHARED_STRING_BYTES {
        return Err("The XLSX shared strings exceed the configured byte limit.".into());
    }
    target.push_str(value);
    *total = new_total;
    Ok(())
}

fn resolve_reference(reference: &quick_xml::events::BytesRef<'_>) -> Result<String, String> {
    if let Some(character) = reference
        .resolve_char_ref()
        .map_err(|error| format!("An XLSX XML character reference is invalid: {error}"))?
    {
        return Ok(character.to_string());
    }
    quick_xml::escape::resolve_predefined_entity(reference.as_ref())
        .map(str::to_owned)
        .ok_or_else(|| "An XLSX XML entity is not predefined.".to_owned())
}

struct CellBuilder {
    row: usize,
    column: usize,
    kind: String,
    value: String,
    inline: String,
}

fn read_worksheet<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    path: &str,
    shared_strings: &[String],
) -> Result<(usize, Vec<Vec<String>>), String> {
    let entry = archive
        .by_name(path)
        .map_err(|_| "The XLSX worksheet part referenced by the workbook is missing.".to_owned())?;
    let mut xml = Reader::from_reader(quick_xml::encoding::DecodingReader::new(BufReader::new(
        entry,
    )));
    xml.config_mut().trim_text(false);
    let mut rows = Vec::new();
    let mut cell: Option<CellBuilder> = None;
    let mut root_seen = false;
    let mut sheet_data_seen = false;
    let mut in_sheet_data = false;
    let mut in_value = false;
    let mut in_inline_text = false;
    let mut value_seen = false;
    let mut inline_seen = false;
    let mut current_row = 0_usize;
    let mut current_column = 0_usize;
    let mut first_row = None;
    let mut first_column = None;
    let mut last_column = None;
    let mut cell_count = 0_usize;
    let mut buffer = Vec::with_capacity(1024);
    loop {
        buffer.clear();
        match xml
            .read_event_into(&mut buffer)
            .map_err(|error| format!("The XLSX worksheet is malformed: {error}"))?
        {
            Event::Start(element) if !root_seen => {
                if element.local_name().as_ref() != "worksheet" {
                    return Err(
                        "The workbook relationship does not target a worksheet part.".into(),
                    );
                }
                root_seen = true;
            }
            Event::Empty(element) if !root_seen => {
                if element.local_name().as_ref() != "worksheet" {
                    return Err(
                        "The workbook relationship does not target a worksheet part.".into(),
                    );
                }
                root_seen = true;
            }
            Event::Start(element) if root_seen && element.local_name().as_ref() == "sheetData" => {
                if sheet_data_seen {
                    return Err("The XLSX worksheet has duplicate sheetData elements.".into());
                }
                sheet_data_seen = true;
                in_sheet_data = true;
            }
            Event::Empty(element) if root_seen && element.local_name().as_ref() == "sheetData" => {
                if sheet_data_seen {
                    return Err("The XLSX worksheet has duplicate sheetData elements.".into());
                }
                sheet_data_seen = true;
            }
            Event::End(element) if element.local_name().as_ref() == "sheetData" => {
                in_sheet_data = false;
            }
            Event::Start(element) if element.local_name().as_ref() == "row" => {
                if !in_sheet_data {
                    return Err("The XLSX worksheet row is outside sheetData.".into());
                }
                let row = attribute(&element, "r")?
                    .map(|row| parse_positive_usize(&row, "row"))
                    .transpose()?
                    .unwrap_or_else(|| current_row.saturating_add(1));
                validate_coordinates(row, 1)?;
                if row <= current_row {
                    return Err("XLSX worksheet rows are duplicate or out of order.".into());
                }
                current_row = row;
                current_column = 0;
            }
            Event::Empty(element) if element.local_name().as_ref() == "row" => {
                if !in_sheet_data {
                    return Err("The XLSX worksheet row is outside sheetData.".into());
                }
                let row = attribute(&element, "r")?
                    .map(|row| parse_positive_usize(&row, "row"))
                    .transpose()?
                    .unwrap_or_else(|| current_row.saturating_add(1));
                validate_coordinates(row, 1)?;
                if row <= current_row {
                    return Err("XLSX worksheet rows are duplicate or out of order.".into());
                }
                current_row = row;
                current_column = 0;
            }
            Event::Start(element) if element.local_name().as_ref() == "c" => {
                if !in_sheet_data || current_row == 0 {
                    return Err("The XLSX worksheet cell is outside a row in sheetData.".into());
                }
                if cell.is_some() {
                    return Err("The XLSX worksheet contains nested cells.".into());
                }
                let (row, column) = cell_position(&element, current_row, current_column)?;
                validate_coordinates(row, column)?;
                if current_row != 0 && row != current_row {
                    return Err("An XLSX cell does not match its containing row.".into());
                }
                if row < current_row || column <= current_column {
                    return Err("XLSX worksheet cells are duplicate or out of order.".into());
                }
                current_row = row;
                current_column = column;
                let kind = attribute(&element, "t")?.unwrap_or_default();
                cell = Some(CellBuilder {
                    row,
                    column,
                    kind,
                    value: String::new(),
                    inline: String::new(),
                });
                value_seen = false;
                inline_seen = false;
            }
            Event::Empty(element) if element.local_name().as_ref() == "c" => {
                if !in_sheet_data || current_row == 0 {
                    return Err("The XLSX worksheet cell is outside a row in sheetData.".into());
                }
                if cell.is_some() {
                    return Err("The XLSX worksheet contains nested cells.".into());
                }
                let (row, column) = cell_position(&element, current_row, current_column)?;
                validate_coordinates(row, column)?;
                if current_row != 0 && row != current_row {
                    return Err("An XLSX cell does not match its containing row.".into());
                }
                if row < current_row || column <= current_column {
                    return Err("XLSX worksheet cells are duplicate or out of order.".into());
                }
                current_row = row;
                current_column = column;
                cell_count = cell_count.saturating_add(1);
                ensure_cell_count(cell_count)?;
                append_cell(
                    &mut rows,
                    &mut first_row,
                    &mut first_column,
                    &mut last_column,
                    row,
                    column,
                    String::new(),
                )?;
            }
            Event::Start(element) if cell.is_some() && element.local_name().as_ref() == "v" => {
                if in_value || value_seen {
                    return Err("An XLSX cell has nested or duplicate value elements.".into());
                }
                in_value = true;
                value_seen = true;
            }
            Event::Empty(element) if cell.is_some() && element.local_name().as_ref() == "v" => {
                if value_seen {
                    return Err("An XLSX cell has duplicate value elements.".into());
                }
                value_seen = true;
            }
            Event::Start(element) if cell.is_some() && element.local_name().as_ref() == "is" => {
                if inline_seen {
                    return Err("An XLSX cell has duplicate inline-string elements.".into());
                }
                inline_seen = true;
            }
            Event::Empty(element) if cell.is_some() && element.local_name().as_ref() == "is" => {
                if inline_seen {
                    return Err("An XLSX cell has duplicate inline-string elements.".into());
                }
                inline_seen = true;
            }
            Event::Start(element) if cell.is_some() && element.local_name().as_ref() == "t" => {
                in_inline_text = true;
            }
            Event::Text(text) if cell.is_some() && (in_value || in_inline_text) => {
                let decoded = text.xml10_content();
                let current_cell = cell
                    .as_mut()
                    .ok_or_else(|| "XLSX worksheet text is outside a cell.".to_owned())?;
                let target = if in_value {
                    &mut current_cell.value
                } else {
                    &mut current_cell.inline
                };
                let mut total = target.len();
                append_bounded(target, &decoded, &mut total)?;
            }
            Event::CData(text) if cell.is_some() && (in_value || in_inline_text) => {
                let decoded = text.xml10_content();
                let current_cell = cell
                    .as_mut()
                    .ok_or_else(|| "XLSX worksheet text is outside a cell.".to_owned())?;
                let target = if in_value {
                    &mut current_cell.value
                } else {
                    &mut current_cell.inline
                };
                let mut total = target.len();
                append_bounded(target, &decoded, &mut total)?;
            }
            Event::GeneralRef(reference) if cell.is_some() && (in_value || in_inline_text) => {
                let decoded = resolve_reference(&reference)?;
                let current_cell = cell
                    .as_mut()
                    .ok_or_else(|| "XLSX worksheet text is outside a cell.".to_owned())?;
                let target = if in_value {
                    &mut current_cell.value
                } else {
                    &mut current_cell.inline
                };
                let mut total = target.len();
                append_bounded(target, &decoded, &mut total)?;
            }
            Event::End(element) if element.local_name().as_ref() == "v" => in_value = false,
            Event::End(element) if element.local_name().as_ref() == "t" => {
                in_inline_text = false;
            }
            Event::End(element) if element.local_name().as_ref() == "c" => {
                let parsed = cell
                    .take()
                    .ok_or_else(|| "The XLSX worksheet has an unmatched cell end.".to_owned())?;
                let row = parsed.row;
                let column = parsed.column;
                let value = convert_cell_value(parsed, shared_strings)?;
                cell_count = cell_count.saturating_add(1);
                ensure_cell_count(cell_count)?;
                append_cell(
                    &mut rows,
                    &mut first_row,
                    &mut first_column,
                    &mut last_column,
                    row,
                    column,
                    value,
                )?;
                in_value = false;
                in_inline_text = false;
            }
            Event::DocType(_) => return Err("XLSX XML document types are not supported.".into()),
            Event::Eof => break,
            _ => {}
        }
    }
    if !root_seen || !sheet_data_seen {
        return Err("The XLSX worksheet has no valid root or sheetData element.".into());
    }
    if cell.is_some() || in_value || in_inline_text || in_sheet_data {
        return Err("The XLSX worksheet ends inside a cell value.".into());
    }
    Ok((first_row.unwrap_or(1), rows))
}

fn append_cell(
    rows: &mut Vec<Vec<String>>,
    first_row: &mut Option<usize>,
    first_column: &mut Option<usize>,
    last_column: &mut Option<usize>,
    row: usize,
    column: usize,
    value: String,
) -> Result<(), String> {
    let start_row = *first_row.get_or_insert(row);
    let start_column = *first_column.get_or_insert(column);
    let end_column = last_column.unwrap_or(column).max(column);
    let new_start_column = start_column.min(column);
    let width = end_column
        .checked_sub(new_start_column)
        .and_then(|width| width.checked_add(1))
        .ok_or_else(|| "The XLSX worksheet range is too wide.".to_owned())?;
    let row_count = row
        .checked_sub(start_row)
        .and_then(|row| row.checked_add(1))
        .ok_or_else(|| "The XLSX worksheet range is too large.".to_owned())?;
    if width
        .checked_mul(row_count)
        .is_none_or(|area| area > crate::MAX_BULK_IMPORT_WORKSHEET_CELLS)
    {
        return Err(format!(
            "The worksheet's used cell range exceeds the {}-cell import limit.",
            crate::MAX_BULK_IMPORT_WORKSHEET_CELLS
        ));
    }
    if new_start_column < start_column {
        let shift = start_column - new_start_column;
        for existing in rows.iter_mut() {
            existing.splice(0..0, std::iter::repeat_n(String::new(), shift));
        }
        *first_column = Some(new_start_column);
    }
    let previous_width = rows.first().map_or(0, Vec::len);
    if width > previous_width {
        for existing in rows.iter_mut() {
            existing.resize_with(width, String::new);
        }
    }
    while rows.len() < row_count {
        rows.push(vec![String::new(); width]);
    }
    rows[row - start_row][column - new_start_column] = value;
    *last_column = Some(end_column);
    Ok(())
}

fn ensure_cell_count(count: usize) -> Result<(), String> {
    if count > crate::MAX_BULK_IMPORT_WORKSHEET_CELLS {
        Err(format!(
            "The worksheet exceeds the {}-cell import limit.",
            crate::MAX_BULK_IMPORT_WORKSHEET_CELLS
        ))
    } else {
        Ok(())
    }
}

fn cell_position(
    element: &quick_xml::events::BytesStart<'_>,
    current_row: usize,
    current_column: usize,
) -> Result<(usize, usize), String> {
    let Some(reference) = attribute(element, "r")? else {
        if current_row == 0 {
            return Err("The first XLSX worksheet cell has no coordinate.".into());
        }
        return Ok((current_row, current_column.saturating_add(1)));
    };
    let (column, row) = super::xlsx_reference::parse_cell_reference(&reference)?;
    Ok((row as usize, column))
}

fn validate_coordinates(row: usize, column: usize) -> Result<(), String> {
    if row == 0 || row > crate::MAX_BULK_IMPORT_ROWS.saturating_add(1) {
        return Err(format!(
            "The worksheet contains a cell beyond the {}-row import limit.",
            crate::MAX_BULK_IMPORT_ROWS
        ));
    }
    if column == 0 || column > crate::MAX_BULK_IMPORT_COLUMNS {
        return Err(format!(
            "The worksheet contains a cell beyond the {}-column import limit.",
            crate::MAX_BULK_IMPORT_COLUMNS
        ));
    }
    Ok(())
}

fn parse_positive_usize(value: &str, label: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("The XLSX worksheet {label} reference is invalid."))
}

fn convert_cell_value(cell: CellBuilder, shared_strings: &[String]) -> Result<String, String> {
    match cell.kind.as_str() {
        "s" => {
            let index = cell
                .value
                .parse::<usize>()
                .map_err(|_| "An XLSX cell has an invalid shared-string index.".to_owned())?;
            shared_strings
                .get(index)
                .cloned()
                .ok_or_else(|| "An XLSX cell references a missing shared string.".to_owned())
        }
        "inlineStr" => Ok(cell.inline),
        "b" => match cell.value.as_str() {
            "0" => Ok("FALSE".into()),
            "1" => Ok("TRUE".into()),
            _ => Err("An XLSX boolean cell must contain exactly 0 or 1.".into()),
        },
        "e" => Err("The selected XLSX worksheet contains an error-valued cell.".into()),
        "" | "n" => {
            if cell.value.is_empty() {
                return Ok(String::new());
            }
            let number = cell
                .value
                .parse::<f64>()
                .map_err(|_| "An XLSX numeric cell has an invalid value.".to_owned())?;
            if !number.is_finite() {
                return Err("An XLSX numeric cell is not finite.".into());
            }
            Ok(number.to_string())
        }
        "str" | "d" => Ok(cell.value),
        _ => Err(format!(
            "The selected XLSX worksheet uses unsupported cell type {:?}.",
            cell.kind
        )),
    }
}

fn attribute(
    element: &quick_xml::events::BytesStart<'_>,
    name: &str,
) -> Result<Option<String>, String> {
    let mut value = None;
    for attribute in element.attributes() {
        let attribute =
            attribute.map_err(|error| format!("Malformed XLSX XML attribute: {error}"))?;
        if attribute.key.local_name().as_ref() == name {
            if value.is_some() {
                return Err("An XLSX XML element has a duplicate attribute.".into());
            }
            value = Some(
                attribute
                    .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                    .map_err(|error| format!("Malformed XLSX XML attribute value: {error}"))?
                    .into_owned(),
            );
        }
    }
    Ok(value)
}

fn relationship_part_path(source: &str) -> String {
    let (directory, file) = source.rsplit_once('/').unwrap_or(("", source));
    if directory.is_empty() {
        format!("_rels/{file}.rels")
    } else {
        format!("{directory}/_rels/{file}.rels")
    }
}

fn part_directory(source: &str) -> &str {
    source
        .rsplit_once('/')
        .map_or("", |(directory, _)| directory)
}

pub(super) fn resolve_part_path(base_directory: &str, target: &str) -> Result<String, String> {
    if target.is_empty() || target.contains(['\\', '?', '#']) {
        return Err("An XLSX relationship has an invalid target path.".into());
    }
    let decoded = percent_decode(target)?;
    let mut parts = if decoded.starts_with('/') {
        Vec::new()
    } else {
        base_directory
            .split('/')
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect()
    };
    for part in decoded.trim_start_matches('/').split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err("An XLSX relationship escapes the package root.".into());
                }
            }
            value => parts.push(value.to_owned()),
        }
    }
    if parts.is_empty() {
        return Err("An XLSX relationship target is empty.".into());
    }
    Ok(parts.join("/"))
}

fn percent_decode(value: &str) -> Result<String, String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = bytes
                .get(index + 1)
                .and_then(|byte| (*byte as char).to_digit(16))
                .ok_or_else(|| "An XLSX relationship has malformed percent encoding.".to_owned())?;
            let low = bytes
                .get(index + 2)
                .and_then(|byte| (*byte as char).to_digit(16))
                .ok_or_else(|| "An XLSX relationship has malformed percent encoding.".to_owned())?;
            decoded.push(((high << 4) | low) as u8);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded)
        .map_err(|_| "An XLSX relationship target is not valid UTF-8.".to_owned())
}
