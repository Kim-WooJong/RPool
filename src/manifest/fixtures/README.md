# Compatibility golden fixtures

Synthetic v1 (omitted defaults), v2 uncoded (coding:null), v2 RS (1+1).
Logical data is ASCII abc. No user archive or secret is included.
Roots are fixed using independently constructed Python struct.pack LE preimages
following compatibility.md, hashed with the existing BLAKE3 crate using a standalone
Rust utility, not content_root_v1/v2. Core root source matches the original input ZIP
(see docs/architecture/step10-baseline-audit.json). Tests never regenerate expected roots.
UTF-8 addresses, repeated separators, percent text and literal backslash are intentional.
Fixtures retain a final newline and compact key order to catch reserialization.
