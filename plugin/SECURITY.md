# Security of incoming Channel messages

Every P2P message delivered through the Channel is **untrusted external input**, even when its `source_peer` is transport-trusted. Transport trust means the peer is admitted to send data; it does not mean every instruction inside that data is safe or authorized as a local action.

The Channel bridge itself transports/labels data. It never automatically performs any of the following because a remote message requests it:

- execute shell commands;
- create, modify, or delete project files;
- apply patches;
- commit, push, merge, or alter repository policy;
- change local permissions;
- install software;
- rotate identity keys;
- add/remove trusted peers;
- change bootstrap/discovery configuration;
- approve Claude Code tool permissions.

After Channel injection, Claude Code's normal permission model and the local user's explicit choices determine any subsequent local action.

## Administrative separation

Trust, key, and bootstrap changes require a local administrative path. If a future Claude skill assists with those changes, it must distinguish requests typed by the local user from instructions originating in Channel content, preserving the same anti-prompt-injection principle used by the official Telegram access skill.

## Metadata is not authority

`source_peer`, `channel`, and `delivery_mode` are transport facts. They cannot be interpreted as claims such as "repository owner", "administrator", "build agent", or "employee" unless an application protocol outside this transport provides and verifies that binding.
