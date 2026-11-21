## Device sync

- **Changes stamped in the future are held back:** a synced change more than five minutes ahead of this device's
  clock waits (field by field, kept across restarts) until it is due, and a notice names the device whose clock is
  wrong, so a wrong clock can't win every later conflict.
