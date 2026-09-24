use msduck_core::result::{Origin, Properties};
use msduck_tds::{Column, Type, collation::Collation, metadata};

#[test]
fn resource_catalog_collation_matches_captured_descriptor_and_encoded_metadata() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../../reference/index-catalog.json")).unwrap();
    let declarations = &fixture["declarations"]["indexes"];
    let declaration = declarations["result"]["sets"][0]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r[0] == "type_desc")
        .unwrap();
    let name = declaration[7].as_str().unwrap();
    let collation = Collation::for_name(name).unwrap();
    assert_eq!(Collation::for_name(&name.to_uppercase()), Some(collation));
    let descriptor = declarations["wire"]["sets"][0]["columns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "type_desc")
        .unwrap();
    let captured = &descriptor["collation"];
    let bytes = collation.bytes();
    let info = u32::from_le_bytes(bytes[..4].try_into().unwrap());
    assert_eq!(serde_json::json!(info & 0xfffff), captured["lcid"]);
    assert_eq!(serde_json::json!((info >> 20) & 0xff), captured["flags"]);
    assert_eq!(serde_json::json!(info >> 28), captured["version"]);
    assert_eq!(serde_json::json!(bytes[4]), captured["sortId"]);
    assert_eq!(bytes, [0x09, 0x04, 0x10, 0x00, 0x00]);
    assert_ne!(collation, Collation::default());

    let mut output = vec![];
    metadata(
        &mut output,
        &[Column {
            name: "type_desc".into(),
            kind: Type::Nvarchar(declaration[4].as_u64().unwrap() as u16 / 2),
            properties: Properties {
                nullable: declaration[8].as_bool(),
                origin: Origin::Stored,
            },
            collation: Some(collation),
        }],
    )
    .unwrap();
    assert_eq!(&output[..7], &[0x81, 1, 0, 0, 0, 0, 0]);
    assert_eq!(
        serde_json::json!(u16::from_le_bytes(output[7..9].try_into().unwrap())),
        descriptor["flags"]
    );
    assert_eq!(output[9], 0xe7);
    assert_eq!(
        serde_json::json!(u16::from_le_bytes(output[10..12].try_into().unwrap())),
        descriptor["length"]
    );
    assert_eq!(&output[12..17], &bytes);
    assert_eq!(output[17], 9);
    assert_eq!(
        &output[18..],
        "type_desc"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
    );
    assert!(Collation::for_name("Latin1_General_CI_AS_KS_WS_SC").is_none());
}
