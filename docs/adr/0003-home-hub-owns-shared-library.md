# Home Hub owns the Shared Library

Status: accepted

Audetic will synchronize personal data with a hub-and-spoke topology, not peer-to-peer database replication. One user-designated Home Hub owns the Shared Library, shared mutations, deletion markers, and Shared Configuration; Connected Devices remain fully capable local Audetic installations, durably upload successful results when reachable, and normally browse the Shared Library live through the hub over Tailscale.

Full multi-master replication was rejected because offline edit reconciliation for records, files, deletions, artifacts, settings, and schema changes would turn a small personal feature into a distributed database. Directly synchronizing SQLite and absolute-path-backed files was rejected because concurrent copies are unsafe. Optional Library Caches remain read-only while offline, so they improve availability without creating additional authorities. Manual hub replacement from a complete cache or backup is supported; election and automatic failover are not.

Tailscale Serve provides private HTTPS transport and caller identity. Audetic exposes a narrow sync API on a separate loopback listener rather than exposing the daemon's local recording and machine-control API. The first version authorizes the same Tailscale user who configured the Home Hub and keeps provider credentials device-local.
