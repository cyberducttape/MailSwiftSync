//! Bounded parsing for XLSX worksheet dimension references.

pub(crate) fn validate_dimension_reference(
    reference: &str,
    max_columns: usize,
    max_rows: u32,
) -> Result<(), String> {
    let mut endpoints = reference.split(':');
    let (start_columns, start_rows) = parse_cell_reference(
        endpoints
            .next()
            .ok_or_else(|| "The worksheet dimension has no starting cell.".to_owned())?,
    )?;
    let (columns, rows) = if let Some(end) = endpoints.next() {
        if endpoints.next().is_some() {
            return Err("The worksheet dimension contains too many range endpoints.".into());
        }
        let (end_columns, end_rows) = parse_cell_reference(end)?;
        (start_columns.max(end_columns), start_rows.max(end_rows))
    } else {
        (start_columns, start_rows)
    };
    if columns > max_columns {
        return Err(format!(
            "The worksheet declares {columns} columns; the limit is {max_columns}."
        ));
    }
    if rows > max_rows {
        return Err(format!(
            "The worksheet declares {rows} rows; the limit is {max_rows}."
        ));
    }
    Ok(())
}

pub(crate) fn parse_cell_reference(reference: &str) -> Result<(usize, u32), String> {
    let split = reference
        .find(|character: char| character.is_ascii_digit())
        .ok_or_else(|| "The worksheet dimension has no row number.".to_owned())?;
    let (letters, digits) = reference.split_at(split);
    let mut columns = 0_usize;
    for character in letters.chars() {
        if !character.is_ascii_alphabetic() {
            return Err("The worksheet dimension has an invalid column.".into());
        }
        let value = character.to_ascii_uppercase() as usize - 'A' as usize + 1;
        columns = columns
            .checked_mul(26)
            .and_then(|columns| columns.checked_add(value))
            .ok_or_else(|| "The worksheet dimension column is too large.".to_owned())?;
    }
    let rows = digits
        .parse::<u32>()
        .map_err(|_| "The worksheet dimension has an invalid row.".to_owned())?;
    if rows == 0 || columns == 0 {
        return Err("The worksheet dimension cell coordinates must be positive.".into());
    }
    Ok((columns, rows))
}
