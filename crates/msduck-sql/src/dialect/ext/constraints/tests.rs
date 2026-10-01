use super::*;

fn claimed(sql: &str) -> Alter {
    let statements = crate::batch::parse(sql).unwrap_or_else(|error| panic!("{sql}: {error}"));
    assert_eq!(statements.len(), 1, "{sql}");
    let alter = decode(&statements[0])
        .unwrap_or_else(|| panic!("{sql} was not claimed"))
        .unwrap();
    // The carrier payload is canonical: it parses back to the same value.
    let again = crate::batch::parse(&alter.to_string()).unwrap();
    assert_eq!(decode(&again[0]).unwrap().unwrap(), alter, "{sql}");
    alter
}

fn declined(sql: &str) {
    let statements = crate::batch::parse(sql).unwrap_or_else(|error| panic!("{sql}: {error}"));
    assert!(decode(&statements[0]).is_none(), "{sql} was claimed");
}

/// The first ADD item, which must be a table constraint.
fn first(alter: &Alter) -> &Constraint {
    let Action::Add(items) = &alter.action else {
        panic!("not an ADD")
    };
    let AddItem::Constraint(constraint) = &items[0] else {
        panic!("not a constraint")
    };
    constraint
}

#[test]
fn table_constraints_are_claimed() {
    let alter = claimed("ALTER TABLE dbo.items ADD CONSTRAINT ck_items CHECK(value>0)");
    assert_eq!(alter.table.to_string(), "dbo.items");
    assert_eq!(alter.with_check, None);
    let constraint = first(&alter);
    assert_eq!(constraint.name.as_ref().unwrap().value, "ck_items");
    assert!(matches!(constraint.kind, Kind::Check(_)));

    let alter = claimed("ALTER TABLE items ADD CONSTRAINT df_items DEFAULT(0) FOR value");
    assert!(
        matches!(&first(&alter).kind, Kind::Default { column, with_values: false, .. } if column.value == "value")
    );

    let alter = claimed(
        "ALTER TABLE items ADD CONSTRAINT uq_items UNIQUE NONCLUSTERED (id ASC, v DESC) WITH (FILLFACTOR = 80) ON [PRIMARY]",
    );
    assert!(matches!(&first(&alter).kind, Kind::Unique(columns) if columns.len() == 2));

    let alter = claimed("ALTER TABLE items ADD CONSTRAINT pk PRIMARY KEY CLUSTERED (id)");
    assert!(matches!(first(&alter).kind, Kind::PrimaryKey(_)));
    let alter = claimed("ALTER TABLE items WITH CHECK ADD PRIMARY KEY (id)");
    assert!(first(&alter).name.is_none());

    let alter = claimed(
        "ALTER TABLE items WITH NOCHECK ADD CONSTRAINT fk_items FOREIGN KEY(parent_id) REFERENCES dbo.parent(id) ON UPDATE CASCADE ON DELETE SET NULL",
    );
    assert_eq!(alter.with_check, Some(false));
    let Kind::ForeignKey(key) = &first(&alter).kind else {
        panic!()
    };
    assert_eq!(key.on_delete, Referential::SetNull);
    assert_eq!(key.on_update, Referential::Cascade);
    assert_eq!(key.referred.len(), 1);

    let alter = claimed("ALTER TABLE c ADD FOREIGN KEY (a, b) REFERENCES p ON DELETE SET DEFAULT");
    let Kind::ForeignKey(key) = &first(&alter).kind else {
        panic!()
    };
    assert!(key.referred.is_empty());
    assert_eq!(key.on_delete, Referential::SetDefault);
    assert_eq!(key.on_update, Referential::NoAction);
}

#[test]
fn mixed_additions_keep_their_order() {
    let alter = claimed(
        "ALTER TABLE items ADD CONSTRAINT ck_x CHECK (value < 100), extra int NOT NULL CONSTRAINT df_extra DEFAULT 5 WITH VALUES, CONSTRAINT ck_y CHECK (extra > 0)",
    );
    let Action::Add(items) = &alter.action else {
        panic!()
    };
    assert_eq!(items.len(), 3);
    assert!(
        matches!(&items[1], AddItem::Column(column) if column.name.value == "extra" && column_has_constraints(column))
    );
}

#[test]
fn named_column_constraints_are_claimed() {
    claimed("ALTER TABLE items ADD n int NOT NULL CONSTRAINT df_n DEFAULT 3 WITH VALUES");
    claimed("ALTER TABLE items ADD n int NOT NULL CONSTRAINT df_n DEFAULT 3");
    claimed("ALTER TABLE items ADD n int NULL CONSTRAINT df_n DEFAULT 3 WITH VALUES");
    claimed("ALTER TABLE items ADD n int CHECK (n > 0)");
    claimed("ALTER TABLE items ADD n int REFERENCES parent(id) ON DELETE CASCADE");
    claimed("ALTER TABLE items ADD n int CONSTRAINT uq_n UNIQUE");
}

#[test]
fn toggles_and_drops_are_claimed() {
    let alter = claimed("ALTER TABLE items NOCHECK CONSTRAINT ALL");
    assert_eq!(
        alter.action,
        Action::Toggle {
            enable: false,
            targets: Targets::All
        }
    );
    let alter = claimed("ALTER TABLE items WITH CHECK CHECK CONSTRAINT ALL");
    assert_eq!(alter.with_check, Some(true));
    assert_eq!(
        alter.action,
        Action::Toggle {
            enable: true,
            targets: Targets::All
        }
    );
    let alter = claimed("ALTER TABLE items CHECK CONSTRAINT a, [b]");
    assert!(
        matches!(alter.action, Action::Toggle { enable: true, targets: Targets::Names(ref names) } if names.len() == 2)
    );

    let alter = claimed("ALTER TABLE items DROP CONSTRAINT IF EXISTS a, b, COLUMN c, CONSTRAINT d");
    let Action::Drop(items) = &alter.action else {
        panic!()
    };
    assert_eq!(items.len(), 4);
    assert!(matches!(
        &items[0],
        DropItem::Constraint {
            if_exists: true,
            ..
        }
    ));
    assert!(matches!(
        &items[1],
        DropItem::Constraint {
            if_exists: false,
            ..
        }
    ));
    assert!(matches!(&items[2], DropItem::Column { .. }));
    assert!(matches!(&items[3], DropItem::Constraint { .. }));
    let alter = claimed("ALTER TABLE ch DROP fk_ch");
    assert!(
        matches!(&alter.action, Action::Drop(items) if matches!(&items[0], DropItem::Constraint { .. }))
    );
}

#[test]
fn a_lone_unnamed_primary_key_keeps_its_native_shape() {
    // The built-in parser reads it as a column `PRIMARY` of type `KEY(a)`,
    // which the key index syntax tests pin; `native_primary_key` recovers it.
    let statements =
        crate::batch::parse("ALTER TABLE T ADD PRIMARY KEY NONCLUSTERED (a, b DESC)").unwrap();
    assert!(decode(&statements[0]).is_none());
    let Statement::AlterTable(alter) = &statements[0] else {
        panic!()
    };
    let columns = native_primary_key(alter).unwrap();
    assert_eq!(
        columns.iter().map(|c| c.value.as_str()).collect::<Vec<_>>(),
        ["a", "b"]
    );
    let statements = crate::batch::parse("ALTER TABLE T ADD c INT NULL").unwrap();
    let Statement::AlterTable(alter) = &statements[0] else {
        panic!()
    };
    assert!(native_primary_key(alter).is_none());
}

#[test]
fn plain_column_changes_are_declined() {
    declined("ALTER TABLE items ADD n int NULL");
    declined("ALTER TABLE items ADD n int NOT NULL DEFAULT 3 WITH VALUES");
    // Still refused by the built-in path (docs/gaps-constraints.md).
    declined("ALTER TABLE items ADD n int CONSTRAINT df_n DEFAULT 2");
    declined("ALTER TABLE items ADD n int UNIQUE");
    declined("ALTER TABLE items ADD n int NOT NULL PRIMARY KEY");
    declined("ALTER TABLE items DROP COLUMN n");
    declined("ALTER TABLE items DROP COLUMN n, m");
    declined("ALTER TABLE items ALTER COLUMN n bigint NULL");
    declined("ALTER DATABASE CURRENT SET MULTI_USER");
}

#[test]
fn statements_follow_without_semicolons() {
    let statements =
        crate::batch::parse("ALTER TABLE t NOCHECK CONSTRAINT ALL INSERT t VALUES (1) ALTER TABLE t CHECK CONSTRAINT ALL")
            .unwrap();
    assert_eq!(statements.len(), 3);
    assert!(decode(&statements[0]).is_some());
    assert!(decode(&statements[2]).is_some());
}

#[test]
fn carriers_are_owned_and_nest() {
    let statements =
        crate::batch::parse("IF 1 = 1 BEGIN ALTER TABLE t ADD CONSTRAINT c CHECK (a > 0) END")
            .unwrap();
    assert_eq!(statements.len(), 1);
    let statement = carrier(KIND, "ALTER TABLE t NOCHECK CONSTRAINT ALL", vec![]);
    assert!(super::super::owns(&statement));
}

#[test]
fn bracketed_names_with_closing_brackets_survive_the_payload() {
    let alter = claimed("ALTER TABLE [x]]y] DROP CONSTRAINT [x]], CONSTRAINT [y]");
    let Action::Drop(items) = &alter.action else {
        panic!()
    };
    assert_eq!(alter.table.0[0].as_ident().unwrap().value, "x]y");
    assert!(
        matches!(&items[..], [DropItem::Constraint { name, .. }] if name.value == "x], CONSTRAINT [y")
    );
    let alter = claimed("ALTER TABLE t ADD CONSTRAINT [c]]k] CHECK ([a]]b] > 0)");
    assert_eq!(first(&alter).name.as_ref().unwrap().value, "c]k");
    let Kind::Check(Expr::BinaryOp { left, .. }) = &first(&alter).kind else {
        panic!()
    };
    assert!(matches!(left.as_ref(), Expr::Identifier(ident) if ident.value == "a]b"));
}

#[test]
fn stored_expressions_round_trip_bracketed_names() {
    let mut expr = Parser::new(&crate::dialect::ServerDialect)
        .try_with_sql("[a]]b] > 0")
        .unwrap()
        .parse_expr()
        .unwrap();
    delimit_expr(&mut expr);
    let again = Parser::new(&crate::dialect::ServerDialect)
        .try_with_sql(&expr.to_string())
        .unwrap()
        .parse_expr()
        .unwrap();
    let Expr::BinaryOp { left, .. } = again else {
        panic!()
    };
    assert!(matches!(left.as_ref(), Expr::Identifier(ident) if ident.value == "a]b"));
}
