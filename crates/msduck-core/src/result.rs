//! Logical result-column properties, independent of storage and wire encoding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Origin {
    #[default]
    Unknown,
    Expression,
    Stored,
    Identity,
    Derived,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Properties {
    /// None means inference was unavailable, never proof of NOT NULL.
    pub nullable: Option<bool>,
    pub origin: Origin,
}
impl Properties {
    pub const fn expression(nullable: bool) -> Self {
        Self {
            nullable: Some(nullable),
            origin: Origin::Expression,
        }
    }
    pub fn null_extend(&mut self) {
        self.nullable = Some(true);
    }
    pub fn union(self, other: Self) -> Self {
        Self {
            nullable: match (self.nullable, other.nullable) {
                (Some(true), _) | (_, Some(true)) => Some(true),
                (Some(false), Some(false)) => Some(false),
                _ => None,
            },
            origin: Origin::Derived,
        }
    }
}
