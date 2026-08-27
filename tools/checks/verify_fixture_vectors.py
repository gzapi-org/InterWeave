#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
# tools/checks/verify_fixture_vectors.py
#
# >>> help
# Recompute every frozen conformance vector under fixtures/ from its
# declared algorithm and compare it to the stored value.
#
#   tools/checks/verify_fixture_vectors.py
#   tools/checks/verify_fixture_vectors.py --root <dir>
#
# A frozen vector nobody recomputes is a number in a file. These vectors
# are what independent implementations agree on, so a silent drift — a
# domain prefix edited, a length field widened, a canonicalization
# "clarified" — is a protocol break that no test would otherwise catch.
# The check exists so that changing a vector requires deciding to change
# it (CLAUDE.md §9, ADR-0049).
#
# A fixture file declares `algorithm.id`; this script implements that id
# independently, from the specification rather than from the fixture. An
# unknown id is a FAILURE, not a skip: a vector file that nothing can
# verify is exactly the case this guards against.
#
# Vectors carrying `frozen_by` are goldens re-frozen by an ADR. Their
# hashes are additionally required to be distinct from every other vector
# in the file, because collisions between the edge cases are the bug the
# edge cases exist to catch.
#
# PROSE COPIES are checked too. A golden is quoted in documents as well as
# stored here — ADR-0047 lists the re-frozen values, ENDPOINTS.md shows
# one inline, testing.md cites it in a conformance list. Only the vector
# file is recomputed, so a legitimate re-freeze would leave those copies
# confidently wrong in the documents a reader trusts most. Any file
# quoting a 64-hex value within a few lines of a golden's own inputs must
# quote a value this fixture actually holds.
#
# Options:
#   --root <dir>   verify this repository instead of the one containing
#                  this script
#   -h, --help     this text
#
# Exit codes:
#   0  every vector recomputes to its stored value
#   1  a vector drifted, or a file declares an algorithm this cannot verify
#   2  invocation problem, or fixtures/ is missing
# <<< help

import hashlib
import json
import pathlib
import re
import sys

HELP_RE = re.compile(r"^# >>> help$(.*?)^# <<< help$", re.M | re.S)

problems: list[str] = []


def report(msg: str) -> None:
    problems.append(msg)
    print(msg)


def show_help() -> None:
    src = pathlib.Path(__file__).read_text(encoding="utf-8")
    m = HELP_RE.search(src)
    for line in (m.group(1) if m else "").splitlines():
        print(line[2:] if line.startswith("# ") else line.lstrip("#"))


# --- algorithms -----------------------------------------------------------
# Implemented from architecture/contracts/ENDPOINTS.md, deliberately NOT
# from the fixture's own description: a verifier that reads its rule from
# the artifact it checks proves nothing.

DIRECT_CONTENT_FINGERPRINT_V1_DOMAIN = b"interweave/direct-content-fingerprint/v1\x00"


def direct_content_fingerprint_v1(vector: dict) -> str:
    media = vector.get("media_type")
    payload = bytes.fromhex(vector.get("payload_hex", ""))

    buf = DIRECT_CONTENT_FINGERPRINT_V1_DOMAIN
    if media is None:
        buf += b"\x00"
    else:
        m = media.encode("ascii")
        if not 1 <= len(m) <= 128:
            raise ValueError(
                f"media type is {len(m)} bytes; the contract allows 1..128, "
                "and empty is invalid rather than absent"
            )
        buf += b"\x01" + len(m).to_bytes(2, "big") + m
    buf += len(payload).to_bytes(4, "big") + payload
    return hashlib.sha256(buf).hexdigest()


def direct_message_v2_frame(vector: dict) -> str:
    """Encode one DirectMessageV2 request frame, returning hex.

    From architecture/transport/libp2p/DIRECT.md §Request. Multi-byte
    integers are big-endian, which that document pins explicitly — it did
    not until these vectors forced the question, and the answer had to
    match the IPC length prefix and the content fingerprint or the three
    would disagree about the same repository's byte order.
    """
    mid = bytes.fromhex(vector["message_id"])
    if len(mid) != 16:
        raise ValueError(f"message_id is {len(mid)} bytes; the frame carries exactly 16 (128 bits)")

    out = mid + int(vector["sent_at_ms"]).to_bytes(8, "big")

    src = vector["source_endpoint"].encode("ascii")
    if not 1 <= len(src) <= 64:
        raise ValueError("source_endpoint is 1..64 bytes and is always present")
    out += bytes([len(src)]) + src

    dst = vector.get("destination_endpoint")
    dst_b = dst.encode("ascii") if dst is not None else b""
    if len(dst_b) > 64:
        raise ValueError("destination_endpoint exceeds 64 bytes")
    out += bytes([len(dst_b)]) + dst_b

    media = vector.get("media_type")
    media_b = media.encode("ascii") if media is not None else b""
    if media is not None and not 1 <= len(media_b) <= 128:
        raise ValueError("a present media type is 1..128 bytes; empty is absence, not a value")
    out += bytes([len(media_b)]) + media_b

    payload = bytes.fromhex(vector.get("payload_hex", ""))
    out += len(payload).to_bytes(4, "big") + payload

    # `frame_len` is stored beside `frame_hex` as a reader convenience, so
    # it is recomputed too. A stored number nobody checks is the exact
    # thing this script exists to prevent — and a length that disagrees
    # with its own frame is the most quietly misleading kind, because the
    # frame stays correct while the documentation of it does not.
    stated_len = vector.get("frame_len")
    if stated_len is not None and stated_len != len(out):
        raise ValueError(f"frame_len disagrees: stored {stated_len}, computed {len(out)}")
    return out.hex()


def broadcast_message_v1_frame(vector: dict) -> str:
    """Encode one BroadcastMessageV1 envelope, returning hex.

    From architecture/transport/libp2p/PUBSUB.md §Envelope. The same
    big-endian discipline as the direct frame, and the same two absences:
    `media_type_len = 0` is ABSENCE rather than an empty string, and a
    `payload_len` of zero is still written.

    Two differences from `DirectMessageV2`, both deliberate. There are no
    endpoint fields, because ADR-0030 keeps EndpointId out of broadcast
    entirely. And there is a leading `version` byte, because direct takes
    its version from the negotiated protocol name `/interweave/direct/
    2.0.0` while a GossipSub topic negotiates nothing — the envelope is
    the only place a broadcast reader can learn what it is holding.
    """
    version = int(vector["version"])
    if version != 1:
        raise ValueError(f"version is {version}; BroadcastMessageV1 frames are version 1")
    out = bytes([version])

    mid = bytes.fromhex(vector["message_id"])
    if len(mid) != 16:
        raise ValueError(f"message_id is {len(mid)} bytes; the envelope carries exactly 16 (128 bits)")
    out += mid

    sent_at = int(vector["sent_at_ms"])
    if not 0 <= sent_at < 2**64:
        raise ValueError(f"sent_at_ms {sent_at} does not fit the u64 the wire carries")
    out += sent_at.to_bytes(8, "big")

    media = vector.get("media_type")
    media_b = media.encode("ascii") if media is not None else b""
    if media is not None and not 1 <= len(media_b) <= 128:
        raise ValueError("a present media type is 1..128 bytes; empty is absence, not a value")
    out += bytes([len(media_b)]) + media_b

    payload = bytes.fromhex(vector.get("payload_hex", ""))
    out += len(payload).to_bytes(4, "big") + payload

    # Recomputed for the same reason the direct frame recomputes it: a
    # stored length nobody checks is what this script exists to prevent.
    stated_len = vector.get("frame_len")
    if stated_len is not None and stated_len != len(out):
        raise ValueError(f"frame_len disagrees: stored {stated_len}, computed {len(out)}")
    return out.hex()


def _base58btc(data: bytes) -> str:
    alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
    n = int.from_bytes(data, "big")
    out = ""
    while n:
        n, r = divmod(n, 58)
        out = alphabet[r] + out
    # Every leading ZERO BYTE encodes as a leading '1'. The identity
    # multihash prefix is 0x00, so dropping this loses the '1' that
    # begins every Ed25519 PeerId — a mistake that produces a
    # plausible-looking value, which is the dangerous kind.
    return "1" * (len(data) - len(data.lstrip(b"\x00"))) + out


def ed25519_bip39_entropy_v1(vector: dict) -> str:
    """entropy -> Ed25519 public key -> libp2p PeerId.

    From architecture/contracts/IDENTITY-RECOVERY.md. The entropy IS the
    Ed25519 secret seed: no PBKDF2, no passphrase, no further KDF. That
    is the single most important property to keep verified, because a
    wallet-style derivation would also produce a valid-looking PeerId —
    just not the right one, and not recoverable by anyone else.
    """
    try:
        from cryptography.hazmat.primitives import serialization
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
    except ImportError as e:  # pragma: no cover - environment problem, not drift
        raise RuntimeError(f"python3 cryptography is required to verify this vector: {e}") from e

    entropy = bytes.fromhex(vector["entropy_hex"])
    if len(entropy) != 32:
        raise ValueError(f"entropy is {len(entropy)} bytes; this format is exactly 32 (256 bits)")

    # Word indexes are checked here too: entropy || 8-bit checksum, split
    # into 24 eleven-bit indexes. Resolving them to words needs the
    # 2048-word list, which is not vendored for one vector.
    checksum = hashlib.sha256(entropy).digest()[0]
    bits = "".join(f"{b:08b}" for b in entropy) + f"{checksum:08b}"
    indexes = [int(bits[i * 11:(i + 1) * 11], 2) for i in range(24)]
    stated = vector.get("word_indexes")
    if stated is not None and stated != indexes:
        raise ValueError(f"word indexes disagree: stored {stated[:3]}…{stated[-1]}, computed {indexes[:3]}…{indexes[-1]}")

    public = Ed25519PrivateKey.from_private_bytes(entropy).public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    )
    stated_pub = vector.get("ed25519_public_key_hex")
    if stated_pub is not None and stated_pub != public.hex():
        raise ValueError(f"public key disagrees: stored {stated_pub}, computed {public.hex()}")

    protobuf = bytes([0x08, 0x01, 0x12, len(public)]) + public
    return _base58btc(bytes([0x00, len(protobuf)]) + protobuf)


def _base58btc_decode(text: str) -> bytes:
    """Inverse of _base58btc. Needed because a PeerId reaches a fixture in
    its printable form, while every derivation that consumes one hashes
    `PeerId::to_bytes()` — the raw multihash. Decoding here rather than
    storing the bytes alongside keeps one source of truth per vector: a
    stored byte copy could drift from the printable form it claims to be.
    """
    alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
    n = 0
    for ch in text:
        i = alphabet.find(ch)
        if i < 0:
            raise ValueError(f"'{ch}' is not a base58btc character")
        n = n * 58 + i
    body = n.to_bytes((n.bit_length() + 7) // 8, "big") if n else b""
    # Leading '1's are leading zero bytes; int arithmetic discards them,
    # and the identity-multihash prefix 0x00 is exactly one of them.
    return b"\x00" * (len(text) - len(text.lstrip("1"))) + body


GOSSIPSUB_MESSAGE_ID_V1_DOMAIN = b"interweave/gossipsub-message-id/v1\x00"
TOPIC_KEY_V1_DOMAIN = b"interweave/topic/v1\x00"
KAD_NETWORK_V1_DOMAIN = b"interweave/kad-network/v1\x00"


def gossipsub_message_id_v1(vector: dict) -> str:
    """From architecture/transport/libp2p/PUBSUB.md §Mesh-level message identity.

    Binds the authenticated source PeerId and the GossipSub WIRE sequence
    number, never the application envelope `message_id` — ADR-0004 makes
    keying mesh duplicate suppression on that envelope field illegal,
    because two publishers may legitimately choose the same 128 bits.
    """
    source = _base58btc_decode(vector["peer_id"])
    seq = int(vector["sequence_number"])
    if not 0 <= seq < 2**64:
        raise ValueError(f"sequence_number {seq} does not fit the u64 the wire carries")
    canonical = (
        GOSSIPSUB_MESSAGE_ID_V1_DOMAIN
        + len(source).to_bytes(2, "big")
        + source
        + seq.to_bytes(8, "big")
    )
    return hashlib.sha256(canonical).hexdigest()


def gossipsub_topic_key_v1(vector: dict) -> str:
    """From architecture/transport/libp2p/PUBSUB.md §Topic mapping.

    sha256(domain || channel_id_ascii). The hash keeps raw channel names
    off the wire; it does NOT resist dictionary guessing of a
    low-entropy name, and the specification says so rather than implying
    secrecy the construction does not provide.
    """
    channel = vector["channel_id"]
    raw = channel.encode("ascii")
    if not 1 <= len(raw) <= 128:
        raise ValueError(f"ChannelId is {len(raw)} bytes; the contract allows 1..128")
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._:/-]{0,127}", channel):
        raise ValueError(f"ChannelId '{channel}' does not match the ADR-0025 grammar")
    return hashlib.sha256(TOPIC_KEY_V1_DOMAIN + raw).hexdigest()


def kad_network_namespace_v1(vector: dict) -> dict[str, str]:
    """From architecture/docs/architecture/kademlia-integration.md §4.

    Lowercase unpadded RFC4648 base32 of the FIRST 16 BYTES of the
    digest, not the whole thing: the truncation is what keeps the
    protocol string short, and a verifier that hashed the full digest
    would produce a plausible-looking namespace nobody else computes.
    """
    import base64

    network_id = vector["network_id"]
    if not re.fullmatch(r"[a-z0-9][a-z0-9._-]{0,63}", network_id):
        raise ValueError(f"network_id '{network_id}' is not lower-case ASCII in the allowed grammar")
    digest = hashlib.sha256(KAD_NETWORK_V1_DOMAIN + network_id.encode("ascii")).digest()
    namespace = base64.b32encode(digest[:16]).decode("ascii").rstrip("=").lower()
    # BOTH fields, because the fixture freezes both. Verifying only the
    # hash left `protocol` unchecked: replacing a golden's protocol with
    # `/definitely/wrong` passed, and consumers read that field as a
    # frozen value on the same authority as the hash beside it.
    return {
        "network_hash": namespace,
        "protocol": f"/interweave/kad/1.0.0/{namespace}",
    }


# --- validation verdicts --------------------------------------------------
# These reproduce a `valid` boolean rather than a digest. Their vectors
# deliberately repeat results (see ALGORITHMS), and each implements the
# rule from its contract so a grammar loosened in prose without loosening
# the fixture — or the reverse — is caught.

ENDPOINT_ID_RE = re.compile(r"^[a-z][a-z0-9._-]{0,63}$")


def endpoint_id_grammar_v1(vector: dict) -> bool:
    """From architecture/contracts/ENDPOINTS.md §EndpointId.

    ASCII, 1..64 bytes, lower-case canonical. The anchored regex already
    bounds the length, but the byte check is kept explicit because the
    contract states both and a future grammar edit could drop the bound
    from one without the other.
    """
    value = vector["endpoint_id"]
    try:
        raw = value.encode("ascii")
    except UnicodeEncodeError:
        return False
    return bool(1 <= len(raw) <= 64 and ENDPOINT_ID_RE.fullmatch(value))


HEX32_RE = re.compile(r"^[0-9a-f]{32}$")
SENT_AT_MS_MAX = 253402300799999


def human_chat_v2_envelope(vector: dict) -> bool:
    """From architecture/clients/human/HUMAN-CHAT.md (ADR-0050).

    Shape and grammar only. The markdown SUBSET is a rendering contract,
    not an envelope-validity one — out-of-subset markdown falls back to
    plain-text display rather than rejecting the message — so it is not
    decided here, and a vector may not claim it is.
    """
    env = vector["envelope"]
    if not isinstance(env, dict):
        return False
    if env.get("v") != 2 or env.get("kind") != "text":
        return False
    mid = env.get("app_message_id")
    if not isinstance(mid, str) or not HEX32_RE.fullmatch(mid):
        return False
    # `text` is REQUIRED. The canonical envelope marks optional fields with
    # `?` and this is not one of them; a v2 object carrying only v, kind and
    # app_message_id is a text message with no text, which no client can
    # render as the envelope it claims to be.
    if not isinstance(env.get("text"), str):
        return False
    reply_to = env.get("reply_to")
    if reply_to is not None and (not isinstance(reply_to, str) or not HEX32_RE.fullmatch(reply_to)):
        return False
    sent = env.get("sent_at_ms")
    if sent is not None:
        if isinstance(sent, bool) or not isinstance(sent, int):
            return False
        if not 0 <= sent <= SENT_AT_MS_MAX:
            return False
    src = env.get("from_endpoint")
    if src is not None and not endpoint_id_grammar_v1({"endpoint_id": src}):
        return False
    return True


def config_v2_cross_field(vector: dict) -> bool:
    """From architecture/config/config.schema.yaml §endpoints cross-field.

    The five rules stated there, in order: unique endpoint IDs; a set
    default naming an ENABLED endpoint; static-subset peers being a
    subset of profile trust; endpoint policy narrowing rather than
    widening; and advertised entries fitting the directory bound. Each is
    a relationship BETWEEN fields, which is exactly what a JSON Schema
    cannot express and why these vectors exist beside the schema.
    """
    cfg = vector["config"]
    trust = set(cfg.get("trust", {}).get("allowed_peers", []))
    endpoints = cfg.get("endpoints", {})
    entries = endpoints.get("entries", [])

    ids = [e.get("id") for e in entries]
    if len(ids) != len(set(ids)):
        return False

    default = endpoints.get("default_direct_endpoint")
    if default is not None:
        match = [e for e in entries if e.get("id") == default]
        if not match or not match[0].get("enabled", True):
            return False

    for e in entries:
        for direction in ("inbound", "outbound"):
            policy = e.get(direction)
            # The shape frozen by endpoints/endpoint-config.schema.json: the
            # bare string "inherit_profile_trust", or {"static_subset": [...]}.
            if isinstance(policy, dict) and "static_subset" in policy:
                # Narrowing means a subset. A peer here that profile trust
                # does not hold would WIDEN it, which ADR-0012 forbids
                # regardless of how the endpoint is configured.
                if not set(policy["static_subset"]) <= trust:
                    return False
            elif policy is not None and policy != "inherit_profile_trust":
                # An unrecognised policy shape is a failure, not a pass. A
                # verifier that ignored it would silently approve exactly
                # the drift this file exists to catch.
                return False

    max_advertised = endpoints.get("directory", {}).get("max_advertised", 16)
    advertised = [e for e in entries if e.get("advertise") and e.get("enabled", True)]
    if len(advertised) > max_advertised:
        return False

    return True


# --- IPC payload fit ------------------------------------------------------

_B64_CHARS_PER_3_BYTES = 4


def _canonical_json_len(obj: dict) -> int:
    """Length of the compact canonical serialization this fixture pins.

    Separators without spaces, keys in the order the schema declares
    them (`sort_keys=False` preserves insertion order). The IPC contract
    does not pin a canonical JSON form — it does not need to, because the
    ceiling is a bound rather than an equality — so the fixture declares
    the form it measures instead of pretending the contract chose one.
    """
    return len(json.dumps(obj, separators=(",", ":"), ensure_ascii=False))


def ipc_v2_payload_fit(vector: dict) -> str:
    """From architecture/contracts/LOCAL-IPC.md §Framing / §Payload-fit invariant.

    Builds the maximal legal object for one direction and returns its
    4-byte big-endian frame length prefix as hex — the same encoding the
    wire uses, so the vector's stored value IS the number the framing
    layer would emit.

    The measured object is the schema-defined one (`ipc.send-params` or
    `endpoints.message-received`). The outer request/event envelope is
    not modelled by any schema, so its overhead is reported by the
    vector's own `envelope_headroom_bytes` rather than invented here.
    """
    payload_bytes = int(vector["payload_bytes"])
    if payload_bytes > 49152:
        raise ValueError(f"{payload_bytes} exceeds the 49,152-byte ADR-0026 payload ceiling")

    # Unpadded base64url: every 3 bytes become 4 characters, and a 1- or
    # 2-byte tail becomes 2 or 3. This is computed rather than encoded
    # because the length is the invariant, not the content.
    whole, tail = divmod(payload_bytes, 3)
    b64_len = whole * _B64_CHARS_PER_3_BYTES + (0 if tail == 0 else tail + 1)
    stated_b64 = vector.get("base64url_chars")
    if stated_b64 is not None and stated_b64 != b64_len:
        raise ValueError(f"base64url length disagrees: stored {stated_b64}, computed {b64_len}")

    def field(n: int, ch: str = "a") -> str:
        return ch * n

    direction = vector["direction"]
    payload = {
        "media_type": field(vector["media_type_bytes"], "x"),
        "bytes": field(b64_len, "A"),
    }
    if direction == "send-params":
        obj = {
            "peer": field(vector["peer_id_bytes"], "P"),
            "endpoint": field(vector["destination_endpoint_bytes"], "d"),
            "payload": payload,
            "message_id": field(32, "0"),
        }
    elif direction == "message-received":
        obj = {
            "message_id": field(32, "0"),
            "mode": "direct",
            "source_peer": field(vector["peer_id_bytes"], "P"),
            "source_endpoint": field(vector["source_endpoint_bytes"], "s"),
            "destination_endpoint": field(vector["destination_endpoint_bytes"], "d"),
            "payload": payload,
            "received_at": int(vector["received_at"]),
        }
    else:
        raise ValueError(f"unknown direction '{direction}'")

    body_len = _canonical_json_len(obj)
    stated_len = vector.get("body_bytes")
    if stated_len is not None and stated_len != body_len:
        raise ValueError(f"body length disagrees: stored {stated_len}, computed {body_len}")

    ceiling = 131072
    if body_len > ceiling:
        raise ValueError(
            f"{direction} maximal body is {body_len} bytes, over the {ceiling}-byte "
            "IPC frame ceiling — the payload-fit invariant is broken"
        )
    stated_headroom = vector.get("envelope_headroom_bytes")
    if stated_headroom is not None and stated_headroom != ceiling - body_len:
        raise ValueError(
            f"headroom disagrees: stored {stated_headroom}, computed {ceiling - body_len}"
        )
    return body_len.to_bytes(4, "big").hex()


# id -> (function, the vector field holding the value it must reproduce,
#        whether distinct inputs must produce distinct results).
# The field is declared rather than guessed from the name: these
# algorithms produce a digest, an encoding, and an identifier
# respectively, and inferring that from a suffix was a hack waiting to
# mislabel the fourth one.
#
# DISTINCTNESS IS PER-ALGORITHM, not universal. For a derivation, two
# vectors sharing a result means the edge cases stopped distinguishing
# anything — the exact bug the edge cases exist to catch. For a
# VALIDATION verdict the opposite holds: a grammar file's whole purpose
# is many inputs mapping onto `true` and many onto `false`, so an
# unconditional collision check would make a correct verdict set
# unrepresentable. Flag it per algorithm rather than letting the shape
# of the first three decide for every later one.
# The fourth element is the INPUT FIELDS a prose copy of a golden is
# recognised by, and it is declared rather than sniffed.
#
# `golden_marker` used to look only for `payload_hex`/`payload_utf8`, so
# every algorithm whose input is something else — a PeerId and a sequence
# number, a channel id, a network id — produced no marker at all. The
# prose scan then matched nothing for those files and reported the same
# cheerful count as for the ones it really checked, which is worse than
# not scanning: a stale hash in PUBSUB.md or ADR-0047 read as covered.
#
# An empty tuple means the goldens in that file are not quoted in prose.
# Saying so is a decision; leaving it to be inferred was the bug.
ALGORITHMS = {
    "direct-content-fingerprint-v1": (
        direct_content_fingerprint_v1, "sha256", True, ("payload_hex", "payload_utf8"),
    ),
    "broadcast-message-v1-frame": (
        broadcast_message_v1_frame, "frame_hex", True, ("payload_hex", "payload_utf8"),
    ),
    "direct-message-v2-frame": (
        direct_message_v2_frame, "frame_hex", True, ("payload_hex", "payload_utf8"),
    ),
    "ed25519-bip39-entropy-v1": (
        ed25519_bip39_entropy_v1, "expected_peer_id", True, ("entropy_hex",),
    ),
    "gossipsub-message-id-v1": (
        gossipsub_message_id_v1, "sha256", True, ("peer_id",),
    ),
    "gossipsub-topic-key-v1": (
        gossipsub_topic_key_v1, "sha256", True, ("channel_id",),
    ),
    "kad-network-namespace-v1": (
        kad_network_namespace_v1, ("network_hash", "protocol"), True, ("network_id",),
    ),
    "ipc-v2-payload-fit": (
        ipc_v2_payload_fit, "frame_length_prefix_hex", True, (),
    ),
    "endpoint-id-grammar-v1": (endpoint_id_grammar_v1, "valid", False, ()),
    "human-chat-v2-envelope": (human_chat_v2_envelope, "valid", False, ()),
    "config-v2-cross-field": (config_v2_cross_field, "valid", False, ()),
}


# --- prose copies ---------------------------------------------------------
# A frozen value is quoted in prose as well as stored in a vector file:
# ADR-0047 lists the re-frozen goldens, ENDPOINTS.md shows one inline,
# testing.md cites it in a conformance list. Those copies are useful and
# should stay — but only the vector file is recomputed, so a legitimate
# re-freeze would leave the prose confidently wrong in exactly the
# documents a reader trusts most. ADR-0047 re-froze these once already.
#
# So: every input that produces a stored hash is recomputed above; here
# the stored hash is looked for in the tracked tree, and any file quoting
# a DIFFERENT 64-hex value on the same line as the vector's own inputs is
# reported. This deliberately does not try to parse prose — it asks the
# narrow question "does some file quote a stale hash for this vector",
# which is the drift that matters.

WINDOW = 4

HEX64_RE = re.compile(r"\b([0-9a-f]{64})\b")

SKIP_DIRS = {".git", "target", "node_modules", ".claude"}


def golden_marker(vector: dict, marker_fields: tuple[str, ...]) -> str | None:
    """The text a prose copy of this vector is recognised by.

    Drawn from the INPUT fields the algorithm declares, in order, because
    an input is what a document quotes next to the hash. `payload_hex` is
    decoded first where it is one of them: it is the actual hash input,
    while `payload_utf8` is a reader convenience a fixture may omit, so
    depending on the latter would stop attributing anything the moment
    one did.

    Returns None when nothing usable is present, and the caller reports
    that rather than scanning nothing and calling it coverage.
    """
    for field in marker_fields:
        raw = vector.get(field)
        if raw is None:
            continue
        if field == "payload_hex":
            try:
                decoded = bytes.fromhex(str(raw)).decode("utf-8")
            except (ValueError, UnicodeDecodeError):
                continue
            if decoded.isprintable() and decoded.strip():
                return decoded
            continue
        text = str(raw)
        if text.strip():
            return text
    return None


def check_prose_copies(
    root: pathlib.Path,
    fixture_rel: pathlib.Path,
    doc: dict,
    marker_fields: tuple[str, ...],
) -> int:
    """Report prose that quotes a stale hash for a vector. Returns files scanned."""
    known: set[str] = {v["sha256"] for v in doc.get("vectors", []) if v.get("sha256")}
    if not known:
        return 0

    # A vector is identified in prose by its distinctive inputs. Only the
    # goldens are quoted in prose, and only they carry `frozen_by`.
    goldens = [v for v in doc.get("vectors", []) if v.get("frozen_by")]
    if not goldens:
        return 0

    # A file whose goldens are declared unquoted is not scanned, and says
    # so by carrying no marker fields. A file that DOES declare them and
    # still yields no marker is a defect: it would scan nothing and be
    # counted alongside the files that scanned everything.
    if not marker_fields:
        return 0
    for g in goldens:
        if golden_marker(g, marker_fields) is None:
            report(
                f"{fixture_rel}[{g.get('name', '(unnamed)')}]: none of the declared "
                f"marker fields {list(marker_fields)} are present, so no prose copy of "
                "this golden can be attributed to it"
            )
            return 0

    scanned = 0
    for path in sorted(root.rglob("*")):
        if not path.is_file():
            continue
        if any(part in SKIP_DIRS for part in path.relative_to(root).parts):
            continue
        if path.suffix not in {".md", ".txt", ".json", ".yaml", ".yml"}:
            continue
        rel = path.relative_to(root)
        if rel == fixture_rel:
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
        if "SHA-256" not in text and "sha256" not in text:
            continue
        scanned += 1

        # PROXIMITY, not whole-file. ADR-0047 lists four different frozen
        # goldens in one document — the fingerprint, the topic key, the
        # GossipSub message ID, the Kademlia namespace — so "this file
        # mentions the inputs somewhere and also contains a hash" flags
        # every neighbour. A hash is attributed to a vector only when
        # that vector's own inputs appear in the few lines leading up to
        # it, which is how all three prose copies are actually written.
        lines = text.splitlines()
        for i, line in enumerate(lines):
            found = HEX64_RE.findall(line)
            if not found:
                continue
            window = "\n".join(lines[max(0, i - WINDOW):i + 1])
            for g in goldens:
                marker = golden_marker(g, marker_fields)
                media = g.get("media_type")
                if not marker or marker not in window:
                    continue
                if media and media not in window:
                    continue
                for h in found:
                    # Compared against THIS golden's hash, not merely
                    # membership in the file. Hashes are input-specific,
                    # so prose that quotes a neighbouring edge vector's
                    # value beside these inputs is wrong in the way that
                    # matters — and a membership test would pass it.
                    if h != g["sha256"]:
                        report(
                            f"{rel}:{i + 1}: quotes SHA-256 {h} for the "
                            f"{g['name']} inputs, which should be "
                            f"{g['sha256']} per {fixture_rel}"
                        )
    return scanned


def main(argv: list[str]) -> int:
    root = pathlib.Path(__file__).resolve().parent.parent.parent

    args = argv[1:]
    while args:
        a = args.pop(0)
        if a in ("-h", "--help"):
            show_help()
            return 0
        if a == "--root":
            if not args:
                print("--root needs a value", file=sys.stderr)
                return 2
            root = pathlib.Path(args.pop(0)).resolve()
        else:
            print(f"verify_fixture_vectors: unexpected argument: {a}", file=sys.stderr)
            return 2

    fixtures = root / "fixtures"
    if not fixtures.is_dir():
        print(f"verify_fixture_vectors: not a directory: {fixtures}", file=sys.stderr)
        return 2

    files = sorted(fixtures.rglob("*.json"))
    checked = 0
    prose_scanned = 0

    for path in files:
        rel = path.relative_to(root)
        try:
            doc = json.loads(path.read_text(encoding="utf-8"))
        except json.JSONDecodeError as e:
            report(f"{rel}: invalid JSON — {e}")
            continue

        if not isinstance(doc, dict) or "vectors" not in doc:
            continue

        alg_id = doc.get("algorithm", {}).get("id")
        entry = ALGORITHMS.get(alg_id)
        if entry is None:
            report(
                f"{rel}: declares algorithm '{alg_id}', which this verifier cannot "
                "compute — an unverifiable vector file is the failure, not a skip"
            )
            continue

        fn, field, distinct_required, marker_fields = entry
        fields = (field,) if isinstance(field, str) else tuple(field)

        seen: dict[str, str] = {}
        for v in doc.get("vectors", []):
            name = v.get("name", "(unnamed)")
            try:
                result = fn(v)
            except Exception as e:  # noqa: BLE001 — the message is the report
                report(f"{rel}[{name}]: cannot compute — {e}")
                continue
            checked += 1
            # An algorithm freezing more than one field returns them all.
            # Every declared field is compared, so a value the fixture
            # publishes as frozen cannot go unchecked beside one that is.
            results = result if isinstance(result, dict) else {fields[0]: result}
            for f in fields:
                stored = v.get(f)
                computed = results.get(f)
                if computed != stored:
                    report(
                        f"{rel}[{name}]: DRIFT in {f}\n"
                        f"      stored:   {stored}\n"
                        f"      computed: {computed}"
                    )
            if not distinct_required:
                continue
            computed = results[fields[0]]
            if computed in seen:
                report(
                    f"{rel}[{name}]: collides with '{seen[computed]}' — "
                    "distinct inputs must produce distinct fingerprints"
                )
            else:
                seen[computed] = name

        # ANCHORING. Every vector file must trace to a decision, but the
        # two ways that happens are different and both are legitimate:
        #
        #   per-vector `frozen_by` — this exact value was published by an
        #     ADR and re-frozen there, so the ADR is the authority for the
        #     number itself;
        #   file-level `adr`       — the ALGORITHM was decided by these
        #     ADRs and the vectors are derived from it, which is the
        #     normal case for a layout with no published golden.
        #
        # Requiring `frozen_by` alone would push a derived-vector file
        # into either inventing a golden or going unanchored, and the
        # second is what this check exists to prevent.
        anchors = [a for a in doc.get("adr", []) if a]
        if not anchors and not any(v.get("frozen_by") for v in doc.get("vectors", [])):
            report(
                f"{rel}: nothing anchors this file to a decision — give it a "
                "file-level `adr` list, or mark an ADR-published golden with `frozen_by`"
            )
        for a in anchors + [v["frozen_by"] for v in doc.get("vectors", []) if v.get("frozen_by")]:
            if not list((root / "architecture" / "adr").glob(f"{a}-*.md")):
                report(f"{rel}: cites ADR-{a}, which does not exist")

        prose_scanned += check_prose_copies(root, rel, doc, marker_fields)

    if problems:
        print(f"\nverify_fixture_vectors: {len(problems)} problem(s).", file=sys.stderr)
        return 1

    print(
        f"verify_fixture_vectors: OK — {checked} vectors recomputed and matched, "
        f"{prose_scanned} prose file(s) checked for stale copies."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
