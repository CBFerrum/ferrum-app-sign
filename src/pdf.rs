//! PDF work for signing: checking an upload is a PDF that can be signed and measuring its pages,
//! stamping signers' entries onto the original pages, and appending the audit certificate as an
//! incremental update. The certificate prints the SHA-256 of everything before it — a byte-exact
//! prefix of the sealed file — so anyone holding the file can check the signed pages
//! (`head -c <bytes> sealed.pdf | sha256sum`) without trusting this server.
//!
//! Field positions are in points (1/72 inch) from the top-left corner of the page as displayed,
//! whatever its page box origin or `/Rotate`; every value is drawn inside its field's box.

use lopdf::xref::XrefType;
use lopdf::{Dictionary, Document, IncrementalDocument, LoadOptions, Object, ObjectId, Stream};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Largest PDF accepted (the platform's upload limit).
pub const MAX_BYTES: usize = 25 * 1024 * 1024;
pub const MAX_PAGES: usize = 1000;
/// Largest size one object or xref stream may inflate to while a PDF loads (decompression bombs).
const MAX_INFLATE: usize = 128 * 1024 * 1024;
/// Smallest font size a value is shrunk to before it no longer fits its box.
pub const MIN_FONT: f64 = 4.0;
const MAX_FONT: f64 = 12.0;
/// Helvetica's cap height, for centring text vertically in a box.
const CAP_HEIGHT: f64 = 0.718;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSize {
    pub width: f64,
    pub height: f64,
}

/// Opens a PDF that can be signed: `%PDF-` magic, a successful parse, no encryption.
pub fn load(bytes: &[u8]) -> Result<Document, String> {
    if bytes.len() > MAX_BYTES {
        return Err(format!("the PDF is larger than {} MB", MAX_BYTES >> 20));
    }
    if !bytes.starts_with(b"%PDF-") {
        return Err("the file is not a PDF".into());
    }
    let options = LoadOptions { max_decompressed_size: Some(MAX_INFLATE), ..Default::default() };
    let doc = Document::load_mem_with_options(bytes, options).map_err(|e| format!("the PDF cannot be read ({e})"))?;
    if doc.is_encrypted() || doc.was_encrypted() {
        return Err("password-protected PDFs cannot be signed; remove the protection first".into());
    }
    Ok(doc)
}

/// Checks that `bytes` is a PDF that can be signed and returns the size of each page as displayed.
pub fn inspect(bytes: &[u8]) -> Result<Vec<PageSize>, String> {
    let doc = load(bytes)?;
    let pages = doc.get_pages();
    if pages.is_empty() {
        return Err("the PDF has no pages".into());
    }
    if pages.len() > MAX_PAGES {
        return Err(format!("the PDF has more than {MAX_PAGES} pages"));
    }
    pages.iter().map(|(n, id)| geometry(&doc, *id).map(|g| g.size()).map_err(|e| format!("page {n}: {e}"))).collect()
}

// ───────────────────────────── page geometry ─────────────────────────────

/// A page's visible box in user space and its display rotation (0, 90, 180 or 270, clockwise).
#[derive(Debug, Clone, Copy)]
struct Geometry {
    llx: f64,
    lly: f64,
    urx: f64,
    ury: f64,
    rotate: i64,
}

impl Geometry {
    fn size(&self) -> PageSize {
        let (w, h) = (round2(self.urx - self.llx), round2(self.ury - self.lly));
        if self.rotate % 180 == 90 { PageSize { width: h, height: w } } else { PageSize { width: w, height: h } }
    }

    /// `cm` operands mapping display space (origin at the bottom-left of the page as shown, y up)
    /// to user space.
    fn ctm(&self) -> [f64; 6] {
        let Geometry { llx, lly, urx, ury, rotate } = *self;
        match rotate {
            90 => [0.0, 1.0, -1.0, 0.0, urx, lly],
            180 => [-1.0, 0.0, 0.0, -1.0, urx, ury],
            270 => [0.0, -1.0, 1.0, 0.0, llx, ury],
            _ => [1.0, 0.0, 0.0, 1.0, llx, lly],
        }
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// A page attribute, inherited from the page tree when the page does not set it.
fn inherited<'a>(doc: &'a Document, page: ObjectId, key: &[u8]) -> Option<&'a Object> {
    let mut node = doc.get_dictionary(page).ok()?;
    for _ in 0..64 {
        if let Ok(v) = node.get(key) {
            return Some(v);
        }
        node = doc.get_dictionary(node.get(b"Parent").ok()?.as_reference().ok()?).ok()?;
    }
    None
}

fn deref<'a>(doc: &'a Document, o: &'a Object) -> &'a Object {
    match o {
        Object::Reference(id) => doc.get_object(*id).unwrap_or(o),
        _ => o,
    }
}

fn number(doc: &Document, o: &Object) -> Option<f64> {
    match deref(doc, o) {
        Object::Integer(i) => Some(*i as f64),
        Object::Real(r) => Some(f64::from(*r)),
        _ => None,
    }
}

fn rect(doc: &Document, o: &Object) -> Option<[f64; 4]> {
    let a = deref(doc, o).as_array().ok()?;
    if a.len() != 4 {
        return None;
    }
    let v: Vec<f64> = a.iter().map(|x| number(doc, x)).collect::<Option<_>>()?;
    Some([v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3])])
}

fn geometry(doc: &Document, page: ObjectId) -> Result<Geometry, String> {
    let media = inherited(doc, page, b"MediaBox").and_then(|o| rect(doc, o)).unwrap_or([0.0, 0.0, 612.0, 792.0]);
    // The visible area is the crop box clipped to the media box.
    let b = match inherited(doc, page, b"CropBox").and_then(|o| rect(doc, o)) {
        Some(c) => [c[0].max(media[0]), c[1].max(media[1]), c[2].min(media[2]), c[3].min(media[3])],
        None => media,
    };
    if !(b.iter().all(|v| v.is_finite()) && b[2] - b[0] >= 1.0 && b[3] - b[1] >= 1.0) {
        return Err("the page has an empty page box".into());
    }
    let rotate = inherited(doc, page, b"Rotate").and_then(|o| deref(doc, o).as_i64().ok()).map(|r| r.rem_euclid(360)).filter(|r| r % 90 == 0).unwrap_or(0);
    Ok(Geometry { llx: b[0], lly: b[1], urx: b[2], ury: b[3], rotate })
}

// ───────────────────────────── text in a standard font ─────────────────────────────

/// Helvetica advance widths (1/1000 em) for WinAnsiEncoding codes 32..=255, from Adobe's AFM
/// (the oblique face shares them). Undefined codes are never produced by [`encode`].
#[rustfmt::skip]
const HELVETICA: [u16; 224] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278,
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556,
    1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778,
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556,
    333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556,
    556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584, 350,
    556, 350, 222, 556, 333, 1000, 556, 556, 333, 1000, 667, 333, 1000, 350, 611, 350,
    350, 222, 222, 333, 333, 350, 556, 1000, 333, 1000, 500, 333, 944, 350, 500, 667,
    278, 333, 556, 556, 556, 556, 260, 556, 333, 737, 370, 556, 584, 333, 737, 333,
    400, 584, 333, 333, 333, 556, 537, 278, 333, 333, 365, 556, 834, 834, 834, 611,
    667, 667, 667, 667, 667, 667, 1000, 722, 667, 667, 667, 667, 278, 278, 278, 278,
    722, 722, 778, 778, 778, 778, 778, 584, 778, 722, 722, 722, 722, 667, 667, 611,
    556, 556, 556, 556, 556, 556, 889, 500, 556, 556, 556, 556, 278, 278, 278, 278,
    556, 556, 556, 556, 556, 556, 556, 584, 611, 556, 556, 556, 556, 500, 556, 500,
];

/// Base letters of U+0100..=U+017F (Latin Extended-A), so Polish, Czech, Turkish, Hungarian...
/// names keep their letters when the standard fonts lack the accent.
const LATIN_EXT_A: &[u8; 128] =
    b"AaAaAaCcCcCcCcDdDdEeEeEeEeEeGgGgGgGgHhHhIiIiIiIiIiIiJjKkkLlLlLlLlLlNnNnNnnNnOoOoOoOoRrRrRrSsSsSsSsTtTtTtUuUuUuUuUuUuWwYyYZzZzZzs";

/// The WinAnsiEncoding byte that shows `c` in a standard font: Latin-1 and the Windows-1252
/// extras as themselves, Latin Extended-A letters as their base letter, anything else as `?`.
pub fn win_ansi(c: char) -> u8 {
    let cp = c as u32;
    match cp {
        0x20..=0x7E | 0xA0..=0xFF => cp as u8,
        _ => match c {
            '\t' => b' ',
            '€' => 128,
            '‚' => 130,
            'ƒ' => 131,
            '„' => 132,
            '…' => 133,
            '†' => 134,
            '‡' => 135,
            'ˆ' => 136,
            '‰' => 137,
            'Š' => 138,
            '‹' => 139,
            'Œ' => 140,
            'Ž' => 142,
            '‘' => 145,
            '’' => 146,
            '“' => 147,
            '”' => 148,
            '•' => 149,
            '–' => 150,
            '—' => 151,
            '˜' => 152,
            '™' => 153,
            'š' => 154,
            '›' => 155,
            'œ' => 156,
            'ž' => 158,
            'Ÿ' => 159,
            'Ș' => b'S',
            'ș' => b's',
            'Ț' => b'T',
            'ț' => b't',
            _ if (0x100..=0x17F).contains(&cp) => LATIN_EXT_A[(cp - 0x100) as usize],
            _ => b'?',
        },
    }
}

/// Text as WinAnsi bytes for a standard font (control characters dropped).
pub fn encode(text: &str) -> Vec<u8> {
    text.chars().filter(|c| !c.is_control() || *c == '\t').map(win_ansi).collect()
}

fn units(bytes: &[u8]) -> f64 {
    bytes.iter().map(|b| f64::from(HELVETICA.get(usize::from(*b).wrapping_sub(32)).copied().unwrap_or(556))).sum::<f64>() / 1000.0
}

/// Width of `text` in Helvetica at `size` points.
#[cfg(test)]
pub fn text_width(text: &str, size: f64) -> f64 {
    units(&encode(text)) * size
}

fn padding(w: f64) -> f64 {
    (w * 0.05).min(2.0)
}

/// The font size `text` is drawn at in a `w` × `h` box: up to 12 pt (or 75% of the box height),
/// shrunk to fit the width; `None` when it would need less than [`MIN_FONT`].
pub fn fit_text(text: &str, w: f64, h: f64) -> Option<f64> {
    let by_height = (h * 0.75).min(MAX_FONT);
    let u = units(&encode(text));
    let size = if u > 0.0 { by_height.min((w - 2.0 * padding(w)) / u) } else { by_height };
    (size >= MIN_FONT).then_some(size)
}

/// A number for a content stream: at most three decimals, no exponent.
fn num(v: f64) -> String {
    let s = format!("{:.3}", (v * 1000.0).round() / 1000.0);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() { "0".into() } else { s.to_string() }
}

fn font_dict(base: &str) -> Dictionary {
    let mut d = Dictionary::new();
    d.set("Type", Object::Name(b"Font".to_vec()));
    d.set("Subtype", Object::Name(b"Type1".to_vec()));
    d.set("BaseFont", Object::Name(base.as_bytes().to_vec()));
    d.set("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));
    d
}

fn compressed(dict: Dictionary, content: Vec<u8>) -> Stream {
    let mut s = Stream::new(dict, content);
    // Compression is an optimisation: an uncompressed stream is just as valid.
    let _ = s.compress();
    s
}

// ───────────────────────────── stamping ─────────────────────────────

/// A decoded image: 8-bit RGB samples and, when it has transparency, 8-bit alpha samples.
#[derive(Debug, Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
    pub alpha: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub enum Mark {
    /// A drawn signature or initials, fitted inside the box keeping its proportions.
    Image(Image),
    /// A typed signature or initials (oblique, ink blue).
    Typed(String),
    /// Plain text: a name, a date, a text field.
    Text(String),
    /// A ticked checkbox.
    Check,
}

/// One value to draw: `page` counts from 1; the box is in points from the top-left corner of
/// the page as displayed.
#[derive(Debug, Clone)]
pub struct Stamp {
    pub page: u32,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub mark: Mark,
}

/// One paragraph of the certificate.
#[derive(Debug, Clone)]
pub struct Para {
    pub text: String,
    pub size: f64,
    pub indent: f64,
    pub space_before: f64,
    pub muted: bool,
}

impl Para {
    pub fn new(text: impl Into<String>, size: f64) -> Para {
        Para { text: text.into(), size, indent: 0.0, space_before: 0.0, muted: false }
    }
    pub fn indent(mut self, v: f64) -> Para {
        self.indent = v;
        self
    }
    pub fn before(mut self, v: f64) -> Para {
        self.space_before = v;
        self
    }
    pub fn muted(mut self) -> Para {
        self.muted = true;
        self
    }
}

/// The sealed PDF: the signed document (the original pages with every value stamped) followed
/// by the certificate pages as an incremental update.
#[derive(Debug, Clone)]
pub struct Sealed {
    pub bytes: Vec<u8>,
    /// Length of the signed document, a prefix of `bytes`.
    pub signed_len: usize,
    /// SHA-256 (hex) of that prefix, as printed on the certificate.
    pub signed_sha256: String,
    pub pages: usize,
}

/// Stamps `stamps` onto `original`, then appends the certificate that `certificate` writes when
/// given the signed document's length and SHA-256; `footer` is printed on each certificate page.
pub fn seal(original: &[u8], stamps: &[Stamp], footer: &str, certificate: &dyn Fn(usize, &str) -> Vec<Para>) -> Result<Sealed, String> {
    let mut doc = load(original)?;
    let pages = doc.get_pages();
    let mut by_page: BTreeMap<u32, Vec<&Stamp>> = BTreeMap::new();
    for s in stamps {
        if !pages.contains_key(&s.page) {
            return Err(format!("page {} does not exist", s.page));
        }
        by_page.entry(s.page).or_default().push(s);
    }
    let fonts = (doc.add_object(font_dict("Helvetica")), doc.add_object(font_dict("Helvetica-Oblique")));
    for (n, list) in &by_page {
        stamp_page(&mut doc, pages[n], list, fonts).map_err(|e| format!("page {n}: {e}"))?;
    }
    let first = geometry(&doc, *pages.get(&1).ok_or("the PDF has no pages")?)?.size();
    // Written with a classic cross-reference table, so the certificate can follow as an
    // incremental update whatever the original used.
    doc.reference_table.cross_reference_type = XrefType::CrossReferenceTable;
    let mut signed = Vec::new();
    doc.save_to(&mut signed).map_err(|e| format!("the signed PDF could not be written ({e})"))?;
    let signed_len = signed.len();
    let signed_sha256 = hex::encode(Sha256::digest(&signed));
    let paras = certificate(signed_len, &signed_sha256);
    // The certificate uses the first page's size unless that is too small to read.
    let size = if first.width >= 300.0 && first.height >= 300.0 { first } else { PageSize { width: 595.28, height: 841.89 } };
    let bytes = append_certificate(signed, &paras, size, footer)?;
    let check = load(&bytes).map_err(|e| format!("the sealed PDF does not read back: {e}"))?;
    Ok(Sealed { pages: check.get_pages().len(), bytes, signed_len, signed_sha256 })
}

/// The page's resources as an owned dictionary (its own or inherited), with `Font` and `XObject`
/// owned too, so adding entries never changes objects other pages share.
fn own_resources(doc: &Document, page: ObjectId) -> Dictionary {
    let dict_of = |o: &Object| match deref(doc, o) {
        Object::Dictionary(d) => Some(d.clone()),
        _ => None,
    };
    let mut res = inherited(doc, page, b"Resources").and_then(dict_of).unwrap_or_default();
    for key in ["Font", "XObject"] {
        let sub = res.get(key.as_bytes()).ok().and_then(dict_of).unwrap_or_default();
        res.set(key, sub);
    }
    res
}

/// A resource name not used yet: `{base}{n}`.
fn fresh_name(dict: &Dictionary, base: &str) -> String {
    (0u32..).map(|i| format!("{base}{i}")).find(|n| !dict.has(n.as_bytes())).unwrap_or_else(|| base.to_string())
}

fn image_xobject(doc: &mut Document, img: &Image) -> ObjectId {
    let base = |space: &str| {
        let mut d = Dictionary::new();
        d.set("Type", Object::Name(b"XObject".to_vec()));
        d.set("Subtype", Object::Name(b"Image".to_vec()));
        d.set("Width", i64::from(img.width));
        d.set("Height", i64::from(img.height));
        d.set("ColorSpace", Object::Name(space.as_bytes().to_vec()));
        d.set("BitsPerComponent", 8i64);
        d
    };
    let mut dict = base("DeviceRGB");
    if let Some(alpha) = &img.alpha {
        let mask = doc.add_object(compressed(base("DeviceGray"), alpha.clone()));
        dict.set("SMask", mask);
    }
    doc.add_object(compressed(dict, img.rgb.clone()))
}

fn stamp_page(doc: &mut Document, page: ObjectId, stamps: &[&Stamp], fonts: (ObjectId, ObjectId)) -> Result<(), String> {
    let g = geometry(doc, page)?;
    let size = g.size();
    let mut res = own_resources(doc, page);
    let font_name = fresh_name(res.get(b"Font").and_then(Object::as_dict).map_err(|e| e.to_string())?, "FerrumSignF");
    let oblique_name = format!("{font_name}O");
    let mut images: Vec<(String, ObjectId)> = Vec::new();
    let c = g.ctm();
    // `Q` first: it balances the `q` put before the page's own content, so the stamps start
    // from the page's initial graphics state whatever that content left behind.
    let mut out = format!("Q\nq\n{} {} {} {} {} {} cm\n", num(c[0]), num(c[1]), num(c[2]), num(c[3]), num(c[4]), num(c[5]));
    for s in stamps {
        if !(s.width > 0.0 && s.height > 0.0 && s.x >= 0.0 && s.y >= 0.0) {
            return Err("a field box is empty or negative".into());
        }
        // Display space has its origin at the bottom-left: flip the top-left based box.
        let (x, y, w, h) = (s.x, size.height - s.y - s.height, s.width, s.height);
        out.push_str(&format!("q\n{} {} {} {} re W n\n", num(x), num(y), num(w), num(h)));
        match &s.mark {
            Mark::Image(img) => {
                if img.width == 0 || img.height == 0 {
                    return Err("an empty image".into());
                }
                let (iw, ih) = (f64::from(img.width), f64::from(img.height));
                let scale = (w / iw).min(h / ih);
                let (dw, dh) = (iw * scale, ih * scale);
                let name = format!("FerrumSignIm{}", images.len());
                let xobjects = res.get(b"XObject").and_then(Object::as_dict).map_err(|e| e.to_string())?;
                let name = if xobjects.has(name.as_bytes()) { fresh_name(xobjects, &format!("{name}_")) } else { name };
                images.push((name.clone(), image_xobject(doc, img)));
                out.push_str(&format!("{} 0 0 {} {} {} cm /{name} Do\n", num(dw), num(dh), num(x + (w - dw) / 2.0), num(y + (h - dh) / 2.0)));
            }
            Mark::Typed(text) | Mark::Text(text) => {
                let typed = matches!(s.mark, Mark::Typed(_));
                let (bh, color) = if typed { (h * 1.1, "0.08 0.2 0.55 rg") } else { (h, "0 g") };
                let size = fit_text(text, w, bh).unwrap_or(MIN_FONT);
                let baseline = y + (h - size * CAP_HEIGHT) / 2.0;
                let font = if typed { &oblique_name } else { &font_name };
                out.push_str(&format!(
                    "BT\n/{font} {} Tf {color}\n{} {} Td <{}> Tj\nET\n",
                    num(size),
                    num(x + padding(w)),
                    num(baseline),
                    hex::encode(encode(text))
                ));
            }
            Mark::Check => {
                let side = w.min(h);
                let (ox, oy) = (x + (w - side) / 2.0, y + (h - side) / 2.0);
                let p = |fx: f64, fy: f64| format!("{} {}", num(ox + fx * side), num(oy + fy * side));
                out.push_str(&format!("0 G {} w 1 J 1 j\n{} m {} l {} l S\n", num((side * 0.12).max(0.6)), p(0.18, 0.52), p(0.42, 0.24), p(0.84, 0.80)));
            }
        }
        out.push_str("Q\n");
    }
    out.push_str("Q\n");

    if let Ok(Object::Dictionary(fd)) = res.get_mut(b"Font") {
        fd.set(font_name.as_str(), fonts.0);
        fd.set(oblique_name.as_str(), fonts.1);
    }
    if let Ok(Object::Dictionary(xd)) = res.get_mut(b"XObject") {
        for (name, id) in &images {
            xd.set(name.as_str(), *id);
        }
    }
    let existing = doc.get_page_contents(page);
    let open = doc.add_object(Stream::new(Dictionary::new(), b"q\n".to_vec()));
    let stamp = doc.add_object(compressed(Dictionary::new(), out.into_bytes()));
    let mut contents = vec![Object::Reference(open)];
    contents.extend(existing.into_iter().map(Object::Reference));
    contents.push(Object::Reference(stamp));
    let page_dict = doc.get_dictionary_mut(page).map_err(|e| e.to_string())?;
    page_dict.set("Resources", res);
    page_dict.set("Contents", Object::Array(contents));
    Ok(())
}

// ───────────────────────────── the certificate ─────────────────────────────

/// One laid-out line: x, baseline, size, muted, WinAnsi text.
type Line = (f64, f64, f64, bool, Vec<u8>);

/// Splits `text` into lines no wider than `max` at `size`, breaking between words and inside
/// words longer than a line (hashes, user agents).
fn wrap(text: &[u8], size: f64, max: f64) -> Vec<Vec<u8>> {
    let fits = |b: &[u8]| units(b) * size <= max;
    let mut lines = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    for word in text.split(|b| *b == b' ').filter(|w| !w.is_empty()) {
        let mut candidate = cur.clone();
        if !candidate.is_empty() {
            candidate.push(b' ');
        }
        candidate.extend_from_slice(word);
        if fits(&candidate) {
            cur = candidate;
            continue;
        }
        if !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
        }
        let mut rest = word;
        while !fits(rest) {
            let mut n = rest.len();
            while n > 1 && !fits(&rest[..n]) {
                n -= 1;
            }
            lines.push(rest[..n].to_vec());
            rest = &rest[n..];
        }
        cur = rest.to_vec();
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

fn layout(paras: &[Para], size: PageSize) -> Vec<Vec<Line>> {
    let margin = 50.0;
    let bottom = margin + 24.0;
    let mut pages: Vec<Vec<Line>> = vec![Vec::new()];
    let mut y = size.height - margin;
    for p in paras {
        let leading = p.size * 1.35;
        let max = size.width - 2.0 * margin - p.indent;
        for (i, line) in wrap(&encode(&p.text), p.size, max).into_iter().enumerate() {
            let gap = if i == 0 { p.space_before } else { 0.0 };
            if y - gap - leading < bottom && pages.last().is_some_and(|pg| !pg.is_empty()) {
                pages.push(Vec::new());
                y = size.height - margin;
            } else {
                y -= gap;
            }
            y -= leading;
            if let Some(pg) = pages.last_mut() {
                pg.push((margin + p.indent, y + p.size * 0.25, p.size, p.muted, line));
            }
        }
    }
    pages
}

fn append_certificate(signed: Vec<u8>, paras: &[Para], size: PageSize, footer: &str) -> Result<Vec<u8>, String> {
    let fail = |e: lopdf::Error| format!("the certificate could not be added ({e})");
    let prev = Document::load_mem(&signed).map_err(fail)?;
    let root = prev.catalog().and_then(|c| c.get(b"Pages")).and_then(Object::as_reference).map_err(fail)?;
    let existing_pages = prev.get_pages().len();
    let kids_ref = prev.get_dictionary(root).and_then(|d| d.get(b"Kids")).and_then(Object::as_reference).ok();
    let mut inc = IncrementalDocument::create_from(signed, prev);
    inc.opt_clone_object_to_new_document(root).map_err(fail)?;
    if let Some(k) = kids_ref {
        inc.opt_clone_object_to_new_document(k).map_err(fail)?;
    }
    let font = inc.new_document.add_object(font_dict("Helvetica"));
    let laid = layout(paras, size);
    let total = laid.len();
    let mut kids = Vec::with_capacity(total);
    for (i, lines) in laid.into_iter().enumerate() {
        let mut content = String::new();
        let foot = encode(&format!("{footer} · certificate page {} of {total}", i + 1));
        for (x, y, sz, muted, text) in lines.iter().map(|l| (l.0, l.1, l.2, l.3, &l.4)).chain(std::iter::once((50.0, 40.0, 7.5, true, &foot))) {
            content.push_str(&format!("BT /F1 {} Tf {} g {} {} Td <{}> Tj ET\n", num(sz), if muted { "0.4" } else { "0" }, num(x), num(y), hex::encode(text)));
        }
        let contents = inc.new_document.add_object(compressed(Dictionary::new(), content.into_bytes()));
        let mut fonts = Dictionary::new();
        fonts.set("F1", font);
        let mut resources = Dictionary::new();
        resources.set("Font", fonts);
        let mut page = Dictionary::new();
        page.set("Type", Object::Name(b"Page".to_vec()));
        page.set("Parent", root);
        page.set("MediaBox", vec![Object::Integer(0), Object::Integer(0), Object::Real(size.width as f32), Object::Real(size.height as f32)]);
        page.set("Resources", resources);
        page.set("Contents", contents);
        kids.push(Object::Reference(inc.new_document.add_object(page)));
    }
    let added = kids.len();
    let kids_array = match kids_ref {
        Some(k) => inc.new_document.get_object_mut(k).and_then(Object::as_array_mut).map_err(fail)?,
        None => inc.new_document.get_dictionary_mut(root).and_then(|d| d.get_mut(b"Kids")).and_then(Object::as_array_mut).map_err(fail)?,
    };
    kids_array.extend(kids);
    inc.new_document.get_dictionary_mut(root).map_err(fail)?.set("Count", (existing_pages + added) as i64);
    let mut out = Vec::new();
    inc.save_to(&mut out).map_err(|e| format!("the sealed PDF could not be written ({e})"))?;
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A PDF with the given `(width, height, rotate)` pages: the first page's resources and media
    /// box are inherited from the page tree, its content leaves a scaled CTM behind (unbalanced
    /// `q`), and the file uses a cross-reference stream.
    pub(crate) fn sample(pages: &[(f64, f64, i64)]) -> Vec<u8> {
        let mut doc = Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let font = doc.add_object(font_dict("Helvetica"));
        let mut fonts = Dictionary::new();
        fonts.set("F1", font);
        let mut res = Dictionary::new();
        res.set("Font", fonts);
        let res_id = doc.add_object(res);
        let mut kids = vec![];
        for (i, (w, h, rot)) in pages.iter().enumerate() {
            let body = format!("q 2 0 0 2 0 0 cm BT /F1 12 Tf 36 36 Td (Page {}) Tj ET", i + 1);
            let content = doc.add_object(Stream::new(Dictionary::new(), body.into_bytes()));
            let mut page = Dictionary::new();
            page.set("Type", Object::Name(b"Page".to_vec()));
            page.set("Parent", pages_id);
            page.set("Contents", content);
            if i > 0 {
                page.set("MediaBox", vec![Object::Integer(0), Object::Integer(0), Object::Real(*w as f32), Object::Real(*h as f32)]);
                page.set("Resources", res_id);
            }
            if *rot != 0 {
                page.set("Rotate", *rot);
            }
            kids.push(Object::Reference(doc.add_object(page)));
        }
        let (w0, h0, _) = pages[0];
        let mut tree = Dictionary::new();
        tree.set("Type", Object::Name(b"Pages".to_vec()));
        tree.set("Count", pages.len() as i64);
        tree.set("Kids", kids);
        tree.set("Resources", res_id);
        tree.set("MediaBox", vec![Object::Integer(0), Object::Integer(0), Object::Real(w0 as f32), Object::Real(h0 as f32)]);
        doc.objects.insert(pages_id, Object::Dictionary(tree));
        let mut catalog = Dictionary::new();
        catalog.set("Type", Object::Name(b"Catalog".to_vec()));
        catalog.set("Pages", pages_id);
        let catalog_id = doc.add_object(catalog);
        doc.trailer.set("Root", catalog_id);
        doc.reference_table.cross_reference_type = XrefType::CrossReferenceStream;
        let mut out = Vec::new();
        doc.save_to(&mut out).unwrap();
        out
    }

    fn page_text(doc: &Document, page: u32) -> String {
        let id = doc.get_pages()[&page];
        String::from_utf8_lossy(&doc.get_page_content(id)).to_string()
    }

    #[test]
    fn inspects_pages_as_displayed() {
        let pdf = sample(&[(612.0, 792.0, 0), (595.0, 842.0, 90), (300.0, 200.0, -90)]);
        let sizes = inspect(&pdf).unwrap();
        assert_eq!(sizes, vec![PageSize { width: 612.0, height: 792.0 }, PageSize { width: 842.0, height: 595.0 }, PageSize { width: 200.0, height: 300.0 }]);
        assert!(inspect(b"%PDF-1.7\nnot really").is_err());
        assert!(inspect(b"PK\x03\x04 a zip file").unwrap_err().contains("not a PDF"));
        let mut cut = pdf.clone();
        cut.truncate(pdf.len() / 3);
        assert!(inspect(&cut).is_err(), "a truncated PDF is refused");
    }

    #[test]
    fn rotation_maps_display_space_to_user_space() {
        let apply = |m: [f64; 6], (x, y): (f64, f64)| (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]);
        // A portrait box shown rotated 90° clockwise is 792 wide and 612 tall: its displayed
        // bottom-left corner is the box's bottom-right... in user space terms (612, 0) rotated.
        let g = Geometry { llx: 0.0, lly: 0.0, urx: 612.0, ury: 792.0, rotate: 90 };
        assert_eq!(g.size(), PageSize { width: 792.0, height: 612.0 });
        assert_eq!(apply(g.ctm(), (0.0, 0.0)), (612.0, 0.0));
        assert_eq!(apply(g.ctm(), (792.0, 612.0)), (0.0, 792.0));
        let g = Geometry { llx: 10.0, lly: 20.0, urx: 110.0, ury: 220.0, rotate: 180 };
        assert_eq!(apply(g.ctm(), (0.0, 0.0)), (110.0, 220.0));
        let g = Geometry { llx: 0.0, lly: 0.0, urx: 612.0, ury: 792.0, rotate: 270 };
        assert_eq!(apply(g.ctm(), (0.0, 0.0)), (0.0, 792.0));
        assert_eq!(apply(g.ctm(), (792.0, 612.0)), (612.0, 0.0));
        let g = Geometry { llx: 5.0, lly: 7.0, urx: 100.0, ury: 100.0, rotate: 0 };
        assert_eq!(apply(g.ctm(), (1.0, 1.0)), (6.0, 8.0));
    }

    #[test]
    fn text_is_measured_encoded_and_fitted() {
        assert_eq!(text_width("Hi", 10.0), (722.0 + 222.0) / 100.0);
        assert_eq!(encode("Zoë – Łukasz Dvořák €5"), b"Zo\xeb \x96 Lukasz Dvor\xe1k \x805".to_vec());
        assert_eq!(encode("日本"), b"??".to_vec());
        assert_eq!(fit_text("Jane Doe", 200.0, 30.0), Some(12.0));
        let small = fit_text("A rather long company name, Inc.", 80.0, 30.0).unwrap();
        assert!(small < 12.0 && text_width("A rather long company name, Inc.", small) <= 80.0 - 2.0 * padding(80.0) + 1e-9);
        assert_eq!(fit_text(&"W".repeat(200), 50.0, 20.0), None);
        assert_eq!(num(1.0), "1");
        assert_eq!(num(-0.0001), "0");
        assert_eq!(num(12.3456), "12.346");
        let lines = wrap(&encode(&format!("SHA-256 {}", "a".repeat(200))), 8.0, 200.0);
        assert!(lines.len() > 2 && lines.iter().all(|l| units(l) * 8.0 <= 200.0));
    }

    #[test]
    fn seals_stamps_and_certificate_as_an_incremental_update() {
        let original = sample(&[(612.0, 792.0, 0), (595.0, 842.0, 90)]);
        let img = Image { width: 4, height: 2, rgb: vec![10; 24], alpha: Some(vec![255, 0, 255, 0, 255, 0, 255, 0]) };
        let stamps = vec![
            Stamp { page: 1, x: 72.0, y: 600.0, width: 200.0, height: 50.0, mark: Mark::Image(img) },
            Stamp { page: 1, x: 72.0, y: 660.0, width: 200.0, height: 20.0, mark: Mark::Text("Jane Doe".into()) },
            Stamp { page: 2, x: 400.0, y: 500.0, width: 150.0, height: 40.0, mark: Mark::Typed("Ravi Kumar".into()) },
            Stamp { page: 2, x: 20.0, y: 20.0, width: 12.0, height: 12.0, mark: Mark::Check },
        ];
        let long: Vec<Para> = (0..120).map(|i| Para::new(format!("Event {i}: viewed by someone"), 9.0)).collect();
        let sealed = seal(&original, &stamps, "SIGN-2026-00001", &|len, sha| {
            let mut p = vec![Para::new("Certificate of Completion", 18.0), Para::new(format!("Signed document: bytes 0-{len}, SHA-256 {sha}"), 9.0)];
            p.extend(long.clone());
            p
        })
        .unwrap();
        let doc = load(&sealed.bytes).unwrap();
        assert!(sealed.pages >= 4, "two pages and a certificate that needed more than one page");
        assert_eq!(doc.get_pages().len(), sealed.pages);
        assert_eq!(hex::encode(Sha256::digest(&sealed.bytes[..sealed.signed_len])), sealed.signed_sha256, "the signed document is a prefix");
        let signed = load(&sealed.bytes[..sealed.signed_len]).unwrap();
        assert_eq!(signed.get_pages().len(), 2, "the prefix alone is the stamped original");
        let p1 = page_text(&doc, 1);
        assert!(p1.starts_with("q\n") && p1.contains("(Page 1) Tj") && p1.contains("/FerrumSignIm0 Do") && p1.contains("<4a616e6520446f65> Tj"));
        assert!(p1.contains("Q\nq\n1 0 0 1 0 0 cm"), "{p1}");
        let p2 = page_text(&doc, 2);
        assert!(p2.contains("0 1 -1 0 595 0 cm"), "rotated page maps display space: {p2}");
        assert!(p2.contains("/FerrumSignF0O"), "typed signature in the oblique face");
        let cert = page_text(&doc, 3);
        assert!(cert.contains(&hex::encode(b"Certificate of Completion")));
        assert!(page_text(&doc, sealed.pages as u32).contains(&hex::encode(encode(&format!("certificate page {} of {}", sealed.pages - 2, sealed.pages - 2)))));
        // The second page's shared resources were copied, not changed.
        let first_res = own_resources(&doc, doc.get_pages()[&1]);
        assert!(first_res.get(b"Font").unwrap().as_dict().unwrap().has(b"F1"), "original fonts kept");
        assert!(seal(&original, &[Stamp { page: 9, x: 0.0, y: 0.0, width: 1.0, height: 1.0, mark: Mark::Check }], "x", &|_, _| vec![]).is_err());
    }
}
