# ADR-0004: Prefer X-Forwarded-For over X-Real-IP behind trusted proxies

## Status

Accepted

## Context

`client_ip()` in `crates/auth/src/rbac.rs` resolves the address written to
audit log INET columns. The prior implementation preferred `X-Real-IP` over
`X-Forwarded-For`, on the reasoning that nginx sets `X-Real-IP` to
`$remote_addr` and a caller cannot influence it.

That reasoning holds for a single in-cluster nginx hop, but not once a second
trusted hop is introduced (an edge proxy in front of nginx). In that topology,
nginx's own `$remote_addr` is the edge proxy's address, not the originating
client — so `X-Real-IP` degrades to the wrong hop, while `X-Forwarded-For`
still carries the full chain if each proxy appends rather than overwrites.
Audit records deanonymizing to a proxy's address instead of the client is a
correctness problem for any deployment with more than one trusted hop.

`X-Forwarded-For` is untrusted as raw input: only entries appended after the
list left attacker control are safe to read, and only when the immediate peer
is itself a trusted proxy.

## Decision

When the immediate peer is a trusted proxy (`is_trusted_proxy`, checked
against the configured `trusted` `IpNet` list), walk `X-Forwarded-For`
right-to-left and return the first entry that is *not* itself a trusted
proxy. This strips every trusted hop from the end of the chain and stops at
the first untrusted address, which is the real client — regardless of how
many trusted hops sit between the client and the gateway.

`X-Real-IP` is now consulted only as a fallback, if `X-Forwarded-For` is
absent or contains no untrusted address. This preserves single-hop nginx
deployments as a working default while fixing multi-hop ones.

An untrusted peer can never satisfy `is_trusted_proxy`, so this change does
not create a new spoofing surface: a direct or untrusted connection still
falls through to `peer_ip`, exactly as before. The three new unit tests in
`rbac.rs` cover the multi-hop case, a value injected before the real client,
and confirm untrusted peers can't supply either header at all.

## Consequences

Audit records now show the correct client IP in deployments with more than
one trusted proxy hop (e.g. an external load balancer plus the in-cluster
nginx). Single-hop deployments are unaffected. Operators must ensure the
`trusted` proxy list in configuration includes every hop between the client
and the gateway, not just the last one — an incomplete list causes the walk
to stop early and misattribute the client address to an intermediate proxy.
