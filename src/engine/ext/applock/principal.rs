//! Database principals that name an application lock's namespace.
//!
//! msduck has no database users or roles beyond those every SQL Server
//! database contains, and every login acts as `dbo`, which is a member of
//! each of them. So exactly these principals are accepted; any other name
//! fails with 1202 as an unknown principal does in SQL Server.
const FIXED: [&str; 14] = [
    "public",
    "dbo",
    "guest",
    "INFORMATION_SCHEMA",
    "sys",
    "db_owner",
    "db_accessadmin",
    "db_securityadmin",
    "db_ddladmin",
    "db_backupoperator",
    "db_datareader",
    "db_datawriter",
    "db_denydatareader",
    "db_denydatawriter",
];

/// The principal's catalog name (names compare case-insensitively).
pub(super) fn canonical(name: &str) -> Option<&'static str> {
    FIXED
        .into_iter()
        .find(|fixed| fixed.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    #[test]
    fn fixed_principals_resolve_case_insensitively() {
        assert_eq!(super::canonical("DBO"), Some("dbo"));
        assert_eq!(
            super::canonical("information_schema"),
            Some("INFORMATION_SCHEMA")
        );
        assert_eq!(super::canonical(""), None);
        assert_eq!(super::canonical("nobody"), None);
    }
}
