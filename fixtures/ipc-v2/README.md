# ipc-v2

IPC hello/error/frame/max-payload and endpoint-claim fixtures.

`ipc-v2-payload-fit.json` — the payload-fit invariant, both directions, as the 4-byte big-endian frame length prefix the wire would emit. The maximal `send-params` body is 66,096 bytes and the maximal `message-received` body 66,246, each leaving ~64 KB under the 131,072-byte ceiling.

Measured over the schema-defined object; the outer request/event envelope is modelled by no schema and is reported as headroom rather than invented. Hello/error/endpoint-claim behaviour is not a vector and belongs in the IPC conformance suite.
