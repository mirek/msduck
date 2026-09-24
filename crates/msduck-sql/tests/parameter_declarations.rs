#[cfg(test)]
mod tests {
    use msduck_core::{
        character::{CharacterType, Family, Length},
        types::{BinaryType, DecimalType, Scale, Type},
    };
    use msduck_sql::sql_type::{ast, declaration};
    #[test]
    fn declaration_defaults_and_aliases_keep_logical_identity() {
        let declarations = msduck_sql::batch::parameter_declarations(
            "@a VARCHAR, @b NVARCHAR, @c CHAR, @d NCHAR, @e DECIMAL, @f TIME, @g DATETIME2, @h DATETIMEOFFSET, @i MONEY, @j SMALLMONEY, @k BIT, @l TINYINT, @m FLOAT(24), @n FLOAT(25), @o BINARY, @p VARBINARY(MAX), @q UNIQUEIDENTIFIER, @r DATETIME, @s SMALLDATETIME"
        ).unwrap();
        for ((_, kind), family) in declarations.iter().take(4).zip([
            Family::Varchar,
            Family::Nvarchar,
            Family::Char,
            Family::Nchar,
        ]) {
            assert_eq!(
                *kind,
                Type::Character(CharacterType::new(family, Length::Bounded(1)).unwrap())
            );
        }
        assert_eq!(
            declarations[4].1,
            Type::Decimal(DecimalType::new(18, 0).unwrap())
        );
        assert_eq!(declarations[5].1, Type::Time(Scale::new(7).unwrap()));
        assert_eq!(declarations[6].1, Type::DateTime2(Scale::new(7).unwrap()));
        assert_eq!(
            declarations[7].1,
            Type::DateTimeOffset(Scale::new(7).unwrap())
        );
        assert_eq!(declarations[8].1, Type::Money);
        assert_eq!(declarations[9].1, Type::SmallMoney);
        assert_eq!(declarations[10].1, Type::Bit);
        assert_eq!(declarations[11].1, Type::TinyInt);
        assert_eq!(declarations[12].1, Type::Real);
        assert_eq!(declarations[13].1, Type::Float);
        assert_eq!(
            declarations[14].1,
            Type::Binary(BinaryType::new(true, Length::Bounded(1)).unwrap())
        );
        assert_eq!(
            declarations[15].1,
            Type::Binary(BinaryType::new(false, Length::Max).unwrap())
        );
        assert_eq!(declarations[16].1, Type::UniqueIdentifier);
        assert_eq!(declarations[17].1, Type::DateTime);
        assert_eq!(declarations[18].1, Type::SmallDateTime);
        for (_, kind) in declarations {
            assert_eq!(declaration(&ast(kind)).unwrap(), kind);
        }
    }

    #[test]
    fn invalid_declarations_fail_before_execution() {
        for source in [
            "VARCHAR(0)",
            "VARCHAR(8001)",
            "NVARCHAR(4001)",
            "CHAR(MAX)",
            "NCHAR(4001)",
            "DECIMAL(0,0)",
            "DECIMAL(39,0)",
            "DECIMAL(5,6)",
            "TIME(8)",
            "DATETIME2(8)",
            "DATETIMEOFFSET(8)",
            "FLOAT(0)",
            "FLOAT(54)",
            "BINARY(8001)",
            "VARBINARY(0)",
            "SQL_VARIANT",
        ] {
            assert!(
                msduck_sql::batch::parameter_declarations(&format!("@p {source}")).is_err(),
                "{source}"
            );
        }
    }
}
