//! Deterministic expression metadata rules shared by binding and lowering.
use sqlparser::ast::*;

pub fn columns(definitions: &[OpenJsonTableColumn]) -> Vec<(String, Option<DataType>)> {
    if !definitions.is_empty() {
        return definitions
            .iter()
            .map(|c| {
                let kind = declared_type(&c.r#type);
                (c.name.value.clone(), Some(kind))
            })
            .collect();
    }
    vec![
        (
            "key".into(),
            Some(DataType::Nvarchar(Some(CharacterLength::IntegerLength {
                length: 4000,
                unit: None,
            }))),
        ),
        (
            "value".into(),
            Some(DataType::Nvarchar(Some(CharacterLength::Max))),
        ),
        ("type".into(), Some(DataType::Int(None))),
    ]
}

pub fn declared_type(kind: &DataType) -> DataType {
    let one = Some(CharacterLength::IntegerLength {
        length: 1,
        unit: None,
    });
    match kind {
        DataType::Varchar(length)
        | DataType::CharacterVarying(length)
        | DataType::CharVarying(length) => DataType::Varchar((*length).or(one)),
        DataType::Char(length) | DataType::Character(length) => DataType::Char((*length).or(one)),
        DataType::Nvarchar(length) => DataType::Nvarchar((*length).or(one)),
        DataType::Custom(name, args)
            if name.to_string().eq_ignore_ascii_case("nchar") && args.is_empty() =>
        {
            DataType::Custom(name.clone(), vec!["1".into()])
        }
        DataType::Binary(None) => DataType::Binary(Some(1)),
        DataType::Varbinary(None) => {
            DataType::Varbinary(Some(BinaryLength::IntegerLength { length: 1 }))
        }
        kind => kind.clone(),
    }
}
