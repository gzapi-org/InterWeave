# store

SQLite implementation of pending_outbound, unread_inbound, kept_inbound, contacts/preferences; deliberately no general durable history API.

**Current status:** Stage 3, active workspace member. The three ADR-0044 states and nothing else: pending outbound, unread inbound, and inbound the receiver explicitly kept. `verify_shape` refuses any other table, view, or trigger on every open, so the absence of a general history API is enforced rather than merely intended.
