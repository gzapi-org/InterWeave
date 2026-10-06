// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! SPIKE-009, the host half: exact-key custody through a versioned
//! AES-256-GCM envelope, against the production identity derivation.
//!
//! Evidence only. Every observation is a counted check, and the process
//! exits non-zero when any is false or none ran, so `cargo run` cannot
//! report success while its own output disproves the record.
//!
//! The wrapping key here is a software AES-256 key standing in for an
//! AndroidKeyStore key: what this proves is the envelope and the
//! derivation, never the Keystore (the device half's).

mod envelope;

use envelope::{Policy, Refused};
use interweave_profile_identity::{ProfileIdentity, RecoveryPhrase};

struct Report {
    failures: Vec<String>,
    checks: usize,
}

impl Report {
    fn check(&mut self, id: &str, claim: &str, held: bool) {
        self.checks += 1;
        println!("  [{}] {id} {claim}", if held { "ok" } else { "FAIL" });
        if !held {
            self.failures.push(format!("{id}: {claim}"));
        }
    }

    fn note(text: &str) {
        println!("  [note] {text}");
    }
}

/// The production derivation: entropy IS the Ed25519 seed (ADR-0033).
fn peer_of(seed: &[u8; 32]) -> Option<String> {
    let phrase = RecoveryPhrase::from_entropy(seed).ok()?;
    let identity = ProfileIdentity::from_phrase(&phrase).ok()?;
    Some(identity.transport_identity().ok()?.as_str().to_owned())
}

fn random_key() -> [u8; 32] {
    use aes_gcm::aead::{KeyInit, OsRng};
    aes_gcm::Aes256Gcm::generate_key(&mut OsRng).into()
}

fn main() {
    let mut r = Report {
        failures: Vec::new(),
        checks: 0,
    };

    // The frozen golden (fixtures/identity, TEST-ONLY public vector).
    let fixture: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../fixtures/identity/ed25519-bip39-entropy-v1.json"),
        )
        .expect("the identity fixture"),
    )
    .expect("the fixture parses");
    let golden = &fixture["vectors"][0];
    let seed: [u8; 32] = hex::decode(golden["entropy_hex"].as_str().expect("hex"))
        .expect("hex decodes")
        .try_into()
        .expect("32 bytes");
    let peer = golden["expected_peer_id"].as_str().expect("a PeerId").to_owned();

    println!("H1 -- the fixture seed derives the frozen PeerId");
    r.check("H1", "the production derivation gives the golden PeerId", peer_of(&seed).as_deref() == Some(peer.as_str()));

    println!("H2 -- round trip, both unlock policies");
    let key = random_key();
    for policy in [Policy::BackgroundCompatible, Policy::UserPresence] {
        let e = envelope::wrap(&key, policy, &peer, &seed);
        r.check("H2", &format!("{policy:?}: the envelope is {} bytes", envelope::LEN), e.len() == envelope::LEN);
        let back = envelope::unwrap(&key, &peer, &e, peer_of);
        r.check("H2", &format!("{policy:?}: unwrap gives the exact seed back"), back == Ok(seed));
        r.check(
            "H2",
            &format!("{policy:?}: that seed derives the same PeerId"),
            back.ok().and_then(|s| peer_of(&s)).as_deref() == Some(peer.as_str()),
        );
    }
    let (a, b) = (
        envelope::wrap(&key, Policy::BackgroundCompatible, &peer, &seed),
        envelope::wrap(&key, Policy::BackgroundCompatible, &peer, &seed),
    );
    r.check("H2", "two wraps of one seed differ (a fresh IV each)", a != b && a[6..18] != b[6..18]);

    println!("H3 -- every single-bit flip, in every byte, is refused");
    let e = envelope::wrap(&key, Policy::UserPresence, &peer, &seed);
    let mut refused_all = true;
    let mut kinds = std::collections::BTreeMap::<String, usize>::new();
    for byte in 0..e.len() {
        for bit in 0..8 {
            let mut t = e.clone();
            t[byte] ^= 1 << bit;
            match envelope::unwrap(&key, &peer, &t, peer_of) {
                Ok(_) => refused_all = false,
                Err(why) => *kinds.entry(format!("{why:?}")).or_default() += 1,
            }
        }
    }
    r.check("H3", &format!("all {} flips refused, none yields a seed", e.len() * 8), refused_all);
    Report::note(&format!("refusals by kind: {kinds:?}"));

    println!("H4 -- a wrong length is refused");
    r.check("H4", "truncated by one", envelope::unwrap(&key, &peer, &e[..e.len() - 1], peer_of) == Err(Refused::Length));
    let mut longer = e.clone();
    longer.push(0);
    r.check("H4", "extended by one", envelope::unwrap(&key, &peer, &longer, peer_of) == Err(Refused::Length));
    r.check("H4", "empty", envelope::unwrap(&key, &peer, &[], peer_of) == Err(Refused::Length));

    println!("H5 -- another wrapping key (a Keystore key invalidated and made again) is refused");
    r.check("H5", "a new key fails authentication", envelope::unwrap(&random_key(), &peer, &e, peer_of) == Err(Refused::Authentication));

    println!("H6 -- the header is checked or authenticated, never trusted");
    let mut t = e.clone();
    t[0] = b'X';
    r.check("H6", "a wrong magic is refused", envelope::unwrap(&key, &peer, &t, peer_of) == Err(Refused::Magic));
    let mut t = e.clone();
    t[4] = 2;
    r.check("H6", "an unknown version is refused", envelope::unwrap(&key, &peer, &t, peer_of) == Err(Refused::Version));
    let mut t = e.clone();
    t[5] = 7;
    r.check("H6", "an unknown policy is refused", envelope::unwrap(&key, &peer, &t, peer_of) == Err(Refused::Policy));
    let mut t = e.clone();
    t[5] = Policy::BackgroundCompatible as u8;
    r.check(
        "H6",
        "a VALID policy swapped (user-presence to background) fails authentication",
        envelope::unwrap(&key, &peer, &t, peer_of) == Err(Refused::Authentication),
    );

    println!("H7 -- an envelope belongs to one profile");
    // TEST-ONLY synthetic seed: a fixed second identity, no real key material.
    let other = peer_of(&[7u8; 32]).expect("another PeerId");
    r.check(
        "H7",
        "unwrapped for another PeerId, it fails authentication",
        envelope::unwrap(&key, &other, &e, peer_of) == Err(Refused::Authentication),
    );
    let mismatched = envelope::wrap(&key, Policy::BackgroundCompatible, &peer, &[7u8; 32]);
    r.check(
        "H7",
        "a seed that authenticates but derives another identity is refused",
        envelope::unwrap(&key, &peer, &mismatched, peer_of) == Err(Refused::WrongIdentity),
    );

    println!("H8 -- random identities round-trip exactly");
    let mut exact = true;
    for _ in 0..100 {
        let id = ProfileIdentity::generate();
        let s = id.recovery_phrase().expect("a phrase").expose_entropy().expect("32 bytes");
        let p = id.transport_identity().expect("a PeerId").as_str().to_owned();
        let k = random_key();
        let w = envelope::wrap(&k, Policy::UserPresence, &p, &s);
        let back = envelope::unwrap(&k, &p, &w, peer_of);
        exact &= back == Ok(s) && back.ok().and_then(|s| peer_of(&s)).as_deref() == Some(p.as_str());
    }
    r.check("H8", "100 of 100 random identities give back the exact seed and PeerId", exact);

    println!();
    if r.checks == 0 {
        eprintln!("no check ran");
        std::process::exit(1);
    }
    if r.failures.is_empty() {
        println!("all {} checks held", r.checks);
    } else {
        eprintln!("{} of {} checks FAILED:", r.failures.len(), r.checks);
        for f in &r.failures {
            eprintln!("  {f}");
        }
        std::process::exit(1);
    }
}
