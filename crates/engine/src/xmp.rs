//! Layout-preserving edits to darktable XMP sidecars. Only the rating
//! attribute and whole list elements (colour labels, tags) are rewritten;
//! history, masks and every other byte stay as darktable wrote them.
//! Anything that isn't laid out the way darktable/Exiv2 write it is refused
//! rather than guessed at.

use anyhow::{Context, Result, bail};

pub const LABELS: &str = "darktable:colorlabels";
pub const SUBJECT: &str = "dc:subject";
pub const HIERARCHY: &str = "lr:hierarchicalSubject";

const NAMESPACES: [(&str, &str); 4] = [
    ("xmp", "http://ns.adobe.com/xap/1.0/"),
    ("darktable", "http://darktable.sf.net/"),
    ("dc", "http://purl.org/dc/elements/1.1/"),
    ("lr", "http://ns.adobe.com/lightroom/1.0/"),
];
const RATING: &str = "xmp:Rating";
const END: &str = "</rdf:Description>";

pub struct Xmp {
    pub text: String,
}

impl Xmp {
    pub fn parse(text: String) -> Result<Self> {
        if text.matches("<rdf:Description").count() != 1 {
            bail!("expected exactly one rdf:Description");
        }
        let mut x = Self { text };
        let end = x.tag_end()?;
        if x.text[..end].ends_with('/') {
            x.text.replace_range(end - 1..=end, &format!(">\n  {END}"));
        }
        if x.text.matches(END).count() != 1 {
            bail!("expected exactly one {END}");
        }
        Ok(x)
    }

    /// A sidecar for an image darktable hasn't seen yet; darktable reads it on import.
    pub fn new(image_file: &str) -> Self {
        let ns: String = NAMESPACES.iter().map(|(p, u)| format!("\n    xmlns:{p}=\"{u}\"")).collect();
        let text = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"sift\">\n \
             <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n  \
             <rdf:Description rdf:about=\"\"\n    \
             xmlns:xmpMM=\"http://ns.adobe.com/xap/1.0/mm/\"{ns}\n   \
             xmpMM:DerivedFrom=\"{}\">\n  \
             {END}\n </rdf:RDF>\n</x:xmpmeta>\n",
            escape(image_file)
        );
        Self { text }
    }

    /// Index of the `>` that ends the rdf:Description start tag.
    fn tag_end(&self) -> Result<usize> {
        let start = self.text.find("<rdf:Description").context("no rdf:Description")?;
        let mut quote = None;
        for (i, c) in self.text[start..].char_indices() {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, '"' | '\'') => quote = Some(c),
                (None, '>') => return Ok(start + i),
                _ => {}
            }
        }
        bail!("unterminated rdf:Description")
    }

    /// Value range of `name="…"` in the Description start tag.
    fn attr(&self, name: &str) -> Result<Option<(usize, usize)>> {
        let start = self.text.find("<rdf:Description").context("no rdf:Description")?;
        let tag = &self.text[start..self.tag_end()?];
        let pat = format!("{name}=");
        let mut from = 0;
        while let Some(k) = tag[from..].find(&pat) {
            let at = from + k;
            from = at + pat.len();
            if !tag[..at].ends_with(char::is_whitespace) {
                continue;
            }
            if !tag[from..].starts_with('"') {
                bail!("{name} is not double-quoted");
            }
            let v0 = start + from + 1;
            let len = self.text[v0..].find('"').context("unterminated attribute")?;
            return Ok(Some((v0, v0 + len)));
        }
        if self.text.contains(&format!("<{name}>")) {
            bail!("{name} written as an element is not supported");
        }
        Ok(None)
    }

    fn ensure_ns(&mut self, prefix: &str) -> Result<()> {
        if self.text.contains(&format!("xmlns:{prefix}=")) {
            return Ok(());
        }
        let (_, uri) = NAMESPACES.iter().find(|(p, _)| *p == prefix).context("unknown namespace")?;
        let at = self.text.find("<rdf:Description").context("no rdf:Description")? + "<rdf:Description".len();
        self.text.insert_str(at, &format!("\n    xmlns:{prefix}=\"{uri}\""));
        Ok(())
    }

    pub fn rating(&self) -> Result<Option<i8>> {
        match self.attr(RATING)? {
            None => Ok(None),
            Some((a, b)) => Ok(Some(self.text[a..b].trim().parse().context("bad xmp:Rating")?)),
        }
    }

    pub fn set_rating(&mut self, r: i8) -> Result<()> {
        match self.attr(RATING)? {
            Some((a, b)) => self.text.replace_range(a..b, &r.to_string()),
            None => {
                self.ensure_ns("xmp")?;
                let end = self.tag_end()?;
                self.text.insert_str(end, &format!("\n   {RATING}=\"{r}\""));
            }
        }
        Ok(())
    }

    /// (line start, element end) of `<name>…</name>` or `<name/>` inside the Description.
    fn element(&self, name: &str) -> Result<Option<(usize, usize)>> {
        let body = self.tag_end()?;
        let end = self.text.find(END).context("no end of rdf:Description")?;
        let (open, empty, close) = (format!("<{name}>"), format!("<{name}/>"), format!("</{name}>"));
        let (at, stop) = if let Some(k) = self.text[body..end].find(&open) {
            let at = body + k;
            let c = self.text[at..end].find(&close).with_context(|| format!("unterminated {name}"))?;
            (at, at + c + close.len())
        } else if let Some(k) = self.text[body..end].find(&empty) {
            (body + k, body + k + empty.len())
        } else {
            return Ok(None);
        };
        let line = self.text[..at].rfind('\n').map_or(0, |n| n + 1);
        if !self.text[line..at].trim().is_empty() {
            bail!("{name} does not start its own line");
        }
        Ok(Some((line, stop)))
    }

    /// Items of a list element (raw, still XML-escaped), empty when absent.
    pub fn list(&self, name: &str) -> Result<Vec<String>> {
        let Some((a, b)) = self.element(name)? else { return Ok(vec![]) };
        let mut out = vec![];
        let mut rest = &self.text[a..b];
        while let Some(k) = rest.find("<rdf:li") {
            rest = &rest[k + "<rdf:li".len()..];
            let Some(inner) = rest.strip_prefix('>') else { bail!("{name} has an rdf:li with attributes") };
            let e = inner.find("</rdf:li>").with_context(|| format!("unterminated rdf:li in {name}"))?;
            out.push(inner[..e].to_string());
            rest = &inner[e..];
        }
        Ok(out)
    }

    /// Replace a list element (`container` is rdf:Seq or rdf:Bag); an empty
    /// list removes the element.
    pub fn set_list(&mut self, name: &str, container: &str, items: &[String]) -> Result<()> {
        let prefix = name.split(':').next().unwrap_or_default();
        let lis: String = items.iter().map(|i| format!("     <rdf:li>{i}</rdf:li>\n")).collect();
        let block = format!("   <{name}>\n    <{container}>\n{lis}    </{container}>\n   </{name}>");
        match self.element(name)? {
            Some((a, b)) if items.is_empty() => self.text.replace_range(a.saturating_sub(1)..b, ""),
            Some((a, b)) => self.text.replace_range(a..b, &block),
            None if items.is_empty() => {}
            None => {
                self.ensure_ns(prefix)?;
                let end = self.text.find(END).context("no end of rdf:Description")?;
                let at = self.text[..end].rfind('\n').context("rdf:Description end shares a line")?;
                self.text.insert_str(at, &format!("\n{block}"));
            }
        }
        Ok(())
    }
}

pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    const EDITED: &str = include_str!("../tests/fixtures/edited.CR3.xmp");
    const FRESH: &str = include_str!("../tests/fixtures/fresh.CR3.xmp");

    #[test]
    fn reads_darktable_sidecars() {
        let x = Xmp::parse(EDITED.into()).unwrap();
        assert_eq!(x.rating().unwrap(), Some(2));
        assert_eq!(x.list(HIERARCHY).unwrap(), ["darktable|changed", "darktable|format|cr3"]);
        assert_eq!(x.list(SUBJECT).unwrap(), ["changed", "cr3", "darktable", "format"]);
        assert!(x.list(LABELS).unwrap().is_empty());
    }

    #[test]
    fn untouched_lists_round_trip_byte_for_byte() {
        for src in [EDITED, FRESH] {
            let mut x = Xmp::parse(src.into()).unwrap();
            for (name, c) in [(SUBJECT, "rdf:Bag"), (HIERARCHY, "rdf:Bag"), (LABELS, "rdf:Seq")] {
                let items = x.list(name).unwrap();
                x.set_list(name, c, &items).unwrap();
            }
            let r = x.rating().unwrap().unwrap();
            x.set_rating(r).unwrap();
            assert_eq!(x.text, src);
        }
    }

    #[test]
    fn edits_only_what_it_is_asked_to() {
        let mut x = Xmp::parse(EDITED.into()).unwrap();
        x.set_rating(-1).unwrap();
        x.set_list(LABELS, "rdf:Seq", &["2".into()]).unwrap();
        let mut h = x.list(HIERARCHY).unwrap();
        h.push("sift|reason|eyes-closed".into());
        x.set_list(HIERARCHY, "rdf:Bag", &h).unwrap();

        let y = Xmp::parse(x.text.clone()).unwrap();
        assert_eq!(y.rating().unwrap(), Some(-1));
        assert_eq!(y.list(LABELS).unwrap(), ["2"]);
        assert_eq!(y.list(HIERARCHY).unwrap().last().unwrap(), "sift|reason|eyes-closed");
        // The edit history is untouched.
        let history = |t: &str| t[t.find("<darktable:history>").unwrap()..t.find("</darktable:history>").unwrap()].to_string();
        assert_eq!(history(&y.text), history(EDITED));

        // Undoing everything gives the original file back.
        let mut z = y;
        z.set_rating(2).unwrap();
        z.set_list(LABELS, "rdf:Seq", &[]).unwrap();
        h.pop();
        z.set_list(HIERARCHY, "rdf:Bag", &h).unwrap();
        assert_eq!(z.text, EDITED);
    }

    #[test]
    fn new_sidecar_takes_rating_labels_and_tags() {
        let mut x = Xmp::new("a.CR3");
        assert_eq!(x.rating().unwrap(), None);
        x.set_rating(3).unwrap();
        x.set_list(LABELS, "rdf:Seq", &["2".into()]).unwrap();
        x.set_list(HIERARCHY, "rdf:Bag", &["sift|reason|sharpest".into()]).unwrap();
        let y = Xmp::parse(x.text).unwrap();
        assert_eq!(y.rating().unwrap(), Some(3));
        assert_eq!(y.list(LABELS).unwrap(), ["2"]);
        assert_eq!(y.list(HIERARCHY).unwrap(), ["sift|reason|sharpest"]);
        assert!(y.text.contains("xmpMM:DerivedFrom=\"a.CR3\""));
    }

    #[test]
    fn refuses_layouts_it_does_not_know() {
        assert!(Xmp::parse("<x/>".into()).is_err());
        let two = FRESH.replace("</rdf:RDF>", "<rdf:Description rdf:about=\"\"/></rdf:RDF>");
        assert!(Xmp::parse(two).is_err());
    }
}
