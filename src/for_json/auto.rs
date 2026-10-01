//! FOR JSON AUTO adapter: binds the nesting plan of
//! `msduck_sql::dialect::ext::json_string::auto` against the catalog,
//! formats top-level results and lowers nested AUTO subqueries to native
//! row and grouping functions. See docs/gaps-json_string.md.
use super::*;
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeHandle as LogicalType, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use msduck_sql::dialect::ext::json_string::{
    self as rules,
    auto::{self as nesting, Grouper, LevelRow},
};
use sqlparser::ast::{
    Expr, ForClause, ForJson, FunctionArg, FunctionArgExpr, FunctionArgumentList,
    FunctionArguments, Ident, ObjectName, Query, SetExpr, TableAlias, TableAliasColumnDef,
    TableFactor,
};

const RESULT_NAME: &str = "JSON_F52E2B61-18A1-11d1-B105-00805F49916B";

/// One result column's placement and formatting.
#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    name: String,
    level: usize,
    /// Part of its level's grouping key.
    key: bool,
    /// Embedded unescaped (nested FOR JSON, JSON_QUERY).
    fragment: bool,
    /// Text keys compare case-insensitively (the default collation).
    ci: bool,
    /// TIME declarations, for nested rows that lost their scale.
    scale: Option<u8>,
}

/// A bound FOR JSON AUTO clause.
#[derive(Clone, Debug)]
pub struct Spec {
    columns: Vec<Column>,
    levels: Vec<String>,
    options: Options,
}

type Encoded = (
    Vec<(String, usize, bool, bool, bool, Option<u8>)>,
    Vec<String>,
    bool,
);

impl Spec {
    fn encode(&self) -> Result<String> {
        let columns = self
            .columns
            .iter()
            .map(|c| (c.name.clone(), c.level, c.key, c.fragment, c.ci, c.scale))
            .collect::<Vec<_>>();
        Ok(serde_json::to_string(&(
            columns,
            &self.levels,
            self.options.include_null_values,
        ))?)
    }
    fn decode(text: &str) -> Result<Self> {
        let (columns, levels, include_null_values): Encoded = serde_json::from_str(text)?;
        ensure!(
            !levels.is_empty() && columns.iter().all(|column| column.1 < levels.len()),
            "invalid FOR JSON AUTO descriptor"
        );
        Ok(Self {
            columns: columns
                .into_iter()
                .map(|(name, level, key, fragment, ci, scale)| Column {
                    name,
                    level,
                    key,
                    fragment,
                    ci,
                    scale,
                })
                .collect(),
            levels,
            options: Options {
                include_null_values,
                without_array_wrapper: true,
                root: None,
            },
        })
    }
    pub fn width(&self) -> usize {
        self.columns.len()
    }
    pub fn options(&self) -> &Options {
        &self.options
    }
}

pub fn is_auto(query: &Query) -> bool {
    matches!(
        query.for_clause,
        Some(ForClause::Json {
            for_json: ForJson::Auto,
            ..
        })
    )
}

/// A marker error from the syntax rules, recovered by the runtime
/// diagnostic chain.
fn marker(message: String) -> anyhow::Error {
    anyhow::anyhow!(message)
}

fn case_sensitive(field: &crate::query_catalog::Field) -> bool {
    matches!(&field.collation, Some(Ok(label))
    if label.name().is_some_and(|name| {
        let name = name.to_ascii_uppercase();
        name.contains("_CS") || name.contains("BIN")
    }))
}

/// Column names of one FROM source, by binding `SELECT * FROM source`.
fn source_fields(
    db: &Connection,
    factor: &TableFactor,
    scope: &crate::query_catalog::Scope,
) -> Option<Vec<crate::query_catalog::Field>> {
    let Statement::Query(mut probe) = sqlparser::parser::Parser::parse_sql(
        &sqlparser::dialect::GenericDialect {},
        "SELECT * FROM (SELECT 1) AS __msduck_probe",
    )
    .ok()?
    .remove(0) else {
        return None;
    };
    let SetExpr::Select(select) = probe.body.as_mut() else {
        return None;
    };
    select.from[0].relation = factor.clone();
    crate::query_catalog::projection_in_scope(db, &probe, scope)
        .ok()
        .flatten()
        .filter(|fields| !fields.is_empty())
}

/// Bind a FOR JSON AUTO query (its clause still attached).
pub fn spec(db: &Connection, query: &Query, scope: &crate::query_catalog::Scope) -> Result<Spec> {
    rules::validate_for_json_auto(query).map_err(marker)?;
    let Some(ForClause::Json {
        root,
        include_null_values,
        without_array_wrapper,
        ..
    }) = &query.for_clause
    else {
        bail!("not a FOR JSON AUTO query");
    };
    let select = nesting::first_select(&query.body).expect("validated FOR JSON AUTO body");
    let mut factors = Vec::new();
    fn collect<'a>(factor: &'a TableFactor, out: &mut Vec<&'a TableFactor>) {
        if let TableFactor::NestedJoin {
            table_with_joins, ..
        } = factor
        {
            collect(&table_with_joins.relation, out);
            for join in &table_with_joins.joins {
                collect(&join.relation, out);
            }
        } else {
            out.push(factor);
        }
    }
    for table in &select.from {
        collect(&table.relation, &mut factors);
        for join in &table.joins {
            collect(&join.relation, &mut factors);
        }
    }
    let fields = factors
        .iter()
        .map(|factor| source_fields(db, factor, scope))
        .collect::<Vec<_>>();
    let unqualified = |name: &str| {
        let mut owners = fields.iter().enumerate().filter(|(_, fields)| {
            fields
                .as_ref()
                .is_some_and(|fields| fields.iter().any(|f| f.name.eq_ignore_ascii_case(name)))
        });
        let first = owners.next()?.0;
        owners.next().is_none().then_some(first)
    };
    let names = |index: usize| {
        fields
            .get(index)?
            .as_ref()
            .map(|fields| fields.iter().map(|f| f.name.clone()).collect())
    };
    let plan = nesting::plan(&query.body, &unqualified, &names).map_err(marker)?;
    // Fragment flags and collations by position, from the bound projection.
    let mut source = query.clone();
    source.for_clause = None;
    let bound = crate::query_catalog::projection_in_scope(db, &source, scope)?
        .filter(|bound| bound.len() == plan.columns.len());
    let mut fragments = Vec::new();
    for item in &select.projection {
        match item {
            SelectItem::ExprWithAlias { expr, .. } | SelectItem::UnnamedExpr(expr) => {
                fragments.push(syntax::fragment(expr))
            }
            _ => {
                let only = match nesting::reference(item, &nesting::sources(select), &unqualified)
                    .map_err(marker)?
                {
                    nesting::Reference::Star(Some(index)) => vec![index],
                    _ => (0..fields.len()).collect(),
                };
                for index in only {
                    for field in fields.get(index).cloned().flatten().unwrap_or_default() {
                        fragments.push(field.json_fragment);
                    }
                }
            }
        }
    }
    let columns = plan
        .columns
        .into_iter()
        .enumerate()
        .map(|(index, column)| {
            let field = bound.as_ref().map(|bound| &bound[index]);
            Column {
                name: column.name,
                level: column.level,
                key: column.key,
                fragment: fragments.get(index).copied().unwrap_or(false)
                    || field.is_some_and(|field| field.json_fragment),
                ci: !field.is_some_and(case_sensitive),
                scale: field
                    .and_then(|field| crate::query_catalog::time_scale(field.info.as_ref())),
            }
        })
        .collect();
    Ok(Spec {
        columns,
        levels: plan.levels,
        options: Options {
            root: root.clone(),
            include_null_values: *include_null_values,
            without_array_wrapper: *without_array_wrapper,
        },
    })
}

/// JSON text of one value, and its grouping key.
fn render_value(
    value: &Value,
    kind: Option<&Type>,
    fragment: bool,
    ci: bool,
) -> Result<(Option<Vec<u16>>, Option<Vec<u16>>)> {
    if matches!(value, Value::Null) {
        return Ok((None, None));
    }
    let unicode = is_unicode(value);
    let text = if unicode {
        String::new()
    } else {
        spelling(value, kind)?
    };
    let units: Vec<u16> = if unicode {
        crate::unicode_carrier::units(value)?
    } else {
        text.encode_utf16().collect()
    };
    let rendered = match value {
        Value::Boolean(v) => (if *v { "true" } else { "false" }).encode_utf16().collect(),
        value if (matches!(value, Value::Text(_)) || unicode) && fragment => {
            Fragment::new(&units)?;
            units.clone()
        }
        Value::TinyInt(_)
        | Value::SmallInt(_)
        | Value::Int(_)
        | Value::BigInt(_)
        | Value::HugeInt(_)
        | Value::UTinyInt(_)
        | Value::USmallInt(_)
        | Value::UInt(_)
        | Value::UBigInt(_)
        | Value::Float(_)
        | Value::Double(_)
        | Value::Decimal(_) => {
            Number::new(&text)?;
            units.clone()
        }
        _ => nesting::quoted(&String::from_utf16_lossy(&units)),
    };
    let key = if matches!(value, Value::Text(_)) || unicode {
        let mut key = units;
        while key.last() == Some(&0x20) {
            key.pop();
        }
        if ci {
            key = String::from_utf16_lossy(&key)
                .to_lowercase()
                .encode_utf16()
                .collect();
        }
        key
    } else {
        rendered.clone()
    };
    Ok((Some(rendered), Some(key)))
}

/// One source row's contribution to every level.
fn levels(spec: &Spec, values: &[Value], types: &[Option<Type>]) -> Result<Vec<LevelRow>> {
    ensure!(
        values.len() == spec.columns.len(),
        "FOR JSON AUTO row has {} values, expected {}",
        values.len(),
        spec.columns.len()
    );
    let mut rows = vec![LevelRow::default(); spec.levels.len()];
    for ((column, value), kind) in spec.columns.iter().zip(values).zip(types) {
        let kind = match column.scale {
            Some(scale) => Some(Type::Time(scale)),
            None => kind.clone(),
        };
        let (rendered, key) = render_value(value, kind.as_ref(), column.fragment, column.ci)?;
        let row = &mut rows[column.level];
        if column.key {
            row.key.push(key);
        }
        if rendered.is_none() && !spec.options.include_null_values {
            continue;
        }
        if !row.members.is_empty() {
            row.members.push(u16::from(b','));
        }
        row.members.extend(nesting::quoted(&column.name));
        row.members.push(u16::from(b':'));
        row.members
            .extend(rendered.unwrap_or_else(|| "null".encode_utf16().collect()));
    }
    Ok(rows)
}

const LIMIT: usize = (tds::MAX_MESSAGE - 256) / 2;

/// Top-level FOR JSON AUTO result writer.
pub struct Writer<'a> {
    spec: &'a Spec,
    grouper: Grouper,
}

impl<'a> Writer<'a> {
    pub fn new(spec: &'a Spec) -> Self {
        Self {
            spec,
            grouper: Grouper::new(&spec.levels, LIMIT),
        }
    }
    pub fn row(&mut self, values: &[Value], types: &[Option<Type>]) -> Result<()> {
        let row = levels(self.spec, values, types)?;
        self.grouper
            .push(row)
            .map_err(|_| anyhow::anyhow!("result exceeds current 16 MiB response limit"))
    }
    /// The wrapped result, or None when there were no rows.
    pub fn finish(self) -> Result<Option<Vec<u16>>> {
        let objects = self.grouper.finish();
        if objects.is_empty() {
            return Ok(None);
        }
        Ok(Some(
            json::wrap_rows_utf16(
                objects.iter().map(Vec::as_slice),
                self.spec.options.root.as_deref(),
                self.spec.options.without_array_wrapper,
                LIMIT,
            )
            .map_err(syntax::diagnostic)?,
        ))
    }
}

/// Lower a nested FOR JSON AUTO query to native row and grouping functions,
/// keeping its source (projection, ordering, row limits) intact.
pub fn lower(
    db: &Connection,
    query: &mut Query,
    scope: &crate::query_catalog::Scope,
    parameters: &HashMap<String, Parameter>,
) -> Result<()> {
    let mut spec = spec(db, query, scope)?;
    let mut source = query.clone();
    source.for_clause = None;
    if spec.columns.iter().all(|column| column.scale.is_none()) {
        let types = crate::result_types::bound_projection(
            db,
            &Statement::Query(Box::new(source.clone())),
            parameters,
        )?;
        for (column, kind) in spec.columns.iter_mut().zip(types) {
            if let Some(Type::Time(scale)) = kind {
                column.scale = Some(scale);
            }
        }
    }
    let encoded = spec.encode()?;
    let root = spec
        .options
        .root
        .as_ref()
        .map(|root| format!("1{root}"))
        .unwrap_or_else(|| "0".into());
    let mut alias = "__msduck_json_source".to_owned();
    while source.to_string().to_lowercase().contains(&alias) {
        alias.push('_');
    }
    let literal =
        |text: String| Expr::Value(sqlparser::ast::Value::SingleQuotedString(text).into());
    let call = |name: &str, args: Vec<Expr>| {
        Expr::Function(sqlparser::ast::Function {
            name: ObjectName::from(vec![Ident::new(name)]),
            uses_odbc_syntax: false,
            parameters: FunctionArguments::None,
            args: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args: args
                    .into_iter()
                    .map(|arg| FunctionArg::Unnamed(FunctionArgExpr::Expr(arg)))
                    .collect(),
                clauses: Vec::new(),
            }),
            filter: None,
            null_treatment: None,
            over: None,
            within_group: Vec::new(),
        })
    };
    let row = call(
        "row",
        (0..spec.width())
            .map(|index| {
                Expr::CompoundIdentifier(vec![Ident::new(&alias), Ident::new(format!("c{index}"))])
            })
            .collect(),
    );
    let grouped = call(
        "__msduck_json_auto",
        vec![
            call(
                "list",
                vec![call(
                    "__msduck_json_auto_row",
                    vec![row, literal(encoded.clone())],
                )],
            ),
            literal(encoded),
        ],
    );
    let wrapped = call(
        if spec.options.without_array_wrapper {
            "__msduck_json_unwrapped"
        } else {
            "__msduck_json_array"
        },
        vec![grouped, literal(root)],
    );
    let Statement::Query(mut outer) = sqlparser::parser::Parser::parse_sql(
        &sqlparser::dialect::GenericDialect {},
        "SELECT NULL AS x FROM (SELECT 1) AS __msduck_json_source",
    )?
    .remove(0) else {
        unreachable!()
    };
    let SetExpr::Select(select) = outer.body.as_mut() else {
        unreachable!()
    };
    select.projection = vec![SelectItem::ExprWithAlias {
        expr: wrapped,
        alias: Ident::with_quote('"', RESULT_NAME),
    }];
    let TableFactor::Derived {
        subquery,
        alias: source_alias,
        ..
    } = &mut select.from[0].relation
    else {
        unreachable!()
    };
    *source_alias = Some(TableAlias {
        explicit: true,
        name: Ident::new(&alias),
        columns: (0..spec.width())
            .map(|index| TableAliasColumnDef::from_name(format!("c{index}")))
            .collect(),
        at: None,
    });
    **subquery = source;
    *query = *outer;
    Ok(())
}

// Row carriers frame each level as: member length (two units, high first),
// members, key count, then per key a presence unit and, when present, its
// length (two units) and units.
fn push_len(out: &mut Vec<u16>, len: usize) -> Result<()> {
    let len = u32::try_from(len)?;
    out.push((len >> 16) as u16);
    out.push(len as u16);
    Ok(())
}

fn frame(rows: &[LevelRow]) -> Result<Vec<u16>> {
    let mut out = Vec::new();
    for row in rows {
        push_len(&mut out, row.members.len())?;
        out.extend_from_slice(&row.members);
        out.push(u16::try_from(row.key.len())?);
        for key in &row.key {
            match key {
                None => out.push(0),
                Some(units) => {
                    out.push(1);
                    push_len(&mut out, units.len())?;
                    out.extend_from_slice(units);
                }
            }
        }
    }
    Ok(out)
}

fn take<'a>(units: &'a [u16], at: &mut usize, count: usize) -> Result<&'a [u16]> {
    let slice = units
        .get(*at..at.saturating_add(count))
        .ok_or_else(|| anyhow::anyhow!("invalid FOR JSON AUTO row carrier"))?;
    *at += count;
    Ok(slice)
}

fn take_len(units: &[u16], at: &mut usize) -> Result<usize> {
    let pair = take(units, at, 2)?;
    Ok((usize::from(pair[0]) << 16) | usize::from(pair[1]))
}

fn unframe(units: &[u16], levels: usize) -> Result<Vec<LevelRow>> {
    let mut at = 0;
    let mut rows = Vec::with_capacity(levels);
    for _ in 0..levels {
        let len = take_len(units, &mut at)?;
        let members = take(units, &mut at, len)?.to_vec();
        let keys = usize::from(take(units, &mut at, 1)?[0]);
        let mut key = Vec::with_capacity(keys);
        for _ in 0..keys {
            if take(units, &mut at, 1)?[0] == 0 {
                key.push(None);
            } else {
                let len = take_len(units, &mut at)?;
                key.push(Some(take(units, &mut at, len)?.to_vec()));
            }
        }
        rows.push(LevelRow { members, key });
    }
    ensure!(at == units.len(), "invalid FOR JSON AUTO row carrier");
    Ok(rows)
}

/// `__msduck_json_auto_row(row, spec)`: one source row's levels.
struct Row;
impl VScalar for Row {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        (|| -> Result<()> {
            let len = input.len();
            ensure!(
                input.flat_vector(0).logical_type().id() == Id::Struct,
                "invalid FOR JSON AUTO row input"
            );
            let source = input.struct_vector(0);
            let specs = input.flat_vector(1);
            let result = output.struct_vector();
            let payload = result.child(0, len);
            let mut remaining = crate::unicode_carrier::CHUNK_LIMIT;
            let mut cached: Option<(String, Spec)> = None;
            for row in 0..len {
                let encoded = native::text(&specs, row, len)?;
                if cached.as_ref().is_none_or(|(key, _)| key != &encoded) {
                    let spec = Spec::decode(&encoded)?;
                    ensure!(
                        spec.width() == source.num_children(),
                        "invalid FOR JSON AUTO row width"
                    );
                    cached = Some((encoded, spec));
                }
                let (_, spec) = cached.as_ref().unwrap();
                let (values, types): (Vec<_>, Vec<_>) = (0..source.num_children())
                    .map(|column| native::value(&source, column, row, len))
                    .collect::<Result<Vec<_>>>()?
                    .into_iter()
                    .unzip();
                let units = frame(&levels(spec, &values, &types)?)?;
                let bytes = native::encoded_units(&units, &mut remaining)?;
                payload.insert(row, bytes.as_slice());
            }
            Ok(())
        })()
        .map_err(Into::into)
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Any.into(), Id::Varchar.into()],
            crate::unicode_carrier::kind(),
        )]
    }
}

/// `__msduck_json_auto(rows, spec)`: the grouped top-level objects, NULL
/// for no rows (a nested FOR JSON over no rows is NULL).
struct Group;
impl VScalar for Group {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        (|| -> Result<()> {
            let len = input.len();
            let source = input.list_vector(0);
            let child_count = source.len();
            let entries = source.child(child_count);
            let carriers = source.struct_child(child_count);
            let bytes = carriers.child(0, child_count);
            let specs = input.flat_vector(1);
            let mut objects: Vec<Vec<u8>> = Vec::new();
            let mut lists = Vec::with_capacity(len);
            let mut remaining = crate::unicode_carrier::CHUNK_LIMIT;
            for row in 0..len {
                if source.row_is_null(row as u64) {
                    lists.push(None);
                    continue;
                }
                let spec = Spec::decode(&native::text(&specs, row, len)?)?;
                let (offset, count) = source.try_get_entry(row)?;
                let end = offset
                    .checked_add(count)
                    .filter(|&end| end <= child_count)
                    .ok_or_else(|| anyhow::anyhow!("invalid FOR JSON AUTO row-list bounds"))?;
                let mut grouper = Grouper::new(&spec.levels, LIMIT);
                for index in offset..end {
                    ensure!(
                        !entries.row_is_null(index as u64) && !bytes.row_is_null(index as u64),
                        "invalid NULL FOR JSON AUTO row carrier"
                    );
                    let raw = crate::unicode_carrier::bytes(&bytes, index, child_count)
                        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                    ensure!(raw.len() % 2 == 0, "invalid Unicode carrier byte length");
                    let units = raw
                        .chunks_exact(2)
                        .map(|b| u16::from_le_bytes([b[0], b[1]]))
                        .collect::<Vec<_>>();
                    grouper
                        .push(unframe(&units, spec.levels.len())?)
                        .map_err(|e| anyhow::anyhow!(e))?;
                }
                let start = objects.len();
                for object in grouper.finish() {
                    objects.push(native::encoded_units(&object, &mut remaining)?);
                }
                lists.push((objects.len() > start).then_some((start, objects.len() - start)));
            }
            let mut list = output.list_vector();
            for (row, entry) in lists.iter().enumerate() {
                match entry {
                    Some((offset, count)) => list.set_entry(row, *offset, *count),
                    None => {
                        list.set_entry(row, objects.len(), 0);
                        list.set_null(row);
                    }
                }
            }
            let child = list.struct_child(objects.len());
            list.try_set_len(objects.len())?;
            let payload = child.child(0, objects.len());
            for (index, object) in objects.iter().enumerate() {
                payload.insert(index, object.as_slice());
            }
            Ok(())
        })()
        .map_err(Into::into)
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![
                LogicalType::list(&crate::unicode_carrier::kind()),
                Id::Varchar.into(),
            ],
            LogicalType::list(&crate::unicode_carrier::kind()),
        )]
    }
}

pub fn register(db: &Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<Row>("__msduck_json_auto_row")?;
    db.register_scalar_function::<Group>("__msduck_json_auto")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_carriers_round_trip() {
        let rows = vec![
            LevelRow {
                members: "\"a\":1".encode_utf16().collect(),
                key: vec![Some(vec![49]), None],
            },
            LevelRow::default(),
        ];
        assert_eq!(unframe(&frame(&rows).unwrap(), 2).unwrap(), rows);
        assert!(unframe(&frame(&rows).unwrap(), 3).is_err());
    }
}
