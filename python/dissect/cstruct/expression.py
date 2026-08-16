"""Length and constant expression evaluation.

`Expression(text).evaluate(cs)` parses an expression once and evaluates it
against a namespace's constants. It is the same Rust evaluator that resolves
`char name[n]` while decoding, exposed under upstream's import path rather than
reimplemented -- expression *semantics* are in scope for this project precisely
because they decide array lengths, and two implementations of the same grammar
would be two places for it to drift.

Upstream builds its `Expression` on a Python tokenizer of its own. This one has
no equivalent to expose -- the tokenizing happens in Rust -- so what matches is
the behaviour rather than the internals: the same text evaluates to the same
number, and a malformed expression raises the same class.
"""

from __future__ import annotations

from dissect.cstruct._cstruct_rs2 import Expression

__all__ = ["Expression"]
