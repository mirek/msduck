//! Deterministic lookup over caller-owned row-source metadata.
use msduck_core::catalog::TypeMetadata as Info;
use sqlparser::ast::Ident;

#[derive(Clone)]
pub struct Field {
    /// None is unknown/not applicable, not a resolved default. Errors remain
    /// available for a later operator-specific binding diagnostic.
    pub collation: Option<Result<msduck_core::collation::Label, msduck_core::collation::Conflict>>,
    pub name: String,
    pub info: Option<Info>,
    /// Expression provenance, never inferred from an NVARCHAR value or storage type.
    pub json_fragment: bool,
    pub properties: msduck_core::result::Properties,
}
#[derive(Clone)]
pub struct Source {
    pub qualifiers: Vec<String>,
    pub fields: Vec<Field>,
}
/// Resolve nearest-scope names, preserving unknown and ambiguous shadowing.
/// Outer scopes are ordered from outermost to innermost; None is a barrier.
pub fn resolve<'a>(
    ids: &[&Ident],
    sources: &'a [Source],
    outer: &'a [Option<Vec<Source>>],
) -> Option<&'a Field> {
    let name = ids.last()?;
    let qualifier = ids[..ids.len() - 1]
        .iter()
        .map(|i| i.value.to_lowercase())
        .collect::<Vec<_>>()
        .join(".");
    for candidates in
        std::iter::once(Some(sources)).chain(outer.iter().rev().map(|scope| scope.as_deref()))
    {
        let candidates = candidates?;
        let matching = candidates
            .iter()
            .filter(|s| ids.len() == 1 || s.qualifiers.contains(&qualifier))
            .collect::<Vec<_>>();
        let fields = matching
            .iter()
            .flat_map(|s| &s.fields)
            .filter(|f| f.name.eq_ignore_ascii_case(&name.value))
            .collect::<Vec<_>>();
        // A local name (even with unknown type), ambiguity or matching qualifier
        // shadows outer scopes. Absent names or qualifiers may fall outward.
        if !fields.is_empty() || (ids.len() > 1 && !matching.is_empty()) {
            return if fields.len() == 1 && (ids.len() == 1 || matching.len() == 1) {
                Some(fields[0])
            } else {
                None
            };
        }
    }
    None
}

/// Resolve a qualified star's source at the nearest visible scope.
/// Ambiguous qualifiers and unresolved scopes block outward lookup.
pub fn resolve_source<'a>(
    qualifier: &str,
    sources: &'a [Source],
    outer: &'a [Option<Vec<Source>>],
) -> Option<&'a Source> {
    let qualifier = qualifier.to_lowercase();
    for candidates in
        std::iter::once(Some(sources)).chain(outer.iter().rev().map(|scope| scope.as_deref()))
    {
        let candidates = candidates?;
        let mut matching = candidates
            .iter()
            .filter(|source| source.qualifiers.contains(&qualifier));
        if let Some(source) = matching.next() {
            return matching.next().is_none().then_some(source);
        }
    }
    None
}

/// Caller-owned catalog snapshot of CTE declarations and enclosing row scopes.
#[derive(Clone, Default)]
pub struct Scope {
    /// Batch declarations survive query/CTE row-scope boundaries; no runtime values.
    pub parameters: std::collections::HashMap<String, Info>,
    pub ctes: std::collections::HashMap<String, Vec<Field>>,
    // Inner scopes come last. None means unresolved sources: do not guess through it.
    pub rows: Vec<Option<Vec<Source>>>,
}
impl std::ops::Deref for Scope {
    type Target = std::collections::HashMap<String, Vec<Field>>;
    fn deref(&self) -> &Self::Target {
        &self.ctes
    }
}
impl std::ops::DerefMut for Scope {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.ctes
    }
}
/// Query visitors enter WITH definitions before the body. Each definition sees
/// preceding declarations; body subqueries see the completed declaration scope.
pub struct QueryScopes {
    pub inherited: Scope,
    pub body: Scope,
    pub definitions: std::collections::VecDeque<Scope>,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source(qualifier: &str, name: &str, known: bool) -> Source {
        Source {
            qualifiers: vec![qualifier.into()],
            fields: vec![Field {
                collation: None,
                name: name.into(),
                properties: Default::default(),
                json_fragment: false,
                info: known.then_some(Info {
                    system_type_id: Some(60),
                    ..Info::default()
                }),
            }],
        }
    }
    #[test]
    fn unknown_and_ambiguous_local_names_block_outer_type_fallback() {
        let name = Ident::new("Price");
        let qualifier = Ident::new("P");
        let outer = [Some(vec![source("p", "price", true)])];
        assert!(resolve(&[&name], &[], &outer).unwrap().info.is_some());
        assert!(
            resolve(&[&qualifier, &name], &[], &outer)
                .unwrap()
                .info
                .is_some()
        );
        let local = [source("q", "price", false)];
        assert!(resolve(&[&name], &local, &outer).unwrap().info.is_none());
        assert!(
            resolve(&[&qualifier, &name], &local, &outer)
                .unwrap()
                .info
                .is_some()
        );
        assert!(resolve(&[&qualifier, &name], &[source("p", "other", true)], &outer).is_none());
        assert!(
            resolve(
                &[&name],
                &[source("q", "price", true), source("r", "price", true)],
                &outer
            )
            .is_none()
        );
        assert!(resolve(&[&name], &[], &[outer[0].clone(), None]).is_none());
        assert!(resolve(&[], &local, &outer).is_none());
    }
}
