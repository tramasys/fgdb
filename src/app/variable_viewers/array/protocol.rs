//! Strict decoding of complete, bounded Python array responses.

use super::*;
use crate::debugger::array::{ArrayOrder, RANK_LIMIT};

pub(super) fn parse_error(output: &str) -> Option<String> {
    if output.len() > 256 * 1024 {
        return None;
    }

    let mut errors = output
        .lines()
        .filter_map(|line| line.strip_prefix("FGDB_ARRAY_ERROR:"));
    let error = errors.next()?;

    if error.len() > 4096 || errors.next().is_some() {
        return None;
    }

    String::from_utf8(type_metadata::decode_hex(error)?).ok()
}

pub(super) fn parse_shape(output: &str) -> Option<ArrayShape> {
    if output.len() > 256 * 1024 {
        return None;
    }

    let mut metadata = output
        .lines()
        .filter_map(|line| line.strip_prefix("FGDB_ARRAY_META:"));
    let mut fields = metadata.next()?.split('\t');

    let order = match fields.next()? {
        "row" => ArrayOrder::RowMajor,
        "column" => ArrayOrder::ColumnMajor,
        "sequence" => ArrayOrder::Sequence,
        _ => return None,
    };

    let bounds = fields
        .next()?
        .split(',')
        .take(RANK_LIMIT + 1)
        .map(|dimension| {
            let (lower, upper) = dimension.split_once(':')?;
            Some((lower.parse::<i64>().ok()?, upper.parse::<i64>().ok()?))
        })
        .collect::<Option<Vec<_>>>()?;

    let sequential = parse_bool(fields.next()?)?;
    let length_known = parse_bool(fields.next()?)?;

    if fields.next().is_some()
        || metadata.next().is_some()
        || (order == ArrayOrder::Sequence && bounds.len() != 1)
        || (order != ArrayOrder::Sequence && (sequential || !length_known))
    {
        return None;
    }

    let shape = ArrayShape {
        bounds,
        order,
        sequential,
        length_known,
    };
    shape.full_slice().ok()?;
    Some(shape)
}

pub(super) fn parse_batch(
    output: &str,
    limit: usize,
) -> Option<(ArrayShape, Vec<VariableViewerRow>, bool)> {
    let shape = parse_shape(output)?;
    let mut rows = Vec::new();
    let mut ended = None;
    let mut metadata_seen = false;
    let decode = |field: &str| String::from_utf8(type_metadata::decode_hex(field)?).ok();

    for line in output.lines() {
        if line.starts_with("FGDB_ARRAY_META:") {
            if metadata_seen || ended.is_some() {
                return None;
            }

            metadata_seen = true;
        } else if let Some(fields) = line.strip_prefix("FGDB_ARRAY_ROW:") {
            if !metadata_seen || rows.len() >= limit.min(BATCH_LIMIT) || ended.is_some() {
                return None;
            }

            let mut fields = fields.split('\t');
            let ordinal = decode(fields.next()?)?;
            let value = decode(fields.next()?)?;
            let type_name = decode(fields.next()?)?;

            if fields.next().is_some()
                || ordinal.len() > 1024
                || value.len() > 4096
                || type_name.len() > 2048
            {
                return None;
            }

            rows.push(VariableViewerRow {
                link: String::new(),
                ordinal,
                value,
                type_name,
                name: String::new(),
                details: String::new(),
            });
        } else if let Some(value) = line.strip_prefix("FGDB_ARRAY_END:") {
            if !metadata_seen || ended.is_some() {
                return None;
            }

            ended = Some(parse_bool(value)?);
        } else if line.starts_with("FGDB_ARRAY_") {
            return None;
        }
    }

    Some((shape, rows, ended?))
}

fn parse_bool(text: &str) -> Option<bool> {
    match text {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_preserve_coordinates_and_reject_partial_or_over_budget_data() {
        let header = "FGDB_ARRAY_META:column\t-1:1,4:5\t0\t1\n";
        let row = "FGDB_ARRAY_ROW:282d312c3429\t3432\t696e7465676572\n";
        let end = "FGDB_ARRAY_END:0\n";
        let response = format!("{header}{row}{end}");
        let (shape, rows, ended) = parse_batch(&response, 1).unwrap();
        assert_eq!(shape.bounds, [(-1, 1), (4, 5)]);
        assert_eq!(rows[0].ordinal, "(-1,4)");
        assert_eq!(rows[0].value, "42");
        assert!(!ended);
        assert!(parse_batch(&format!("{header}{row}"), 1).is_none());
        assert!(parse_batch(&response, 0).is_none());
        assert!(parse_batch(&format!("{response}{row}"), 2).is_none());
        assert!(parse_batch(&format!("{response}{end}"), 1).is_none());
        assert!(parse_batch(&response.replace("3432", "zz"), 1).is_none());
        assert!(parse_batch(&format!("{row}{header}{end}"), 1).is_none());
        assert!(parse_batch(&format!("{response}FGDB_ARRAY_UNSUPPORTED"), 1).is_none());
        assert!(parse_shape(&format!("{header}{header}")).is_none());
        assert!(parse_shape("FGDB_ARRAY_META:row\t0:100\t1\t1").is_none());
        assert_eq!(
            parse_error("FGDB_ARRAY_ERROR:626f756e6473"),
            Some(String::from("bounds"))
        );
        assert!(parse_error("FGDB_ARRAY_ERROR:zz").is_none());
    }
}
