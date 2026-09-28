//! Element tree of an XML part: local names and attributes, no text content.

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

pub(super) struct Node {
    /// Local name, without namespace prefix.
    pub name: String,
    /// Local attribute names and unescaped values.
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
}

impl Node {
    pub(super) fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// This node and all below it, depth-first in document order.
    pub(super) fn descendants(&self) -> Vec<&Node> {
        let mut out = vec![self];
        for c in &self.children {
            out.extend(c.descendants());
        }
        out
    }
}

fn node(e: &BytesStart) -> Result<Node, String> {
    let name = e.local_name().into_inner().to_string();
    let attrs = e
        .attributes()
        .map(|a| {
            let a = a.map_err(|e| e.to_string())?;
            let value = a.normalized_value(XmlVersion::Implicit1_0).map_err(|e| e.to_string())?;
            Ok((a.key.local_name().into_inner().to_string(), value.into_owned()))
        })
        .collect::<Result<_, String>>()?;
    Ok(Node { name, attrs, children: Vec::new() })
}

/// Parses `data` (UTF-8, optionally with a BOM) into its root element.
pub(super) fn parse(data: &[u8]) -> Result<Node, String> {
    let text = std::str::from_utf8(data.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(data)).map_err(|_| "XML part is not UTF-8")?;
    let mut reader = Reader::from_str(text);
    let bad = |e: quick_xml::Error| format!("Malformed XML: {e}");
    let mut stack: Vec<Node> = vec![Node { name: String::new(), attrs: Vec::new(), children: Vec::new() }];
    loop {
        match reader.read_event().map_err(bad)? {
            Event::Start(e) => stack.push(node(&e)?),
            Event::Empty(e) => {
                let n = node(&e)?;
                stack.last_mut().unwrap().children.push(n);
            }
            Event::End(_) => {
                let n = stack.pop().unwrap();
                stack.last_mut().ok_or("Malformed XML: unbalanced end tag")?.children.push(n);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let mut doc = stack.pop().filter(|_| stack.is_empty()).ok_or("Malformed XML: unclosed element")?;
    doc.children.pop().ok_or_else(|| "Empty XML part".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_with_namespaces() {
        let doc = parse(b"\xEF\xBB\xBF<?xml version='1.0'?><a:Root x:id='1'><Child v='a &amp; b'/><Child><Leaf/></Child></a:Root>").unwrap();
        assert_eq!(doc.name, "Root");
        assert_eq!(doc.attr("id"), Some("1"));
        assert_eq!(doc.children[0].attr("v"), Some("a & b"));
        let names: Vec<_> = doc.descendants().iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["Root", "Child", "Child", "Leaf"]);
    }
}
