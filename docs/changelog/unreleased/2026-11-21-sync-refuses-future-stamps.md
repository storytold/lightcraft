## Device sync

- **Changes stamped in the future are refused:** a synced change more than five minutes ahead of this device's
  clock is dropped with a log line naming the device, so a wrong clock can't win every later conflict.
