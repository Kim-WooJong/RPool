# Native mount with rclone-compatible crypt

Updated: 2026-09-29. This is a plan. It extends Phases 2 and 3 of
`MOUNT_WRITE_ROADMAP.md` and does not replace them.

## Goal

Mount an RPool drive with RPool's own OS frontend, without rclone mount, rclone
VFS or the loopback WebDAV server. Encryption must work the way it does with
rclone `crypt` today:

- Existing archives stored through rclone crypt remotes stay readable.
- Objects written by RPool stay readable with the same rclone crypt remote.

## Layering

```text
OS  ──  WinFsp (Windows) │ FUSE (Linux) │ NFS or FSKit (macOS)     frontends
          │
     filesystem core: lookup/open/read_at/write_at/truncate/flush/release
          │                                            (roadmap Phase 2)
     VirtualDrive: namespace, revisions, spool, quota, peer/pool sync
          │
     storage backend (per pool)
       ├─ rclone crypt remote (current behaviour; the default)
       └─ native crypt ─ transport to the crypt's *base* remote
                          ├─ rclone (subprocess now, rcd sidecar later)
                          └─ OpenDAL, opt-in, only for S3/WebDAV/Koofr-class providers
```

The frontend and crypt are independent layers. A native mount can still use
rclone crypt remotes, and native crypt can still sit behind the WebDAV
frontend. Each layer ships and is verified separately.

## Crypt design decisions

- **Format.** Byte-compatible with rclone `backend/crypt`:
  - scrypt key derivation (N=16384, r=8, p=1) into 80 bytes: data key, name key and name tweak.
  - Header `RCLONE\0\0` followed by a 24-byte nonce.
  - XSalsa20-Poly1305 secretbox over each 64 KiB block.
  - Name encryption: standard (EME-AES-256 with base32hex, base64 or base32768), obfuscate, or off.
- **Keys.** Read from the existing crypt remote through `rclone config dump`, which `config_sync` already does. They are revealed in memory only and never logged or persisted. RPool never writes `rclone.conf`.
- **Unsupported settings are refused, never silently mishandled:** `no_data_encryption`, unknown modes or encodings, and nested remote chains.
- **Compatibility is enforced by tests:** rclone's published vectors, plus round-trip oracle tests against the installed rclone on a local directory in both directions. No cloud access is needed.
- **The existing write gate stays.** Uploads must still land on encrypted storage. With native crypt, that guarantee moves from "destination is a crypt remote" to "RPool encrypted the bytes".

## Milestones

| # | Deliverable | Exit gate |
| --- | --- | --- |
| M1 ✅ | `src/crypt`: format library (keys, reveal, streaming data, names, sizes, ranged reads) | Two-way rclone oracle tests pass (rclone 1.75.1, 2026-09-29). No production caller yet. |
| M2 ✅ | Native-crypt storage backend over the base remote. Opt-in per pool. | Objects are interchangeable with the rclone crypt remote. Ranged reads verified. Fault tests pass. |
| M3 | Protocol-independent filesystem core (roadmap Phase 2) | The Phase 2 exit gate in `MOUNT_WRITE_ROADMAP.md` |
| M4 | Windows WinFsp frontend (`winfsp_wrs`, MIT). Read-only first, then writable. | Windows machine: listing, reads, stop, and small writes with remount and recovery |
| M5 | Linux FUSE (`fuser`) | Linux machine |
| M6 | macOS frontend, only after the wedged test mount is cleared by a reboot | New workspace. Clean and uncertain stop measured. |

WebDAV stays the default until M4 or later passes the fresh 4 GiB 9+3 cloud
round-trip gate described in the roadmap.

## M1 status (2026-09-29)

Done in `src/crypt`: obscure/reveal, scrypt keys (default salt or `password2`),
streaming encrypt/decrypt with 64 KiB secretbox blocks, ranged reads from any
plaintext offset, encrypted/decrypted size, and names in `standard` (base32 or
base64, with or without directory name encryption), `obfuscate` and `off`
modes. `CryptConfig` parses a `rclone config dump` crypt section.

Verified by `cargo test --bin rpool -- --ignored crypt::oracle` against the
installed rclone on local directories: obscure both ways, names identical to
`rclone cryptdecode --reverse`, objects written by rclone decrypt in RPool
(including ranged reads), and objects written by RPool read back with
`rclone cat`/`lsf`. No cloud access.

Refused, not implemented: `filename_encoding = base32768`,
`no_data_encryption`, `pass_bad_blocks`, and unknown options. Version-suffixed
names (`--b2-versions`) are not handled. Name decoding is stricter than Go's
about non-canonical trailing bits; rclone never produces those.

Instead of the published rclone test vectors, the rclone binary itself serves
as the reference.

## M2 status (2026-09-29)

Done for `put` and pool reprocess. A pool opts in with `native_crypt`
(`pool set --native-crypt`, GUI "Encrypt in RPool"). Shards are then encrypted
by `CryptBackend` and written with `rclone rcat` to the crypt remote's base.
The writer's readback still goes through the rclone crypt remote, so every
native write is proven readable by rclone before it counts.

The write gate (`src/storage/native_crypt/route.rs`) allows a destination only
when it is a configured crypt remote that `CryptConfig` fully supports, its
base is a plain configured remote (not crypt, alias, union, combine, chunker,
hasher, compress or cache, and not a `:backend:` string), and no
`RCLONE_CRYPT_*` or `RCLONE_CONFIG_*` override except `RCLONE_CONFIG_PASS` is
set. Crypt options that are present with an empty value are refused rather
than read as defaults. Keys that are non-ASCII or not canonical (`a//b`, `./a`,
`a/`) use the gated rclone crypt route instead. The config dump is read once
per command, and failures are not cached.

Verified: gate unit tests, the M2a fault and ranged-read tests, and an ignored
end-to-end test (`native_writer_objects_read_back_through_rclone_crypt`) in
which a native writer publishes onto a local base, `rclone cat` returns the
same bytes, and the base holds only `RCLONE\0\0` ciphertext under encrypted
names. No cloud provider was used.

Not covered: mounts, repair, scrub, migrate and manifest replication still
write through rclone crypt. Objects are interchangeable, so mixing is safe.
Server-side copy and rename through `CryptBackend` are not wired.
