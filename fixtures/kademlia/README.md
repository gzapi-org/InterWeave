# kademlia

Private Kademlia network/protocol-name derivation vectors.

`kad-network-namespace-v1.json` — lowercase unpadded base32 of the first 16 bytes of the domain-separated digest, with the derived `/interweave/kad/1.0.0/<network-hash>` protocol string per vector. Includes the ADR-0047 golden.

The 16-byte truncation is the part worth freezing: hashing to a full digest yields a plausible namespace nobody else computes.
