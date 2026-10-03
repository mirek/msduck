//! SQL Server collation names and the comparison rule msduck applies for
//! each: shared by expression COLLATE, column declarations and key columns.
//!
//! The database default, SQL_Latin1_General_CP1_CI_AS, is case-insensitive
//! and accent-sensitive. Comparisons under it ignore case and trailing
//! spaces; keys enforce the same equality (see [`super::value`]). CHAR and
//! VARCHAR columns carry DuckDB's `nocase` collation for it ([`backend`]).

/// The collation of every msduck database.
pub const DEFAULT: &str = "SQL_Latin1_General_CP1_CI_AS";

/// Windows collation designators (`sys.fn_helpcollations()`), with the ICU
/// locale used for their linguistic order when msduck supports them.
const DESIGNATORS: &[(&str, Option<&str>)] = &[
    ("Albanian", None),
    ("Albanian_100", None),
    ("Arabic", Some("ar")),
    ("Arabic_100", Some("ar")),
    ("Assamese_100", None),
    ("Azeri_Cyrillic_100", None),
    ("Azeri_Latin_100", None),
    ("Bashkir_100", None),
    ("Bengali_100", None),
    ("Bosnian_Cyrillic_100", None),
    ("Bosnian_Latin_100", None),
    ("Breton_100", None),
    ("Chinese_Hong_Kong_Stroke_90", None),
    ("Chinese_PRC", Some("zh")),
    ("Chinese_PRC_90", Some("zh")),
    ("Chinese_PRC_Stroke", None),
    ("Chinese_PRC_Stroke_90", None),
    ("Chinese_Simplified_Pinyin_100", Some("zh")),
    ("Chinese_Simplified_Stroke_Order_100", None),
    ("Chinese_Taiwan_Bopomofo", None),
    ("Chinese_Taiwan_Bopomofo_90", None),
    ("Chinese_Taiwan_Stroke", None),
    ("Chinese_Taiwan_Stroke_90", None),
    ("Chinese_Traditional_Bopomofo_100", None),
    ("Chinese_Traditional_Pinyin_100", None),
    ("Chinese_Traditional_Stroke_Count_100", None),
    ("Chinese_Traditional_Stroke_Order_100", None),
    ("Corsican_100", None),
    ("Croatian", Some("hr")),
    ("Croatian_100", Some("hr")),
    ("Cyrillic_General", Some("ru")),
    ("Cyrillic_General_100", Some("ru")),
    ("Czech", Some("cs")),
    ("Czech_100", Some("cs")),
    ("Danish_Greenlandic_100", Some("da")),
    ("Danish_Norwegian", Some("da")),
    ("Dari_100", None),
    ("Divehi_100", None),
    ("Divehi_90", None),
    ("Estonian", Some("et")),
    ("Estonian_100", Some("et")),
    ("Finnish_Swedish", Some("sv")),
    ("Finnish_Swedish_100", Some("sv")),
    ("French", Some("fr")),
    ("French_100", Some("fr")),
    ("Frisian_100", None),
    ("Georgian_Modern_Sort", None),
    ("Georgian_Modern_Sort_100", None),
    ("German_PhoneBook", None),
    ("German_PhoneBook_100", None),
    ("Greek", Some("el")),
    ("Greek_100", Some("el")),
    ("Hebrew", Some("he")),
    ("Hebrew_100", Some("he")),
    ("Hungarian", Some("hu")),
    ("Hungarian_100", Some("hu")),
    ("Hungarian_Technical", None),
    ("Hungarian_Technical_100", None),
    ("Icelandic", None),
    ("Icelandic_100", None),
    ("Indic_General_100", None),
    ("Indic_General_90", None),
    ("Japanese", Some("ja")),
    ("Japanese_90", Some("ja")),
    ("Japanese_Bushu_Kakusu_100", None),
    ("Japanese_Bushu_Kakusu_140", None),
    ("Japanese_Unicode", None),
    ("Japanese_XJIS_100", Some("ja")),
    ("Japanese_XJIS_140", Some("ja")),
    ("Kazakh_100", None),
    ("Kazakh_90", None),
    ("Khmer_100", None),
    ("Korean_100", Some("ko")),
    ("Korean_90", Some("ko")),
    ("Korean_Wansung", Some("ko")),
    ("Lao_100", None),
    ("Latin1_General", Some("en_us")),
    ("Latin1_General_100", Some("en_us")),
    ("Latin1_General_140", Some("en_us")),
    ("Latvian", Some("lv")),
    ("Latvian_100", Some("lv")),
    ("Lithuanian", Some("lt")),
    ("Lithuanian_100", Some("lt")),
    ("Macedonian_FYROM_100", None),
    ("Macedonian_FYROM_90", None),
    ("Maltese_100", None),
    ("Maori_100", None),
    ("Mapudungan_100", None),
    ("Modern_Spanish", Some("es")),
    ("Modern_Spanish_100", Some("es")),
    ("Mohawk_100", None),
    ("Nepali_100", None),
    ("Norwegian_100", None),
    ("Pashto_100", None),
    ("Persian_100", None),
    ("Polish", Some("pl")),
    ("Polish_100", Some("pl")),
    ("Romanian", Some("ro")),
    ("Romanian_100", Some("ro")),
    ("Romansh_100", None),
    ("Sami_Norway_100", None),
    ("Sami_Sweden_Finland_100", None),
    ("Serbian_Cyrillic_100", None),
    ("Serbian_Latin_100", None),
    ("Slovak", Some("sk")),
    ("Slovak_100", Some("sk")),
    ("Slovenian", Some("sl")),
    ("Slovenian_100", Some("sl")),
    ("Syriac_100", None),
    ("Syriac_90", None),
    ("Tamazight_100", None),
    ("Tatar_100", None),
    ("Tatar_90", None),
    ("Thai", Some("th")),
    ("Thai_100", Some("th")),
    ("Tibetan_100", None),
    ("Traditional_Spanish", None),
    ("Traditional_Spanish_100", None),
    ("Turkish", Some("tr")),
    ("Turkish_100", Some("tr")),
    ("Turkmen_100", None),
    ("Uighur_100", None),
    ("Ukrainian", Some("uk")),
    ("Ukrainian_100", Some("uk")),
    ("Upper_Sorbian_100", None),
    ("Urdu_100", None),
    ("Uzbek_Latin_100", None),
    ("Uzbek_Latin_90", None),
    ("Vietnamese", Some("vi")),
    ("Vietnamese_100", Some("vi")),
    ("Welsh_100", None),
    ("Yakut_100", None),
];

/// SQL Server legacy (SQL_) collations, with the ICU locale msduck uses.
const SQL_COLLATIONS: &[(&str, Option<&str>)] = &[
    ("SQL_1xCompat_CP850_CI_AS", None),
    ("SQL_AltDiction_CP850_CI_AI", None),
    ("SQL_AltDiction_CP850_CI_AS", None),
    ("SQL_AltDiction_CP850_CS_AS", None),
    ("SQL_AltDiction_Pref_CP850_CI_AS", None),
    ("SQL_AltDiction2_CP1253_CS_AS", None),
    ("SQL_Croatian_CP1250_CI_AS", Some("hr")),
    ("SQL_Croatian_CP1250_CS_AS", Some("hr")),
    ("SQL_Czech_CP1250_CI_AS", Some("cs")),
    ("SQL_Czech_CP1250_CS_AS", Some("cs")),
    ("SQL_Danish_Pref_CP1_CI_AS", None),
    ("SQL_EBCDIC037_CP1_CS_AS", None),
    ("SQL_EBCDIC1141_CP1_CS_AS", None),
    ("SQL_EBCDIC273_CP1_CS_AS", None),
    ("SQL_EBCDIC277_2_CP1_CS_AS", None),
    ("SQL_EBCDIC277_CP1_CS_AS", None),
    ("SQL_EBCDIC278_CP1_CS_AS", None),
    ("SQL_EBCDIC280_CP1_CS_AS", None),
    ("SQL_EBCDIC284_CP1_CS_AS", None),
    ("SQL_EBCDIC285_CP1_CS_AS", None),
    ("SQL_EBCDIC297_CP1_CS_AS", None),
    ("SQL_Estonian_CP1257_CI_AS", Some("et")),
    ("SQL_Estonian_CP1257_CS_AS", Some("et")),
    ("SQL_Hungarian_CP1250_CI_AS", Some("hu")),
    ("SQL_Hungarian_CP1250_CS_AS", Some("hu")),
    ("SQL_Icelandic_Pref_CP1_CI_AS", None),
    ("SQL_Latin1_General_CP1_CI_AI", Some("en_us")),
    ("SQL_Latin1_General_CP1_CI_AS", Some("en_us")),
    ("SQL_Latin1_General_CP1_CS_AS", Some("en_us")),
    ("SQL_Latin1_General_CP1250_CI_AS", Some("en_us")),
    ("SQL_Latin1_General_CP1250_CS_AS", Some("en_us")),
    ("SQL_Latin1_General_CP1251_CI_AS", Some("en_us")),
    ("SQL_Latin1_General_CP1251_CS_AS", Some("en_us")),
    ("SQL_Latin1_General_CP1253_CI_AI", Some("en_us")),
    ("SQL_Latin1_General_CP1253_CI_AS", Some("en_us")),
    ("SQL_Latin1_General_CP1253_CS_AS", Some("en_us")),
    ("SQL_Latin1_General_CP1254_CI_AS", Some("en_us")),
    ("SQL_Latin1_General_CP1254_CS_AS", Some("en_us")),
    ("SQL_Latin1_General_CP1255_CI_AS", Some("en_us")),
    ("SQL_Latin1_General_CP1255_CS_AS", Some("en_us")),
    ("SQL_Latin1_General_CP1256_CI_AS", Some("en_us")),
    ("SQL_Latin1_General_CP1256_CS_AS", Some("en_us")),
    ("SQL_Latin1_General_CP1257_CI_AS", Some("en_us")),
    ("SQL_Latin1_General_CP1257_CS_AS", Some("en_us")),
    ("SQL_Latin1_General_CP437_BIN", Some("en_us")),
    ("SQL_Latin1_General_CP437_BIN2", Some("en_us")),
    ("SQL_Latin1_General_CP437_CI_AI", Some("en_us")),
    ("SQL_Latin1_General_CP437_CI_AS", Some("en_us")),
    ("SQL_Latin1_General_CP437_CS_AS", Some("en_us")),
    ("SQL_Latin1_General_CP850_BIN", Some("en_us")),
    ("SQL_Latin1_General_CP850_BIN2", Some("en_us")),
    ("SQL_Latin1_General_CP850_CI_AI", Some("en_us")),
    ("SQL_Latin1_General_CP850_CI_AS", Some("en_us")),
    ("SQL_Latin1_General_CP850_CS_AS", Some("en_us")),
    ("SQL_Latin1_General_Pref_CP1_CI_AS", Some("en_us")),
    ("SQL_Latin1_General_Pref_CP437_CI_AS", Some("en_us")),
    ("SQL_Latin1_General_Pref_CP850_CI_AS", Some("en_us")),
    ("SQL_Latvian_CP1257_CI_AS", Some("lv")),
    ("SQL_Latvian_CP1257_CS_AS", Some("lv")),
    ("SQL_Lithuanian_CP1257_CI_AS", Some("lt")),
    ("SQL_Lithuanian_CP1257_CS_AS", Some("lt")),
    ("SQL_MixDiction_CP1253_CS_AS", None),
    ("SQL_Polish_CP1250_CI_AS", Some("pl")),
    ("SQL_Polish_CP1250_CS_AS", Some("pl")),
    ("SQL_Romanian_CP1250_CI_AS", Some("ro")),
    ("SQL_Romanian_CP1250_CS_AS", Some("ro")),
    ("SQL_Scandinavian_CP850_CI_AS", None),
    ("SQL_Scandinavian_CP850_CS_AS", None),
    ("SQL_Scandinavian_Pref_CP850_CI_AS", None),
    ("SQL_Slovak_CP1250_CI_AS", Some("sk")),
    ("SQL_Slovak_CP1250_CS_AS", Some("sk")),
    ("SQL_Slovenian_CP1250_CI_AS", Some("sl")),
    ("SQL_Slovenian_CP1250_CS_AS", Some("sl")),
    ("SQL_SwedishPhone_Pref_CP1_CI_AS", None),
    ("SQL_SwedishStd_Pref_CP1_CI_AS", None),
    ("SQL_Ukrainian_CP1251_CI_AS", Some("uk")),
    ("SQL_Ukrainian_CP1251_CS_AS", Some("uk")),
];

/// How a valid collation compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// BIN and BIN2: code point order.
    Binary,
    Linguistic {
        locale: Option<&'static str>,
        case_sensitive: bool,
        accent_sensitive: bool,
    },
}

/// Parse the flags after a Windows designator.
fn flags(rest: &str) -> Option<(bool, bool, bool)> {
    let parts: Vec<&str> = rest.split('_').collect();
    match parts.as_slice() {
        [binary] | [binary, "UTF8"] if binary.eq_ignore_ascii_case("BIN2") => {
            return Some((true, true, true));
        }
        [binary] if binary.eq_ignore_ascii_case("BIN") => return Some((true, true, true)),
        _ => {}
    }
    let [case, accent, tail @ ..] = parts.as_slice() else {
        return None;
    };
    let case_sensitive = match case.to_ascii_uppercase().as_str() {
        "CS" => true,
        "CI" => false,
        _ => return None,
    };
    let accent_sensitive = match accent.to_ascii_uppercase().as_str() {
        "AS" => true,
        "AI" => false,
        _ => return None,
    };
    // Optional flags, each at most once and in this order.
    let mut order = ["KS", "WS", "SC", "UTF8"].iter();
    for flag in tail {
        if !order.any(|known| flag.eq_ignore_ascii_case(known)) {
            return None;
        }
    }
    Some((false, case_sensitive, accent_sensitive))
}

/// A name that is not a SQL Server collation (SQL Server's 448).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Invalid;

/// Resolve a collation name. `Err` is a name that does not exist (SQL
/// Server's 448); `Ok(None)` is a valid SQL Server collation msduck does not
/// implement.
pub fn resolve(name: &str) -> Result<Option<Rule>, Invalid> {
    if let Some((_, locale)) = SQL_COLLATIONS
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(name))
    {
        let upper = name.to_ascii_uppercase();
        if upper.ends_with("_BIN") || upper.ends_with("_BIN2") {
            return Ok(Some(Rule::Binary));
        }
        return Ok(locale.map(|locale| Rule::Linguistic {
            locale: Some(locale),
            case_sensitive: upper.contains("_CS_"),
            accent_sensitive: upper.ends_with("_AS"),
        }));
    }
    // The longest designator that prefixes the name wins (Latin1_General_100
    // over Latin1_General).
    let mut matched: Option<(&str, Option<&str>)> = None;
    for (designator, locale) in DESIGNATORS {
        if name.len() > designator.len() + 1
            && name[..designator.len()].eq_ignore_ascii_case(designator)
            && name.as_bytes()[designator.len()] == b'_'
            && flags(&name[designator.len() + 1..]).is_some()
            && matched.is_none_or(|(current, _)| current.len() < designator.len())
        {
            matched = Some((designator, *locale));
        }
    }
    let (designator, locale) = matched.ok_or(Invalid)?;
    let (binary, case_sensitive, accent_sensitive) =
        flags(&name[designator.len() + 1..]).ok_or(Invalid)?;
    if binary {
        return Ok(Some(Rule::Binary));
    }
    Ok(locale.map(|locale| Rule::Linguistic {
        locale: Some(locale),
        case_sensitive,
        accent_sensitive,
    }))
}

/// SQL Server's error for a column declaration's COLLATE clause: 447 for a
/// column that is not character data, 448 for a name that does not exist.
/// Names that exist but msduck does not implement are left to the caller.
pub fn column_error(column: &sqlparser::ast::ColumnDef) -> Option<(i32, u8, u8, String)> {
    use sqlparser::ast::{ColumnOption, ObjectNamePart};
    let name = column
        .options
        .iter()
        .find_map(|option| match &option.option {
            ColumnOption::Collation(name) => Some(name),
            _ => None,
        })?;
    let character = matches!(
        crate::sql_type::declaration(&column.data_type),
        Ok(msduck_core::types::Type::Character(_)
            | msduck_core::types::Type::Text
            | msduck_core::types::Type::Ntext)
    );
    if !character {
        let kind = column.data_type.to_string().to_ascii_lowercase();
        let kind = kind.split('(').next().unwrap_or_default().trim().to_owned();
        return Some((
            447,
            1,
            16,
            format!("Expression type {kind} is invalid for COLLATE clause."),
        ));
    }
    let text = match name.0.as_slice() {
        [ObjectNamePart::Identifier(ident)] => ident.value.clone(),
        _ => name.to_string(),
    };
    let valid =
        matches!(name.0.as_slice(), [ObjectNamePart::Identifier(_)]) && resolve(&text).is_ok();
    (!valid).then(|| (448, 2, 16, format!("Invalid collation '{text}'.")))
}

/// How a column or comparison under `name` distinguishes values, as msduck
/// implements it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sensitivity {
    /// Case-insensitive and accent-sensitive, like the database default:
    /// msduck's default comparison and keys apply.
    Default,
    /// Any other implemented rule (case-sensitive, accent-insensitive or
    /// binary): comparisons need the name as an explicit COLLATE.
    Other,
}

/// The sensitivity of a supported collation name; `None` for names msduck
/// does not implement or that do not exist.
pub fn sensitivity(name: &str) -> Option<Sensitivity> {
    match resolve(name) {
        Ok(Some(Rule::Linguistic {
            case_sensitive: false,
            accent_sensitive: true,
            ..
        })) => Some(Sensitivity::Default),
        Ok(Some(_)) => Some(Sensitivity::Other),
        _ => None,
    }
}

/// Whether values under `name` (the default when `None`) compare without
/// regard to case. Unknown names count as case-sensitive.
pub fn case_insensitive(name: Option<&str>) -> bool {
    match name {
        None => true,
        Some(name) => matches!(
            resolve(name),
            Ok(Some(Rule::Linguistic {
                case_sensitive: false,
                ..
            }))
        ),
    }
}

/// DuckDB's collation for character values stored as VARCHAR under `name`,
/// when it differs from the database default (`nocase`); `None` otherwise.
pub fn backend(name: &str) -> Option<String> {
    match resolve(name) {
        Ok(Some(Rule::Binary)) => Some("C".into()),
        Ok(Some(Rule::Linguistic {
            locale,
            case_sensitive,
            accent_sensitive,
        })) => {
            if !case_sensitive && accent_sensitive {
                return None;
            }
            let mut parts = vec![];
            if !case_sensitive {
                parts.push("nocase");
            }
            if !accent_sensitive {
                parts.push("noaccent");
            }
            if case_sensitive {
                parts.extend(locale);
            }
            Some(parts.join("."))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_sql_server_grammar() {
        let linguistic = |cs, r#as| {
            Ok(Some(Rule::Linguistic {
                locale: Some("en_us"),
                case_sensitive: cs,
                accent_sensitive: r#as,
            }))
        };
        assert_eq!(resolve("Latin1_General_CI_AS"), linguistic(false, true));
        assert_eq!(resolve("latin1_general_cs_as"), linguistic(true, true));
        assert_eq!(
            resolve("Latin1_General_100_CI_AI"),
            linguistic(false, false)
        );
        assert_eq!(
            resolve("Latin1_General_100_CS_AI_SC_UTF8"),
            linguistic(true, false)
        );
        assert_eq!(
            resolve("Latin1_General_CI_AS_KS_WS"),
            linguistic(false, true)
        );
        assert_eq!(
            resolve("SQL_Latin1_General_CP1_CI_AS"),
            linguistic(false, true)
        );
        assert_eq!(
            resolve("SQL_Latin1_General_CP1_CS_AS"),
            linguistic(true, true)
        );
        assert_eq!(resolve("Latin1_General_BIN2"), Ok(Some(Rule::Binary)));
        assert_eq!(
            resolve("Latin1_General_100_BIN2_UTF8"),
            Ok(Some(Rule::Binary))
        );
        assert_eq!(
            resolve("SQL_Latin1_General_CP437_BIN"),
            Ok(Some(Rule::Binary))
        );
        assert_eq!(resolve("German_PhoneBook_CI_AS"), Ok(None));
        for invalid in [
            "Foo_Bar",
            "Latin1_General",
            "Latin1_General_CI",
            "Latin1_General_XI_AS",
            "Latin1_General_CI_AS_WS_KS",
            "Latin1_General_CI_AS_Bogus",
            "SQL_Latin1_General_CP2_CI_AS",
            "",
        ] {
            assert_eq!(resolve(invalid), Err(Invalid), "{invalid}");
        }
    }

    #[test]
    fn column_declarations_report_sql_server_errors() {
        let column = |sql: &str| {
            let sqlparser::ast::Statement::CreateTable(table) =
                crate::batch::parse(&format!("CREATE TABLE t ({sql})"))
                    .unwrap()
                    .remove(0)
            else {
                panic!()
            };
            table.columns[0].clone()
        };
        assert_eq!(
            column_error(&column("a nvarchar(10) COLLATE Not_A_Collation")),
            Some((448, 2, 16, "Invalid collation 'Not_A_Collation'.".into()))
        );
        assert_eq!(
            column_error(&column("a int COLLATE Latin1_General_CI_AS")),
            Some((
                447,
                1,
                16,
                "Expression type int is invalid for COLLATE clause.".into()
            ))
        );
        assert_eq!(
            column_error(&column("a varchar(10) COLLATE Latin1_General_CS_AS")),
            None
        );
        assert_eq!(column_error(&column("a varchar(10)")), None);
    }

    #[test]
    fn sensitivity_and_backend_collations() {
        assert_eq!(sensitivity(DEFAULT), Some(Sensitivity::Default));
        assert_eq!(
            sensitivity("Latin1_General_100_CI_AS_SC_UTF8"),
            Some(Sensitivity::Default)
        );
        for other in [
            "Latin1_General_CS_AS",
            "SQL_Latin1_General_CP1_CS_AS",
            "Latin1_General_BIN2",
            "Latin1_General_CI_AI",
        ] {
            assert_eq!(sensitivity(other), Some(Sensitivity::Other), "{other}");
        }
        assert_eq!(sensitivity("Not_A_Collation"), None);
        assert!(case_insensitive(None));
        assert!(case_insensitive(Some("Latin1_General_CI_AI")));
        assert!(!case_insensitive(Some("Latin1_General_100_BIN2")));
        assert_eq!(backend(DEFAULT), None);
        assert_eq!(backend("Latin1_General_BIN2").as_deref(), Some("C"));
        assert_eq!(backend("Latin1_General_CS_AS").as_deref(), Some("en_us"));
        assert_eq!(
            backend("Latin1_General_CI_AI").as_deref(),
            Some("nocase.noaccent")
        );
    }
}
