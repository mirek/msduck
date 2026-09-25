//! Pure `FOR XML PATH` row serialization over already evaluated, typed values.
//!
//! This file is path-imported until the SQL crate's shared export is available.
//! A future binder supplies ordered aliases, declarations, row values and
//! directives. The root adapter still owns source execution, SQL binding,
//! XML/NText TDS encoding, DONE counts and transport. No expression is evaluated
//! here, and unsupported XML names or code units fail rather than being changed.

pub const TEXT_COLUMN_NAME: &str = "XML_F52E2B61-18A1-11d1-B105-00805F49916B";
const XSI_NAMESPACE: &str = "http://www.w3.org/2001/XMLSchema-instance";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Alias {
    /// `[@name]`, on the current row element.
    Attribute(String),
    /// `[name]` or `[parent/name]`, in original projection order.
    Element(Vec<String>),
    /// An unnamed scalar projection.
    Unnamed,
    /// `[text()]`.
    Text,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValueKind {
    Text,
    Integer,
    Binary,
}

pub struct Field {
    pub alias: Alias,
    pub kind: ValueKind,
}

/// Text remains UTF-16 until validated; an unpaired surrogate is never replaced
/// with U+FFFD. Integer and binary values are likewise explicitly typed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Value {
    Text(Vec<u16>),
    Integer(i64),
    Binary(Vec<u8>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RowName {
    Default,
    Named(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Directive {
    Root(String),
    Type,
    ElementsXsiNil,
    BinaryBase64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Context {
    Direct,
    /// A `FOR XML PATH(...), TYPE` scalar used as a named projection.
    Nested(String),
}

pub struct Plan<C> {
    pub row_name: RowName,
    pub directives: Vec<Directive>,
    pub context: Context,
    /// Explicit database collation; used only by untyped NText output.
    pub collation: C,
    pub fields: Vec<Field>,
}

/// Logical output requirements, independent of TDS token/flag encoding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Descriptor<C> {
    NText { name: &'static str, collation: C },
    Xml { name: String, nested: bool },
}

pub struct Serialized<C> {
    pub descriptor: Descriptor<C>,
    /// One combined XML value for nonempty input, no row for zero source rows.
    pub rows: Vec<Vec<u16>>,
    /// The root adapter uses this for the FOR XML completion row count.
    pub source_row_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    DuplicateRoot,
    DuplicateType,
    DuplicateDirective,
    AttributeAfterElement(String),
    InvalidName(String),
    UnsupportedEmptyRowNameAttribute,
    UnsupportedEmptyRowNameXsiNil,
    UnsupportedEmptySourceRoot,
    UnsupportedEmptySourceType,
    NestedRequiresType,
    RowArity {
        row: usize,
        expected: usize,
        actual: usize,
    },
    ValueKind {
        row: usize,
        column: usize,
    },
    InvalidUtf16 {
        row: usize,
        column: usize,
    },
    UnsupportedXmlCharacter {
        row: usize,
        column: usize,
    },
    BinaryBase64Required {
        row: usize,
        column: usize,
    },
}

impl Error {
    /// Only diagnostics established by the retained capture are mapped here.
    pub fn sql_diagnostic(&self) -> Option<(i32, u8, u8, String)> {
        match self {
            Self::DuplicateRoot | Self::DuplicateType => {
                Some((102, 1, 15, "Incorrect syntax near 'XML'.".into()))
            }
            Self::AttributeAfterElement(name) => Some((
                6852,
                1,
                16,
                format!(
                    "Attribute-centric column '@{name}' must not come after a non-attribute-centric sibling in XML hierarchy in FOR XML PATH."
                ),
            )),
            _ => None,
        }
    }
}

#[derive(Default)]
struct Options {
    root: Option<String>,
    typed: bool,
    xsi_nil: bool,
    base64: bool,
}

fn xml_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z' | b'A'..=b'Z' | b'_'))
        && bytes.all(
            |byte| matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b'.'),
        )
}

fn options<C>(plan: &Plan<C>) -> Result<Options, Error> {
    let mut options = Options::default();
    for directive in &plan.directives {
        match directive {
            Directive::Root(name) => {
                if options.root.is_some() {
                    return Err(Error::DuplicateRoot);
                }
                if !xml_name(name) {
                    return Err(Error::InvalidName(name.clone()));
                }
                options.root = Some(name.clone());
            }
            Directive::Type => {
                if options.typed {
                    return Err(Error::DuplicateType);
                }
                options.typed = true;
            }
            Directive::ElementsXsiNil => {
                if options.xsi_nil {
                    return Err(Error::DuplicateDirective);
                }
                options.xsi_nil = true;
            }
            Directive::BinaryBase64 => {
                if options.base64 {
                    return Err(Error::DuplicateDirective);
                }
                options.base64 = true;
            }
        }
    }
    let row_name = match &plan.row_name {
        RowName::Default => "row",
        RowName::Named(name) => name,
    };
    if !row_name.is_empty() && !xml_name(row_name) {
        return Err(Error::InvalidName(row_name.into()));
    }
    if let Context::Nested(name) = &plan.context {
        if !options.typed {
            return Err(Error::NestedRequiresType);
        }
        if !xml_name(name) {
            return Err(Error::InvalidName(name.clone()));
        }
    }
    let mut non_attribute = false;
    for field in &plan.fields {
        match &field.alias {
            Alias::Attribute(name) => {
                if !xml_name(name) {
                    return Err(Error::InvalidName(name.clone()));
                }
                if row_name.is_empty() {
                    return Err(Error::UnsupportedEmptyRowNameAttribute);
                }
                if non_attribute {
                    return Err(Error::AttributeAfterElement(name.clone()));
                }
            }
            Alias::Element(path) => {
                if path.is_empty() {
                    return Err(Error::InvalidName(String::new()));
                }
                for name in path {
                    if !xml_name(name) {
                        return Err(Error::InvalidName(name.clone()));
                    }
                }
                non_attribute = true;
            }
            Alias::Unnamed | Alias::Text => non_attribute = true,
        }
    }
    if row_name.is_empty() && options.xsi_nil {
        return Err(Error::UnsupportedEmptyRowNameXsiNil);
    }
    Ok(options)
}

fn valid_xml_character(ch: char) -> bool {
    matches!(ch as u32, 0x9 | 0xA | 0xD | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x10FFFF)
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        output.push(ALPHABET[(a >> 2) as usize] as char);
        output.push(ALPHABET[(((a & 0x03) << 4) | (b >> 4)) as usize] as char);
        output.push(if chunk.len() > 1 {
            ALPHABET[(((b & 0x0f) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            ALPHABET[(c & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    output
}

fn lexical(
    value: &Value,
    kind: ValueKind,
    options: &Options,
    row: usize,
    column: usize,
) -> Result<String, Error> {
    match (kind, value) {
        (ValueKind::Text, Value::Text(units)) => {
            let text =
                String::from_utf16(units).map_err(|_| Error::InvalidUtf16 { row, column })?;
            if !text.chars().all(valid_xml_character) {
                return Err(Error::UnsupportedXmlCharacter { row, column });
            }
            Ok(text)
        }
        (ValueKind::Integer, Value::Integer(value)) => Ok(value.to_string()),
        (ValueKind::Binary, Value::Binary(bytes)) if options.base64 => Ok(base64(bytes)),
        (ValueKind::Binary, Value::Binary(_)) => Err(Error::BinaryBase64Required { row, column }),
        _ => Err(Error::ValueKind { row, column }),
    }
}

fn escaped(text: &str, attribute: bool) -> String {
    let mut output = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' if attribute => output.push_str("&quot;"),
            _ => output.push(ch),
        }
    }
    output
}

enum Node {
    Element(Element),
    Text(String),
}

struct Element {
    name: String,
    attributes: Vec<(String, String)>,
    children: Vec<Node>,
}

impl Element {
    fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            attributes: Vec::new(),
            children: Vec::new(),
        }
    }

    fn render(&self, output: &mut String) {
        output.push('<');
        output.push_str(&self.name);
        for (name, value) in &self.attributes {
            output.push(' ');
            output.push_str(name);
            output.push_str("=\"");
            output.push_str(&escaped(value, true));
            output.push('"');
        }
        if self.children.is_empty() {
            output.push_str("/>");
            return;
        }
        output.push('>');
        for child in &self.children {
            child.render(output);
        }
        output.push_str("</");
        output.push_str(&self.name);
        output.push('>');
    }
}

impl Node {
    fn render(&self, output: &mut String) {
        match self {
            Self::Element(element) => element.render(output),
            Self::Text(text) => output.push_str(&escaped(text, false)),
        }
    }
}

fn append_element(children: &mut Vec<Node>, path: &[String], value: Option<String>, xsi_nil: bool) {
    let name = &path[0];
    if path.len() == 1 {
        let mut element = Element::new(name);
        if let Some(value) = value {
            element.children.push(Node::Text(value));
        } else if xsi_nil {
            element.attributes.push(("xsi:nil".into(), "true".into()));
        }
        children.push(Node::Element(element));
        return;
    }
    let parent = match children.last_mut() {
        Some(Node::Element(element)) if element.name == *name => element,
        _ => {
            children.push(Node::Element(Element::new(name)));
            let Some(Node::Element(element)) = children.last_mut() else {
                unreachable!()
            };
            element
        }
    };
    append_element(&mut parent.children, &path[1..], value, xsi_nil);
}

pub fn serialize<C>(plan: Plan<C>, rows: &[Vec<Option<Value>>]) -> Result<Serialized<C>, Error> {
    let options = options(&plan)?;
    if rows.is_empty() {
        if options.root.is_some() {
            return Err(Error::UnsupportedEmptySourceRoot);
        }
        if options.typed {
            return Err(Error::UnsupportedEmptySourceType);
        }
    }
    let row_name = match &plan.row_name {
        RowName::Default => "row",
        RowName::Named(name) => name.as_str(),
    };
    let mut xml = String::new();
    for (row_index, values) in rows.iter().enumerate() {
        if values.len() != plan.fields.len() {
            return Err(Error::RowArity {
                row: row_index,
                expected: plan.fields.len(),
                actual: values.len(),
            });
        }
        let mut element = Element::new(row_name);
        let mut has_xsi_nil = false;
        for (column_index, (field, value)) in plan.fields.iter().zip(values).enumerate() {
            let text = value
                .as_ref()
                .map(|value| lexical(value, field.kind, &options, row_index, column_index))
                .transpose()?;
            match &field.alias {
                Alias::Attribute(name) => {
                    if let Some(text) = text {
                        element.attributes.push((name.clone(), text));
                    }
                }
                Alias::Element(path) => {
                    if text.is_some() || options.xsi_nil {
                        if text.is_none() {
                            has_xsi_nil = true;
                        }
                        append_element(&mut element.children, path, text, options.xsi_nil);
                    }
                }
                Alias::Text | Alias::Unnamed => {
                    if let Some(text) = text {
                        element.children.push(Node::Text(text));
                    }
                }
            }
        }
        if has_xsi_nil {
            element
                .attributes
                .insert(0, ("xmlns:xsi".into(), XSI_NAMESPACE.into()));
        }
        if row_name.is_empty() {
            for child in &element.children {
                child.render(&mut xml);
            }
        } else {
            element.render(&mut xml);
        }
    }
    if let Some(root) = options.root {
        xml = format!("<{root}>{xml}</{root}>");
    }
    let descriptor = if options.typed {
        match plan.context {
            Context::Direct => Descriptor::Xml {
                name: String::new(),
                nested: false,
            },
            Context::Nested(name) => Descriptor::Xml { name, nested: true },
        }
    } else {
        Descriptor::NText {
            name: TEXT_COLUMN_NAME,
            collation: plan.collation,
        }
    };
    Ok(Serialized {
        descriptor,
        rows: if rows.is_empty() {
            Vec::new()
        } else {
            vec![xml.encode_utf16().collect()]
        },
        source_row_count: rows.len(),
    })
}
