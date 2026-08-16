# Resource limits

Binary definitions and forensic input are not trusted. A corrupt length field
must not turn into an unbounded allocation or stream read. `cstruct-rs2` checks
offset arithmetic and applies explicit limits before materializing values.

These limits deliberately differ from current upstream source where upstream
would keep reading or allocating without a fixed ceiling. Inputs below the
limits keep the same values and cursor behavior.

## Arrays and nesting

A decoded array may contain at most 16,777,216 elements (`2**24`). The limit
applies to fixed, expression-sized, terminated, and read-to-end arrays. Array
byte sizes and offsets also use checked arithmetic, so an element count cannot
wrap around to a smaller allocation.

A compiled definition may nest structures up to 32 levels deep. Definitions
that exceed this limit fail while their plan is compiled, before any evidence
is read.

## Dynamic records read from streams

The size of a dynamic record may depend on bytes inside that record. For a
seekable stream, `cstruct-rs2` reads a bounded probe, resolves the layout, and
then leaves the public cursor immediately after the consumed record. Probe
read-ahead is not visible to the caller.

The default maximum dynamic record probe is 64 MiB. A deployment can change it
with `CSTRUCT_RS2_MAX_DYNAMIC_RECORD_BYTES`:

```console
export CSTRUCT_RS2_MAX_DYNAMIC_RECORD_BYTES=134217728
```

The value must be a decimal integer from 512 bytes through 1 GiB, inclusive. An
invalid setting raises `ValueError`. A record that needs more than the selected
limit also raises `ValueError` before that oversized read is attempted.

The probe limit applies to dynamic records read from seekable streams. A direct
`bytes`, `bytearray`, or buffer input is already bounded by the object supplied
by the caller and is decoded in place. A non-seekable stream cannot be probed
and rewound, so dynamic record reads from it fail explicitly.

If a dynamic stream read fails, `cstruct-rs2` restores the cursor to the
record's starting position. Current upstream reads directly from the stream and
may leave the cursor where a failing field stopped. The rewind makes it safe for
a caller to try another parser.

## Null-terminated scalar arrays

A standalone null-terminated scalar array scans at most 16 MiB. Seekable
streams are read in chunks no larger than 4 KiB and are repositioned just after
the terminator. Non-seekable streams read one complete element at a time so
they do not consume bytes belonging to the next value.

The all-zero terminator is consumed but is not included in the returned value.
Physical end of input before the terminator raises `EOFError`. Reaching the
scan limit without a terminator raises `ValueError`.

## Custom readers

Custom Python readers run inside the same dynamic-record probe budget. A custom
reader cannot cause the outer record to bypass the configured maximum. See
[Custom types](CUSTOM_TYPES.md) for callback and cursor behavior.
