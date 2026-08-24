//! Syntax highlighting: the lines of a code fence get keyword,
//! string, comment, and number classes, for a fixed set of
//! languages. The fence label picks the language; a label outside
//! the set gets no classes.
//!
//! Two scanners sit behind one type. The code scanner makes one
//! linear pass over the bytes of each line under a language's rules,
//! with one state that carries to the next line: inside a block
//! comment, or inside a string that runs on. It is a tokenizer, not a
//! grammar: it knows where comments, strings, numbers, and words
//! start and end, and it looks a word up in the language's keyword
//! list. The markdown scanner reads a markdown sample by its lines —
//! headings, quotes, code spans, fences — and carries the open fence.
//! Both cover what a reader scans a sample for, at a few nanoseconds
//! per byte on every frame that renders the block again. The tables
//! are static: no load on the boot path, none on the first fence. The
//! caller widens tabs before the scan.
//!
//! The highlighter starts fresh at the top of each block. A block cut
//! at the byte cap inside a fence continues in the next block with a
//! fresh state, so a comment or a string open across the cut reads
//! as code until its closer, and the closer opens one that runs to
//! the next: wrong classes on a stretch, never a lost row.
//!
//! The scanner cuts only at ASCII bytes or after a whole word, and a
//! word takes every non-ASCII byte, so every offset it reports is a
//! char boundary.

/// The class of one span inside a code fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Class {
    /// Code with no class of its own: identifiers, operators,
    /// punctuation.
    Plain,
    Keyword,
    String,
    Comment,
    Number,
}

/// What a single quote opens.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Single {
    /// Nothing: a plain byte.
    None,
    /// A string; `escapes` says a backslash keeps the next byte in.
    Str { escapes: bool },
    /// A character literal, `'x'` or `'\n'`; anything else is a
    /// lifetime or a plain quote.
    Char,
}

/// What a backtick opens.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Backtick {
    None,
    /// A raw string with no escapes, across lines.
    Raw,
    /// A template with escapes, across lines.
    Template,
}

/// One language: its keywords and its lexical rules.
struct Spec {
    /// Sorted, for a binary search.
    keywords: &'static [&'static str],
    /// The line-comment opener; empty for none.
    line_comment: &'static str,
    /// The line comment opens the line or follows whitespace: `$#`
    /// and `a#b` are not comments.
    comment_after_space: bool,
    /// `/* … */` comments.
    block_comment: bool,
    /// An opener inside a block comment deepens it.
    nested_comments: bool,
    single: Single,
    backtick: Backtick,
    /// `"""` and `'''` open strings that run across lines.
    triple: bool,
    /// A `"` or `'` string a line does not close runs on to the next.
    multiline: bool,
    /// A quote opens a string only where a value starts: at the
    /// start of the line or after `:`, `-`, `,`, `[`, `{`. An
    /// apostrophe inside a plain scalar is then a plain byte.
    quote_at_value: bool,
    /// Letters a string prefix can use, one or two before the quote.
    prefixes: &'static [u8],
    /// `r#"…"#`: a quote and as many hashes as opened it close a raw
    /// string, which has no escapes.
    raw_hashes: bool,
    /// Numbers get a class; off where a digit run is an argument.
    numbers: bool,
    /// Dates and times read as numbers: `-`, `:`, and `+` between
    /// digits stay inside one.
    dates: bool,
}

/// The rules a language starts from: C-style comments, strings with
/// escapes on one line, numbers.
const BASE: Spec = Spec {
    keywords: &[],
    line_comment: "//",
    comment_after_space: false,
    block_comment: true,
    nested_comments: false,
    single: Single::Str { escapes: true },
    backtick: Backtick::None,
    triple: false,
    multiline: false,
    quote_at_value: false,
    prefixes: &[],
    raw_hashes: false,
    numbers: true,
    dates: false,
};

static RUST: Spec = Spec {
    keywords: RUST_KEYWORDS,
    nested_comments: true,
    single: Single::Char,
    multiline: true,
    prefixes: b"brc",
    raw_hashes: true,
    ..BASE
};

static PYTHON: Spec = Spec {
    keywords: PYTHON_KEYWORDS,
    line_comment: "#",
    block_comment: false,
    triple: true,
    prefixes: b"rbfuRBFU",
    ..BASE
};

static JAVASCRIPT: Spec = Spec {
    keywords: JAVASCRIPT_KEYWORDS,
    backtick: Backtick::Template,
    ..BASE
};

static GO: Spec = Spec {
    keywords: GO_KEYWORDS,
    single: Single::Char,
    backtick: Backtick::Raw,
    ..BASE
};

static SHELL: Spec = Spec {
    keywords: SHELL_KEYWORDS,
    line_comment: "#",
    comment_after_space: true,
    block_comment: false,
    single: Single::Str { escapes: false },
    multiline: true,
    numbers: false,
    ..BASE
};

/// Plain JSON has no comments. The rules keep them for the files
/// that carry them; a `//` outside a string is never data.
static JSON: Spec = Spec {
    keywords: JSON_KEYWORDS,
    single: Single::None,
    ..BASE
};

static TOML: Spec = Spec {
    keywords: TOML_KEYWORDS,
    line_comment: "#",
    block_comment: false,
    single: Single::Str { escapes: false },
    triple: true,
    dates: true,
    ..BASE
};

static YAML: Spec = Spec {
    keywords: YAML_KEYWORDS,
    line_comment: "#",
    comment_after_space: true,
    block_comment: false,
    single: Single::Str { escapes: false },
    multiline: true,
    quote_at_value: true,
    ..BASE
};

/// What a label names: a language under the code rules, or a
/// markdown sample with rules of its own.
#[derive(Clone, Copy)]
enum Lang {
    Code(&'static Spec),
    Markdown,
}

/// Fence labels and the language each names, compared without case.
static LABELS: &[(&str, Lang)] = &[
    ("rust", Lang::Code(&RUST)),
    ("rs", Lang::Code(&RUST)),
    ("python", Lang::Code(&PYTHON)),
    ("py", Lang::Code(&PYTHON)),
    ("python3", Lang::Code(&PYTHON)),
    ("javascript", Lang::Code(&JAVASCRIPT)),
    ("js", Lang::Code(&JAVASCRIPT)),
    ("jsx", Lang::Code(&JAVASCRIPT)),
    ("mjs", Lang::Code(&JAVASCRIPT)),
    ("typescript", Lang::Code(&JAVASCRIPT)),
    ("ts", Lang::Code(&JAVASCRIPT)),
    ("tsx", Lang::Code(&JAVASCRIPT)),
    ("go", Lang::Code(&GO)),
    ("golang", Lang::Code(&GO)),
    ("sh", Lang::Code(&SHELL)),
    ("bash", Lang::Code(&SHELL)),
    ("zsh", Lang::Code(&SHELL)),
    ("shell", Lang::Code(&SHELL)),
    ("json", Lang::Code(&JSON)),
    ("jsonc", Lang::Code(&JSON)),
    ("toml", Lang::Code(&TOML)),
    ("yaml", Lang::Code(&YAML)),
    ("yml", Lang::Code(&YAML)),
    ("markdown", Lang::Markdown),
    ("md", Lang::Markdown),
];

const RUST_KEYWORDS: &[&str] = &[
    "Self", "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
    "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
    "mut", "pub", "ref", "return", "self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
];
const PYTHON_KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield",
];
const JAVASCRIPT_KEYWORDS: &[&str] = &[
    "abstract",
    "as",
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "declare",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "from",
    "function",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "keyof",
    "let",
    "namespace",
    "new",
    "null",
    "of",
    "private",
    "protected",
    "public",
    "readonly",
    "return",
    "satisfies",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "type",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "with",
    "yield",
];
const GO_KEYWORDS: &[&str] = &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "fallthrough",
    "false",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "iota",
    "map",
    "nil",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "true",
    "type",
    "var",
];
const SHELL_KEYWORDS: &[&str] = &[
    "case", "declare", "do", "done", "elif", "else", "esac", "exit", "export", "fi", "for",
    "function", "if", "in", "local", "readonly", "return", "select", "set", "shift", "source",
    "then", "trap", "unset", "until", "while",
];
const JSON_KEYWORDS: &[&str] = &["false", "null", "true"];
const TOML_KEYWORDS: &[&str] = &["false", "inf", "nan", "true"];
const YAML_KEYWORDS: &[&str] = &[
    "FALSE", "False", "NULL", "Null", "TRUE", "True", "false", "null", "true",
];

/// A string under scan: what closes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Quote {
    /// The closing byte.
    ch: u8,
    /// Three of them close it.
    triple: bool,
    /// A backslash keeps the next byte inside.
    escapes: bool,
    /// Hashes that must follow the closing quote.
    hashes: u8,
    /// The string runs on past the end of the line.
    lines: bool,
}

/// What carries from one line to the next under the code rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Code,
    /// Inside a block comment, `depth` openers deep.
    Comment {
        depth: u32,
    },
    /// Inside a string.
    Str(Quote),
}

/// The class changes of one line, with runs of one class joined.
struct Emit<F> {
    emit: F,
    last: Option<Class>,
}

impl<F: FnMut(usize, Class)> Emit<F> {
    fn mark(&mut self, at: usize, class: Class) {
        if self.last != Some(class) {
            (self.emit)(at, class);
            self.last = Some(class);
        }
    }
}

/// The scanner for one fence: its language and the state that
/// carries between lines.
pub(crate) enum Highlighter {
    Code(Code),
    Markdown(Markdown),
}

/// A language under the code rules.
pub(crate) struct Code {
    spec: &'static Spec,
    state: State,
}

/// A markdown sample: the fence open inside it, as its byte and its
/// run length.
pub(crate) struct Markdown {
    fence: Option<(u8, usize)>,
}

impl Highlighter {
    /// The highlighter for a fence label, or `None` for a label
    /// outside the set. The label ends at whitespace, a comma, or a
    /// brace, where the info string's own conventions start:
    /// `rust,ignore`, `python {.numberLines}`.
    pub fn for_label(label: &str) -> Option<Self> {
        let end = label
            .find(|ch: char| ch.is_whitespace() || ch == ',' || ch == '{')
            .unwrap_or(label.len());
        let label = &label[..end];
        let lang = LABELS
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(label))
            .map(|(_, lang)| *lang)?;
        Some(match lang {
            Lang::Code(spec) => Self::Code(Code {
                spec,
                state: State::Code,
            }),
            Lang::Markdown => Self::Markdown(Markdown { fence: None }),
        })
    }

    /// Classify `line`, which holds no newline. `emit(at, class)`
    /// runs at each class change in byte order: the first at zero
    /// unless the line is empty, each later one past the last and
    /// inside the line.
    pub fn line(&mut self, line: &str, emit: impl FnMut(usize, Class)) {
        let mut out = Emit { emit, last: None };
        match self {
            Self::Code(code) => code.line(line, &mut out),
            Self::Markdown(markdown) => markdown.line(line.as_bytes(), &mut out),
        }
    }
}

impl Code {
    fn line(&mut self, line: &str, out: &mut Emit<impl FnMut(usize, Class)>) {
        let bytes = line.as_bytes();
        let mut at = 0usize;
        while at < bytes.len() {
            at = match self.state {
                State::Comment { depth } => {
                    out.mark(at, Class::Comment);
                    self.block_comment(bytes, at, depth)
                }
                State::Str(quote) => {
                    out.mark(at, Class::String);
                    self.string(bytes, at, quote)
                }
                State::Code => self.token(line, at, out),
            };
        }
        // A string that stays on its line ends with it.
        if let State::Str(quote) = self.state
            && !quote.lines
        {
            self.state = State::Code;
        }
    }

    /// Classify the token at `at` and return where the next starts.
    fn token(&mut self, line: &str, at: usize, out: &mut Emit<impl FnMut(usize, Class)>) -> usize {
        let bytes = line.as_bytes();
        let spec = self.spec;
        let byte = bytes[at];
        let rest = &bytes[at..];
        if !spec.line_comment.is_empty()
            && rest.starts_with(spec.line_comment.as_bytes())
            && (!spec.comment_after_space || at == 0 || bytes[at - 1].is_ascii_whitespace())
        {
            out.mark(at, Class::Comment);
            return bytes.len();
        }
        if spec.block_comment && rest.starts_with(b"/*") {
            out.mark(at, Class::Comment);
            self.state = State::Comment { depth: 1 };
            return at + 2;
        }
        // The value test runs only at a quote: its scan back over
        // the spaces before one stops at the quote before that, so
        // the line stays linear.
        if (byte == b'"'
            || (byte == b'\'' && spec.single != Single::None && spec.single != Single::Char))
            && (!spec.quote_at_value || value_start(bytes, at))
        {
            let escapes = byte == b'"' || spec.single == Single::Str { escapes: true };
            return self.open_string(bytes, at, at, escapes, 0, out);
        }
        if byte == b'\'' && spec.single == Single::Char {
            if let Some(end) = char_literal_end(bytes, at) {
                out.mark(at, Class::String);
                return end;
            }
            out.mark(at, Class::Plain);
            return at + 1;
        }
        if byte == b'`' && spec.backtick != Backtick::None {
            let quote = Quote {
                ch: b'`',
                triple: false,
                escapes: spec.backtick == Backtick::Template,
                hashes: 0,
                lines: true,
            };
            out.mark(at, Class::String);
            self.state = State::Str(quote);
            return at + 1;
        }
        if is_word_start(byte) {
            let end = word_end(bytes, at);
            if let Some(next) = self.prefixed_string(bytes, at, end, out) {
                return next;
            }
            // A raw identifier, `r#type`, is one plain word.
            if spec.raw_hashes
                && &bytes[at..end] == b"r"
                && bytes.get(end) == Some(&b'#')
                && bytes.get(end + 1).is_some_and(|&next| is_word_start(next))
            {
                out.mark(at, Class::Plain);
                return word_end(bytes, end + 1);
            }
            let class = if spec.keywords.binary_search(&&line[at..end]).is_ok() {
                Class::Keyword
            } else {
                Class::Plain
            };
            out.mark(at, class);
            return end;
        }
        if spec.numbers && byte.is_ascii_digit() {
            out.mark(at, Class::Number);
            return number_end(bytes, at, spec.dates);
        }
        out.mark(at, Class::Plain);
        at + 1
    }

    /// A string that opens with prefix letters, `at..end` in the
    /// line, before its quote: `b"…"`, `r#"…"#`, `f'…'`. Returns
    /// where the scan continues, or `None` when the word is no
    /// prefix.
    fn prefixed_string(
        &mut self,
        bytes: &[u8],
        at: usize,
        end: usize,
        out: &mut Emit<impl FnMut(usize, Class)>,
    ) -> Option<usize> {
        let spec = self.spec;
        let word = &bytes[at..end];
        if word.len() > 2 || !word.iter().all(|byte| spec.prefixes.contains(byte)) {
            return None;
        }
        let raw = spec.raw_hashes && word.contains(&b'r');
        let hashes = if raw {
            bytes[end..]
                .iter()
                .take_while(|&&byte| byte == b'#')
                .count()
        } else {
            0
        };
        // More hashes than the closer can count open no string.
        let hashes = u8::try_from(hashes).ok()?;
        let quote_at = end + usize::from(hashes);
        let quote = *bytes.get(quote_at)?;
        let opens = quote == b'"' || (quote == b'\'' && spec.single != Single::None && !raw);
        if !opens {
            return None;
        }
        // A raw string has no escapes; the hashes close it.
        Some(self.open_string(bytes, at, quote_at, !raw, hashes, out))
    }

    /// Open the string whose quote is at `quote_at` and whose span
    /// starts at `from`; `hashes` must follow the closing quote.
    /// Returns where the scan continues.
    fn open_string(
        &mut self,
        bytes: &[u8],
        from: usize,
        quote_at: usize,
        escapes: bool,
        hashes: u8,
        out: &mut Emit<impl FnMut(usize, Class)>,
    ) -> usize {
        let spec = self.spec;
        let ch = bytes[quote_at];
        let triple = spec.triple && bytes[quote_at..].starts_with(&[ch, ch, ch]);
        let quote = Quote {
            ch,
            triple,
            escapes,
            hashes,
            lines: spec.multiline || triple || hashes > 0,
        };
        out.mark(from, Class::String);
        self.state = State::Str(quote);
        quote_at + if triple { 3 } else { 1 }
    }

    /// Scan the open string from `at`. Returns where it closed, or the
    /// line's end with the state still open.
    fn string(&mut self, bytes: &[u8], at: usize, quote: Quote) -> usize {
        let mut at = at;
        while at < bytes.len() {
            let byte = bytes[at];
            if quote.escapes && byte == b'\\' {
                at += 2;
                continue;
            }
            // A doubled quote stays inside a string with no escapes:
            // `'it''s'`.
            if !quote.escapes && !quote.triple && byte == b'\'' && bytes.get(at + 1) == Some(&byte)
            {
                at += 2;
                continue;
            }
            if byte == quote.ch {
                let closer = if quote.triple {
                    bytes[at..]
                        .starts_with(&[byte, byte, byte])
                        .then_some(at + 3)
                } else {
                    let end = at + 1 + usize::from(quote.hashes);
                    bytes
                        .get(at + 1..end)
                        .is_some_and(|run| run.iter().all(|&b| b == b'#'))
                        .then_some(end)
                };
                if let Some(end) = closer {
                    self.state = State::Code;
                    return end;
                }
            }
            at += 1;
        }
        bytes.len()
    }

    /// Scan the open block comment from `at`. Returns where it closed,
    /// or the line's end with the state still open.
    fn block_comment(&mut self, bytes: &[u8], at: usize, depth: u32) -> usize {
        let mut at = at;
        let mut depth = depth;
        while at < bytes.len() {
            if bytes[at..].starts_with(b"*/") {
                at += 2;
                depth -= 1;
                if depth == 0 {
                    self.state = State::Code;
                    return at;
                }
            } else if self.spec.nested_comments && bytes[at..].starts_with(b"/*") {
                at += 2;
                depth += 1;
            } else {
                at += 1;
            }
        }
        self.state = State::Comment { depth };
        bytes.len()
    }
}

impl Markdown {
    /// A heading line is a keyword, a quote line a comment, code
    /// spans and fenced or indented code strings.
    fn line(&mut self, bytes: &[u8], out: &mut Emit<impl FnMut(usize, Class)>) {
        if bytes.is_empty() {
            return;
        }
        let indent = bytes.iter().take_while(|&&byte| byte == b' ').count();
        let rest = &bytes[indent..];
        if let Some((ch, run)) = self.fence {
            out.mark(0, Class::String);
            if indent < 4
                && let Some((close_ch, close_run)) = fence_run(rest)
                && close_ch == ch
                && close_run >= run
                && rest[close_run..].iter().all(u8::is_ascii_whitespace)
            {
                self.fence = None;
            }
            return;
        }
        if indent >= 4 {
            out.mark(0, Class::String);
            return;
        }
        if let Some((ch, run)) = fence_run(rest)
            && (ch == b'~' || !rest[run..].contains(&b'`'))
        {
            out.mark(0, Class::String);
            self.fence = Some((ch, run));
            return;
        }
        match rest.first() {
            Some(b'#') => {
                let marks = rest.iter().take_while(|&&byte| byte == b'#').count();
                if marks <= 6 && rest.get(marks).is_none_or(|&byte| byte == b' ') {
                    out.mark(0, Class::Keyword);
                    return;
                }
            }
            Some(b'>') => {
                out.mark(0, Class::Comment);
                return;
            }
            _ => {}
        }
        // Code spans: a run of backticks closes at the next run of
        // the same length, or runs to the end of the line.
        let mut at = 0usize;
        while at < bytes.len() {
            if bytes[at] != b'`' {
                out.mark(at, Class::Plain);
                at += 1;
                continue;
            }
            let run = bytes[at..].iter().take_while(|&&byte| byte == b'`').count();
            out.mark(at, Class::String);
            let mut close = at + run;
            loop {
                let Some(offset) = bytes[close..].iter().position(|&byte| byte == b'`') else {
                    return;
                };
                let start = close + offset;
                let len = bytes[start..]
                    .iter()
                    .take_while(|&&byte| byte == b'`')
                    .count();
                close = start + len;
                if len == run {
                    break;
                }
            }
            at = close;
        }
    }
}

/// Three or more of one fence byte at the start of `rest`.
fn fence_run(rest: &[u8]) -> Option<(u8, usize)> {
    let ch = *rest.first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let run = rest.iter().take_while(|&&byte| byte == ch).count();
    (run >= 3).then_some((ch, run))
}

/// Whether `at` is where a value starts: the line's start, or the
/// first non-blank after `:`, `-`, `,`, `[`, `{`.
fn value_start(bytes: &[u8], at: usize) -> bool {
    let before = bytes[..at]
        .iter()
        .rev()
        .find(|&&byte| byte != b' ' && byte != b'\t');
    before.is_none_or(|byte| matches!(byte, b':' | b'-' | b',' | b'[' | b'{'))
}

/// A letter, an underscore, or the first byte of a non-ASCII char.
fn is_word_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte >= 0x80
}

/// Where the word at `at` ends: letters, digits, underscores, and
/// every non-ASCII byte.
fn word_end(bytes: &[u8], at: usize) -> usize {
    at + bytes[at..]
        .iter()
        .take_while(|&&byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80)
        .count()
}

/// Where the number at `at` ends: digits, letters, and underscores
/// — `0x1f`, `1_000u32`, `10n` — a point before a digit, an exponent
/// sign, and with `dates` the separators of a date or a time.
fn number_end(bytes: &[u8], at: usize, dates: bool) -> usize {
    let hex = bytes[at..].starts_with(b"0x") || bytes[at..].starts_with(b"0X");
    let mut end = at;
    while end < bytes.len() {
        let byte = bytes[end];
        let digit_next = bytes.get(end + 1).is_some_and(u8::is_ascii_digit);
        if byte.is_ascii_alphanumeric() || byte == b'_' {
            end += 1;
            if !hex
                && (byte == b'e' || byte == b'E')
                && bytes
                    .get(end)
                    .is_some_and(|&sign| sign == b'+' || sign == b'-')
                && bytes.get(end + 1).is_some_and(u8::is_ascii_digit)
            {
                end += 1;
            }
        } else if digit_next && (byte == b'.' || (dates && matches!(byte, b'-' | b':' | b'+'))) {
            end += 1;
        } else {
            break;
        }
    }
    end
}

/// Where the character literal at `at` ends, when `'x'` or an escape
/// closed by a quote is there; `None` for a lifetime or a lone quote.
fn char_literal_end(bytes: &[u8], at: usize) -> Option<usize> {
    let first = *bytes.get(at + 1)?;
    if first == b'\\' {
        // The escaped byte cannot close: `'\''`.
        let close = bytes
            .get(at + 3..)?
            .iter()
            .position(|&byte| byte == b'\'')?;
        return Some(at + 3 + close + 1);
    }
    if first == b'\'' {
        return None;
    }
    let len = match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    };
    (bytes.get(at + 1 + len) == Some(&b'\'')).then_some(at + 2 + len)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The classes of every line of `source` under `label`, as
    /// `(offset, class)` changes per line.
    fn classes(label: &str, source: &str) -> Vec<Vec<(usize, Class)>> {
        let mut highlighter = Highlighter::for_label(label).expect("a known label");
        source
            .lines()
            .map(|line| {
                let mut out = Vec::new();
                highlighter.line(line, |at, class| out.push((at, class)));
                out
            })
            .collect()
    }

    /// The spans of one line as `(text, class)`.
    fn spans(label: &str, line: &str) -> Vec<(String, Class)> {
        let changes = classes(label, line).remove(0);
        changes
            .iter()
            .enumerate()
            .map(|(index, &(at, class))| {
                let to = changes.get(index + 1).map_or(line.len(), |&(next, _)| next);
                (line[at..to].to_string(), class)
            })
            .collect()
    }

    use Class::{Comment, Keyword, Number, Plain, String as Str};

    #[test]
    fn keyword_lists_are_sorted_and_unique() {
        for (label, lang) in LABELS {
            let Lang::Code(spec) = lang else {
                continue;
            };
            assert!(spec.keywords.is_sorted(), "{label}");
            assert!(
                spec.keywords.windows(2).all(|pair| pair[0] != pair[1]),
                "{label}"
            );
        }
    }

    #[test]
    fn labels_resolve_without_case_and_unknown_ones_do_not() {
        assert!(Highlighter::for_label("Rust").is_some());
        assert!(Highlighter::for_label("TypeScript").is_some());
        assert!(Highlighter::for_label("yml").is_some());
        assert!(Highlighter::for_label("cobol").is_none());
        assert!(Highlighter::for_label("").is_none());
        // The label ends where the info string's conventions start.
        assert!(Highlighter::for_label("rust,ignore").is_some());
        assert!(Highlighter::for_label("python {.numberLines}").is_some());
        assert!(Highlighter::for_label("md title=\"x\"").is_some());
        assert!(Highlighter::for_label(",rust").is_none());
    }

    #[test]
    fn rust_classes_keywords_strings_comments_and_numbers() {
        assert_eq!(
            spans("rust", "let x = 0x1f + 42; // note"),
            vec![
                ("let".into(), Keyword),
                (" x = ".into(), Plain),
                ("0x1f".into(), Number),
                (" + ".into(), Plain),
                ("42".into(), Number),
                ("; ".into(), Plain),
                ("// note".into(), Comment),
            ]
        );
        assert_eq!(
            spans("rust", "f(\"a\\\"b\", b'c', r#\"raw\"#, 'x', 'a)"),
            vec![
                ("f(".into(), Plain),
                ("\"a\\\"b\"".into(), Str),
                (", ".into(), Plain),
                ("b'c'".into(), Str),
                (", ".into(), Plain),
                ("r#\"raw\"#".into(), Str),
                (", ".into(), Plain),
                ("'x'".into(), Str),
                (", 'a)".into(), Plain),
            ]
        );
        // A two-letter prefix counts its hashes too; a raw string
        // holds a quote followed by fewer hashes.
        assert_eq!(
            spans("rust", "br##\"a\"# b\"## + 1"),
            vec![
                ("br##\"a\"# b\"##".into(), Str),
                (" + ".into(), Plain),
                ("1".into(), Number),
            ]
        );
        // A lifetime is plain; a raw identifier is plain; an escaped
        // quote is one char literal.
        assert_eq!(
            spans("rust", "fn f<'a>(r#type: &'a str) {}"),
            vec![
                ("fn".into(), Keyword),
                (" f<'a>(r#type: &'a str) {}".into(), Plain),
            ]
        );
        assert_eq!(
            spans("rust", "'\\'' '\\u{1F600}' 'é'"),
            vec![
                ("'\\''".into(), Str),
                (" ".into(), Plain),
                ("'\\u{1F600}'".into(), Str),
                (" ".into(), Plain),
                ("'é'".into(), Str),
            ]
        );
    }

    #[test]
    fn a_block_comment_nests_and_spans_lines_in_rust() {
        let lines = classes("rust", "a /* one /* two */ still\n*/ b /* c\n");
        assert_eq!(lines[0], vec![(0, Plain), (2, Comment)]);
        assert_eq!(lines[1], vec![(0, Comment), (2, Plain), (5, Comment)]);
        // A JavaScript comment does not nest.
        let lines = classes("js", "/* one /* two */ x");
        assert_eq!(lines[0], vec![(0, Comment), (16, Plain)]);
    }

    #[test]
    fn python_strings_take_prefixes_and_triple_quotes() {
        assert_eq!(
            spans("py", "s = f'{x}' + rb\"y\" # c"),
            vec![
                ("s = ".into(), Plain),
                ("f'{x}'".into(), Str),
                (" + ".into(), Plain),
                ("rb\"y\"".into(), Str),
                (" ".into(), Plain),
                ("# c".into(), Comment),
            ]
        );
        let lines = classes(
            "python",
            "x = \"\"\"doc\nmore \"\" not\n\"\"\" + 1\n'unclosed\ny = 2",
        );
        assert_eq!(lines[0], vec![(0, Plain), (4, Str)]);
        assert_eq!(lines[1], vec![(0, Str)]);
        assert_eq!(lines[2], vec![(0, Str), (3, Plain), (6, Number)]);
        // A single-quoted string ends with its line.
        assert_eq!(lines[3], vec![(0, Str)]);
        assert_eq!(lines[4], vec![(0, Plain), (4, Number)]);
        // A keyword is case-sensitive: `None` is one, `none` is not.
        assert_eq!(
            spans("py", "None none"),
            vec![("None".into(), Keyword), (" none".into(), Plain)]
        );
    }

    #[test]
    fn javascript_templates_and_go_raw_strings_span_lines() {
        let lines = classes("ts", "const s = `a ${b}\n\\` c` + 1.5e-3;");
        assert_eq!(lines[0], vec![(0, Keyword), (5, Plain), (10, Str)]);
        assert_eq!(
            lines[1],
            vec![(0, Str), (5, Plain), (8, Number), (14, Plain)]
        );
        let lines = classes("go", "s := `raw \\\nend` // c");
        assert_eq!(lines[0], vec![(0, Plain), (5, Str)]);
        assert_eq!(lines[1], vec![(0, Str), (4, Plain), (5, Comment)]);
        assert_eq!(
            spans("go", "r := 'x'; n := nil"),
            vec![
                ("r := ".into(), Plain),
                ("'x'".into(), Str),
                ("; n := ".into(), Plain),
                ("nil".into(), Keyword),
            ]
        );
        assert_eq!(
            spans("ts", "private x = 1 satisfies T;"),
            vec![
                ("private".into(), Keyword),
                (" x = ".into(), Plain),
                ("1".into(), Number),
                (" ".into(), Plain),
                ("satisfies".into(), Keyword),
                (" T;".into(), Plain),
            ]
        );
    }

    #[test]
    fn shell_comments_follow_whitespace_and_numbers_stay_plain() {
        assert_eq!(
            spans("sh", "echo $# ${#x} a#b 42 # c"),
            vec![
                ("echo $# ${#x} a#b 42 ".into(), Plain),
                ("# c".into(), Comment),
            ]
        );
        assert_eq!(
            spans("bash", "if [ -f 'a\\' ]; then"),
            vec![
                ("if".into(), Keyword),
                (" [ -f ".into(), Plain),
                ("'a\\'".into(), Str),
                (" ]; ".into(), Plain),
                ("then".into(), Keyword),
            ]
        );
        assert_eq!(classes("sh", "#!/bin/sh")[0], vec![(0, Comment)]);
    }

    #[test]
    fn data_languages_class_their_literals() {
        assert_eq!(
            spans("json", "{\"k\": [true, null, -1.5, \"v\"]}"),
            vec![
                ("{".into(), Plain),
                ("\"k\"".into(), Str),
                (": [".into(), Plain),
                ("true".into(), Keyword),
                (", ".into(), Plain),
                ("null".into(), Keyword),
                (", -".into(), Plain),
                ("1.5".into(), Number),
                (", ".into(), Plain),
                ("\"v\"".into(), Str),
                ("]}".into(), Plain),
            ]
        );
        assert_eq!(
            spans("toml", "born = 1979-05-27T07:32:00-08:00 # local"),
            vec![
                ("born = ".into(), Plain),
                ("1979-05-27T07:32:00-08:00".into(), Number),
                (" ".into(), Plain),
                ("# local".into(), Comment),
            ]
        );
        assert_eq!(
            spans("yaml", "key: 'it''s' # c"),
            vec![
                ("key: ".into(), Plain),
                ("'it''s'".into(), Str),
                (" ".into(), Plain),
                ("# c".into(), Comment),
            ]
        );
        assert_eq!(
            spans("yaml", "url: http://x/#frag"),
            vec![("url: http://x/#frag".into(), Plain)]
        );
    }

    #[test]
    fn a_yaml_quote_opens_a_string_only_where_a_value_starts() {
        // An apostrophe inside a plain scalar is plain, and does not
        // carry to the next line.
        let lines = classes(
            "yml",
            "  MSG: it's fine\n  OTHER: 2\n- 'a'\nlist: [\"b\", 'c']\nk:\t'd'",
        );
        assert_eq!(lines[0], vec![(0, Plain)]);
        assert_eq!(lines[1], vec![(0, Plain), (9, Number)]);
        assert_eq!(lines[2], vec![(0, Plain), (2, Str)]);
        assert_eq!(
            lines[3],
            vec![(0, Plain), (7, Str), (10, Plain), (12, Str), (15, Plain)]
        );
        assert_eq!(lines[4], vec![(0, Plain), (3, Str)]);
        // A quoted scalar still runs across lines.
        let lines = classes("yaml", "key: \"one\ntwo\" tail");
        assert_eq!(lines[0], vec![(0, Plain), (5, Str)]);
        assert_eq!(lines[1], vec![(0, Str), (4, Plain)]);
        assert_eq!(
            spans("yaml", "flags: [True, FALSE, Null]"),
            vec![
                ("flags: [".into(), Plain),
                ("True".into(), Keyword),
                (", ".into(), Plain),
                ("FALSE".into(), Keyword),
                (", ".into(), Plain),
                ("Null".into(), Keyword),
                ("]".into(), Plain),
            ]
        );
    }

    #[test]
    fn a_markdown_sample_classes_its_structure() {
        let lines = classes(
            "md",
            "# Title\n> quote\ntext with `code` and ``a`b`` here\n```rs\nfn a() {}\n```\nafter\n    indented",
        );
        assert_eq!(lines[0], vec![(0, Keyword)]);
        assert_eq!(lines[1], vec![(0, Comment)]);
        assert_eq!(
            lines[2],
            vec![(0, Plain), (10, Str), (16, Plain), (21, Str), (28, Plain)]
        );
        assert_eq!(lines[3], vec![(0, Str)]);
        assert_eq!(lines[4], vec![(0, Str)]);
        assert_eq!(lines[5], vec![(0, Str)]);
        assert_eq!(lines[6], vec![(0, Plain)]);
        assert_eq!(lines[7], vec![(0, Str)]);
        // `#hashtag` is no heading; an unclosed span runs to the end.
        assert_eq!(classes("md", "#tag `open")[0], vec![(0, Plain), (5, Str)]);
        // A span at the line's start or end changes the class once.
        assert_eq!(classes("md", "`code` and")[0], vec![(0, Str), (6, Plain)]);
        assert_eq!(classes("md", "x `code`")[0], vec![(0, Plain), (2, Str)]);
    }

    #[test]
    fn every_offset_is_a_char_boundary_inside_the_line_and_increases() {
        let sources = [
            "let s = \"héllo\"; // ünïcode → ok",
            "x = 'ß' + \"日本\" # 注釈",
            "r := 'é'; s := `日\\本`",
            "# 見出し `コード` ``x``",
            "born = 2000-01-01 # 日付",
            "`日本` and ``x`` `",
            "'\\''",
        ];
        for (label, _) in LABELS {
            for source in sources {
                let mut highlighter = Highlighter::for_label(label).unwrap();
                let mut last = None;
                highlighter.line(source, |at, _| {
                    assert!(source.is_char_boundary(at), "{label}: {source:?} at {at}");
                    assert!(at < source.len(), "{label}: {source:?} at {at}");
                    assert!(last.is_none_or(|last| at > last), "{label}: {source:?}");
                    if last.is_none() {
                        assert_eq!(at, 0, "{label}: {source:?}");
                    }
                    last = Some(at);
                });
            }
        }
    }

    #[test]
    fn an_empty_line_emits_nothing_and_keeps_the_state() {
        let lines = classes("rust", "/* a\n\nb */ c");
        assert_eq!(lines[1], vec![]);
        assert_eq!(lines[2], vec![(0, Comment), (4, Plain)]);
    }
}
