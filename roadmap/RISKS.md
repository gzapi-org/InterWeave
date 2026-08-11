# Architecture and implementation risks

| Risk | Current treatment | Trigger / owner action |
|---|---|---|
| Claude Channel research-preview contract changes | isolated bridge + SPIKE-001 | revalidate before bridge implementation/release |
| MCP 2026 protocol evolution vs Claude embedded SDK | Claude docs authoritative for Channel; compatibility spike | pin supported SDK and record exact target |
| daemon adds deployment complexity | profile lifecycle/IPC explicit | measure install/update friction before broad release |
| same-user IPC compromise | OS ACLs only in v1 | harden if threat model includes hostile same-user code |
| GossipSub plaintext at forwarding peers | documented; trust peer set | add higher-layer/group encryption only with concrete design |
| static trust does not scale | deliberate safe v1 | design signed membership/enterprise policy when needed |
| Kademlia poisoning/complexity | deferred | SPIKE-003 + explicit ADR update |
| NAT prevents remote operation | narrow v1 reachability | SPIKE-004; optional relay/NAT features |
| Sybil/eclipse in permissive future modes | deny-by-default v1 | require new policy/scoring design before AllowAll/public networks |
| backpressure causes message loss | explicit best-effort + bounded queues | tune limits and surface counters; do not add hidden spool |
| payload limit too small for intended protocols | 48 KiB v1 | use higher-level chunking/object transfer or future capability revision |
| topic hash dictionary guessing | privacy hardening only | keyed topic derivation if namespace privacy becomes required |
| profile key loss breaks trust continuity | backup/explicit rotation docs | future signed rotation or managed identity system |
| request-response acceptance misread as app ack | precise contract/tool wording | integration tests/documentation enforce wording |
| over-abstraction in Rust workspace | public traits restricted to real variation | collapse crate/module granularity while keeping dependency direction |
| discovery/provider config schema churn | tagged namespaces | add config migrations only when a provider truly changes schema |
