//! Pure plans and evaluation over explicitly SQL-converted text operands.
//! Formatting, catalog acquisition and evaluation of source expressions belong
//! to adapters. Neither planning nor evaluation reads process/session state.
use msduck_core::{
    character::{CharacterType, Family, Length},
    collation::{Conflict, Label},
    diagnostic::SqlError,
    types::Type,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Function {
    ConcatWs,
    Translate,
}
impl Function {
    fn name(self) -> &'static str {
        match self {
            Self::ConcatWs => "concat_ws",
            Self::Translate => "translate",
        }
    }
}

/// The adapter provides any conversion width from declarations, never values.
/// Unknown per-type formatting widths remain None rather than being guessed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Argument {
    pub kind: Option<Type>,
    pub converted_width: Option<Length>,
    pub collation: Option<Label>,
}
impl Argument {
    pub fn null_literal() -> Self {
        Self {
            kind: None,
            converted_width: Some(Length::Bounded(0)),
            collation: None,
        }
    }
    pub fn character(kind: CharacterType, collation: Label) -> Self {
        Self {
            kind: Some(Type::Character(kind)),
            converted_width: Some(kind.length()),
            collation: Some(collation),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Cp1252,
    Utf8,
}

/// Explicit catalog properties; unknown collations must not silently disappear.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collation {
    pub name: String,
    pub supplementary: bool,
    pub case_sensitive: bool,
    pub encoding: Encoding,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Sql(SqlError),
    UnknownCollation,
    UnknownConversionWidth,
    UnknownConversion,
    UnknownComparison,
    ComparisonLimit,
    LookupLimit,
    OutputLimit,
    InputLimit,
    UnknownEncoding,
    UnknownDiagnosticContext,
    InvalidDeclaration,
    InvalidPayload,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    function: Function,
    pub declaration: CharacterType,
    pub collation: Label,
    pub flags: u16,
    pub supplementary: bool,
    pub encoding: Encoding,
    source_encodings: Vec<Encoding>,
    arguments: Vec<Argument>,
}
fn unicode(kind: CharacterType) -> bool {
    matches!(kind.family(), Family::Nchar | Family::Nvarchar)
}
fn sql(number: i32, state: u8, message: String) -> Error {
    Error::Sql(SqlError::new(number, state, message))
}

/// Statement context is supplied by the binder, never inferred from arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticContext {
    SelectColumn(std::num::NonZeroUsize),
}

pub fn plan(
    function: Function,
    arguments: &[Argument],
    default_collation: &str,
    catalog: &[Collation],
) -> Result<Plan, Error> {
    plan_with_context(function, arguments, default_collation, catalog, None)
}

pub fn plan_with_context(
    function: Function,
    arguments: &[Argument],
    default_collation: &str,
    catalog: &[Collation],
    context: Option<DiagnosticContext>,
) -> Result<Plan, Error> {
    let count = arguments.len();
    if function == Function::ConcatWs && !(3..=254).contains(&count) {
        return Err(Error::Sql(SqlError::syntax(
            189,
            1,
            "The concat_ws function requires 3 to 254 arguments.",
        )));
    }
    if function == Function::Translate && count != 3 {
        return Err(Error::Sql(SqlError::syntax(
            174,
            1,
            "The translate function requires 3 argument(s).",
        )));
    }
    let lookup = |name: &str| {
        let mut matches = catalog
            .iter()
            .filter(|item| item.name.eq_ignore_ascii_case(name));
        let first = matches.next().ok_or(Error::UnknownCollation)?;
        if matches.next().is_some() {
            return Err(Error::UnknownCollation);
        }
        Ok(first)
    };
    let mut combined: Option<Label> = None;
    // Conversion family is determined by all original declarations, including
    // a Unicode argument following a source rejected during conversion.
    let is_unicode = arguments
        .iter()
        .any(|argument| matches!(argument.kind, Some(Type::Character(kind)) if unicode(kind)));
    let mut source_encodings = Vec::new();
    for (index, argument) in arguments.iter().enumerate() {
        match argument.kind {
            Some(Type::Character(kind)) if argument.converted_width != Some(kind.length()) => {
                return Err(Error::InvalidDeclaration);
            }
            Some(Type::Character(_)) => {}
            Some(Type::Variant | Type::Xml) if function == Function::ConcatWs => {
                let kind = if argument.kind == Some(Type::Variant) {
                    "sql_variant"
                } else {
                    "xml"
                };
                let target = if is_unicode { "nvarchar" } else { "varchar" };
                return Err(sql(
                    257,
                    3,
                    format!(
                        "Implicit conversion from data type {kind} to {target} is not allowed. Use the CONVERT function to run this query."
                    ),
                ));
            }
            Some(Type::Text | Type::Ntext | Type::Image) => return Err(Error::UnknownConversion),
            Some(Type::Xml | Type::Variant) => return Err(Error::UnknownConversion),
            Some(Type::Int) if function == Function::Translate && index == 2 => {
                return Err(sql(
                    8116,
                    1,
                    "Argument data type int is invalid for argument 3 of translate function."
                        .into(),
                ));
            }
            Some(_) if function == Function::Translate && index > 0 => {
                return Err(Error::UnknownConversion);
            }
            _ => {}
        }
        if argument.kind.is_none() && argument != &Argument::null_literal() {
            return Err(Error::InvalidDeclaration);
        }
        if matches!(argument.kind, Some(Type::Character(_))) && argument.collation.is_none() {
            return Err(Error::UnknownCollation);
        }
        if let Some(label) = &argument.collation {
            match label {
                Label::NoCollation { left, right } => {
                    lookup(left)?;
                    lookup(right)?;
                }
                _ => {
                    lookup(label.name().ok_or(Error::UnknownCollation)?)?;
                }
            }
            combined = Some(match combined {
                None => label.clone(),
                Some(previous) => match previous.combine(label) {
                    Ok(label) => label,
                    Err(Conflict::Explicit { left, right }) => {
                        return Err(sql(
                            468,
                            9,
                            format!(
                                "Cannot resolve the collation conflict between \"{right}\" and \"{left}\" in the {} operation.",
                                function.name()
                            ),
                        ));
                    }
                    Err(_) => return Err(Error::UnknownCollation),
                },
            });
        }
        source_encodings.push(match &argument.collation {
            Some(Label::NoCollation { left, right }) => {
                let left = lookup(left)?.encoding;
                let right = lookup(right)?.encoding;
                if left == right {
                    Ok(left)
                } else {
                    Err(Error::UnknownEncoding)
                }
            }
            Some(label) => Ok(lookup(label.name().ok_or(Error::UnknownCollation)?)?.encoding),
            None => Ok(lookup(default_collation)?.encoding),
        });
    }
    let label = combined.unwrap_or_else(|| Label::CoercibleDefault(default_collation.into()));
    if let Label::NoCollation { left, right } = &label {
        if function == Function::ConcatWs {
            let Some(DiagnosticContext::SelectColumn(column)) = context else {
                return Err(Error::UnknownDiagnosticContext);
            };
            return Err(sql(
                451,
                1,
                format!(
                    "Cannot resolve collation conflict between \"{right}\" and \"{left}\" in concat_ws operator occurring in SELECT statement column {column}."
                ),
            ));
        }
        return Err(Error::UnknownCollation);
    }
    let properties = lookup(label.name().ok_or(Error::UnknownCollation)?)?;
    let family = if is_unicode {
        Family::Nvarchar
    } else {
        Family::Varchar
    };
    let max_width = if is_unicode { 4000 } else { 8000 };
    let length = match function {
        Function::ConcatWs => {
            let mut width = 0u64;
            let mut is_max = false;
            for (index, argument) in arguments.iter().enumerate() {
                let length = match argument.kind {
                    Some(Type::Character(kind)) => {
                        if argument.converted_width != Some(kind.length()) {
                            return Err(Error::InvalidDeclaration);
                        }
                        kind.length()
                    }
                    Some(Type::Int) => {
                        if argument
                            .converted_width
                            .is_some_and(|width| width != Length::Bounded(12))
                        {
                            return Err(Error::InvalidDeclaration);
                        }
                        Length::Bounded(12)
                    }
                    _ => argument
                        .converted_width
                        .ok_or(Error::UnknownConversionWidth)?,
                };
                match length {
                    Length::Max => is_max = true,
                    Length::Bounded(n) => {
                        width += u64::from(n) * if index == 0 { (count - 2) as u64 } else { 1 }
                    }
                }
            }
            if is_max {
                Length::Max
            } else {
                Length::Bounded(width.max(1).min(u64::from(max_width)) as u16)
            }
        }
        Function::Translate => {
            // A converted MAX width does not establish the result declaration
            // for an uncaptured noncharacter source. Do not fabricate a bounded
            // declaration and silently truncate its converted payload.
            if matches!(arguments[0].kind, Some(kind) if !matches!(kind, Type::Character(_)))
                && arguments[0].converted_width == Some(Length::Max)
            {
                return Err(Error::UnknownConversion);
            }
            if matches!(arguments[0].kind, Some(Type::Character(kind)) if kind.length() == Length::Max)
            {
                Length::Max
            } else {
                Length::Bounded(max_width)
            }
        }
    };
    Ok(Plan {
        function,
        declaration: CharacterType::new(family, length).map_err(|_| Error::InvalidDeclaration)?,
        flags: 32
            | u16::from(function == Function::Translate)
            | (u16::from(properties.case_sensitive) << 1),
        collation: label,
        supplementary: properties.supplementary,
        encoding: properties.encoding,
        // Preserve collation diagnostics above before reporting unresolved
        // source encodings. An explicit result label cannot restore provenance.
        source_encodings: source_encodings.into_iter().collect::<Result<_, _>>()?,
        arguments: arguments.to_vec(),
    })
}

/// Maximum pairwise comparisons in the compatibility matcher path. Callers
/// with established collation keys can use the indexed evaluator instead.
pub const MAX_MATCH_COMPARISONS: usize = 1_000_000;
/// Maximum distinct keys/units retained by either translation lookup.
/// Callers remain responsible for the allocation size of their supplied keys.
pub const MAX_LOOKUP_ENTRIES: usize = 65_536;

/// Allocation policy, not SQL Server MAX capacity or a SQL diagnostic.
/// The default evaluator produces at most 16 MiB of UTF-16 payload.
pub const MAX_OUTPUT_UNITS: usize = 8 * 1024 * 1024;
/// Bound each input before temporary text/code-page validation allocations.
pub const MAX_INPUT_UNITS: usize = 8 * 1024 * 1024;

/// Already formatted and converted into the result's text domain by adapters.
/// NULL is None; UTF-16 units preserve isolated surrogates. For ANSI results the
/// adapter must have applied the chosen code page before supplying these units.
/// The matcher operates on one SQL character (unit or surrogate pair), returning
/// None when its explicitly supplied collation weights do not establish equality.
/// Comparisons must be stable for the call; repeated input units reuse the result.
/// ComparisonLimit is an explicit work barrier, not a fabricated SQL diagnostic.
pub fn evaluate(
    plan: &Plan,
    values: &[Option<Vec<u16>>],
    matches: &impl Fn(&[u16], &[u16]) -> Option<bool>,
) -> Result<Option<Vec<u16>>, Error> {
    evaluate_with_translator(
        plan,
        values,
        &|plan, input, source, replacements, output| {
            let mut resolved = std::collections::BTreeMap::<&[u16], &[u16]>::new();
            let mut comparisons = 0;
            for unit in characters(input, plan.supplementary) {
                let replacement = if let Some(replacement) = resolved.get(unit) {
                    *replacement
                } else {
                    if resolved.len() == MAX_LOOKUP_ENTRIES {
                        return Err(Error::LookupLimit);
                    }
                    let mut replacement = unit;
                    for (candidate, mapped) in characters(source, plan.supplementary)
                        .zip(characters(replacements, plan.supplementary))
                    {
                        if comparisons == MAX_MATCH_COMPARISONS {
                            return Err(Error::ComparisonLimit);
                        }
                        comparisons += 1;
                        if matches(unit, candidate).ok_or(Error::UnknownComparison)? {
                            replacement = mapped;
                            break;
                        }
                    }
                    resolved.insert(unit, replacement);
                    replacement
                };
                append(output, replacement, plan, Boundary::Value)?;
            }
            Ok(())
        },
    )
}

/// Keys must equate exactly the SQL characters equated by the selected
/// collation, including ignorable units and supplementary behavior. The caller
/// supplies established keys; unavailable weights remain unknown. BTreeMap
/// keeps lookup deterministic, without process randomness, and costs
/// O((mapping characters + input characters) * log(distinct mapping keys)).
/// The first mapping for an equal key wins; replacements are never remapped.
pub fn evaluate_with_keys<K: Ord>(
    plan: &Plan,
    values: &[Option<Vec<u16>>],
    key: &impl Fn(&[u16]) -> Option<K>,
) -> Result<Option<Vec<u16>>, Error> {
    evaluate_with_translator(
        plan,
        values,
        &|plan, input, source, replacements, output| {
            let mut lookup = std::collections::BTreeMap::new();
            let mut entries = 0;
            for (candidate, mapped) in characters(source, plan.supplementary)
                .zip(characters(replacements, plan.supplementary))
            {
                match lookup.entry(key(candidate).ok_or(Error::UnknownComparison)?) {
                    std::collections::btree_map::Entry::Occupied(_) => {}
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        if entries == MAX_LOOKUP_ENTRIES {
                            return Err(Error::LookupLimit);
                        }
                        entry.insert(mapped);
                        entries += 1;
                    }
                }
            }
            for unit in characters(input, plan.supplementary) {
                let resolved = lookup
                    .get(&key(unit).ok_or(Error::UnknownComparison)?)
                    .copied()
                    .unwrap_or(unit);
                append(output, resolved, plan, Boundary::Value)?;
            }
            Ok(())
        },
    )
}

fn evaluate_with_translator(
    plan: &Plan,
    values: &[Option<Vec<u16>>],
    translate: &impl Fn(&Plan, &[u16], &[u16], &[u16], &mut Output) -> Result<(), Error>,
) -> Result<Option<Vec<u16>>, Error> {
    if values.len() != plan.arguments.len() {
        return Err(Error::InvalidPayload);
    }
    if plan
        .arguments
        .iter()
        .zip(values)
        .any(|(argument, value)| argument.kind.is_none() && value.is_some())
    {
        return Err(Error::InvalidPayload);
    }
    if !unicode(plan.declaration) && plan.encoding == Encoding::Utf8 {
        return Err(Error::UnknownEncoding);
    }
    for ((argument, value), encoding) in plan
        .arguments
        .iter()
        .zip(values)
        .zip(&plan.source_encodings)
    {
        let Some(value) = value else {
            continue;
        };
        if value.len() > MAX_INPUT_UNITS {
            return Err(Error::InputLimit);
        }
        let source_unicode = matches!(argument.kind, Some(Type::Character(kind)) if unicode(kind))
            || (unicode(plan.declaration) && matches!(argument.kind, Some(Type::Binary(_))));
        let size = if source_unicode {
            value.len()
        } else {
            let text = String::from_utf16(value).map_err(|_| Error::InvalidPayload)?;
            match encoding {
                Encoding::Utf8 => text.len(),
                Encoding::Cp1252 => msduck_core::encoding::encode_cp1252(&text)
                    .map_err(|_| Error::InvalidPayload)?
                    .len(),
            }
        };
        let width = match argument.kind {
            Some(Type::Character(kind)) => kind.length(),
            Some(Type::Int) => Length::Bounded(12),
            _ => argument
                .converted_width
                .ok_or(Error::UnknownConversionWidth)?,
        };
        let fixed = matches!(argument.kind, Some(Type::Character(kind)) if matches!(kind.family(), Family::Char | Family::Nchar));
        if let Length::Bounded(width) = width
            && (size > usize::from(width) || (fixed && size != usize::from(width)))
        {
            return Err(Error::InvalidPayload);
        }
        if !unicode(plan.declaration) {
            let text = String::from_utf16(value).map_err(|_| Error::InvalidPayload)?;
            msduck_core::encoding::encode_cp1252(&text).map_err(|_| Error::InvalidPayload)?;
        }
    }

    let mut output = Output::default();
    match plan.function {
        Function::ConcatWs => {
            let separator = values[0].as_deref().unwrap_or(&[]);
            if plan.declaration.length() == Length::Max {
                // Reject repeated-separator amplification before allocating output.
                let mut size = 0usize;
                let mut count = 0usize;
                for value in values[1..].iter().flatten() {
                    size = size.checked_add(value.len()).ok_or(Error::OutputLimit)?;
                    count += 1;
                }
                let gaps = count.saturating_sub(1);
                size = size
                    .checked_add(
                        separator
                            .len()
                            .checked_mul(gaps)
                            .ok_or(Error::OutputLimit)?,
                    )
                    .ok_or(Error::OutputLimit)?;
                if size > MAX_OUTPUT_UNITS {
                    return Err(Error::OutputLimit);
                }
            }
            let mut first = true;
            for value in values[1..].iter().flatten() {
                if !first {
                    append(&mut output, separator, plan, Boundary::Separator)?;
                }
                append(&mut output, value, plan, Boundary::Value)?;
                first = false;
            }
        }
        Function::Translate => {
            let [Some(input), Some(from), Some(to)] = values else {
                return Ok(None);
            };
            // Count without allocating per-character slice vectors, including
            // MAX mappings. Stream the same borrowed payloads during evaluation.
            if characters(from, plan.supplementary).count()
                != characters(to, plan.supplementary).count()
            {
                return Err(sql(9828, if unicode(plan.declaration) { 3 } else { 1 }, "The second and third arguments of the TRANSLATE built-in function must contain an equal number of characters.".into()));
            }
            if from.is_empty() || input.is_empty() {
                append(&mut output, input, plan, Boundary::Value)?;
            } else {
                translate(plan, input, from, to, &mut output)?;
            }
        }
    }
    Ok(Some(output.units))
}
#[derive(Default)]
struct Output {
    units: Vec<u16>,
    exhausted: bool,
}
#[derive(Clone, Copy)]
enum Boundary {
    Value,
    Separator,
}
fn append(
    output: &mut Output,
    value: &[u16],
    plan: &Plan,
    boundary: Boundary,
) -> Result<(), Error> {
    if output.exhausted {
        return Ok(());
    }
    let available = match plan.declaration.length() {
        Length::Max => value.len(),
        Length::Bounded(n) => usize::from(n).saturating_sub(output.units.len()),
    };
    let mut count = available.min(value.len());
    if count < value.len() {
        // SQL Server truncates each argument/replacement independently. A pair
        // cut inside this append is dropped, but a high surrogate from a prior
        // argument is retained even when this argument starts with its low half.
        // Separators instead retain the exact prefix, including a split high.
        output.exhausted = true;
        if matches!(boundary, Boundary::Value)
            && unicode(plan.declaration)
            && count > 0
            && (0xd800..=0xdbff).contains(&value[count - 1])
            && (0xdc00..=0xdfff).contains(&value[count])
        {
            count -= 1;
        }
    }
    if count > MAX_OUTPUT_UNITS.saturating_sub(output.units.len()) {
        return Err(Error::OutputLimit);
    }
    output.units.extend_from_slice(&value[..count]);
    Ok(())
}
fn characters(units: &[u16], supplementary: bool) -> impl Iterator<Item = &[u16]> {
    let mut offset = 0;
    std::iter::from_fn(move || {
        if offset == units.len() {
            return None;
        }
        let width = if supplementary
            && (0xd800..=0xdbff).contains(&units[offset])
            && units
                .get(offset + 1)
                .is_some_and(|unit| (0xdc00..=0xdfff).contains(unit))
        {
            2
        } else {
            1
        };
        let unit = &units[offset..offset + width];
        offset += width;
        Some(unit)
    })
}
