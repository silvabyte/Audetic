# Library Sync

Library Sync gathers records from a person's Audetic devices into one place without requiring an Audetic-hosted service.

## Language

**Home Hub**:
The designated, usually available Audetic installation that owns the Shared Library.
_Avoid_: Cloud, primary replica, sync server

**Connected Device**:
An Audetic installation that contributes its completed records to a Home Hub and normally browses the Shared Library through that hub. It retains its own local records and remains usable when the Home Hub is unavailable.
_Avoid_: Peer, replica, mesh node

**Shared Library**:
The combined history accepted by the Home Hub from all connected Audetic devices.
_Avoid_: Replica, cloud database

**Pending Upload**:
A record completed on a device but not yet accepted by the Home Hub. It remains available on its originating device while waiting.
_Avoid_: Unsynced replica, conflict

**Library Cache**:
An optional, read-only local copy of Shared Library records for offline browsing. The Home Hub remains authoritative for edits and deletion.
_Avoid_: Replica, local authority, offline sync

**Shared Configuration**:
Settings owned by the Home Hub and applied to every Connected Device that has not opted out. Changing a shared setting on an opted-in device changes it for all opted-in devices.
_Avoid_: Synced config file, replicated settings

**Device-Local Setting**:
A setting whose meaning or value belongs to one device, including credentials and machine-specific paths. It is never overwritten by Shared Configuration.
_Avoid_: Sync exception, unsynced field

**Effective Configuration**:
The settings a device actually uses after applying Shared Configuration, when enabled, together with its Device-Local Settings.
_Avoid_: Merged config file, resolved sync settings

**Recording Payload**:
The audio associated with a record. Syncing it is optional independently of the record's text and metadata.
_Avoid_: Record, meeting data
