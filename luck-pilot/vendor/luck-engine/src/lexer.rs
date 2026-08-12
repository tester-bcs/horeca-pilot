//! Luck — лексер (токенизатор)
//! Ветка: Rust-проход (Маркер В.0 -> возврат)
//! Зеркалит luck/lexer.py. Лексер не знает о графе, узлах, рёбрах как о
//! структурах — производит плоский поток токенов.

use crate::registry::{self, Kind};
use std::collections::BTreeMap;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenType {
    Node,
    Edges,
    End,
    Generative,
    External,
    Reject,
    Kind,
    SlotKeyword,
    ArrowSeq,
    ArrowBranch,
    ArrowMerge,
    Colon,
    Comma,
    LBracket,
    RBracket,
    At,
    Eq,
    Neq,
    Gt,
    Lt,
    Gte,
    Lte,
    Identifier,
    String,
    Number,
    Eof,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub ttype: TokenType,
    pub value: String,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug)]
pub struct LuckLexError {
    pub message: String,
    pub line: usize,
    pub col: usize,
}

impl std::fmt::Display for LuckLexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[лексер] {} (строка {}, столбец {})",
            self.message, self.line, self.col
        )
    }
}
impl std::error::Error for LuckLexError {}

static KEYWORDS: LazyLock<BTreeMap<&'static str, TokenType>> = LazyLock::new(|| {
    let mut kw: BTreeMap<&'static str, TokenType> = BTreeMap::new();
    kw.insert("NODE", TokenType::Node);
    kw.insert("EDGES", TokenType::Edges);
    kw.insert("END", TokenType::End);
    kw.insert("GENERATIVE", TokenType::Generative);
    kw.insert("EXTERNAL", TokenType::External);
    kw.insert("REJECT", TokenType::Reject);

    for k in Kind::ALL {
        kw.insert(k.as_str(), TokenType::Kind);
    }
    for word in registry::keywords() {
        kw.entry(word).or_insert(TokenType::SlotKeyword);
    }
    kw
});

pub struct Lexer<'a> {
    source: &'a [char],
    pos: usize,
    line: usize,
    col: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(source: &'a [char]) -> Self {
        Lexer {
            source,
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.source.get(self.pos + offset).copied()
    }

    fn advance(&mut self) -> char {
        let ch = self.source[self.pos];
        self.pos += 1;
        if ch == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        ch
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            match self.peek_at(0) {
                Some(c) if c == ' ' || c == '\t' || c == '\r' || c == '\n' => {
                    self.advance();
                }
                Some('#') => {
                    while let Some(c) = self.peek_at(0) {
                        if c == '\n' {
                            break;
                        }
                        self.advance();
                    }
                }
                _ => break,
            }
        }
    }

    pub fn tokenize(mut self) -> Result<Vec<Token>, LuckLexError> {
        let mut tokens = Vec::new();
        loop {
            self.skip_whitespace_and_comments();
            if self.pos >= self.source.len() {
                tokens.push(Token {
                    ttype: TokenType::Eof,
                    value: String::new(),
                    line: self.line,
                    col: self.col,
                });
                break;
            }

            let (start_line, start_col) = (self.line, self.col);
            let ch = self.peek_at(0).unwrap();

            if ch == '"' {
                tokens.push(self.read_string(start_line, start_col)?);
                continue;
            }
            if ch.is_ascii_digit() {
                tokens.push(self.read_number(start_line, start_col));
                continue;
            }
            if ch.is_alphabetic() || ch == '_' {
                tokens.push(self.read_identifier(start_line, start_col));
                continue;
            }

            let two_char: String = [ch, self.peek_at(1).unwrap_or('\0')].iter().collect();
            let two = match two_char.as_str() {
                "->" => Some(TokenType::ArrowSeq),
                "=>" => Some(TokenType::ArrowBranch),
                "~>" => Some(TokenType::ArrowMerge),
                "!=" => Some(TokenType::Neq),
                ">=" => Some(TokenType::Gte),
                "<=" => Some(TokenType::Lte),
                _ => None,
            };
            if let Some(tt) = two {
                self.advance();
                self.advance();
                tokens.push(Token {
                    ttype: tt,
                    value: two_char,
                    line: start_line,
                    col: start_col,
                });
                continue;
            }

            let single = match ch {
                ':' => Some(TokenType::Colon),
                ',' => Some(TokenType::Comma),
                '[' => Some(TokenType::LBracket),
                ']' => Some(TokenType::RBracket),
                '@' => Some(TokenType::At),
                '=' => Some(TokenType::Eq),
                '>' => Some(TokenType::Gt),
                '<' => Some(TokenType::Lt),
                _ => None,
            };
            if let Some(tt) = single {
                self.advance();
                tokens.push(Token {
                    ttype: tt,
                    value: ch.to_string(),
                    line: start_line,
                    col: start_col,
                });
                continue;
            }

            return Err(LuckLexError {
                message: format!("неожиданный символ {ch:?}"),
                line: start_line,
                col: start_col,
            });
        }
        Ok(tokens)
    }

    fn read_string(&mut self, line: usize, col: usize) -> Result<Token, LuckLexError> {
        self.advance(); // открывающая "
        let mut chars = String::new();
        loop {
            match self.peek_at(0) {
                None => {
                    return Err(LuckLexError {
                        message: "незакрытый строковый литерал".into(),
                        line,
                        col,
                    });
                }
                Some('"') => {
                    self.advance();
                    break;
                }
                Some(_) => chars.push(self.advance()),
            }
        }
        Ok(Token {
            ttype: TokenType::String,
            value: chars,
            line,
            col,
        })
    }

    fn read_number(&mut self, line: usize, col: usize) -> Token {
        let mut chars = String::new();
        while let Some(c) = self.peek_at(0) {
            if !c.is_ascii_digit() {
                break;
            }
            chars.push(self.advance());
        }
        Token {
            ttype: TokenType::Number,
            value: chars,
            line,
            col,
        }
    }

    fn read_identifier(&mut self, line: usize, col: usize) -> Token {
        let mut chars = String::new();
        while let Some(c) = self.peek_at(0) {
            if !(c.is_alphanumeric() || c == '_') {
                break;
            }
            chars.push(self.advance());
        }
        let ttype = KEYWORDS
            .get(chars.as_str())
            .copied()
            .unwrap_or(TokenType::Identifier);
        Token {
            ttype,
            value: chars,
            line,
            col,
        }
    }
}

pub fn tokenize(source: &str) -> Result<Vec<Token>, LuckLexError> {
    let chars: Vec<char> = source.chars().collect();
    Lexer::new(&chars).tokenize()
}

/// Экранирует произвольный текст для безопасной вставки внутрь Luck
/// STRING-литерала (между двойными кавычками). `read_string` выше НЕ
/// поддерживает escape-последовательности вообще — читает всё буквально
/// до следующей `"`. Значит кавычки и переносы строк внутри вставляемого
/// текста физически ломают исходник, если их не убрать заранее — не
/// «на всякий случай», а потому что реальные внешние данные (diff,
/// issue-текст) нередко содержат `"` (цитаты, пути в кавычках).
///
/// Найдено дважды независимо (run_batch.rs, потом интеграция pipegrab)
/// — вынесено сюда, в лексер, как владельца грамматики STRING, а не
/// оставлено копией в каждом вызывающем месте.
pub fn escape_string_literal(s: &str) -> String {
    s.replace('"', "'").replace(['\n', '\r'], " ")
}
