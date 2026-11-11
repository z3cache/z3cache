//! Streaming command splitter and canonical form (DESIGN section 5).
//!
//! The splitter finds the boundaries of top-level commands in a byte stream
//! that may arrive in arbitrary pieces. It understands nested parentheses,
//! string literals (`""` is the only escape), `|quoted symbols|` and `;`
//! comments. Each complete command is returned with its raw bytes (for
//! forwarding unchanged) and its canonical form (for hashing and replay).

/// One element of a command list, in canonical form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// A symbol, keyword, numeral, string literal or quoted symbol, byte-exact.
    Atom(Vec<u8>),
    /// A parenthesised list, canonicalised.
    List(Vec<u8>),
}

impl Item {
    pub fn bytes(&self) -> &[u8] {
        match self {
            Item::Atom(b) | Item::List(b) => b,
        }
    }

    pub fn atom(&self) -> Option<&[u8]> {
        match self {
            Item::Atom(b) => Some(b),
            Item::List(_) => None,
        }
    }
}

/// A complete top-level command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// The client's bytes since the end of the previous command, up to and
    /// including this command's closing parenthesis. Leading whitespace and
    /// comments are included so that forwarding `raw` reproduces the input
    /// stream exactly.
    pub raw: Vec<u8>,
    /// Offset in `raw` where the command itself starts, after the leading
    /// whitespace and comments.
    pub text_start: usize,
    /// Tokens joined by single spaces, with no space after `(` or before
    /// `)`, and comments dropped. String literals and quoted symbols are kept
    /// byte-exact.
    pub canonical: Vec<u8>,
    /// The elements of the command list. `items[0]` is normally the command
    /// name.
    pub items: Vec<Item>,
}

impl Command {
    /// The command exactly as the client wrote it, from its opening to its
    /// closing parenthesis.
    pub fn text(&self) -> &[u8] {
        &self.raw[self.text_start..]
    }

    /// The command name, if the first element is an atom.
    pub fn head(&self) -> Option<&[u8]> {
        self.items.first()?.atom()
    }

    /// The elements after the command name.
    pub fn args(&self) -> &[Item] {
        self.items.get(1..).unwrap_or(&[])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub &'static str);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lex {
    Normal,
    Comment,
    Str,
    /// Saw a `"` inside a string; the next byte decides whether it was an
    /// escaped quote or the end of the string.
    StrQuote,
    QSym,
}

/// Incremental splitter. Feed it bytes, then drain commands with
/// [`Splitter::next_command`] until it returns `None`.
pub struct Splitter {
    buf: Vec<u8>,
    /// Bytes before this index belong to commands already returned.
    consumed: usize,
    /// Start of the command being scanned, when `depth > 0`.
    cmd_start: usize,
    /// Next byte to scan.
    pos: usize,
    depth: usize,
    lex: Lex,
    failed: bool,
}

impl Default for Splitter {
    fn default() -> Self {
        Self::new()
    }
}

impl Splitter {
    pub fn new() -> Self {
        Splitter {
            buf: Vec::new(),
            consumed: 0,
            cmd_start: 0,
            pos: 0,
            depth: 0,
            lex: Lex::Normal,
            failed: false,
        }
    }

    pub fn feed(&mut self, data: &[u8]) {
        if self.consumed > 0 {
            let c = self.consumed;
            self.buf.drain(..c);
            self.pos -= c;
            self.cmd_start = self.cmd_start.saturating_sub(c);
            self.consumed = 0;
        }
        self.buf.extend_from_slice(data);
    }

    /// Returns the next complete command, `None` if more input is needed, or
    /// an error once if the input cannot be split. After an error the
    /// splitter returns `None` forever; use [`Splitter::take_remaining`] to
    /// recover the unprocessed bytes.
    pub fn next_command(&mut self) -> Option<Result<Command, ParseError>> {
        if self.failed {
            return None;
        }
        while self.pos < self.buf.len() {
            let b = self.buf[self.pos];
            match self.lex {
                Lex::Comment => {
                    if b == b'\n' {
                        self.lex = Lex::Normal;
                    }
                }
                Lex::Str => {
                    if b == b'"' {
                        self.lex = Lex::StrQuote;
                    }
                }
                Lex::StrQuote => {
                    if b == b'"' {
                        self.lex = Lex::Str;
                    } else {
                        // The string ended at the previous byte; rescan this
                        // one as normal input.
                        self.lex = Lex::Normal;
                        continue;
                    }
                }
                Lex::QSym => {
                    if b == b'|' {
                        self.lex = Lex::Normal;
                    }
                }
                Lex::Normal => match b {
                    b';' => self.lex = Lex::Comment,
                    b'(' => {
                        if self.depth == 0 {
                            self.cmd_start = self.pos;
                        }
                        self.depth += 1;
                    }
                    b')' => {
                        if self.depth == 0 {
                            return Some(self.fail("unbalanced ')'"));
                        }
                        self.depth -= 1;
                        if self.depth == 0 {
                            let end = self.pos + 1;
                            self.pos = end;
                            return Some(match tokenize(&self.buf[self.cmd_start..end]) {
                                Ok((canonical, items)) => {
                                    let raw = self.buf[self.consumed..end].to_vec();
                                    let text_start = self.cmd_start - self.consumed;
                                    self.consumed = end;
                                    Ok(Command {
                                        raw,
                                        text_start,
                                        canonical,
                                        items,
                                    })
                                }
                                Err(e) => self.fail(e.0),
                            });
                        }
                    }
                    _ if self.depth == 0 && !b.is_ascii_whitespace() => {
                        return Some(self.fail("text outside a command"));
                    }
                    b'"' => self.lex = Lex::Str,
                    b'|' => self.lex = Lex::QSym,
                    _ => {}
                },
            }
            self.pos += 1;
        }
        None
    }

    fn fail(&mut self, reason: &'static str) -> Result<Command, ParseError> {
        self.failed = true;
        Err(ParseError(reason))
    }

    /// True if a command has started but not finished.
    pub fn in_command(&self) -> bool {
        self.depth > 0
    }

    /// Removes and returns every byte not yet returned as part of a command:
    /// trailing whitespace and comments, an incomplete command, or the input
    /// from the point where splitting failed.
    pub fn take_remaining(&mut self) -> Vec<u8> {
        let rest = self.buf.split_off(self.consumed);
        self.buf.clear();
        self.consumed = 0;
        self.cmd_start = 0;
        self.pos = 0;
        self.depth = 0;
        self.lex = Lex::Normal;
        rest
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok {
    Open,
    Close,
    Text,
}

/// Tokenizes one complete, lexically balanced command (starting with `(`
/// and ending with its matching `)`).
fn tokenize(src: &[u8]) -> Result<(Vec<u8>, Vec<Item>), ParseError> {
    let mut canonical = Vec::with_capacity(src.len());
    let mut items = Vec::new();
    let mut depth = 0usize;
    let mut prev: Option<Tok> = None;
    let mut separated = true;
    let mut list_start = 0usize;
    let mut i = 0usize;
    while i < src.len() {
        let b = src[i];
        if b.is_ascii_whitespace() {
            separated = true;
            i += 1;
            continue;
        }
        if b == b';' {
            while i < src.len() && src[i] != b'\n' {
                i += 1;
            }
            separated = true;
            continue;
        }
        let (tok, end) = match b {
            b'(' => (Tok::Open, i + 1),
            b')' => (Tok::Close, i + 1),
            b'"' => (Tok::Text, string_end(src, i)),
            b'|' => (Tok::Text, qsym_end(src, i)),
            _ => (Tok::Text, atom_end(src, i)),
        };
        if tok == Tok::Text && prev == Some(Tok::Text) && !separated {
            // For example `abc"def"`: the solver's reading of this is not
            // obvious, so do not guess at it.
            return Err(ParseError("tokens not separated by whitespace"));
        }
        if let Some(p) = prev
            && p != Tok::Open
            && tok != Tok::Close
        {
            canonical.push(b' ');
        }
        match tok {
            Tok::Open => {
                depth += 1;
                if depth == 2 {
                    list_start = canonical.len();
                }
                canonical.push(b'(');
            }
            Tok::Close => {
                canonical.push(b')');
                if depth == 2 {
                    items.push(Item::List(canonical[list_start..].to_vec()));
                }
                depth -= 1;
            }
            Tok::Text => {
                canonical.extend_from_slice(&src[i..end]);
                if depth == 1 {
                    items.push(Item::Atom(src[i..end].to_vec()));
                }
            }
        }
        prev = Some(tok);
        separated = false;
        i = end;
    }
    Ok((canonical, items))
}

fn string_end(src: &[u8], start: usize) -> usize {
    let mut j = start + 1;
    while j < src.len() {
        if src[j] == b'"' {
            if src.get(j + 1) == Some(&b'"') {
                j += 2;
                continue;
            }
            return j + 1;
        }
        j += 1;
    }
    src.len()
}

fn qsym_end(src: &[u8], start: usize) -> usize {
    match src[start + 1..].iter().position(|&c| c == b'|') {
        Some(p) => start + 1 + p + 1,
        None => src.len(),
    }
}

fn atom_end(src: &[u8], start: usize) -> usize {
    let mut j = start;
    while j < src.len() {
        let c = src[j];
        if c.is_ascii_whitespace() || matches!(c, b'(' | b')' | b'"' | b'|' | b';') {
            break;
        }
        j += 1;
    }
    j
}

/// The contents of a string literal token, with the surrounding quotes
/// removed and `""` unescaped. `None` if `tok` is not a string literal.
pub fn string_contents(tok: &[u8]) -> Option<Vec<u8>> {
    if tok.len() < 2 || tok[0] != b'"' || tok[tok.len() - 1] != b'"' {
        return None;
    }
    let inner = &tok[1..tok.len() - 1];
    let mut out = Vec::with_capacity(inner.len());
    let mut i = 0;
    while i < inner.len() {
        out.push(inner[i]);
        if inner[i] == b'"' {
            i += 1; // skip the second quote of the escape
        }
        i += 1;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split_all(chunks: &[&[u8]]) -> (Vec<Command>, Option<ParseError>, Vec<u8>) {
        let mut s = Splitter::new();
        let mut cmds = Vec::new();
        for c in chunks {
            s.feed(c);
            while let Some(r) = s.next_command() {
                match r {
                    Ok(c) => cmds.push(c),
                    Err(e) => return (cmds, Some(e), s.take_remaining()),
                }
            }
        }
        let rest = s.take_remaining();
        (cmds, None, rest)
    }

    fn canon(input: &str) -> Vec<String> {
        let (cmds, err, _) = split_all(&[input.as_bytes()]);
        assert_eq!(err, None);
        cmds.iter()
            .map(|c| String::from_utf8(c.canonical.clone()).unwrap())
            .collect()
    }

    #[test]
    fn canonical_form_collapses_whitespace_and_drops_comments() {
        assert_eq!(
            canon("  (assert\n  (>   x ; comment )\n 1 ) )\n(check-sat)"),
            vec!["(assert (> x 1))", "(check-sat)"]
        );
        assert_eq!(canon("(a(b)(c))"), vec!["(a (b) (c))"]);
        assert_eq!(canon("(push)(pop 1)"), vec!["(push)", "(pop 1)"]);
    }

    #[test]
    fn strings_and_quoted_symbols_are_byte_exact() {
        assert_eq!(
            canon("(echo \"a  ( ; ) \"\"q\"\" b\")"),
            vec!["(echo \"a  ( ; ) \"\"q\"\" b\")"]
        );
        assert_eq!(
            canon("(declare-const |x  ) ;\n y| Int)"),
            vec!["(declare-const |x  ) ;\n y| Int)"]
        );
    }

    #[test]
    fn items_are_top_level_elements() {
        let (cmds, _, _) = split_all(&[b"(set-option :rlimit  0)(assert (and a\n b))"]);
        assert_eq!(
            cmds[0].items,
            vec![
                Item::Atom(b"set-option".to_vec()),
                Item::Atom(b":rlimit".to_vec()),
                Item::Atom(b"0".to_vec()),
            ]
        );
        assert_eq!(cmds[1].head(), Some(&b"assert"[..]));
        assert_eq!(cmds[1].args(), &[Item::List(b"(and a b)".to_vec())]);
    }

    #[test]
    fn raw_bytes_reproduce_the_input() {
        let input = b"; header\n(set-logic ALL)  ; trailing\n(check-sat)\n\n";
        let (cmds, err, rest) = split_all(&[input]);
        assert_eq!(err, None);
        let mut joined: Vec<u8> = cmds.iter().flat_map(|c| c.raw.clone()).collect();
        joined.extend(rest);
        assert_eq!(joined, input);
        assert_eq!(cmds[0].raw, b"; header\n(set-logic ALL)");
    }

    #[test]
    fn every_split_point_gives_the_same_commands() {
        let input: &[u8] = b"(declare-fun f (Int) Int) ; c (\n(echo \"x\"\"y\")\
            (assert (= |a b| \"s;\"))\n(check-sat)\n";
        let (whole, err, _) = split_all(&[input]);
        assert_eq!(err, None);
        assert_eq!(whole.len(), 4);
        for i in 0..=input.len() {
            for j in i..=input.len() {
                let (parts, err, _) = split_all(&[&input[..i], &input[i..j], &input[j..]]);
                assert_eq!(err, None, "split at {i},{j}");
                assert_eq!(parts, whole, "split at {i},{j}");
            }
        }
    }

    #[test]
    fn string_ending_at_chunk_boundary() {
        let (cmds, err, _) = split_all(&[b"(echo \"ab\"", b"\"c\")"]);
        assert_eq!(err, None);
        assert_eq!(cmds[0].canonical, b"(echo \"ab\"\"c\")");
    }

    #[test]
    fn errors() {
        let (_, err, rest) = split_all(&[b"(push) )(pop)"]);
        assert_eq!(err, Some(ParseError("unbalanced ')'")));
        assert_eq!(rest, b" )(pop)");

        let (_, err, rest) = split_all(&[b"(push)\nfoo"]);
        assert_eq!(err, Some(ParseError("text outside a command")));
        assert_eq!(rest, b"\nfoo");

        let (_, err, rest) = split_all(&[b"(echo abc\"def\")"]);
        assert_eq!(err, Some(ParseError("tokens not separated by whitespace")));
        assert_eq!(rest, b"(echo abc\"def\")");
    }

    #[test]
    fn incomplete_command_is_remaining() {
        let mut s = Splitter::new();
        s.feed(b"(push)\n(assert (> x");
        assert!(s.next_command().unwrap().is_ok());
        assert!(s.next_command().is_none());
        assert!(s.in_command());
        assert_eq!(s.take_remaining(), b"\n(assert (> x");
    }

    #[test]
    fn text_is_the_command_as_written() {
        let (cmds, _, _) = split_all(&[b"; c (\n  (get-value ((+  x\n 1)))"]);
        assert_eq!(cmds[0].text(), b"(get-value ((+  x\n 1)))");
        assert_eq!(cmds[0].canonical, b"(get-value ((+ x 1)))");
    }

    #[test]
    fn string_contents_unescapes() {
        assert_eq!(string_contents(b"\"a\"\"b\""), Some(b"a\"b".to_vec()));
        assert_eq!(string_contents(b"\"\""), Some(Vec::new()));
        assert_eq!(string_contents(b"abc"), None);
    }

    // Property tests: commands generated from fixed seeds, each written out
    // with random whitespace and comments.

    use crate::TestRng;

    /// A generated s-expression.
    enum Sx {
        Atom(String),
        List(Vec<Sx>),
    }

    fn gen_atom(rng: &mut TestRng) -> String {
        match rng.below(6) {
            0 => rng
                .pick(&[
                    "assert",
                    "check-sat",
                    "push",
                    "x",
                    "f%1",
                    "a.b!c",
                    "+",
                    "=>",
                    "<=",
                    "_",
                ])
                .to_string(),
            1 => format!(
                ":{}",
                rng.pick(&["rlimit", "pattern", "qid", "all-statistics"])
            ),
            2 => rng.below(100_000).to_string(),
            3 => {
                // A string literal: `""` is the only escape; anything else,
                // parentheses, `;`, `|` and newlines included, is literal.
                let mut t = String::from("\"");
                for _ in 0..rng.below(6) {
                    t.push_str(
                        rng.pick(&["a", " ", "\"\"", "(", ")", ";", "|", "\n", "\\", "x y"]),
                    );
                }
                t.push('"');
                t
            }
            4 => {
                // A quoted symbol: anything but `|`.
                let mut t = String::from("|");
                for _ in 0..rng.below(6) {
                    t.push_str(rng.pick(&["a", " ", "(", ")", ";", "\"", "\n", "x y"]));
                }
                t.push('|');
                t
            }
            _ => format!("v{}", rng.below(20)),
        }
    }

    fn gen_sx(rng: &mut TestRng, depth: usize) -> Sx {
        if depth == 0 || rng.chance(45) {
            return Sx::Atom(gen_atom(rng));
        }
        Sx::List((0..rng.below(4)).map(|_| gen_sx(rng, depth - 1)).collect())
    }

    /// A command: a non-empty list.
    fn gen_command(rng: &mut TestRng) -> Sx {
        let mut items = vec![Sx::Atom(gen_atom(rng))];
        items.extend((0..rng.below(4)).map(|_| gen_sx(rng, 3)));
        Sx::List(items)
    }

    /// Whitespace and comments: never empty, so it always separates.
    fn gap(rng: &mut TestRng) -> String {
        match rng.below(8) {
            0 => "\n".into(),
            1 => "  \t".into(),
            2 => format!(" ;{}\n", rng.pick(&["", " c", " ( \" |", " ) ;"])),
            3 => "\r\n ".into(),
            _ => " ".into(),
        }
    }

    /// Optional whitespace, where the grammar needs none.
    fn maybe_gap(rng: &mut TestRng) -> String {
        if rng.chance(30) {
            gap(rng)
        } else {
            String::new()
        }
    }

    /// Writes `sx` with random formatting.
    fn write(rng: &mut TestRng, sx: &Sx, out: &mut String) {
        match sx {
            Sx::Atom(a) => out.push_str(a),
            Sx::List(items) => {
                out.push('(');
                out.push_str(&maybe_gap(rng));
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        let both_atoms =
                            matches!((&items[i - 1], item), (Sx::Atom(_), Sx::Atom(_)));
                        out.push_str(&if both_atoms { gap(rng) } else { maybe_gap(rng) });
                    }
                    write(rng, item, out);
                }
                out.push_str(&maybe_gap(rng));
                out.push(')');
            }
        }
    }

    /// The canonical form DESIGN section 5 specifies.
    fn canonical_of(sx: &Sx) -> String {
        match sx {
            Sx::Atom(a) => a.clone(),
            Sx::List(items) => format!(
                "({})",
                items.iter().map(canonical_of).collect::<Vec<_>>().join(" ")
            ),
        }
    }

    /// A random input: commands with gaps, and a tail that is whitespace,
    /// a comment or an incomplete command. Returns the input, the expected
    /// canonical forms and the expected remainder.
    fn gen_input(rng: &mut TestRng) -> (Vec<u8>, Vec<String>, Vec<u8>) {
        let mut input = String::new();
        let mut canon = Vec::new();
        for _ in 0..1 + rng.below(6) {
            input.push_str(&maybe_gap(rng));
            let cmd = gen_command(rng);
            write(rng, &cmd, &mut input);
            canon.push(canonical_of(&cmd));
        }
        let tail = match rng.below(4) {
            0 => String::new(),
            1 => gap(rng),
            2 => " ; trailing (".into(),
            _ => {
                let mut t = maybe_gap(rng);
                let mut cmd = String::new();
                let incomplete = gen_command(rng);
                write(rng, &incomplete, &mut cmd);
                t.push_str(&cmd[..cmd.len() - 1]);
                t
            }
        };
        input.push_str(&tail);
        (input.into_bytes(), canon, tail.into_bytes())
    }

    /// Splits `input` at the given increasing cut points.
    fn split_at(input: &[u8], cuts: &[usize]) -> (Vec<Command>, Option<ParseError>, Vec<u8>) {
        let mut chunks: Vec<&[u8]> = Vec::new();
        let mut from = 0;
        for &c in cuts {
            chunks.push(&input[from..c]);
            from = c;
        }
        chunks.push(&input[from..]);
        split_all(&chunks)
    }

    const SEEDS: u64 = 2000;

    #[test]
    fn property_any_split_gives_the_same_commands() {
        for seed in 0..SEEDS {
            let mut rng = TestRng::new(seed);
            let (input, canon, tail) = gen_input(&mut rng);
            let (whole, err, rest) = split_all(&[&input]);
            let shown = String::from_utf8_lossy(&input);
            assert_eq!(err, None, "seed {seed}: {shown:?}");
            let got: Vec<String> = whole
                .iter()
                .map(|c| String::from_utf8(c.canonical.clone()).unwrap())
                .collect();
            assert_eq!(got, canon, "seed {seed}: {shown:?}");
            assert_eq!(rest, tail, "seed {seed}: {shown:?}");
            for _ in 0..8 {
                let mut cuts: Vec<usize> = (0..rng.below(5))
                    .map(|_| rng.below(input.len() + 1))
                    .collect();
                cuts.sort_unstable();
                let (parts, err, rest) = split_at(&input, &cuts);
                assert_eq!(err, None, "seed {seed}, cuts {cuts:?}: {shown:?}");
                assert_eq!(parts, whole, "seed {seed}, cuts {cuts:?}: {shown:?}");
                assert_eq!(rest, tail, "seed {seed}, cuts {cuts:?}: {shown:?}");
            }
        }
    }

    #[test]
    fn property_raw_bytes_and_remainder_reproduce_the_input() {
        for seed in 0..SEEDS {
            let mut rng = TestRng::new(seed);
            let (input, _, _) = gen_input(&mut rng);
            let mut cuts: Vec<usize> = (0..rng.below(4))
                .map(|_| rng.below(input.len() + 1))
                .collect();
            cuts.sort_unstable();
            let (cmds, err, rest) = split_at(&input, &cuts);
            assert_eq!(err, None, "seed {seed}");
            let mut joined: Vec<u8> = cmds.iter().flat_map(|c| c.raw.clone()).collect();
            joined.extend(rest);
            assert_eq!(joined, input, "seed {seed}, cuts {cuts:?}");
        }
    }

    #[test]
    fn property_canonical_form_is_a_fixed_point() {
        for seed in 0..SEEDS {
            let mut rng = TestRng::new(seed);
            let (input, _, _) = gen_input(&mut rng);
            let (cmds, err, _) = split_all(&[&input]);
            assert_eq!(err, None, "seed {seed}");
            for c in cmds {
                let (again, err, rest) = split_all(&[&c.canonical]);
                let shown = String::from_utf8_lossy(&c.canonical);
                assert_eq!(err, None, "seed {seed}: {shown}");
                assert!(rest.is_empty(), "seed {seed}: {shown}");
                assert_eq!(again.len(), 1, "seed {seed}: {shown}");
                assert_eq!(again[0].canonical, c.canonical, "seed {seed}");
                assert_eq!(again[0].raw, c.canonical, "seed {seed}");
                assert_eq!(again[0].items, c.items, "seed {seed}: {shown}");
            }
        }
    }

    #[test]
    fn property_formatting_does_not_change_the_canonical_form() {
        for seed in 0..SEEDS {
            let mut rng = TestRng::new(seed);
            let cmd = gen_command(&mut rng);
            let (mut a, mut b) = (String::new(), String::new());
            write(&mut rng, &cmd, &mut a);
            write(&mut rng, &cmd, &mut b);
            let (ca, err_a, _) = split_all(&[a.as_bytes()]);
            let (cb, err_b, _) = split_all(&[b.as_bytes()]);
            assert_eq!((err_a, err_b), (None, None), "seed {seed}: {a:?} {b:?}");
            assert_eq!(ca[0].canonical, cb[0].canonical, "seed {seed}: {a:?} {b:?}");
            assert_eq!(ca[0].items, cb[0].items, "seed {seed}: {a:?} {b:?}");
        }
    }
}
