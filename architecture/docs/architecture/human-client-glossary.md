# Human client — glossary of person-facing words

Status: English baseline. Owned by language-culture; each value is supplied
by locator and committed by the caller.

This file names the words the human client uses for a person, and the
words that stay out. Each row names a state or thing defined elsewhere.
It defines no state of its own. The "Names" column cites the definition.
The interface text itself lives in `crates/human/ui-model/src/labels.rs`
and its platform equivalents, never here. A later locale adds a column to
each table; it does not add a file.

English is global English: plain words, short sentences, no idioms, en-US
spelling. Sentence case, with no final period for labels, buttons and
headings. A complete sentence ends with a period.

## 1. Identity, trust and route words

| Term (en) | Names | Stays out |
|---|---|---|
| PeerId | The profile's network identity, shown and copied in exact canonical form (human-client-ui.md §2, §11). Never translated or reshaped. | account, user name, ID, address, key |
| profile | One identity with its settings on this device (human-client-ui.md §2). | account, user |
| peer | Another device's PeerId as this profile sees it (human-client-ui.md §3). | contact, user, person, friend; a contact is application metadata and is not authenticated |
| trust, trusted | A peer on this profile's list of trusted peers may exchange messages with it (human-client-ui.md §8). | allow, approve, verify, friend, follow |
| Remove trust | The removal action; its confirmation says that connections close (human-client-ui.md §8). | block, ban, delete, revoke |
| list of trusted peers | The list the trust settings show and change (human-client-ui.md §8). | allowlist, whitelist, trust list |
| route | A remote PeerId with the EndpointId it asserts (human-client-ui.md §3, §4). It is a routing label, never a name. | address, account, recipient, identity |
| endpoint | This device's routing selector (the local EndpointId), named to a person only in errors about this device (human-client-ui.md §12). A remote EndpointId is shown as part of a route, never as "endpoint". | identity, user, role, account |
| Connected directly | The route indicator for a direct path (human-client-ui.md §7). | online (as a route indicator), secure |
| Connected through a relay | The route indicator for a relayed path (human-client-ui.md §7). | slow, indirect, insecure, via server |
| Online, reachable directly; Online, reachable through a relay; Online, some peers might not reach you; Offline, transport daemon not running; Network status not known yet | The five normalized connectivity states (human-client-ui.md §7), as worded in labels.rs. Unknown never reads as offline. The Offline text names the daemon, so it is desktop-only (see the next rows). | disconnected, signal |
| transport daemon | DESKTOP ONLY: the separate process that runs the transport (human-client-ui.md §8: the admin IPC). Android has no daemon: the runtime runs inside the app's foreground service (human-client-android.md, "Deployment decision"). | on Android, any text that names a daemon |
| the network service | ANDROID: the foreground service that hosts the transport (human-client-android.md, "Stay reachable"). Proposed; it takes effect when the Android keys exist. | daemon, server, background task |

## 2. Message status words

| Term (en) | Names | Stays out |
|---|---|---|
| Sending, Not confirmed, Not sent | Outbound states before a terminal one (human-client-ui.md §5). "Not sent" waits for the person to send again or cancel; it is not final. "Not confirmed" never says failed. | failed, error, lost |
| Accepted by remote transport | `AcceptedV2`: the remote side's transport queue admitted it (human-client-ui.md §5). | read, seen, delivered, received, processed |
| Accepted by local transport | A broadcast publication accepted by this device (human-client-ui.md §5). | published to everyone, sent to all, delivered |
| may have reached the peer | An earlier attempt may have been admitted remotely. | received, read |
| Unread | Inbound, not yet read on this device (human-client-ui.md §6). | new (as a status label), unseen |
| Read, not kept | Inbound, read on this device and not kept: removed at restart (human-client-ui.md §6). "Read" here is this person's own reading, never a receipt to the sender. | seen by them, opened |
| Kept | Inbound, read and kept by the receiver (human-client-ui.md §6). | saved, archived, starred |
| Keep, Stop keeping | The receiver-local retention action (human-client-ui.md §6). | save, archive, pin, star |

## 3. Notification words (Android step 4)

| Term (en) | Names | Stays out |
|---|---|---|
| Stay reachable | The opt-in mode in which the network service runs with a persistent notification (human-client-android.md, "Stay reachable"; ADR-0041). The mode's name, not a promise: the "reachable" row below holds. Translators: render it as a request ("try to stay reachable"), not as a guarantee. | always on, always online, guaranteed, push |
| Only while open | Foreground-only mode: reachable only while the app is open on the screen (human-client-android.md, "Foreground-only"). | offline mode, limited mode |
| reachable | Other peers can open a connection to this profile now. It is never a promise for the future (ADR-0041, Operational implications). | always, guaranteed, never miss a message |
| New message | A local notification for an inbound message already committed as unread (human-client-android.md, "Notifications"). | delivered, received from (a person's name as authenticated) |
| message preview | The message text a notification may show; the person chooses whether it is shown, and whether on the lock screen (human-client-ui.md §10). | snippet, content |
| unlock | Device credential or biometric authorization that releases the profile's key under user-presence (android-key-custody.md, "user-presence"). Person-facing: "unlock with your screen lock". | log in, sign in, password, PIN (unless the device asks for one) |
| After a restart, … until you unlock | The `background_restart_requires_user_authentication` diagnostic (human-client-ui.md §14). Never claims reachability across a restart. | automatically, stays reachable after a restart |

## 4. Recovery and phrase words (Android steps 7 and 8)

| Term (en) | Names | Stays out |
|---|---|---|
| recovery phrase | The 24 words that represent the profile's identity secret (android-key-custody.md, "Recovery"; human-client-ui.md §9). It restores the identity only: configuration, trust and endpoint settings are backed up separately (human-client-ui.md §9). | seed, seed phrase, mnemonic, password, backup code, secret key, backup (for the phrase) |
| Write the words down | The only way to keep the phrase: no copy, no screenshot (android-key-custody.md, "Recovery UI hardening"). | save, copy or export (as ways to keep the phrase) |
| word, word 3 of 24 | One position of the phrase, chosen from the word list (android-key-custody.md, "Recovery UI hardening"). | key, code, token |
| Restore from the recovery phrase | Recovery of an established profile's PeerId from its phrase, checked against the expected PeerId (ADR-0033; android-key-custody.md). | reset, new profile, create a key, sign in |
| needs its recovery phrase | The recovery-required state: no valid wrapped identity on this device (android-key-custody.md, "Android backup and device-transfer policy"). | lost, corrupted, deleted, hacked |
| can no longer unlock this profile | Keystore key missing or invalidated: the device refuses the wrapped identity (android-key-custody.md, "Failure behavior"). Says what happened, never a guessed cause. | Keystore, invalidated, hacked, tampered |
| is not the PeerId this profile expects | Expected PeerId mismatch after unwrap or restore (android-key-custody.md, "Failure behavior"). | wrong password, invalid account |
| Copying is turned off for the recovery phrase | The no-clipboard path (android-key-custody.md, "Recovery UI hardening"). Explains the absence, tells the person to write the words down. | disabled for your security (as a slogan), not supported |
| Screenshots are blocked on this screen | `FLAG_SECURE` on the recovery Activity (android-key-custody.md). | secure mode, protected mode |
| one device at a time | A restored PeerId does not run on two devices at once; restoring on a new device is moving, not copying (android-key-custody.md, "Recovery"). | sync, copy to another device, multi-device |

## 5. Store-listing words (Android step 10)

| Term (en) | Names | Stays out until the owner verifies the fact |
|---|---|---|
| peer-to-peer | Messages travel between devices over libp2p (ADR-0041; human-client-android.md, "Deployment decision"). | serverless (relays exist), decentralized (as a slogan) |
| no account | The profile is a device identity, not an account with a provider (human-client-ui.md §2). | anonymous, untraceable |
| no central message store | No transport mailbox; messages arrive only while the app is reachable (human-client-android.md, "Process death and offline semantics"). | messages are never stored (the app stores unread and kept messages on the device) |
| encrypted in transit | Transport connections are encrypted. The listing says only what the owner verifies against the code. | end-to-end encrypted, military-grade, unbreakable, private |
| relay | A device that forwards a connection when a direct path is not possible (human-client-ui.md §7; human-client-android.md, "Android discovery/resource profile"). | server (as if central), cloud |
