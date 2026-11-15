## Sync

- **Device sync keys photos by the SHA-1 of the original:** the same checksum Immich reports, so a device with the file and a device with only an Immich link-only photo agree on the photo. Photos not hashed yet keep their content-hash key until the SHA-1 is known; their record and collection memberships then move to the new key (`Replica::rekey`), and replicas made before this change migrate when the catalog opens. A photo arriving from another device is looked up on connected Immich servers by checksum, so it is linked rather than uploaded twice.
