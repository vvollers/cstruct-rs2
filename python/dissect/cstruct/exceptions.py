"""Compatibility alias for :mod:`dissect.cstruct.exception`.

Current upstream uses the singular module. This plural path re-exports the same
objects instead of defining a second exception hierarchy.

These are re-exports, not redefinitions. A consumer that catches
``dissect.cstruct.exceptions.ParserError`` and a consumer that catches
``dissect.cstruct.exception.ParserError`` must be catching one class, or one of
them silently stops working.
"""

from __future__ import annotations

from dissect.cstruct.exception import (
    ArraySizeError,
    Error,
    ExpressionParserError,
    ExpressionTokenizerError,
    LexerError,
    NullPointerDereference,
    ParserError,
    ResolveError,
)

__all__ = [
    "ArraySizeError",
    "Error",
    "ExpressionParserError",
    "ExpressionTokenizerError",
    "LexerError",
    "NullPointerDereference",
    "ParserError",
    "ResolveError",
]
