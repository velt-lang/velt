//! Token vocabulary shared by the lexer and the parser.
//!
//! `Tok` is a small `Copy` value; literal values (numbers, cooked strings) live out of line in a
//! [`Payload`] table indexed by the token, so the token stream stays compact and cache friendly.

/// Keywords. Contextual ones ([`Kw::is_soft`]) may also be used as identifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kw {
    As,
    Async,
    Await,
    Break,
    Case,
    Catch,
    Class,
    Const,
    Constructor,
    Continue,
    Declare,
    Default,
    Do,
    Else,
    Enum,
    Export,
    Extends,
    False,
    Finally,
    For,
    From,
    Function,
    If,
    Implements,
    Import,
    In,
    Interface,
    Let,
    New,
    Null,
    Of,
    Readonly,
    Return,
    Shared,
    Static,
    Struct,
    Switch,
    This,
    Throw,
    True,
    Try,
    Type,
    Typeof,
    Instanceof,
    Void,
    While,
}

impl Kw {
    /// Keyword for an identifier-shaped word, if it is one.
    pub(crate) fn from_word(s: &str) -> Option<Kw> {
        use Kw::*;
        Some(match s {
            "as" => As,
            "async" => Async,
            "await" => Await,
            "break" => Break,
            "case" => Case,
            "catch" => Catch,
            "class" => Class,
            "const" => Const,
            "constructor" => Constructor,
            "continue" => Continue,
            "declare" => Declare,
            "default" => Default,
            "do" => Do,
            "else" => Else,
            "enum" => Enum,
            "export" => Export,
            "extends" => Extends,
            "false" => False,
            "finally" => Finally,
            "for" => For,
            "from" => From,
            "function" => Function,
            "if" => If,
            "implements" => Implements,
            "import" => Import,
            "in" => In,
            "interface" => Interface,
            "let" => Let,
            "new" => New,
            "null" => Null,
            "of" => Of,
            "readonly" => Readonly,
            "return" => Return,
            "shared" => Shared,
            "static" => Static,
            "struct" => Struct,
            "switch" => Switch,
            "this" => This,
            "throw" => Throw,
            "true" => True,
            "try" => Try,
            "type" => Type,
            "typeof" => Typeof,
            "instanceof" => Instanceof,
            "void" => Void,
            "while" => While,
            _ => return None,
        })
    }

    /// Contextual keywords that may also be used as ordinary identifiers (`type`, `from`, `of`, ...).
    pub(crate) fn is_soft(self) -> bool {
        use Kw::*;
        matches!(
            self,
            Case | Constructor
                | Declare
                | Default
                | From
                | Implements
                | In
                | Of
                | Readonly
                | Shared
                | Static
                | Type
        )
    }
}

/// Which piece of a template literal a `Template` token is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TplPart {
    /// `` `abc` ``
    NoSub,
    /// `` `abc${ ``
    Head,
    /// `}abc${`
    Middle,
    /// `` }abc` ``
    Tail,
}

/// Token kind. Literal tokens carry an index into the lexer's payload table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tok {
    Ident,
    Kw(Kw),
    Int(u32),
    Float(u32),
    Str(u32),
    /// `/body/flags` (`Payload::Regex`).
    Regex(u32),
    Template(u32, TplPart),

    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Semi,
    Comma,
    Colon,
    Dot,
    DotDot,
    DotDotEq,
    DotDotDot,
    Question,
    QuestionDot,
    QuestionQuestion,
    QuestionQuestionEq,
    Plus,
    PlusPlus,
    PlusEq,
    Minus,
    MinusMinus,
    MinusEq,
    Star,
    StarEq,
    StarStar,
    StarStarEq,
    Slash,
    SlashEq,
    Percent,
    PercentEq,
    Lt,
    LtEq,
    Shl,
    ShlEq,
    /// Always a single `>`: the parser glues `>>`, `>=`, `>>=`, ... from adjacent tokens so that
    /// nested generics like `Map<K, Array<V>>` need no token splitting.
    Gt,
    Eq,
    EqEq,
    EqEqEq,
    FatArrow,
    Bang,
    BangEq,
    BangEqEq,
    Amp,
    AmpAmp,
    AmpEq,
    AmpAmpEq,
    Pipe,
    PipePipe,
    PipeEq,
    PipePipeEq,
    Caret,
    CaretEq,
    Tilde,
    /// `<` opening a JSX tag (an operand was expected and a name or `>` follows).
    JsxLt,
    /// `</` starting a JSX closing tag.
    JsxLtSlash,
    /// A JSX tag or attribute name piece: identifier characters plus `-` (`data-id`).
    JsxIdent,
    /// `>` ending a JSX tag.
    JsxGt,
    /// `/>` ending a self-closing JSX tag.
    JsxSlashGt,
    /// JSX child text (`Payload::Text`, cooked; may be empty after whitespace removal).
    JsxText(u32),
    Eof,
}

impl Tok {
    /// Spelling used in "expected `...`" diagnostics.
    pub(crate) fn describe(self) -> &'static str {
        use Tok::*;
        match self {
            Ident => "identifier",
            Kw(_) => "keyword",
            Int(_) | Float(_) => "number",
            Str(_) => "string literal",
            Regex(_) => "regular expression",
            Template(..) => "template literal",
            LParen => "(",
            RParen => ")",
            LBrace => "{",
            RBrace => "}",
            LBracket => "[",
            RBracket => "]",
            Semi => ";",
            Comma => ",",
            Colon => ":",
            Dot => ".",
            DotDot => "..",
            DotDotEq => "..=",
            DotDotDot => "...",
            Question => "?",
            QuestionDot => "?.",
            QuestionQuestion => "??",
            QuestionQuestionEq => "??=",
            Plus => "+",
            PlusPlus => "++",
            PlusEq => "+=",
            Minus => "-",
            MinusMinus => "--",
            MinusEq => "-=",
            Star => "*",
            StarEq => "*=",
            StarStar => "**",
            StarStarEq => "**=",
            Slash => "/",
            SlashEq => "/=",
            Percent => "%",
            PercentEq => "%=",
            Lt => "<",
            LtEq => "<=",
            Shl => "<<",
            ShlEq => "<<=",
            Gt => ">",
            Eq => "=",
            EqEq => "==",
            EqEqEq => "===",
            FatArrow => "=>",
            Bang => "!",
            BangEq => "!=",
            BangEqEq => "!==",
            Amp => "&",
            AmpAmp => "&&",
            AmpEq => "&=",
            AmpAmpEq => "&&=",
            Pipe => "|",
            PipePipe => "||",
            PipeEq => "|=",
            PipePipeEq => "||=",
            Caret => "^",
            CaretEq => "^=",
            Tilde => "~",
            JsxLt => "<",
            JsxLtSlash => "</",
            JsxIdent => "JSX name",
            JsxGt => ">",
            JsxSlashGt => "/>",
            JsxText(_) => "JSX text",
            Eof => "end of file",
        }
    }
}

/// A token with its byte range in the source.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Token {
    pub kind: Tok,
    pub lo: u32,
    pub hi: u32,
}

/// Out-of-line literal value referenced by `Tok::Int/Float/Str/Template`.
#[derive(Clone, Debug)]
pub(crate) enum Payload {
    Int {
        value: u128,
        suffix: Option<String>,
    },
    Float {
        value: f64,
        suffix: Option<String>,
    },
    /// Cooked (unescaped) string or template-quasi text.
    Text(String),
    /// A regular expression literal: its raw body and flags.
    Regex {
        source: String,
        flags: String,
    },
}
