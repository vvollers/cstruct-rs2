"""Public token values from the native cstruct definition lexer.

Upstream exposes its recursive-descent lexer's Python classes. rs2 keeps the
lexer in the Python-independent Rust core and exposes only the useful result:
immutable tokens with a category, source value, line and column. Definition
loading and :func:`tokenize` therefore agree on lexical rules without shipping
a second Python lexer.

The private cursor and lexer hierarchy used by upstream's parser compiler is
not reproduced. Code that needs to inspect a definition can use
:func:`tokenize`; code that needs to load it should use :class:`cstruct` or
``CStyleParser``.
"""

from __future__ import annotations

import enum
from dataclasses import dataclass

from dissect.cstruct._cstruct_rs2 import _tokenize


class TokenType(enum.Enum):
    """Categories used by the public token stream.

    The names and values follow current upstream so code can compare either
    ``token.type`` or ``token.type.value``. A native-only punctuation category
    is retained for characters that have no upstream public category.
    """

    IDENTIFIER = "IDENTIFIER"
    NUMBER = "NUMBER"
    STRING = "STRING"
    BYTES = "BYTES"
    LBRACE = "{"
    RBRACE = "}"
    LBRACKET = "["
    RBRACKET = "]"
    LPAREN = "("
    RPAREN = ")"
    SEMICOLON = ";"
    COMMA = ","
    COLON = ":"
    QUESTION = "?"
    STAR = "*"
    EQUALS = "="
    STRUCT = "STRUCT"
    UNION = "UNION"
    ENUM = "ENUM"
    FLAG = "FLAG"
    TYPEDEF = "TYPEDEF"
    SIZEOF = "SIZEOF"
    OFFSETOF = "OFFSETOF"
    PLUS = "+"
    MINUS = "-"
    UNARY_MINUS = "-u"
    SLASH = "/"
    PERCENT = "%"
    AMPERSAND = "&"
    PIPE = "|"
    CARET = "^"
    TILDE = "~"
    LSHIFT = "<<"
    RSHIFT = ">>"
    PP_DEFINE = "PP_DEFINE"
    PP_UNDEF = "PP_UNDEF"
    PP_IFDEF = "PP_IFDEF"
    PP_IFNDEF = "PP_IFNDEF"
    PP_ELSE = "PP_ELSE"
    PP_ENDIF = "PP_ENDIF"
    PP_INCLUDE = "PP_INCLUDE"
    PP_FLAGS = "PP_FLAGS"
    EOF = "EOF"
    PUNCT = "PUNCT"


IDENTIFIER_TYPES = {
    TokenType.STRUCT,
    TokenType.UNION,
    TokenType.ENUM,
    TokenType.FLAG,
    TokenType.TYPEDEF,
    TokenType.SIZEOF,
    TokenType.OFFSETOF,
    TokenType.IDENTIFIER,
}


@dataclass(frozen=True, slots=True)
class Token:
    """One immutable token and its 1-based source location."""

    type: TokenType
    value: str
    line: int
    column: int = 0

    def __repr__(self) -> str:
        return f"<Token.{self.type.name} value={self.value!r} line={self.line}>"


def tokenize(data: str) -> list[Token]:
    """Tokenise a complete definition with the same Rust lexer used by loading."""
    return [
        Token(TokenType[kind], value, line, column) for kind, value, line, column in _tokenize(data)
    ]


def format_error_context(data: str, lineno: int, window: int = 1) -> str:
    """Return the source excerpt used in parser diagnostics.

    Current upstream includes the failing line and two preceding lines for its
    default window. The native parser formats its own errors the same way; this
    helper remains public for callers that build related diagnostics.
    """
    start = max(0, lineno - window - 2)
    context = data.splitlines()[start:lineno]
    width = len(str(lineno)) + 2
    return "\n".join(f"{start + index + 1:>{width}}: {line}" for index, line in enumerate(context))


__all__ = ["IDENTIFIER_TYPES", "Token", "TokenType", "format_error_context", "tokenize"]
