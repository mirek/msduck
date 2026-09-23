//! Pure recognition and validation of temporal SQL declarations.
use sqlparser::ast::DataType;

pub fn datetime2(kind: &DataType) -> Result<Option<u8>, String> {
    if let DataType::Custom(name, args) = kind
        && name.to_string().eq_ignore_ascii_case("datetime2")
    {
        return match args.as_slice() {
            [] => Ok(Some(7)),
            [s] => s
                .parse::<u8>()
                .ok()
                .filter(|s| *s <= 7)
                .map(Some)
                .ok_or_else(|| "DATETIME2 scale must be between 0 and 7".into()),
            _ => Err("DATETIME2 requires at most one scale".into()),
        };
    }
    Ok(None)
}

pub fn datetimeoffset(kind: &DataType) -> Result<Option<u8>, String> {
    if let DataType::Custom(name, args) = kind
        && name.to_string().eq_ignore_ascii_case("datetimeoffset")
    {
        return match args.as_slice() {
            [] => Ok(Some(7)),
            [s] => s
                .parse::<u8>()
                .ok()
                .filter(|s| *s <= 7)
                .map(Some)
                .ok_or_else(|| "DATETIMEOFFSET scale must be between 0 and 7".into()),
            _ => Err("DATETIMEOFFSET requires at most one scale".into()),
        };
    }
    Ok(None)
}
