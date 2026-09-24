//! Numeric arithmetic result types; no value evaluation or database access.
//! Decimal formulas adapted from mssqlite's transpile/src/decimal.ts and
//! Microsoft's precision/scale rules (see docs/reference-review.md).
use crate::catalog_snapshot::CatalogSnapshot;
use msduck_core::catalog::TypeMetadata;
use sqlparser::ast::{BinaryOperator, DataType, ExactNumberInfo};

fn rank(info: &TypeMetadata) -> Option<u8> {
    let id = info.system_type_id?;
    if info.user_type_id.is_some_and(|user| user != i32::from(id)) {
        return None;
    }
    Some(match id {
        48 => 0,
        52 => 1,
        56 => 2,
        127 => 3,
        122 => 4,
        60 => 5,
        106 | 108 => 6,
        59 => 7,
        62 => 8,
        _ => return None,
    })
}
fn decimal(info: &TypeMetadata) -> Option<(u16, u16)> {
    let p = u16::from(info.precision?);
    let s = u16::from(info.scale?);
    (p > 0 && p <= 38 && s <= p).then_some((p, s))
}

pub fn result(
    catalog: &CatalogSnapshot,
    op: &BinaryOperator,
    left: &TypeMetadata,
    right: &TypeMetadata,
) -> Option<TypeMetadata> {
    use BinaryOperator::*;
    if !matches!(op, Plus | Minus | Multiply | Divide | Modulo) {
        return None;
    }
    let (a, b) = (rank(left)?, rank(right)?);
    let winner = if a >= b { left } else { right };
    if a.max(b) != 6 {
        if matches!(op, Modulo) && a.max(b) >= 7 {
            return None;
        }
        return Some(winner.clone());
    }
    catalog.cast_info(&decimal_kind(op, left, right)?)
}

fn decimal_kind(
    op: &BinaryOperator,
    left: &TypeMetadata,
    right: &TypeMetadata,
) -> Option<DataType> {
    use BinaryOperator::*;
    let (a, b) = (rank(left)?, rank(right)?);
    if a.max(b) != 6 {
        return None;
    }
    let winner = if a >= b { left } else { right };
    let ((p1, s1), (p2, s2)) = (decimal(left)?, decimal(right)?);
    let (mut p, mut s) = match op {
        Plus | Minus => {
            let s = s1.max(s2);
            ((p1 - s1).max(p2 - s2) + s + 1, s)
        }
        Multiply => (p1 + p2 + 1, s1 + s2),
        Divide => {
            let s = 6.max(s1 + p2 + 1);
            (p1 - s1 + s2 + s, s)
        }
        Modulo => {
            let s = s1.max(s2);
            ((p1 - s1).min(p2 - s2) + s, s)
        }
        _ => return None,
    };
    if p > 38 {
        let integral = p - s;
        s = if matches!(op, Multiply | Divide) && integral > 32 {
            s.min(6)
        } else {
            s.min(38u16.saturating_sub(integral))
        };
        p = 38;
    }
    let shape = ExactNumberInfo::PrecisionAndScale(u64::from(p), i64::from(s));
    Some(if winner.system_type_id == Some(108) {
        DataType::Numeric(shape)
    } else {
        DataType::Decimal(shape)
    })
}

/// Common numeric type for UNION/INTERSECT/EXCEPT, without the carry digit
/// introduced by arithmetic addition. Unknown or alias types stay unresolved.
fn set_kind(left: &TypeMetadata, right: &TypeMetadata) -> Option<DataType> {
    let (a, b) = (rank(left)?, rank(right)?);
    let winner = if a >= b { left } else { right };
    let id = winner.system_type_id?;
    if a.max(b) == 6 {
        let ((p1, s1), (p2, s2)) = (decimal(left)?, decimal(right)?);
        let integral = (p1 - s1).max(p2 - s2);
        let scale = s1.max(s2).min(38 - integral);
        let shape =
            ExactNumberInfo::PrecisionAndScale(u64::from(integral + scale), i64::from(scale));
        return Some(if id == 108 {
            DataType::Numeric(shape)
        } else {
            DataType::Decimal(shape)
        });
    }
    Some(match id {
        48 => DataType::TinyInt(None),
        52 => DataType::SmallInt(None),
        56 => DataType::Int(None),
        127 => DataType::BigInt(None),
        _ => super::storage::catalog_scalar(
            i32::from(id),
            i32::from(winner.precision?),
            i32::from(winner.scale?),
        )?,
    })
}

pub fn set_info(
    catalog: &CatalogSnapshot,
    left: &TypeMetadata,
    right: &TypeMetadata,
) -> Option<TypeMetadata> {
    catalog.cast_info(&set_kind(left, right)?)
}

// AST declarations carry no catalog alias identity. Use SQL Server's fixed
// built-in numeric dimensions when the syntax does not specify dimensions.
fn declared_info(kind: &DataType) -> Option<TypeMetadata> {
    let shape = crate::catalog_shape::cast(kind)?;
    let (id, p, s) = match shape.name {
        "tinyint" => (48, 3, 0),
        "smallint" => (52, 5, 0),
        "int" => (56, 10, 0),
        "bigint" => (127, 19, 0),
        "smallmoney" => (122, 10, 4),
        "money" => (60, 19, 4),
        "decimal" => (106, 18, 0),
        "numeric" => (108, 18, 0),
        "real" => (59, 24, 0),
        "float" => (62, 53, 0),
        _ => return None,
    };
    Some(TypeMetadata {
        system_type_id: Some(id),
        user_type_id: Some(i32::from(id)),
        precision: Some(shape.precision.unwrap_or(p)),
        scale: Some(shape.scale.unwrap_or(s)),
        ..Default::default()
    })
}

pub fn set_type(left: &DataType, right: &DataType) -> Option<DataType> {
    set_kind(&declared_info(left)?, &declared_info(right)?)
}

/// SQL decimal arithmetic declarations without a catalog or backend types.
pub fn decimal_type(op: &BinaryOperator, left: &DataType, right: &DataType) -> Option<DataType> {
    decimal_kind(op, &declared_info(left)?, &declared_info(right)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn info(id: u8, p: u8, s: u8) -> TypeMetadata {
        TypeMetadata {
            system_type_id: Some(id),
            user_type_id: Some(i32::from(id)),
            precision: Some(p),
            scale: Some(s),
            ..Default::default()
        }
    }
    #[test]
    fn set_results_combine_integral_and_fractional_capacity_without_a_carry() {
        let d = |p, s| DataType::Decimal(ExactNumberInfo::PrecisionAndScale(p, s));
        for (a, b, expected) in [
            (d(5, 2), d(7, 4), d(7, 4)),
            (d(5, 2), DataType::Int(None), d(12, 2)),
            (d(38, 2), d(38, 10), d(38, 2)),
            (
                DataType::SmallInt(None),
                DataType::Int(None),
                DataType::Int(None),
            ),
            (
                DataType::Int(None),
                DataType::BigInt(None),
                DataType::BigInt(None),
            ),
        ] {
            assert_eq!(set_type(&a, &b), Some(expected.clone()));
            assert_eq!(set_type(&b, &a), Some(expected));
        }
        assert!(set_type(&d(5, 2), &DataType::Text).is_none());
    }

    #[test]
    fn integer_promotion_and_decimal_formulas_include_precision_reduction() {
        use BinaryOperator::*;
        let mut catalog = CatalogSnapshot::default();
        catalog.types.insert("decimal".into(), info(106, 38, 0));
        let small = info(52, 5, 0);
        let integer = info(56, 10, 0);
        assert_eq!(
            result(&catalog, &Plus, &small, &integer)
                .unwrap()
                .system_type_id,
            Some(56)
        );
        assert_eq!(
            result(&catalog, &Plus, &small, &small)
                .unwrap()
                .system_type_id,
            Some(52)
        );
        for (op, p1, s1, p2, s2, p, s) in [
            (Plus, 5, 2, 10, 0, 13, 2),
            (Minus, 5, 2, 10, 0, 13, 2),
            (Multiply, 5, 2, 10, 0, 16, 2),
            (Divide, 5, 2, 10, 0, 16, 13),
            (Modulo, 5, 2, 10, 0, 5, 2),
            (Multiply, 30, 20, 30, 20, 38, 17),
            (Multiply, 30, 10, 30, 10, 38, 6),
            (Plus, 38, 2, 38, 2, 38, 1),
            (Multiply, 38, 0, 38, 0, 38, 0),
        ] {
            let r = result(&catalog, &op, &info(106, p1, s1), &info(106, p2, s2)).unwrap();
            assert_eq!((r.precision, r.scale), (Some(p), Some(s)), "{op}");
        }
        assert!(result(&catalog, &Plus, &integer, &info(167, 0, 0)).is_none());
        let mut alias = integer.clone();
        alias.user_type_id = Some(5000);
        assert!(result(&catalog, &Plus, &integer, &alias).is_none());
    }
}
