# ipc-v2

IPC hello/error/frame/max-payload and endpoint-claim fixtures.

`ipc-v2-payload-fit.json` — the payload-fit invariant, both directions, as the 4-byte big-endian frame length prefix the wire would emit. The maximal `send-params` body is 66,096 bytes and the maximal `message-received` body 66,246, each leaving ~64 KB under the 131,072-byte ceiling.

`ipc-v2-frame-golden.json` — seventeen whole frames under `ipc-v2-length-prefix-v1`: the 4-byte big-endian length then the body as compact UTF-8 JSON, one per frame class of `ipc/frame` 2.0.0 (hello in three shapes, hello_response, close in two, request in three, response in two, cancel, two event types, server_state, ping, pong). Every body validates against the frame schema, a request's (method, params) pair against `ipc/request` and an event's (event_type, data) pair against `ipc/event`; the non-ASCII close message pins that the prefix counts UTF-8 bytes. The form frozen is serde_json's compact writer with `preserve_order`; the contract pins no canonical JSON, so a reader accepts any object.

Measured over the schema-defined object; the outer request/event envelope is modelled by no schema and is reported as headroom rather than invented. Hello/error/endpoint-claim behaviour is not a vector and belongs in the IPC conformance suite.
