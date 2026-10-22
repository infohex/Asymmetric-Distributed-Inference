# adi-node

Node agent: registers with the scheduler, pulls rounds of U (long-poll), fetches inputs by content hash once and caches them, computes the round with rayon (one task per U, `--threads` = physical cores by default), uploads each result block, reports. The only kernel in prototype A is the strict CPU block matmul (`adi_core::blobfmt::block_matmul_strict`); `trait Kernel` is where a GPU kernel goes later.

```
adi-node --scheduler http://HOST:7700 --name NAME [--tier general] [--threads N]
```

- `--name` must be unique across the fleet: re-registering a name revokes its previous token and reclaims its in-flight round (that is how a restarted node recovers), so two live agents with the same name evict each other.
- No scheduling parameters on the node: W and the retry interval come from the scheduler.
- Killing the process is how a node goes offline; restart it to reconnect.
- Only outbound connections: the node works from behind NAT.
