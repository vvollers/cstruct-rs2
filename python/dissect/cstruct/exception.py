"""Exception types at current upstream's canonical singular import path.

``dissect.cstruct.exceptions`` is a compatibility alias that re-exports these
same classes. Two separately defined hierarchies with equal names would make an
``isinstance`` check depend on which spelling a consumer imported.

# On the ``ValueError`` mixin

Upstream derives these from ``Error(Exception)`` alone. Here each leaf also
inherits ``ValueError``.

A bad definition or unusable length is also an invalid value at a broader API
boundary. The mixin lets callers choose either level: ``except ParserError``
and ``except ValueError`` both catch a parse failure, while ``except Error``
still catches every cstruct-specific error.
"""

from __future__ import annotations


class Error(Exception):
    """Base for every cstruct error."""


class LexerError(Error, ValueError):
    """A definition could not be tokenised."""


class ParserError(Error, ValueError):
    """A definition could not be parsed."""


class ResolveError(Error, ValueError):
    """A type name could not be resolved."""


class ArraySizeError(Error, ValueError):
    """An array length was not usable."""


class ExpressionParserError(Error, ValueError):
    """An expression could not be parsed."""


class ExpressionTokenizerError(Error, ValueError):
    """Compatibility-only name for an expression-tokenizer failure.

    Current upstream exposes :class:`LexerError` instead. No parser path here
    raises this separate class, but keeping the importable name avoids inventing
    a second meaning for it.
    """


class NullPointerDereference(Error, ValueError):  # noqa: N818
    """A null pointer was dereferenced.

    The name has no ``Error`` suffix because upstream's does not, and the class
    name is what a consumer catches. Renaming it to satisfy the convention would
    break every ``except NullPointerDereference``.
    """


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
